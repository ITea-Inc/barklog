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
    AnalyzeError, LiveRecorder, PersonalModel, ReportSettings, Sensitivity, analyze, build_report,
};
use barklog_ml::Yamnet;
use barklog_store::{FIRST_DOG, Store, personal_model_name};
use std::collections::HashMap;

mod csv;

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
    pub note: Option<String>,
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
    /// Чужая собака: не голос своей, для личной модели — отдельный класс.
    ForeignDog,
}

impl From<core::Label> for LabelKind {
    fn from(l: core::Label) -> Self {
        match l {
            core::Label::Bark => LabelKind::Bark,
            core::Label::Whine => LabelKind::Whine,
            core::Label::Howl => LabelKind::Howl,
            core::Label::Growl => LabelKind::Growl,
            core::Label::NotDog => LabelKind::NotDog,
            core::Label::ForeignDog => LabelKind::ForeignDog,
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
            LabelKind::ForeignDog => core::Label::ForeignDog,
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

/// Чувствительность: порог «это собака» и правила сборки эпизодов.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct SensitivitySettings {
    /// 0–1, меньше — чувствительнее (больше звуков считается собакой).
    pub dog_min: f32,
    /// Паузы короче этого не разрывают эпизод.
    pub merge_gap_ms: u64,
    /// Эпизоды короче этого не считаются.
    pub min_episode_ms: u64,
}

impl From<Sensitivity> for SensitivitySettings {
    fn from(s: Sensitivity) -> Self {
        Self {
            dog_min: s.dog_min,
            merge_gap_ms: s.merge_gap_ms,
            min_episode_ms: s.min_episode_ms,
        }
    }
}

impl From<SensitivitySettings> for Sensitivity {
    fn from(s: SensitivitySettings) -> Self {
        Self {
            dog_min: s.dog_min,
            merge_gap_ms: s.merge_gap_ms,
            min_episode_ms: s.min_episode_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TrendPeriod {
    Day,
    /// Неделя с понедельника.
    Week,
}

/// Записи за день или неделю.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TrendBucket {
    /// Первый день периода (для недели — понедельник).
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub sessions: u32,
    pub duration_ms: u64,
    pub dog_ms: u64,
    /// Лай, скулёж, вой, рычание.
    pub dog_class_ms: Vec<u64>,
    pub episodes: u32,
    /// Доля голоса собаки от времени записи, %.
    pub dog_percent: f32,
}

/// Собака: у каждой свои записи, тренды, отметки и личная модель.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DogInfo {
    pub id: i64,
    pub name: String,
    pub sessions: u32,
}

impl From<barklog_store::Dog> for DogInfo {
    fn from(d: barklog_store::Dog) -> Self {
        Self {
            id: d.id,
            name: d.name,
            sessions: d.sessions,
        }
    }
}

/// Значения по умолчанию и допустимые пределы — для ползунков настроек.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct SensitivityLimits {
    pub defaults: SensitivitySettings,
    pub dog_min_low: f32,
    pub dog_min_high: f32,
    pub max_merge_gap_ms: u64,
    pub max_min_episode_ms: u64,
}

#[uniffi::export]
pub fn sensitivity_limits() -> SensitivityLimits {
    SensitivityLimits {
        defaults: Sensitivity::default().into(),
        dog_min_low: Sensitivity::DOG_MIN.0,
        dog_min_high: Sensitivity::DOG_MIN.1,
        max_merge_gap_ms: Sensitivity::MAX_GAP_MS,
        max_min_episode_ms: Sensitivity::MAX_MIN_EPISODE_MS,
    }
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
    /// Заметка хозяина к сессии.
    pub note: Option<String>,
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
    /// Сколько звучали чужие собаки (входит в «другие звуки»).
    pub foreign_dog_ms: u64,
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
            note: None,
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
            foreign_dog_ms: s.foreign_dog_ms,
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
    /// Личные модели собак, загруженные из базы (`None` — у собаки модели нет).
    personal: RwLock<HashMap<i64, Option<Arc<PersonalModel>>>>,
    sensitivity: RwLock<Sensitivity>,
    /// Чьи записи показываются и к кому идут новые.
    current_dog: RwLock<i64>,
}

const SENSITIVITY: &str = "sensitivity";
const CURRENT_DOG: &str = "current_dog";
/// Имя собаки — короткое, чтобы помещалось в шапку.
const MAX_DOG_NAME_CHARS: usize = 40;
/// Заметка — пара фраз, не дневник.
const MAX_NOTE_CHARS: usize = 1_000;
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
        let dogs = store.dogs()?;
        let current_dog = store
            .load_setting(CURRENT_DOG)?
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|id| dogs.iter().any(|d| d.id == *id))
            .or_else(|| dogs.first().map(|d| d.id))
            .unwrap_or(FIRST_DOG);
        let sensitivity = store
            .load_setting(SENSITIVITY)?
            .and_then(|json| serde_json::from_str::<Sensitivity>(&json).ok())
            .unwrap_or_default()
            .clamped();
        Ok(Arc::new(Self {
            model: Arc::new(model),
            store: Arc::new(Mutex::new(store)),
            personal: RwLock::new(HashMap::new()),
            sensitivity: RwLock::new(sensitivity),
            current_dog: RwLock::new(current_dog),
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
        let list = self.store.lock().unwrap().list(Some(self.dog()))?;
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
                note: s.note,
            })
            .collect())
    }

    pub fn get_report(&self, id: i64) -> Result<SessionReport> {
        let store = self.store.lock().unwrap();
        let (report, uri) = store.get_with_source(id)?.ok_or(BarkLogError::NotFound)?;
        let labels = store.labels(id)?;
        let mut out = SessionReport::from_core(id, report, uri);
        out.editable = store.has_analysis(id)?;
        out.note = store.note(id)?;
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
            self.current_sensitivity(),
            input_rate,
            started_at,
            &dir,
        )
        .map_err(storage_error)?
        .with_personal(self.personal());
        Ok(Arc::new(LiveSession {
            recorder: Mutex::new(Some(recorder)),
            store: self.store.clone(),
            personal: self.personal(),
            dog_id: self.dog(),
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
        let dog = self.dog();
        let store = self.store.lock().unwrap();
        let (model, trained_at) = match store.load_model(&personal_model_name(dog))? {
            Some((json, at)) => (serde_json::from_str::<PersonalModel>(&json).ok(), Some(at)),
            None => (None, None),
        };
        let mut available = 0u32;
        for id in store.sessions_with_analysis(Some(dog))? {
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
            labeled_episodes: store.label_count(Some(dog))?,
            available_samples: available,
            min_samples: core::personal::MIN_SAMPLES as u32,
        })
    }

    /// Обучает личную модель текущей собаки на её отмеченных эпизодах и пересчитывает
    /// ею записи этой собаки.
    pub fn train_personal(&self) -> Result<PersonalStatus> {
        let dog = self.dog();
        let samples = {
            let store = self.store.lock().unwrap();
            let mut all = Vec::new();
            for id in store.sessions_with_analysis(Some(dog))? {
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
            .save_model(&personal_model_name(dog), &json, now_ms())?;
        self.personal
            .write()
            .unwrap()
            .insert(dog, Some(Arc::new(model)));
        self.recompute_dog(dog)?;
        self.personal_status()
    }

    /// Забывает личную модель текущей собаки; её записи пересчитываются обратно
    /// по YAMNet и отметкам.
    pub fn reset_personal(&self) -> Result<()> {
        let dog = self.dog();
        self.store
            .lock()
            .unwrap()
            .delete_model(&personal_model_name(dog))?;
        self.personal.write().unwrap().insert(dog, None);
        self.recompute_dog(dog)
    }

    pub fn dogs(&self) -> Result<Vec<DogInfo>> {
        Ok(self
            .store
            .lock()
            .unwrap()
            .dogs()?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn current_dog(&self) -> i64 {
        self.dog()
    }

    /// Делает собаку текущей: её записи в списке, к ней идут новые.
    pub fn select_dog(&self, id: i64) -> Result<()> {
        let store = self.store.lock().unwrap();
        if !store.dogs()?.iter().any(|d| d.id == id) {
            return Err(BarkLogError::NotFound);
        }
        store.save_setting(CURRENT_DOG, &id.to_string())?;
        *self.current_dog.write().unwrap() = id;
        Ok(())
    }

    /// Добавляет собаку (текущей не делает).
    pub fn add_dog(&self, name: String) -> Result<DogInfo> {
        let name = dog_name(&name)?;
        let id = self.store.lock().unwrap().add_dog(&name, now_ms())?;
        Ok(DogInfo {
            id,
            name,
            sessions: 0,
        })
    }

    pub fn rename_dog(&self, id: i64, name: String) -> Result<()> {
        let name = dog_name(&name)?;
        if !self.store.lock().unwrap().rename_dog(id, &name)? {
            return Err(BarkLogError::NotFound);
        }
        Ok(())
    }

    /// Удаляет собаку без записей. Если она была текущей — текущей становится первая.
    pub fn delete_dog(&self, id: i64) -> Result<()> {
        let store = self.store.lock().unwrap();
        if !store.delete_dog(id)? {
            return Err(BarkLogError::Storage {
                reason: "Удалить можно только собаку без записей, и хотя бы одна должна остаться. \
                         Записи можно перенести к другой собаке"
                    .into(),
            });
        }
        self.personal.write().unwrap().remove(&id);
        if self.dog() == id {
            let first = store.dogs()?.first().map(|d| d.id).unwrap_or(FIRST_DOG);
            store.save_setting(CURRENT_DOG, &first.to_string())?;
            *self.current_dog.write().unwrap() = first;
        }
        Ok(())
    }

    /// Переносит запись к другой собаке и пересчитывает её личной моделью той собаки.
    pub fn move_session(&self, session_id: i64, dog_id: i64) -> Result<()> {
        if !self
            .store
            .lock()
            .unwrap()
            .set_session_dog(session_id, dog_id)?
        {
            return Err(BarkLogError::NotFound);
        }
        self.recompute(session_id)
    }

    pub fn delete_session(&self, id: i64) -> Result<bool> {
        Ok(self.store.lock().unwrap().delete(id)?)
    }

    /// Заметка к сессии; пустая строка убирает её. Возвращает сохранённый текст.
    pub fn set_note(&self, id: i64, note: String) -> Result<Option<String>> {
        let note: String = note.trim().chars().take(MAX_NOTE_CHARS).collect();
        let note = (!note.is_empty()).then_some(note);
        if !self.store.lock().unwrap().set_note(id, note.as_deref())? {
            return Err(BarkLogError::NotFound);
        }
        Ok(note)
    }

    /// Записи по дням или неделям, от старых к новым. `utc_offset_min` — для записей
    /// с неизвестным началом: их день считается по времени добавления в этом поясе.
    pub fn trends(&self, period: TrendPeriod, utc_offset_min: i32) -> Result<Vec<TrendBucket>> {
        let sessions: Vec<core::TrendSession> = self
            .store
            .lock()
            .unwrap()
            .list(Some(self.dog()))?
            .into_iter()
            .map(|s| {
                let (start, offset) = match s.started_at {
                    Some(st) => (st.unix_ms, st.utc_offset_min),
                    None => (s.created_at_ms, utc_offset_min),
                };
                core::TrendSession {
                    start_unix_ms: start,
                    utc_offset_min: offset,
                    duration_ms: s.duration_ms,
                    dog_class_ms: [s.bark_ms, s.whine_ms, s.howl_ms, s.growl_ms],
                    episodes: s.episodes,
                }
            })
            .collect();
        let period = match period {
            TrendPeriod::Day => core::TrendPeriod::Day,
            TrendPeriod::Week => core::TrendPeriod::Week,
        };
        Ok(core::trends(&sessions, period)
            .into_iter()
            .map(|b| TrendBucket {
                year: b.first_day.year,
                month: b.first_day.month,
                day: b.first_day.day,
                sessions: b.sessions,
                duration_ms: b.duration_ms,
                dog_ms: b.dog_ms,
                dog_class_ms: b.dog_class_ms.to_vec(),
                episodes: b.episodes,
                dog_percent: b.dog_percent,
            })
            .collect())
    }

    /// Эпизоды записи в CSV (`;`, десятичная запятая, BOM — для Excel).
    pub fn export_episodes_csv(&self, id: i64) -> Result<String> {
        Ok(csv::episodes(&self.get_report(id)?))
    }

    /// Все записи в CSV, по строке на запись.
    pub fn export_sessions_csv(&self) -> Result<String> {
        Ok(csv::sessions(&self.list_sessions()?))
    }

    pub fn sensitivity(&self) -> SensitivitySettings {
        self.current_sensitivity().into()
    }

    /// Сохраняет чувствительность и пересчитывает ею все записи (без повторного анализа).
    /// Значения вне пределов подрезаются; возвращаются фактически применённые.
    pub fn set_sensitivity(&self, settings: SensitivitySettings) -> Result<SensitivitySettings> {
        let s = Sensitivity::from(settings).clamped();
        let json = serde_json::to_string(&s).map_err(|e| BarkLogError::Storage {
            reason: e.to_string(),
        })?;
        self.store
            .lock()
            .unwrap()
            .save_setting(SENSITIVITY, &json)?;
        *self.sensitivity.write().unwrap() = s;
        self.recompute_all()?;
        Ok(s.into())
    }
}

impl BarkLog {
    fn dog(&self) -> i64 {
        *self.current_dog.read().unwrap()
    }

    /// Личная модель текущей собаки.
    fn personal(&self) -> Option<Arc<PersonalModel>> {
        self.personal_for(self.dog()).ok().flatten()
    }

    /// Личная модель собаки; повреждённую или устаревшую просто не используем.
    fn personal_for(&self, dog: i64) -> Result<Option<Arc<PersonalModel>>> {
        if let Some(m) = self.personal.read().unwrap().get(&dog) {
            return Ok(m.clone());
        }
        let model = self
            .store
            .lock()
            .unwrap()
            .load_model(&personal_model_name(dog))?
            .and_then(|(json, _)| serde_json::from_str::<PersonalModel>(&json).ok())
            .filter(|m| m.version == PersonalModel::VERSION)
            .map(Arc::new);
        self.personal.write().unwrap().insert(dog, model.clone());
        Ok(model)
    }

    fn current_sensitivity(&self) -> Sensitivity {
        *self.sensitivity.read().unwrap()
    }

    /// Пересчитывает отчёт сессии по сохранённым окнам, чувствительности, личной модели и отметкам.
    fn recompute(&self, id: i64) -> Result<()> {
        // Модель — той собаки, чья это запись (до блокировки базы: загрузка её тоже берёт).
        let dog = self.store.lock().unwrap().session_dog(id)?;
        let personal = match dog {
            Some(d) => self.personal_for(d)?,
            None => return Ok(()),
        };
        let store = self.store.lock().unwrap();
        let Some(mut analysis) = store.analysis(id)? else {
            return Ok(());
        };
        let old = store.get(id)?.ok_or(BarkLogError::NotFound)?;
        let labels = store.labels(id)?;
        let sensitivity = self.current_sensitivity();
        let report = finalize_report(
            &mut analysis,
            &sensitivity.apply(&old.settings),
            old.started_at,
            old.source,
            Some(&self.model),
            &sensitivity,
            personal.as_deref(),
            &labels,
        );
        store.update_report(id, &report)?;
        Ok(())
    }

    fn recompute_all(&self) -> Result<()> {
        let ids = self.store.lock().unwrap().sessions_with_analysis(None)?;
        for id in ids {
            self.recompute(id)?;
        }
        Ok(())
    }

    fn recompute_dog(&self, dog: i64) -> Result<()> {
        let ids = self
            .store
            .lock()
            .unwrap()
            .sessions_with_analysis(Some(dog))?;
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

        // Запись — той собаки, что была выбрана в начале, даже если за время анализа переключили.
        let dog = self.dog();
        let personal = self.personal_for(dog)?;
        let sensitivity = self.current_sensitivity();
        let analysis = analyze(
            &mut source,
            &self.model,
            sensitivity.thresholds(),
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
        let mut analysis = analysis;
        let settings = sensitivity.report_settings(analysis.duration_ms);
        let report = finalize_report(
            &mut analysis,
            &settings,
            started_at,
            Some(file.display_name.clone()),
            None,
            &sensitivity,
            self.personal_for(dog)?.as_deref(),
            &[],
        );
        let now = now_ms();
        Ok(self.store.lock().unwrap().insert_for_dog(
            dog,
            &report,
            now,
            file.uri.as_deref(),
            Some(&analysis),
        )?)
    }
}

/// Отчёт по окнам сессии: окна перерешиваются (порог, личная модель, отметки) и
/// считается время чужих собак. `yamnet` нужен, только если порог мог измениться
/// с момента анализа; для свежего анализа хватает `None`.
#[allow(clippy::too_many_arguments)]
fn finalize_report(
    analysis: &mut core::Analysis,
    settings: &ReportSettings,
    started_at: Option<core::StartTime>,
    source: Option<String>,
    yamnet: Option<&Yamnet>,
    sensitivity: &Sensitivity,
    personal: Option<&PersonalModel>,
    labels: &[core::EpisodeLabel],
) -> core::Report {
    let thresholds = sensitivity.thresholds();
    let with = core::compact::Reclassify {
        yamnet,
        thresholds: &thresholds,
        analyzed_dog_min: settings.windows_dog_min,
        personal,
    };
    let foreign = core::compact::reclassify(analysis, &with, labels);
    let mut report = build_report(analysis, settings, started_at, source);
    report.stats.foreign_dog_ms = foreign as u64 * barklog_ml::PATCH_HOP_MS;
    report
}

/// Имя собаки без пробелов по краям, не пустое и не длиннее [`MAX_DOG_NAME_CHARS`].
fn dog_name(name: &str) -> Result<String> {
    let name: String = name.trim().chars().take(MAX_DOG_NAME_CHARS).collect();
    if name.is_empty() {
        return Err(BarkLogError::Storage {
            reason: "Имя собаки не может быть пустым".into(),
        });
    }
    Ok(name)
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
    /// Личная модель на момент начала записи — чтобы посчитать чужих собак в отчёте.
    personal: Option<Arc<PersonalModel>>,
    /// Чья запись — собака, выбранная при старте.
    dog_id: i64,
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
        let sensitivity = rec.sensitivity();
        let dir = rec.clips_dir().to_string_lossy().into_owned();
        let (mut analysis, _) = rec.finish().map_err(storage_error)?;
        let settings = sensitivity.report_settings(analysis.duration_ms);
        let report = finalize_report(
            &mut analysis,
            &settings,
            Some(started_at),
            Some("Запись в приложении".into()),
            None,
            &sensitivity,
            self.personal.as_deref(),
            &[],
        );
        let uri = format!("{CLIPS_URI_PREFIX}{dir}");
        Ok(self.store.lock().unwrap().insert_for_dog(
            self.dog_id,
            &report,
            now_ms(),
            Some(&uri),
            Some(&analysis),
        )?)
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
    use barklog_core::ReportSettings;
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
        let emb = |i: u64| -> core::Embedding {
            let v: Vec<f32> = (0..1024)
                .map(|k| {
                    let center = if (k % 2 == 0) == (i < 30) { 1.0 } else { 0.0 };
                    center + ((i * 7 + k as u64) % 13) as f32 * 0.02
                })
                .collect();
            core::Embedding::quantize(&v)
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
        assert_eq!(before.note, None);
        assert_eq!(
            bl.set_note(
                id,
                "  оставил конг 
"
                .into()
            )
            .unwrap()
            .as_deref(),
            Some("оставил конг")
        );
        assert_eq!(
            bl.get_report(id).unwrap().note.as_deref(),
            Some("оставил конг")
        );
        assert_eq!(bl.set_note(id, "   ".into()).unwrap(), None);
        assert_eq!(bl.list_sessions().unwrap()[0].note, None);
        assert!(matches!(
            bl.set_note(id + 100, "x".into()),
            Err(BarkLogError::NotFound)
        ));
        let weeks = bl.trends(TrendPeriod::Week, 180).unwrap();
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].sessions, 1);
        assert_eq!(weeks[0].dog_ms, before.dog_ms);
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
        assert_eq!(status.samples, vec![30, 0, 0, 0, 30, 0]);
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

    #[test]
    fn sensitivity_recomputes_reports_and_persists() {
        use barklog_core::{Analysis, ClassScores, SoundClass, WindowResult};
        let Some(model) = model_path() else { return };
        let db = std::env::temp_dir().join(format!("barklog-ffi-sens-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model.clone()).unwrap();
        assert_eq!(bl.sensitivity(), sensitivity_limits().defaults);

        // Одиночный уверенный гав, пауза, и длинный лай с вероятностью «собака» 0,4.
        let pattern = "b..............bbbbbbbb";
        let windows: Vec<WindowResult> = pattern
            .chars()
            .enumerate()
            .map(|(i, c)| WindowResult {
                start_ms: i as u64 * 480,
                class: if c == 'b' {
                    SoundClass::Bark
                } else {
                    SoundClass::Silence
                },
                confidence: if i == 0 { 0.9 } else { 0.4 },
                rms_db: if c == 'b' { -20.0 } else { -60.0 },
                peak_db: -10.0,
                gated: c != 'b',
                scores: ClassScores::default(),
            })
            .collect();
        let analysis = Analysis {
            duration_ms: pattern.len() as u64 * 480,
            windows,
            embeddings: Vec::new(),
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
        assert_eq!(bl.get_report(id).unwrap().episodes_count, 2);

        let applied = bl
            .set_sensitivity(SensitivitySettings {
                min_episode_ms: 1_000,
                ..bl.sensitivity()
            })
            .unwrap();
        assert_eq!(applied.min_episode_ms, 1_000);
        let r = bl.get_report(id).unwrap();
        assert_eq!(
            r.episodes_count, 1,
            "одиночный гав короче секунды не считается"
        );
        assert_eq!(r.dog_ms, 8 * 480);

        // Порог выше уверенности длинного лая — остаётся только гав, но он короткий.
        bl.set_sensitivity(SensitivitySettings {
            dog_min: 0.5,
            min_episode_ms: 0,
            ..applied
        })
        .unwrap();
        let r = bl.get_report(id).unwrap();
        assert_eq!(r.episodes_count, 1);
        assert_eq!(r.dog_ms, 480);

        // Одиночный гав — соседская собака: из голоса уходит, но учитывается отдельно.
        let r = bl
            .label_episode(id, 0, 480, Some(LabelKind::ForeignDog))
            .unwrap();
        assert_eq!(r.dog_ms, 0);
        assert_eq!(r.foreign_dog_ms, 480);
        let r = bl.label_episode(id, 0, 480, None).unwrap();
        assert_eq!(r.foreign_dog_ms, 0);

        // Вне пределов — подрезается; настройка переживает перезапуск.
        let clamped = bl
            .set_sensitivity(SensitivitySettings {
                dog_min: 0.0,
                merge_gap_ms: 3_000,
                min_episode_ms: 0,
            })
            .unwrap();
        assert_eq!(clamped.dog_min, sensitivity_limits().dog_min_low);
        drop(bl);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model).unwrap();
        assert_eq!(bl.sensitivity(), clamped);
        drop(bl);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn dogs_have_their_own_sessions() {
        use barklog_core::{Analysis, ClassScores, SoundClass, WindowResult};
        let Some(model) = model_path() else { return };
        let db = std::env::temp_dir().join(format!("barklog-ffi-dogs-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model.clone()).unwrap();
        assert_eq!(bl.current_dog(), FIRST_DOG);

        let windows: Vec<WindowResult> = (0..10)
            .map(|i| WindowResult {
                start_ms: i * 480,
                class: SoundClass::Bark,
                confidence: 0.9,
                rms_db: -20.0,
                peak_db: -10.0,
                gated: false,
                scores: ClassScores::default(),
            })
            .collect();
        let analysis = Analysis {
            duration_ms: 4800,
            windows,
            embeddings: Vec::new(),
        };
        let report = build_report(
            &analysis,
            &ReportSettings::default(),
            None,
            Some("a".into()),
        );
        let id = bl
            .store
            .lock()
            .unwrap()
            .insert(&report, 1, None, Some(&analysis))
            .unwrap();

        let rex = bl.add_dog("  Рекс  ".into()).unwrap();
        assert_eq!(rex.name, "Рекс");
        assert!(matches!(
            bl.add_dog("   ".into()),
            Err(BarkLogError::Storage { .. })
        ));
        bl.select_dog(rex.id).unwrap();
        assert!(bl.list_sessions().unwrap().is_empty());
        assert!(bl.trends(TrendPeriod::Week, 180).unwrap().is_empty());
        assert_eq!(bl.personal_status().unwrap().labeled_episodes, 0);

        bl.move_session(id, rex.id).unwrap();
        assert_eq!(bl.list_sessions().unwrap().len(), 1);
        assert!(bl.delete_dog(rex.id).is_err(), "с записями не удалить");
        bl.rename_dog(rex.id, "Рексик".into()).unwrap();

        // Выбор собаки переживает перезапуск.
        drop(bl);
        let bl = BarkLog::new(db.to_string_lossy().into_owned(), model).unwrap();
        assert_eq!(bl.current_dog(), rex.id);
        assert_eq!(bl.dogs().unwrap()[1].name, "Рексик");

        bl.move_session(id, FIRST_DOG).unwrap();
        bl.delete_dog(rex.id).unwrap();
        assert_eq!(
            bl.current_dog(),
            FIRST_DOG,
            "после удаления текущей — первая"
        );
        assert_eq!(bl.list_sessions().unwrap().len(), 1);
        assert!(bl.select_dog(rex.id).is_err());
        drop(bl);
        let _ = std::fs::remove_file(&db);
    }
}
