//! Контрольные точки живой записи: если систему убьёт приложение посреди
//! многочасовой записи, всё проанализированное до последней точки можно сохранить.
//!
//! Рядом с фрагментами звука лежат три файла: `windows.bin` (окна по 4 байта,
//! только дописывается), `embeddings.bin` (номер окна + сжатый эмбеддинг, только
//! дописывается) и `checkpoint.json` — сколько из этого точно записано. Сводка
//! заменяется атомарно, поэтому недописанный хвост файлов при восстановлении
//! просто отбрасывается.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::compact::{decode_windows, encode_windows};
use crate::embedding::Embedding;
use crate::pipeline::{Analysis, WindowResult};
use crate::report::StartTime;
use crate::settings::Sensitivity;

const META_FILE: &str = "checkpoint.json";
const WINDOWS_FILE: &str = "windows.bin";
const EMBEDDINGS_FILE: &str = "embeddings.bin";
const FORMAT: u32 = 1;
/// Номер окна (u32) + эмбеддинг.
const EMBEDDING_RECORD: usize = 4 + Embedding::BYTES;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    format: u32,
    started_at: StartTime,
    sensitivity: Sensitivity,
    /// Метка вызывающей стороны (например, чья это запись).
    tag: Option<String>,
    windows: u64,
    embeddings: u64,
    duration_ms: u64,
}

/// Что удалось восстановить после прерванной записи.
#[derive(Debug)]
pub struct Recovered {
    pub analysis: Analysis,
    pub started_at: StartTime,
    pub sensitivity: Sensitivity,
    pub tag: Option<String>,
}

pub struct CheckpointWriter {
    dir: PathBuf,
    windows: BufWriter<File>,
    embeddings: BufWriter<File>,
    meta: Meta,
}

