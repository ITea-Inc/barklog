//! Интерфейс ядра для мобильных приложений (UniFFI → Kotlin / Swift).
//!
//! Весь анализ, хранение и статистика — здесь; приложения только показывают
//! данные и дают доступ к системным API (выбор файла, фон, уведомления).

use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use barklog_audio::{AudioSource, FileSource};
use barklog_core as core;
use barklog_core::{
    AnalyzeError, LiveRecorder, PersonalModel, ReportSettings, Thresholds, analyze, build_report,
};
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
    #[error("{reason}")]
    Training { reason: String },
}

impl From<barklog_store::StoreError> for BarkLogError {
    fn from(e: barklog_store::StoreError) -> Self {
        BarkLogError::Storage {
            reason: e.to_string(),
        }
    }
}

impl From<barklog_audio::AudioError> for BarkLogError {
    fn from(e: barklog_audio::AudioError) -> Self {
        BarkLogError::Audio {
            reason: e.to_string(),
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
    Recorded,
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
                core::StartSource::Recorded => StartSource::Recorded,
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
    /// Ответ пользователя на этот эпизод, если он есть.
    pub label: Option<LabelKind>,
}

/// Ответ пользователя «что это было».
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LabelKind {
    Bark,
    Whine,
    Howl,
    Growl,
    NotDog,
}

impl From<core::Label> for LabelKind {
    fn from(l: core::Label) -> Self {
        match l {
            core::Label::Bark => LabelKind::Bark,
            core::Label::Whine => LabelKind::Whine,
            core::Label::Howl => LabelKind::Howl,
            core::Label::Growl => LabelKind::Growl,
            core::Label::NotDog => LabelKind::NotDog,
        }
    }
}

impl From<LabelKind> for core::Label {
    fn from(l: LabelKind) -> Self {
        match l {
            LabelKind::Bark => core::Label::Bark,
            LabelKind::Whine => core::Label::Whine,
            LabelKind::Howl => core::Label::Howl,
            LabelKind::Growl => core::Label::Growl,
            LabelKind::NotDog => core::Label::NotDog,
        }
    }
}

/// Отмеченный пользователем отрезок записи.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LabelInfo {
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: LabelKind,
}

/// Состояние личной модели.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct PersonalStatus {
    pub trained: bool,
    pub trained_at_ms: Option<i64>,
    /// Окон в обучении по классам: лай, скулёж, вой, рычание, не собака.
    pub samples: Vec<u32>,
    /// Точность на отложенной выборке, 0..1.
    pub accuracy: Option<f32>,
    /// Сколько эпизодов отмечено во всех записях.
    pub labeled_episodes: u32,
    /// Сколько окон с отпечатками есть внутри отмеченных эпизодов (материал для обучения).
    pub available_samples: u32,
    /// Минимум окон для обучения.
    pub min_samples: u32,
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
    /// Ссылка на исходный файл для прослушивания эпизодов, если она сохранена.
    pub source_uri: Option<String>,
    /// Можно ли отмечать эпизоды (для записей старых версий — нельзя).
    pub editable: bool,
    /// Ответы пользователя, включая «не собака» (такие эпизоды из списка пропадают).
    pub labels: Vec<LabelInfo>,
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
    fn from_core(id: i64, r: core::Report, source_uri: Option<String>) -> Self {
        let s = r.stats;
        SessionReport {
            id,
            source_name: r.source.unwrap_or_default(),
            source_uri,
            editable: false,
            labels: Vec::new(),
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
                    label: None,
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
    /// Постоянная ссылка на файл (content:// на Android) — сохраняется, чтобы потом
    /// можно было прослушать эпизоды. `None`, если доступ к файлу временный.
    pub uri: Option<String>,
}

#[derive(uniffi::Object)]
pub struct BarkLog {
    model: Arc<Yamnet>,
    store: Arc<Mutex<Store>>,
    personal: RwLock<Option<Arc<PersonalModel>>>,
}

const PERSONAL_MODEL: &str = "personal";
/// Ниже этой точности на отложенной выборке модель не применяем.
const MIN_PERSONAL_ACCURACY: f32 = 0.6;

#[uniffi::export]
impl BarkLog {
    /// `db_path` — файл базы (создаётся при первом запуске), `model_path` — yamnet.safetensors.
    #[uniffi::constructor]
    pub fn new(db_path: String, model_path: String) -> Result<Arc<Self>> {
        let model = Yamnet::load(&model_path).map_err(|e| BarkLogError::Model {
            reason: e.to_string(),
        })?;
        let store = Store::open(&db_path)?;
        // Повреждённую или устаревшую личную модель просто не используем.
        let personal = store
            .load_model(PERSONAL_MODEL)?
            .and_then(|(json, _)| serde_json::from_str::<PersonalModel>(&json).ok())
            .filter(|m| m.version == PersonalModel::VERSION)
            .map(Arc::new);
        Ok(Arc::new(Self {
            model: Arc::new(model),
            store: Arc::new(Mutex::new(store)),
            personal: RwLock::new(personal),
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
            Err(BarkLogError::Audio {
                reason: "дескрипторы поддерживаются только на Android/iOS".into(),
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
        let f = File::open(&path).map_err(|e| BarkLogError::Audio {
            reason: e.to_string(),
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
        let store = self.store.lock().unwrap();
        let (report, uri) = store.get_with_source(id)?.ok_or(BarkLogError::NotFound)?;
        let labels = store.labels(id)?;
        let mut out = SessionReport::from_core(id, report, uri);
        out.editable = store.has_analysis(id)?;
        for e in &mut out.episodes {
            e.label = labels
                .iter()
                .find(|l| l.start_ms < e.end_ms && e.start_ms < l.end_ms)
                .map(|l| l.label.into());
        }
        out.labels = labels
            .iter()
            .map(|l| LabelInfo {
                start_ms: l.start_ms,
                end_ms: l.end_ms,
                label: l.label.into(),
            })
            .collect();
        Ok(out)
    }

    /// Начинает живую запись. `input_rate` — частота микрофона (моно, PCM 16 бит),
    /// `clips_root` — папка, в которой для сессии создаётся каталог с фрагментами звука.
    pub fn start_live(
        &self,
        input_rate: u32,
        utc_offset_min: i32,
        clips_root: String,
    ) -> Result<Arc<LiveSession>> {
        let now = now_ms();
        let started_at = core::StartTime {
            unix_ms: now,
            utc_offset_min,
            source: core::StartSource::Recorded,
        };
        let dir = Path::new(&clips_root).join(format!("live-{now}"));
        let recorder = LiveRecorder::new(
            self.model.clone(),
            core::Sensitivity::default(),
            input_rate,
            started_at,
            &dir,
        )
        .map_err(storage_error)?
        .with_personal(self.personal());
        Ok(Arc::new(LiveSession {
            recorder: Mutex::new(Some(recorder)),
            store: self.store.clone(),
        }))
    }

    /// Звук фрагмента сессии для прослушивания: PCM 16 бит LE, 16 кГц, моно.
    /// Работает для сессий, записанных в приложении (у них сохранены фрагменты).
    pub fn clip_pcm16(&self, session_id: i64, start_ms: u64, end_ms: u64) -> Result<Vec<u8>> {
        let (_, uri) = self
            .store
            .lock()
            .unwrap()
            .get_with_source(session_id)?
            .ok_or(BarkLogError::NotFound)?;
        let dir = uri
            .as_deref()
            .and_then(|u| u.strip_prefix(CLIPS_URI_PREFIX))
            .ok_or(BarkLogError::NotFound)?;
        let samples = core::clips::read_clip(dir, start_ms, end_ms).map_err(storage_error)?;
        Ok(samples.iter().flat_map(|v| v.to_le_bytes()).collect())
    }

    /// Ответ пользователя на эпизод (`label = None` — снять ответ). Отчёт сразу
    /// пересчитывается; возвращается обновлённый отчёт.
    pub fn label_episode(
        &self,
        session_id: i64,
        start_ms: u64,
        end_ms: u64,
        label: Option<LabelKind>,
    ) -> Result<SessionReport> {
        {
            let store = self.store.lock().unwrap();
            if !store.has_analysis(session_id)? {
                return Err(BarkLogError::Training {
                    reason:
                        "эту запись сделала старая версия приложения — отметки для неё недоступны"
                            .into(),
                });
            }
            store.set_label(session_id, start_ms, end_ms, label.map(Into::into))?;
        }
        self.recompute(session_id)?;
        self.get_report(session_id)
    }

    pub fn personal_status(&self) -> Result<PersonalStatus> {
        let store = self.store.lock().unwrap();
        let (model, trained_at) = match store.load_model(PERSONAL_MODEL)? {
            Some((json, at)) => (serde_json::from_str::<PersonalModel>(&json).ok(), Some(at)),
            None => (None, None),
        };
        let mut available = 0u32;
        for id in store.sessions_with_analysis()? {
            let labels = store.labels(id)?;
            if labels.is_empty() {
                continue;
            }
            if let Some(a) = store.analysis(id)? {
                available += core::compact::training_samples(&a, &labels).len() as u32;
            }
        }
        Ok(PersonalStatus {
            trained: model.is_some(),
            trained_at_ms: trained_at.filter(|_| model.is_some()),
            samples: model
                .as_ref()
                .map(|m| m.samples.clone())
                .unwrap_or_default(),
            accuracy: model.as_ref().and_then(|m| m.accuracy),
            labeled_episodes: store.label_count()?,
            available_samples: available,
            min_samples: core::personal::MIN_SAMPLES as u32,
        })
    }

    /// Обучает личную модель на всех отмеченных эпизодах и пересчитывает ею все записи.
    pub fn train_personal(&self) -> Result<PersonalStatus> {
        let samples = {
            let store = self.store.lock().unwrap();
            let mut all = Vec::new();
            for id in store.sessions_with_analysis()? {
                let labels = store.labels(id)?;
                if labels.is_empty() {
                    continue;
                }
                if let Some(a) = store.analysis(id)? {
                    all.extend(core::compact::training_samples(&a, &labels));
                }
            }
            all
        };
        let model = core::personal::train(&samples).map_err(|e| BarkLogError::Training {
            reason: e.to_string(),
        })?;
        // Плохая модель хуже никакой: она перепишет все отчёты. Такую не сохраняем.
        if let Some(acc) = model.accuracy.filter(|&a| a < MIN_PERSONAL_ACCURACY) {
            return Err(BarkLogError::Training {
                reason: format!(
                    "Модель пока плохо различает ваши отметки (угадала {:.0} % на проверке) и не применена. Отметьте больше разных эпизодов и проверьте, что отметки верные",
                    acc * 100.0
                ),
            });
        }
        let json = serde_json::to_string(&model).map_err(|e| BarkLogError::Storage {
            reason: e.to_string(),
        })?;
        self.store
            .lock()
            .unwrap()
            .save_model(PERSONAL_MODEL, &json, now_ms())?;
        *self.personal.write().unwrap() = Some(Arc::new(model));
        self.recompute_all()?;
        self.personal_status()
    }

    /// Забывает личную модель; записи пересчитываются обратно по YAMNet и отметкам.
    pub fn reset_personal(&self) -> Result<()> {
        self.store.lock().unwrap().delete_model(PERSONAL_MODEL)?;
        *self.personal.write().unwrap() = None;
        self.recompute_all()
    }

    pub fn delete_session(&self, id: i64) -> Result<bool> {
        Ok(self.store.lock().unwrap().delete(id)?)
    }
}

impl BarkLog {
    fn personal(&self) -> Option<Arc<PersonalModel>> {
        self.personal.read().unwrap().clone()
    }

    /// Пересчитывает отчёт сессии по сохранённым окнам, личной модели и отметкам.
    fn recompute(&self, id: i64) -> Result<()> {
        let store = self.store.lock().unwrap();
        let Some(mut analysis) = store.analysis(id)? else {
            return Ok(());
        };
        let old = store.get(id)?.ok_or(BarkLogError::NotFound)?;
        let labels = store.labels(id)?;
        let personal = self.personal();
        let thresholds = Thresholds::default();
        let with = core::compact::Reclassify {
            yamnet: Some(&self.model),
            thresholds: &thresholds,
            analyzed_dog_min: old.settings.windows_dog_min,
            personal: personal.as_deref(),
        };
        core::compact::reclassify(&mut analysis, &with, &labels);
        let report = build_report(&analysis, &old.settings, old.started_at, old.source);
        store.update_report(id, &report)?;
        Ok(())
    }

    fn recompute_all(&self) -> Result<()> {
        let ids = self.store.lock().unwrap().sessions_with_analysis()?;
        for id in ids {
            self.recompute(id)?;
        }
        Ok(())
    }

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

        let personal = self.personal();
        let analysis = analyze(
            &mut source,
            &self.model,
            Thresholds::default(),
            true,
            personal,
            |p| listener.on_progress(p.processed_ms, p.total_ms),
        )
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
        let now = now_ms();
        Ok(self
            .store
            .lock()
            .unwrap()
            .insert(&report, now, file.uri.as_deref(), Some(&analysis))?)
    }
}

/// Префикс `source_uri` сессий, записанных в приложении: дальше — путь к фрагментам.
pub const CLIPS_URI_PREFIX: &str = "clips:";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn storage_error(e: std::io::Error) -> BarkLogError {
    BarkLogError::Storage {
        reason: e.to_string(),
    }
}

/// Снимок живой записи для экрана и для отправки на другой телефон.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct LiveSnapshot {
    /// Сколько звука получено.
    pub elapsed_ms: u64,
    /// Сколько уже проанализировано (отстаёт на несколько секунд).
    pub analyzed_ms: u64,
    /// Собака слышна прямо сейчас (последние ~10 с).
    pub dog_now: bool,
    /// Когда собака последний раз подавала голос (от начала записи).
    pub last_dog_sound_ms: Option<u64>,
    /// Голос собаки за последние 15 минут.
    pub recent_dog_ms: u64,
    /// Громкость последнего окна, дБFS.
    pub level_db: f32,
    /// Идущий сейчас период лая: начало (от начала записи) и сколько длится.
    pub active_bout_start_ms: Option<u64>,
    pub active_bout_ms: Option<u64>,
    /// Отчёт по уже проанализированной части (id = -1).
    pub report: SessionReport,
}

impl From<core::LiveSnapshot> for LiveSnapshot {
    fn from(s: core::LiveSnapshot) -> Self {
        LiveSnapshot {
            elapsed_ms: s.elapsed_ms,
            analyzed_ms: s.analyzed_ms,
            dog_now: s.dog_now,
            last_dog_sound_ms: s.last_dog_sound_ms,
            recent_dog_ms: s.recent_dog_ms,
            level_db: s.level_db,
            active_bout_start_ms: s.active_bout.map(|b| b.start_ms),
            active_bout_ms: s.active_bout.map(|b| b.duration_ms),
            report: SessionReport::from_core(-1, s.report, None),
        }
    }
}

/// Разбирает снимок, присланный другим телефоном (см. `LiveSession::snapshot_json`).
#[uniffi::export]
pub fn parse_live_snapshot(json: String) -> Result<LiveSnapshot> {
    core::LiveSnapshot::from_sync_json(&json)
        .map(Into::into)
        .map_err(|e| BarkLogError::Storage {
            reason: e.to_string(),
        })
}

/// Идущая запись. Звук передаётся порциями из потока записи платформы.
#[derive(uniffi::Object)]
pub struct LiveSession {
    recorder: Mutex<Option<LiveRecorder<Arc<Yamnet>>>>,
    store: Arc<Mutex<Store>>,
}

#[uniffi::export]
impl LiveSession {
    /// Моно PCM 16 бит little-endian с частотой, указанной в `start_live`.
    /// Анализ идёт прямо в этом вызове (раз в ~4 с звука вызов заметно дольше).
    pub fn push_pcm16(&self, pcm: Vec<u8>) -> Result<()> {
        let mut guard = self.recorder.lock().unwrap();
        let rec = guard.as_mut().ok_or(BarkLogError::NotFound)?;
        rec.push_pcm16(&pcm).map_err(storage_error)
    }

    pub fn snapshot(&self) -> Result<LiveSnapshot> {
        let guard = self.recorder.lock().unwrap();
        let rec = guard.as_ref().ok_or(BarkLogError::NotFound)?;
        Ok(rec.snapshot().into())
    }

    /// Снимок в виде JSON для отправки на другой телефон: цифры и таймлайн, без звука.
    pub fn snapshot_json(&self) -> Result<String> {
        let guard = self.recorder.lock().unwrap();
        let rec = guard.as_ref().ok_or(BarkLogError::NotFound)?;
        Ok(rec.snapshot().to_sync_json())
    }

    /// Останавливает запись, сохраняет сессию и возвращает её id.
    pub fn finish(&self) -> Result<i64> {
        let rec = self
            .recorder
            .lock()
            .unwrap()
            .take()
            .ok_or(BarkLogError::NotFound)?;
        let started_at = rec.started_at();
        let dir = rec.clips_dir().to_string_lossy().into_owned();
        let (analysis, _) = rec.finish().map_err(storage_error)?;
        let report = build_report(
            &analysis,
            &ReportSettings::for_duration(analysis.duration_ms),
            Some(started_at),
            Some("Запись в приложении".into()),
        );
        let uri = format!("{CLIPS_URI_PREFIX}{dir}");
        Ok(self
            .store
            .lock()
            .unwrap()
            .insert(&report, now_ms(), Some(&uri), Some(&analysis))?)
    }
}

/// Читает аудиофайл порциями PCM 16 бит 16 кГц — чтобы в отладочной сборке
/// «проиграть» файл в живую запись вместо микрофона.
#[derive(uniffi::Object)]
pub struct PcmFileReader {
    source: Mutex<FileSource>,
}

#[uniffi::export]
impl PcmFileReader {
    #[uniffi::constructor]
    pub fn open_path(path: String) -> Result<Arc<Self>> {
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_owned);
        let f = File::open(&path).map_err(|e| BarkLogError::Audio {
            reason: e.to_string(),
        })?;
        Ok(Arc::new(Self {
            source: Mutex::new(FileSource::from_file(f, ext.as_deref())?),
        }))
    }

    /// Дескриптор переходит во владение Rust (см. `analyze_fd`).
    #[uniffi::constructor]
    pub fn open_fd(fd: i32, display_name: String) -> Result<Arc<Self>> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd;
            let ext = Path::new(&display_name)
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_owned);
            // SAFETY: вызывающая сторона передаёт открытый дескриптор и отказывается от владения.
            let f = unsafe { File::from_raw_fd(fd) };
            Ok(Arc::new(Self {
                source: Mutex::new(FileSource::from_file(f, ext.as_deref())?),
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = (fd, display_name);
            Err(BarkLogError::Audio {
                reason: "дескрипторы поддерживаются только на Android/iOS".into(),
            })
        }
    }

    /// Следующая порция (не меньше `min_samples`, если файл не кончился); пусто — конец.
    pub fn next_chunk(&self, min_samples: u32) -> Result<Vec<u8>> {
        let mut src = self.source.lock().unwrap();
        let mut buf = Vec::new();
        while buf.len() < min_samples as usize {
            if !src.read(&mut buf)? {
                break;
            }
        }
        Ok(buf
            .iter()
            .flat_map(|&x| ((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
            .collect())
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
            uri: Some("content://test/rec".into()),
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
        assert_eq!(report.source_uri.as_deref(), Some("content://test/rec"));
        assert_eq!(report.classes.len(), 6);
        assert!(report.timeline.iter().all(|b| b.class_ms.len() == 6));

        // Отмена из слушателя.
        let cancel = Arc::new(Counter(AtomicU32::new(0), false));
        assert!(matches!(
            bl.analyze_path(path.clone(), file, cancel),
            Err(BarkLogError::Cancelled)
        ));
        assert_eq!(bl.list_sessions().unwrap().len(), 1);

        // Живая запись из того же файла: фрагменты пишутся, отчёт сохраняется.
        let clips = std::env::temp_dir().join(format!("barklog-ffi-clips-{}", std::process::id()));
        let live = bl
            .start_live(16_000, 180, clips.to_string_lossy().into_owned())
            .unwrap();
        let reader = PcmFileReader::open_path(path.clone()).unwrap();
        loop {
            let chunk = reader.next_chunk(1600).unwrap();
            if chunk.is_empty() {
                break;
            }
            live.push_pcm16(chunk).unwrap();
        }
        let snap = live.snapshot().unwrap();
        assert_eq!(snap.elapsed_ms, 5000);
        let remote = parse_live_snapshot(live.snapshot_json().unwrap()).unwrap();
        assert_eq!(remote, snap);
        assert_eq!(snap.report.id, -1);
        let live_id = live.finish().unwrap();
        assert!(matches!(live.snapshot(), Err(BarkLogError::NotFound)));
        let live_report = bl.get_report(live_id).unwrap();
        assert_eq!(
            live_report.started_at.unwrap().source,
            StartSource::Recorded
        );
        assert!(
            live_report
                .source_uri
                .unwrap()
                .starts_with(CLIPS_URI_PREFIX)
        );
        // Синус — не собака: фрагментов нет, звук эпизода — тишина нужной длины.
        let pcm = bl.clip_pcm16(live_id, 0, 1000).unwrap();
        assert_eq!(pcm.len(), 16_000 * 2);
        assert!(pcm.iter().all(|&b| b == 0));
        let _ = std::fs::remove_dir_all(&clips);

        assert!(bl.delete_session(id).unwrap());
        assert!(matches!(bl.get_report(id), Err(BarkLogError::NotFound)));
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn labels_and_personal_model_recompute_reports() {
        use barklog_core::{Analysis, ClassScores, SoundClass, WindowResult};
        let Some(model) = model_path() else { return };
        let db =
            std::env::temp_dir().join(format!("barklog-ffi-personal-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model).unwrap();

        // 60 «собачьих» окон: первые 30 — настоящий лай (центр A), вторые 30 —
        // телевизор, который YAMNet тоже принял за лай (центр B).
        let n = 60u64;
        let windows: Vec<WindowResult> = (0..n)
            .map(|i| WindowResult {
                start_ms: i * 480,
                class: SoundClass::Bark,
                confidence: 0.6,
                rms_db: -25.0,
                peak_db: -15.0,
                gated: false,
                scores: ClassScores::default(),
            })
            .collect();
        let emb = |i: u64| -> Vec<half::f16> {
            (0..1024)
                .map(|k| {
                    let center = if (k % 2 == 0) == (i < 30) { 1.0 } else { 0.0 };
                    half::f16::from_f32(center + ((i * 7 + k as u64) % 13) as f32 * 0.02)
                })
                .collect()
        };
        let analysis = Analysis {
            duration_ms: n * 480,
            embeddings: (0..n).map(|i| (i as u32, emb(i))).collect(),
            windows,
        };
        let report = build_report(
            &analysis,
            &ReportSettings::default(),
            None,
            Some("t".into()),
        );
        let id = bl
            .store
            .lock()
            .unwrap()
            .insert(&report, 1, None, Some(&analysis))
            .unwrap();
        let before = bl.get_report(id).unwrap();
        assert!(before.editable);
        assert_eq!(before.dog_ms, n * 480);

        // Одинаковые звуки с разными ответами — модель не различает их и не сохраняется.
        {
            let same = Analysis {
                duration_ms: n * 480,
                embeddings: (0..n).map(|i| (i as u32, emb(0))).collect(),
                windows: analysis.windows.clone(),
            };
            let r2 = build_report(&same, &ReportSettings::default(), None, Some("same".into()));
            let id2 = bl
                .store
                .lock()
                .unwrap()
                .insert(&r2, 2, None, Some(&same))
                .unwrap();
            bl.label_episode(id2, 0, 30 * 480, Some(LabelKind::Bark))
                .unwrap();
            bl.label_episode(id2, 30 * 480, n * 480, Some(LabelKind::NotDog))
                .unwrap();
            assert!(matches!(
                bl.train_personal(),
                Err(BarkLogError::Training { .. })
            ));
            assert!(!bl.personal_status().unwrap().trained);
            bl.delete_session(id2).unwrap();
        }

        // Мало отметок — обучение отказывается.
        assert!(matches!(
            bl.train_personal(),
            Err(BarkLogError::Training { .. })
        ));

        let tv = bl
            .label_episode(id, 30 * 480, n * 480, Some(LabelKind::NotDog))
            .unwrap();
        assert_eq!(
            tv.dog_ms,
            30 * 480,
            "отметка сразу убирает телевизор из голоса"
        );
        assert_eq!(tv.labels.len(), 1);
        bl.label_episode(id, 0, 30 * 480, Some(LabelKind::Bark))
            .unwrap();

        let status = bl.train_personal().unwrap();
        assert!(status.trained);
        assert_eq!(status.samples, vec![30, 0, 0, 0, 30]);
        assert_eq!(status.labeled_episodes, 2);

        // Снимаем отметки — теперь классы решает только личная модель, и она уже знает разницу.
        bl.label_episode(id, 0, 0, None).unwrap();
        let after = bl.label_episode(id, 30 * 480, 0, None).unwrap();
        assert!(after.labels.is_empty());
        assert_eq!(after.dog_ms, 30 * 480);

        // Модель переживает перезапуск.
        drop(bl);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model_path().unwrap()).unwrap();
        assert!(bl.personal_status().unwrap().trained);
        bl.reset_personal().unwrap();
        assert_eq!(
            bl.get_report(id).unwrap().dog_ms,
            n * 480,
            "без модели и отметок — как было"
        );
        let _ = std::fs::remove_file(&db);
    }
}
