//! Потоковый анализ: сигнал 16 кГц → патчи → гейт по громкости → YAMNet → окна.

use std::borrow::Borrow;
use std::sync::Arc;

use half::f16;

use barklog_audio::{AudioError, AudioSource, TARGET_RATE};
use barklog_ml::{FeatureExtractor, PATCH_HOP_MS, Patch, Yamnet};
use serde::{Deserialize, Serialize};

use crate::SoundClass;
use crate::classify::{ClassScores, Thresholds, decide};
use crate::personal::{PersonalModel, to_f16};

/// Сколько патчей копить перед запуском сети (≈ 30 с звука). Большой пакет
/// эффективнее; для живой записи берут меньше — см. [`StreamAnalyzer::with_batch`].
const BATCH_PATCHES: usize = 64;
/// Насколько быстро оценка фонового шума может расти, дБ на окно (≈ 6 дБ в минуту).
const FLOOR_RISE_DB: f32 = 0.05;
/// Окно — кандидат для личной модели, если YAMNet хоть немного слышит собаку или животное.
/// Порог ниже `dog_min`: так личная модель может «спасти» тихий скулёж. Эмбеддинги
/// кандидатов сохраняются, поэтому `dog_min` можно менять задним числом не ниже этого.
pub const CANDIDATE_DOG: f32 = 0.05;
const CANDIDATE_ANIMAL: f32 = 0.2;
/// Сколько эмбеддингов держать за сессию (по 2 КБ): хватает на ~1,5 ч голоса собаки.
const MAX_EMBEDDINGS: usize = 12_000;
/// Шаг вызова колбэка прогресса.
const PROGRESS_STEP_MS: u64 = 1_000;

/// Итог по одному окну 0.96 с (шаг 0.48 с).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowResult {
    pub start_ms: u64,
    pub class: SoundClass,
    pub confidence: f32,
    pub rms_db: f32,
    pub peak_db: f32,
    /// `true`, если окно отсеяно по громкости и сеть не запускалась.
    pub gated: bool,
    pub scores: ClassScores,
}

/// Результат анализа записи целиком.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub windows: Vec<WindowResult>,
    /// Эмбеддинги YAMNet для собачьих и «спорных» окон: (номер окна, вектор f16).
    /// Нужны, чтобы обучать личную модель и пересчитывать ею старые записи.
    #[serde(skip)]
    pub embeddings: Vec<(u32, Vec<f16>)>,
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub processed_ms: u64,
    pub total_ms: Option<u64>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        self.total_ms
            .filter(|&t| t > 0)
            .map(|t| (self.processed_ms as f32 / t as f32).min(1.0))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error("анализ отменён")]
    Cancelled,
}

/// Инкрементальный анализатор: подходит и для файла, и для живой записи.
/// Модель — `&Yamnet` или `Arc<Yamnet>` (когда анализатор живёт дольше вызова).
pub struct StreamAnalyzer<M: Borrow<Yamnet>> {
    model: M,
    personal: Option<Arc<PersonalModel>>,
    batch: usize,
    thresholds: Thresholds,
    use_gate: bool,
    features: FeatureExtractor,
    pending: Vec<Patch>,
    windows: Vec<WindowResult>,
    embeddings: Vec<(u32, Vec<f16>)>,
    floor: NoiseFloor,
    gated: usize,
}

impl<M: Borrow<Yamnet>> StreamAnalyzer<M> {
    pub fn new(model: M, thresholds: Thresholds) -> Self {
        Self {
            model,
            personal: None,
            batch: BATCH_PATCHES,
            thresholds,
            use_gate: true,
            features: FeatureExtractor::new(),
            pending: Vec::new(),
            windows: Vec::new(),
            embeddings: Vec::new(),
            floor: NoiseFloor::default(),
            gated: 0,
        }
    }

    /// Отключает гейт по громкости: каждое окно идёт в сеть (медленнее, для сверки).
    pub fn without_gate(mut self) -> Self {
        self.use_gate = false;
        self
    }

    /// Личная модель уточняет класс «спорных» окон (см. [`crate::personal`]).
    pub fn with_personal(mut self, model: Option<Arc<PersonalModel>>) -> Self {
        self.personal = model;
        self
    }

    /// Запускать сеть каждые `patches` окон (по 0,48 с) — меньше задержка, больше накладных.
    pub fn with_batch(mut self, patches: usize) -> Self {
        self.batch = patches.max(1);
        self
    }

    pub fn processed_ms(&self) -> u64 {
        self.features.samples_seen() * 1000 / TARGET_RATE as u64
    }

    /// Готовые на данный момент окна.
    pub fn windows(&self) -> &[WindowResult] {
        &self.windows
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.features.push(samples, &mut self.pending);
        if self.pending.len() >= self.batch {
            self.run_batch();
        }
    }

    /// Текущее состояние без завершения потока: окна, готовые на этот момент.
    /// Длительность — до конца последнего окна (звук, ещё не прошедший сеть, не учитывается).
    pub fn snapshot(&self) -> Analysis {
        let covered = self
            .windows
            .last()
            .map(|w| (w.start_ms + PATCH_HOP_MS).min(self.processed_ms()))
            .unwrap_or(0);
        Analysis {
            duration_ms: covered,
            windows: self.windows.clone(),
            embeddings: Vec::new(),
        }
    }