impl CheckpointWriter {
    pub fn create(
        dir: impl AsRef<Path>,
        started_at: StartTime,
        sensitivity: Sensitivity,
    ) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let open = |name: &str| -> std::io::Result<BufWriter<File>> {
            Ok(BufWriter::new(
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(dir.join(name))?,
            ))
        };
        Ok(Self {
            windows: open(WINDOWS_FILE)?,
            embeddings: open(EMBEDDINGS_FILE)?,
            meta: Meta {
                format: FORMAT,
                started_at,
                sensitivity,
                tag: None,
                windows: 0,
                embeddings: 0,
                duration_ms: 0,
            },
            dir,
        })
    }

    pub fn set_tag(&mut self, tag: Option<String>) {
        self.meta.tag = tag;
    }

    /// Дописывает новые окна и эмбеддинги и обновляет сводку. Окна и эмбеддинги
    /// анализатора только добавляются, поэтому пишется лишь то, что появилось.
    pub fn write(
        &mut self,
        windows: &[WindowResult],
        embeddings: &[(u32, Embedding)],
        duration_ms: u64,
    ) -> std::io::Result<()> {
        let new_windows = &windows[(self.meta.windows as usize).min(windows.len())..];
        self.windows.write_all(&encode_windows(new_windows))?;
        for (idx, e) in &embeddings[(self.meta.embeddings as usize).min(embeddings.len())..] {
            self.embeddings.write_all(&idx.to_le_bytes())?;
            self.embeddings.write_all(&e.to_bytes())?;
        }
        self.windows.flush()?;
        self.embeddings.flush()?;
        self.meta.windows = windows.len() as u64;
        self.meta.embeddings = embeddings.len() as u64;
        self.meta.duration_ms = duration_ms;
        let tmp = self.dir.join(format!("{META_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&self.meta)?)?;
        std::fs::rename(tmp, self.dir.join(META_FILE))
    }
}

/// Есть ли в папке незавершённая запись.
pub fn exists(dir: impl AsRef<Path>) -> bool {
    dir.as_ref().join(META_FILE).exists()
}

/// Читает последнюю контрольную точку; `None`, если её нет или она нечитаема.
pub fn recover(dir: impl AsRef<Path>) -> std::io::Result<Option<Recovered>> {
    let dir = dir.as_ref();
    let Ok(bytes) = std::fs::read(dir.join(META_FILE)) else {
        return Ok(None);
    };
    let Ok(meta) = serde_json::from_slice::<Meta>(&bytes) else {
        return Ok(None);
    };
    if meta.format != FORMAT {
        return Ok(None);
    }
    let windows_bytes = read_prefix(&dir.join(WINDOWS_FILE), meta.windows as usize * 4)?;
    let windows = decode_windows(&windows_bytes);
    let emb_bytes = read_prefix(
        &dir.join(EMBEDDINGS_FILE),
        meta.embeddings as usize * EMBEDDING_RECORD,
    )?;
    let embeddings = emb_bytes
        .as_chunks::<EMBEDDING_RECORD>()
        .0
        .iter()
        .filter_map(|r| {
            let idx = u32::from_le_bytes(r[..4].try_into().ok()?);
            Some((idx, Embedding::from_bytes(&r[4..])?))
        })
        .filter(|(idx, _)| (*idx as usize) < windows.len())
        .collect();
    Ok(Some(Recovered {
        analysis: Analysis {
            duration_ms: meta.duration_ms,
            windows,
            embeddings,
        },
        started_at: meta.started_at,
        sensitivity: meta.sensitivity,
        tag: meta.tag,
    }))
}

/// Убирает контрольную точку — запись сохранена как сессия. Фрагменты звука остаются.
pub fn discard(dir: impl AsRef<Path>) -> std::io::Result<()> {
    let dir = dir.as_ref();
    for name in [META_FILE, WINDOWS_FILE, EMBEDDINGS_FILE] {
        match std::fs::remove_file(dir.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Первые `len` байт файла (меньше, если файл короче — тогда сводка соврать не могла,
/// но хвост просто не успел записаться).
fn read_prefix(path: &Path, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(len);
    match File::open(path) {
        Ok(f) => {
            f.take(len as u64).read_to_end(&mut buf)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClassScores, SoundClass, StartSource};
    use barklog_ml::PATCH_HOP_MS;

    fn window(i: u64, class: SoundClass) -> WindowResult {
        WindowResult {
            start_ms: i * PATCH_HOP_MS,
            class,
            confidence: 0.8,
            rms_db: -30.0,
            peak_db: -12.0,
            gated: false,
            scores: ClassScores::default(),
        }
    }

    #[test]
    fn writes_incrementally_and_recovers() {
        let dir = std::env::temp_dir().join(format!("barklog-ckpt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let start = StartTime {
            unix_ms: 1_000,
            utc_offset_min: 180,
            source: StartSource::Recorded,
        };
        let mut w = CheckpointWriter::create(&dir, start, Sensitivity::default()).unwrap();
        w.set_tag(Some("dog:2".into()));
        assert!(
            recover(&dir).unwrap().is_none(),
            "до первой точки восстанавливать нечего"
        );

        let mut windows: Vec<_> = (0..4).map(|i| window(i, SoundClass::Silence)).collect();
        let mut embs = vec![(1u32, Embedding::quantize(&[0.25; 1024]))];
        w.write(&windows, &embs, 4 * PATCH_HOP_MS).unwrap();
        windows.extend((4..7).map(|i| window(i, SoundClass::Bark)));
        embs.push((5, Embedding::quantize(&[1.0; 1024])));
        w.write(&windows, &embs, 7 * PATCH_HOP_MS).unwrap();
        assert!(exists(&dir));

        // Процесс убит посреди дописывания: хвост файлов длиннее сводки.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(WINDOWS_FILE))
            .unwrap()
            .write_all(&[1, 2, 3])
            .unwrap();

        let r = recover(&dir).unwrap().unwrap();
        assert_eq!(r.tag.as_deref(), Some("dog:2"));
        assert_eq!(r.started_at, start);
        assert_eq!(r.analysis.duration_ms, 7 * PATCH_HOP_MS);
        assert_eq!(r.analysis.windows.len(), 7);
        assert_eq!(r.analysis.windows[5].class, SoundClass::Bark);
        assert_eq!(r.analysis.embeddings.len(), 2);
        assert_eq!(r.analysis.embeddings[1].0, 5);

        discard(&dir).unwrap();
        assert!(!exists(&dir));
        assert!(recover(&dir).unwrap().is_none());
        discard(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
