//! Интерфейс ядра для мобильных приложений (UniFFI → Kotlin / Swift).
//!
//! Весь анализ, хранение и статистика — здесь; приложения только показывают
//! данные и дают доступ к системным API (выбор файла, фон, уведомления).

use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

use barklog_audio::FileSource;
use barklog_core as core;
use barklog_core::{AnalyzeError, ReportSettings, Thresholds, analyze, build_report};
use barklog_ml::Yamnet;
use barklog_store::Store;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum BarkLogError {
    #[error("{reason}")]
    Audio { reason: String },
    #[error("{reason}")]
    Model { reason: String },
    #[error("{reason}")]
    Storage { reason: String },
    #[error("анализ отменён")]
    Cancelled,
    #[error("сессия не найдена")]
    NotFound,
}

impl From<barklog_store::StoreError> for BarkLogError {
    fn from(e: barklog_store::StoreError) -> Self {
        BarkLogError::Storage { reason: e.to_string(),
        }
    }
}

impl From<barklog_audio::AudioError> for BarkLogError {
    fn from(e: barklog_audio::AudioError) -> Self {
        BarkLogError::Audio { reason: e.to_string(),
        }
    }
}

type Result<T> = std::result::Result<T, BarkLogError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SoundKind {
    Bark,
    Whine,
    Howl,
    Growl,
    Other,
    Silence,
}

