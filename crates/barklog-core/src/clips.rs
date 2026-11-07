//! Хранение звука живой записи: только фрагменты вокруг звуков собаки.
//!
//! Восемь часов звука — почти гигабайт, а нужны из них обычно минуты. Пишем в
//! `clips.pcm` (16 кГц, i16 LE) лишь участки вокруг собачьих окон, а в `clips.json` —
//! где каждый участок лежит во времени записи. Потом по этому индексу собирается
//! звук любого эпизода.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use barklog_audio::TARGET_RATE;
use serde::{Deserialize, Serialize};

pub const PCM_FILE: &str = "clips.pcm";
pub const INDEX_FILE: &str = "clips.json";

/// Сколько звука держать в памяти, пока сеть решает, собака ли это.
const RING_SECONDS: usize = 30;

/// Непрерывный кусок записи, сохранённый в файл.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Начало во времени записи, в отсчётах 16 кГц.
    pub rec_start: u64,
    /// Смещение в `clips.pcm`, в отсчётах.
    pub file_offset: u64,
    pub len: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClipIndex {
    pub sample_rate: u32,
    pub segments: Vec<Segment>,
}

/// Пишет нужные участки потока на диск.
pub struct ClipWriter {
    dir: PathBuf,
    out: BufWriter<File>,
    index: ClipIndex,
    /// Последние отсчёты потока; `ring[0]` имеет абсолютный индекс `ring_start`.
    ring: VecDeque<i16>,
    ring_start: u64,
    /// Участки, которые надо сохранить: [start, end) в отсчётах, по возрастанию.
    wanted: Vec<(u64, u64)>,
    /// Всё до этого индекса уже записано (или безвозвратно ушло из буфера).
    written_to: u64,
    file_len: u64,
}

impl ClipWriter {
    pub fn create(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let out = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(dir.join(PCM_FILE))?,
        );
        Ok(Self {
            dir,
            out,
            index: ClipIndex {
                sample_rate: TARGET_RATE,
                segments: Vec::new(),
            },
            ring: VecDeque::with_capacity(RING_SECONDS * TARGET_RATE as usize),
            ring_start: 0,
            wanted: Vec::new(),
            written_to: 0,
            file_len: 0,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Новые отсчёты потока (16 кГц).
    pub fn push(&mut self, samples: &[f32]) -> std::io::Result<()> {
        self.ring.extend(
            samples
                .iter()
                .map(|&x| (x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16),
        );
        self.flush_ready()?;
        let cap = RING_SECONDS * TARGET_RATE as usize;
        if self.ring.len() > cap {
            let drop = self.ring.len() - cap;
            self.ring.drain(..drop);
            self.ring_start += drop as u64;
        }
        Ok(())
    }

    /// Отметить участок [start_ms, end_ms) как нужный.
    pub fn keep(&mut self, start_ms: u64, end_ms: u64) {
        let (s, e) = (ms_to_samples(start_ms), ms_to_samples(end_ms));
        match self.wanted.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => self.wanted.push((s, e)),
        }
    }

    /// Записывает всё, что уже пришло, и сохраняет индекс.
    pub fn finish(mut self) -> std::io::Result<ClipIndex> {
        self.flush_ready()?;
        self.out.flush()?;
        self.save_index()?;
        Ok(self.index)
    }

    /// Сохраняет индекс (можно звать периодически — на случай, если процесс убьют).
    pub fn save_index(&mut self) -> std::io::Result<()> {
        self.out.flush()?;
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&self.index)?)?;
        std::fs::rename(tmp, self.dir.join(INDEX_FILE))
    }