    pub fn finish(mut self) -> Analysis {
        self.features.finish(&mut self.pending);
        self.run_batch();
        let duration_ms = self.processed_ms();
        // Последние патчи могут целиком лежать в добавленной тишине.
        let mut windows = self.windows;
        while windows.len() > 1 && windows.last().is_some_and(|w| w.start_ms >= duration_ms) {
            windows.pop();
        }
        let embeddings = self
            .embeddings
            .into_iter()
            .filter(|(i, _)| (*i as usize) < windows.len())
            .collect();
        Analysis {
            duration_ms,
            windows,
            embeddings,
        }
    }

    fn run_batch(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let patches = std::mem::take(&mut self.pending);

        let (gated, near_floor): (Vec<bool>, Vec<bool>) = patches
            .iter()
            .map(|p| {
                let (gate, quiet) = self.floor.level(p.rms_db, &self.thresholds);
                (self.use_gate && gate, quiet)
            })
            .unzip();
        let to_run: Vec<Patch> = patches
            .iter()
            .zip(&gated)
            .filter(|(_, g)| !**g)
            .map(|(p, _)| p.clone())
            .collect();
        let mut predictions = self.model.borrow().predict(&to_run).into_iter();

        for ((p, g), quiet) in patches.iter().zip(gated).zip(near_floor) {
            let (class, confidence, scores) = if g {
                self.gated += 1;
                (SoundClass::Silence, 1.0, ClassScores::default())
            } else {
                let pred = predictions.next().expect("one prediction per patch");
                let scores = ClassScores::from_audioset(&pred.scores);
                let candidate = scores.dog >= CANDIDATE_DOG || scores.animal >= CANDIDATE_ANIMAL;
                let (class, conf) = match &self.personal {
                    Some(m) if candidate => m.decide(&pred.embedding),
                    _ => decide(&scores, &self.thresholds),
                };
                let class = background_as_silence(class, quiet);
                if (candidate || class.is_dog()) && self.embeddings.len() < MAX_EMBEDDINGS {
                    let index = self.windows.len() as u32;
                    self.embeddings.push((index, to_f16(&pred.embedding)));
                }
                (class, conf, scores)
            };
            self.windows.push(WindowResult {
                start_ms: p.index * PATCH_HOP_MS,
                class,
                confidence,
                rms_db: p.rms_db,
                peak_db: p.peak_db,
                gated: g,
                scores,
            });
        }
    }
}

/// Оценка фонового шума по ходу записи.
#[derive(Debug, Default, Clone)]
pub(crate) struct NoiseFloor {
    db: Option<f32>,
}

impl NoiseFloor {
    /// Обновляет оценку и возвращает (отсеять ли окно гейтом, на уровне ли оно фона).
    /// Оценка быстро падает к тихим окнам и медленно растёт, поэтому лай её не «поднимает».
    /// Громкие окна гейтом не отсеиваются никогда — иначе запись, начавшаяся с лая, стала бы
    /// «фоном»; такие окна идут в сеть, и только если собаки в них нет, считаются тишиной.
    pub(crate) fn level(&mut self, rms_db: f32, th: &Thresholds) -> (bool, bool) {
        let floor = match self.db {
            None => rms_db,
            Some(f) => rms_db.min(f + FLOOR_RISE_DB),
        };
        self.db = Some(floor);
        let near_floor = rms_db < floor + th.gate_margin_db;
        let gate = rms_db < th.silence_db || (rms_db < th.quiet_max_db && near_floor);
        (gate, near_floor || rms_db < th.silence_db)
    }
}

/// Громкий фон (шумный диктофон) проходит гейт, но без собаки это та же тишина.
pub(crate) fn background_as_silence(class: SoundClass, quiet: bool) -> SoundClass {
    if class == SoundClass::Other && quiet {
        SoundClass::Silence
    } else {
        class
    }
}

/// Анализирует источник целиком. `progress` вызывается после каждой порции;
/// вернуть `false` — отменить анализ.
pub fn analyze(
    source: &mut dyn AudioSource,
    model: &Yamnet,
    thresholds: Thresholds,
    use_gate: bool,
    personal: Option<Arc<PersonalModel>>,
    mut progress: impl FnMut(Progress) -> bool,
) -> Result<Analysis, AnalyzeError> {
    let mut analyzer = StreamAnalyzer::new(model, thresholds).with_personal(personal);
    if !use_gate {
        analyzer = analyzer.without_gate();
    }
    let total_ms = source.duration_ms();
    let mut buf = Vec::with_capacity(TARGET_RATE as usize * 4);
    // Колбэк дёргаем не чаще раза в секунду звука: через FFI каждый вызов недёшев,
    // а пакетов в многочасовой записи — больше миллиона.
    let mut reported_ms = 0;
    loop {
        buf.clear();
        if !source.read(&mut buf)? {
            break;
        }
        analyzer.push(&buf);
        let processed_ms = analyzer.processed_ms();
        if processed_ms >= reported_ms + PROGRESS_STEP_MS {
            reported_ms = processed_ms;
            if !progress(Progress {
                processed_ms,
                total_ms,
            }) {
                return Err(AnalyzeError::Cancelled);
            }
        }
    }
    Ok(analyzer.finish())
}