impl From<core::SoundClass> for SoundKind {
    fn from(c: core::SoundClass) -> Self {
        match c {
            core::SoundClass::Bark => SoundKind::Bark,
            core::SoundClass::Whine => SoundKind::Whine,
            core::SoundClass::Howl => SoundKind::Howl,
            core::SoundClass::Growl => SoundKind::Growl,
            core::SoundClass::Other => SoundKind::Other,
            core::SoundClass::Silence => SoundKind::Silence,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum StartSource {
    Manual,
    FileName,
    FileModified,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct StartInfo {
    pub unix_ms: i64,
    pub utc_offset_min: i32,
    pub source: StartSource,
}

impl From<core::StartTime> for StartInfo {
    fn from(s: core::StartTime) -> Self {
        StartInfo {
            unix_ms: s.unix_ms,
            utc_offset_min: s.utc_offset_min,
            source: match s.source {
                core::StartSource::Manual => StartSource::Manual,
                core::StartSource::FileName => StartSource::FileName,
                core::StartSource::FileModified => StartSource::FileModified,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SessionSummary {
    pub id: i64,
    pub created_at_ms: i64,
    pub source_name: String,
    pub started_at: Option<StartInfo>,
    pub duration_ms: u64,
    pub dog_ms: u64,
    pub dog_percent: f32,
    pub bark_ms: u64,
    pub whine_ms: u64,
    pub howl_ms: u64,
    pub growl_ms: u64,
    pub episodes: u32,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ClassStat {
    pub kind: SoundKind,
    pub time_ms: u64,
    /// Доля от всей записи, %.
    pub percent: f32,
    /// Доля среди звуков собаки, % (`None` для тишины и прочего).
    pub dog_percent: Option<f32>,
    pub episodes: u32,
    pub longest_episode_ms: u64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct EpisodeInfo {
    pub kind: SoundKind,
    pub start_ms: u64,
    pub end_ms: u64,
    pub sound_ms: u64,
    pub peak_db: f32,
    pub confidence: f32,
}

/// Время по классам — в порядке `SoundKind` (лай, скулёж, вой, рычание, прочее, тишина).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TimeBucket {
    pub start_ms: u64,
    pub end_ms: u64,
    pub class_ms: Vec<u64>,
    /// Для почасовой разбивки — час на часах (или номер часа от начала), иначе 0.
    pub hour: u32,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SessionReport {
    pub id: i64,
    pub source_name: String,
    pub started_at: Option<StartInfo>,
    pub duration_ms: u64,
    pub dog_ms: u64,
    pub dog_percent: f32,
    pub episodes_count: u32,
    pub first_dog_sound_ms: Option<u64>,
    pub last_dog_sound_ms: Option<u64>,
    pub longest_episode_ms: Option<u64>,
    pub longest_calm_ms: u64,
    pub gated_percent: f32,
    pub merge_gap_ms: u64,
    pub timeline_bin_ms: u64,
    pub classes: Vec<ClassStat>,
    pub episodes: Vec<EpisodeInfo>,
    pub timeline: Vec<TimeBucket>,
    pub hourly: Vec<TimeBucket>,
}

impl SessionReport {
    fn from_core(id: i64, r: core::Report) -> Self {
        let s = r.stats;
        SessionReport {
            id,
            source_name: r.source.unwrap_or_default(),
            started_at: r.started_at.map(Into::into),
            duration_ms: s.duration_ms,
            dog_ms: s.dog_time_ms,
            dog_percent: s.dog_percent,
            episodes_count: s.episodes,
            first_dog_sound_ms: s.first_dog_sound_ms,
            last_dog_sound_ms: s.last_dog_sound_ms,
            longest_episode_ms: s.longest_episode.as_ref().map(|e| e.span_ms()),
            longest_calm_ms: s.longest_calm_ms,
            gated_percent: s.gated_percent,
            merge_gap_ms: r.settings.merge_gap_ms,
            timeline_bin_ms: r.settings.timeline_bin_ms,
            classes: s
                .classes
                .into_iter()
                .map(|c| ClassStat {
                    kind: c.class.into(),
                    time_ms: c.time_ms,
                    percent: c.percent,
                    dog_percent: c.dog_percent,
                    episodes: c.episodes,
                    longest_episode_ms: c.longest_episode_ms,
                })
                .collect(),
            episodes: r
                .episodes
                .into_iter()
                .map(|e| EpisodeInfo {
                    kind: e.class.into(),
                    start_ms: e.start_ms,
                    end_ms: e.end_ms,
                    sound_ms: e.sound_ms,
                    peak_db: e.peak_db,
                    confidence: e.confidence,
                })
                .collect(),
            timeline: r
                .timeline
                .into_iter()
                .map(|b| TimeBucket {
                    start_ms: b.start_ms,
                    end_ms: b.end_ms,
                    class_ms: b.class_ms.to_vec(),
                    hour: 0,
                })
                .collect(),
            hourly: r
                .hourly
                .into_iter()
                .map(|b| TimeBucket {
                    start_ms: b.start_ms,
                    end_ms: b.end_ms,
                    class_ms: b.class_ms.to_vec(),
                    hour: b.hour,
                })
                .collect(),
        }
    }
}

/// Прогресс анализа. Вернуть `false` — отменить.
#[uniffi::export(with_foreign)]
pub trait ProgressListener: Send + Sync {
    fn on_progress(&self, processed_ms: u64, total_ms: Option<u64>) -> bool;
}

/// Описание анализируемого файла.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SourceFile {
    /// Имя файла: из него берутся расширение (подсказка формата) и дата записи.
    pub display_name: String,
    /// Время изменения файла (мс UTC), если известно.
    pub modified_ms: Option<i64>,
    /// Текущее смещение часового пояса устройства, минуты.
    pub utc_offset_min: i32,
}

#[derive(uniffi::Object)]
pub struct BarkLog {
    model: Yamnet,
    store: Mutex<Store>,
}

#[uniffi::export]
impl BarkLog {
    /// `db_path` — файл базы (создаётся при первом запуске), `model_path` — yamnet.safetensors.
    #[uniffi::constructor]
    pub fn new(db_path: String, model_path: String) -> Result<Arc<Self>> {
        let model = Yamnet::load(&model_path).map_err(|e| BarkLogError::Model { reason: e.to_string(),
        })?;
        let store = Store::open(&db_path)?;
        Ok(Arc::new(Self {
            model,
            store: Mutex::new(store),
        }))
    }

    /// Анализирует файл по открытому дескриптору и сохраняет сессию; возвращает её id.
    /// Дескриптор переходит во владение Rust и закрывается по окончании
    /// (на Android: `ParcelFileDescriptor.detachFd()`). Вызов блокирующий.
    pub fn analyze_fd(
        &self,
        fd: i32,
        file: SourceFile,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<i64> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            // SAFETY: вызывающая сторона передаёт открытый дескриптор и отказывается от владения.
            let f = unsafe { File::from_raw_fd(fd) };
            self.analyze_file(f, file, listener)
        }
        #[cfg(not(unix))]
        {
            let _ = (fd, file, listener);
            Err(BarkLogError::Audio { reason: "дескрипторы поддерживаются только на Android/iOS".into(),
            })
        }
    }

    /// Анализирует файл по пути (десктоп, тесты, iOS).
    pub fn analyze_path(
        &self,
        path: String,
        file: SourceFile,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<i64> {
        let f = File::open(&path).map_err(|e| BarkLogError::Audio { reason: e.to_string(),
        })?;
        self.analyze_file(f, file, listener)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let list = self.store.lock().unwrap().list()?;
        Ok(list
            .into_iter()
            .map(|s| SessionSummary {
                dog_percent: s.dog_percent(),
                id: s.id,
                created_at_ms: s.created_at_ms,
                source_name: s.source_name,
                started_at: s.started_at.map(Into::into),
                duration_ms: s.duration_ms,
                dog_ms: s.dog_ms,
                bark_ms: s.bark_ms,
                whine_ms: s.whine_ms,
                howl_ms: s.howl_ms,
                growl_ms: s.growl_ms,
                episodes: s.episodes,
            })
            .collect())
    }

    pub fn get_report(&self, id: i64) -> Result<SessionReport> {
        let report = self
            .store
            .lock()
            .unwrap()
            .get(id)?
            .ok_or(BarkLogError::NotFound)?;
        Ok(SessionReport::from_core(id, report))
    }

    pub fn delete_session(&self, id: i64) -> Result<bool> {
        Ok(self.store.lock().unwrap().delete(id)?)
    }
}

impl BarkLog {
    fn analyze_file(
        &self,
        f: File,
        file: SourceFile,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<i64> {
        let ext = Path::new(&file.display_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_owned);
        let mut source = FileSource::from_file(f, ext.as_deref())?;

        let analysis = analyze(&mut source, &self.model, Thresholds::default(), true, |p| {
            listener.on_progress(p.processed_ms, p.total_ms)
        })
        .map_err(|e| match e {
            AnalyzeError::Cancelled => BarkLogError::Cancelled,
            AnalyzeError::Audio(e) => e.into(),
        })?;

        let stem = Path::new(&file.display_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let started_at = core::start_from_name(stem, file.utc_offset_min).or_else(|| {
            file.modified_ms
                .map(|m| core::start_from_modified(m, analysis.duration_ms, file.utc_offset_min))
        });
        let report = build_report(
            &analysis,
            &ReportSettings::for_duration(analysis.duration_ms),
            started_at,
            Some(file.display_name.clone()),
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        Ok(self.store.lock().unwrap().insert(&report, now)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Counter(AtomicU32, bool);
    impl ProgressListener for Counter {
        fn on_progress(&self, _: u64, _: Option<u64>) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            self.1
        }
    }

    fn model_path() -> Option<String> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/yamnet.safetensors");
        p.exists().then(|| p.to_string_lossy().into_owned())
    }

    fn wav(name: &str, secs: u32) -> String {
        let path = std::env::temp_dir().join(name);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..16_000 * secs {
            let v = (i as f32 * 440.0 * std::f32::consts::TAU / 16_000.0).sin() * 3000.0;
            w.write_sample(v as i16).unwrap();
        }
        w.finalize().unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn analyze_store_and_read_back() {
        let Some(model) = model_path() else { return };
        let db = std::env::temp_dir().join(format!("barklog-ffi-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model).unwrap();

        let path = wav("REC_20261107_093000.wav", 5);
        let counter = Arc::new(Counter(AtomicU32::new(0), true));
        let file = SourceFile {
            display_name: "REC_20261107_093000.wav".into(),
            modified_ms: None,
            utc_offset_min: 180,
        };
        let id = bl
            .analyze_path(path.clone(), file.clone(), counter.clone())
            .unwrap();
        assert!(counter.0.load(Ordering::Relaxed) > 0);

        let list = bl.list_sessions().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(
            list[0].started_at.as_ref().unwrap().source,
            StartSource::FileName
        );

        let report = bl.get_report(id).unwrap();
        assert_eq!(report.duration_ms, 5000);
        assert_eq!(report.classes.len(), 6);
        assert!(report.timeline.iter().all(|b| b.class_ms.len() == 6));

        // Отмена из слушателя.
        let cancel = Arc::new(Counter(AtomicU32::new(0), false));
        assert!(matches!(
            bl.analyze_path(path, file, cancel),
            Err(BarkLogError::Cancelled)
        ));
        assert_eq!(bl.list_sessions().unwrap().len(), 1);

        assert!(bl.delete_session(id).unwrap());
        assert!(matches!(bl.get_report(id), Err(BarkLogError::NotFound)));
        let _ = std::fs::remove_file(&db);
    }
}