    fn flush_ready(&mut self) -> std::io::Result<()> {
        let ring_end = self.ring_start + self.ring.len() as u64;
        while let Some(&(s, e)) = self.wanted.first() {
            let from = s.max(self.written_to).max(self.ring_start);
            let to = e.min(ring_end);
            if from < to {
                let (a, b) = (
                    (from - self.ring_start) as usize,
                    (to - self.ring_start) as usize,
                );
                let mut bytes = Vec::with_capacity((b - a) * 2);
                for v in self.ring.range(a..b) {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                self.out.write_all(&bytes)?;
                self.add_segment(from, to - from);
                self.written_to = to;
            }
            if e <= ring_end {
                self.wanted.remove(0);
            } else {
                break;
            }
        }
        Ok(())
    }

    fn add_segment(&mut self, rec_start: u64, len: u64) {
        match self.index.segments.last_mut() {
            Some(last) if last.rec_start + last.len == rec_start => last.len += len,
            _ => self.index.segments.push(Segment {
                rec_start,
                file_offset: self.file_len,
                len,
            }),
        }
        self.file_len += len;
    }
}

fn ms_to_samples(ms: u64) -> u64 {
    ms * TARGET_RATE as u64 / 1000
}

/// Собирает звук [start_ms, end_ms) из сохранённых фрагментов. Места, которых нет
/// в файле (там было тихо), заполняются тишиной. Результат — i16 16 кГц.
pub fn read_clip(dir: impl AsRef<Path>, start_ms: u64, end_ms: u64) -> std::io::Result<Vec<i16>> {
    let dir = dir.as_ref();
    let index: ClipIndex = serde_json::from_slice(&std::fs::read(dir.join(INDEX_FILE))?)?;
    let (s, e) = (ms_to_samples(start_ms), ms_to_samples(end_ms.max(start_ms)));
    let mut out = vec![0i16; (e - s) as usize];
    let mut file = File::open(dir.join(PCM_FILE))?;
    for seg in &index.segments {
        let from = s.max(seg.rec_start);
        let to = e.min(seg.rec_start + seg.len);
        if from >= to {
            continue;
        }
        file.seek(SeekFrom::Start(
            (seg.file_offset + from - seg.rec_start) * 2,
        ))?;
        let mut bytes = vec![0u8; ((to - from) * 2) as usize];
        file.read_exact(&mut bytes)?;
        for (o, b) in out[(from - s) as usize..(to - s) as usize]
            .iter_mut()
            .zip(bytes.as_chunks::<2>().0)
        {
            *o = i16::from_le_bytes(*b);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("barklog-clips-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Сигнал, где отсчёт n имеет значение n % 1000 (в шкале i16) — легко проверить, что откуда.
    fn ramp(from: u64, n: usize) -> Vec<f32> {
        (0..n as u64)
            .map(|i| ((from + i) % 1000) as f32 / i16::MAX as f32)
            .collect()
    }

    #[test]
    fn keeps_only_wanted_ranges() {
        let dir = tmp("keep");
        let mut w = ClipWriter::create(&dir).unwrap();
        // 10 секунд звука порциями по 0,1 с; нужны 2–3 с и 6–7 с.
        let mut pos = 0u64;
        for i in 0..100 {
            if i == 30 {
                w.keep(2000, 3000);
            }
            if i == 75 {
                w.keep(6000, 7000);
            }
            w.push(&ramp(pos, 1600)).unwrap();
            pos += 1600;
        }
        let index = w.finish().unwrap();
        assert_eq!(index.segments.len(), 2);
        assert_eq!(
            index.segments[0],
            Segment {
                rec_start: 32_000,
                file_offset: 0,
                len: 16_000
            }
        );
        // Два фрагмента по 16 000 отсчётов по 2 байта.
        assert_eq!(
            std::fs::metadata(dir.join(PCM_FILE)).unwrap().len(),
            2 * 16_000 * 2
        );

        let clip = read_clip(&dir, 2500, 2501).unwrap();
        assert_eq!(clip.len(), 16);
        assert_eq!(clip[0], (40_000 % 1000) as i16);
        // Между фрагментами — тишина.
        assert!(read_clip(&dir, 4000, 5000).unwrap().iter().all(|&v| v == 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlapping_and_future_ranges_merge() {
        let dir = tmp("merge");
        let mut w = ClipWriter::create(&dir).unwrap();
        w.keep(0, 1000);
        w.keep(500, 1500); // пересекается — продолжает тот же участок
        w.push(&ramp(0, 8000)).unwrap(); // 0,5 с — часть ещё в будущем
        w.push(&ramp(8000, 32_000)).unwrap();
        let index = w.finish().unwrap();
        assert_eq!(
            index.segments,
            vec![Segment {
                rec_start: 0,
                file_offset: 0,
                len: 24_000
            }]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
