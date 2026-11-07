//! Потоковый анализ: сигнал 16 кГц → патчи → гейт по громкости → YAMNet → окна.

use barklog_audio::{AudioError, AudioSource, TARGET_RATE};
use barklog_ml::{FeatureExtractor, PATCH_HOP_MS, Patch, Yamnet};
use serde::{Deserialize, Serialize};

use crate::SoundClass;
use crate::classify::{ClassScores, Thresholds, decide};

/// Сколько патчей копить перед запуском сети (≈ 30 с звука).
const BATCH_PATCHES: usize = 64;
/// Насколько быстро оценка фонового шума может расти, дБ на окно (≈ 6 дБ в минуту).
const FLOOR_RISE_DB: f32 = 0.05;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub duration_ms: u64,
    pub windows: Vec<WindowResult>,
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
pub struct StreamAnalyzer<'m> {
    model: &'m Yamnet,
    thresholds: Thresholds,
    use_gate: bool,
    features: FeatureExtractor,
    pending: Vec<Patch>,
    windows: Vec<WindowResult>,
    noise_floor_db: Option<f32>,
    gated: usize,
}

impl<'m> StreamAnalyzer<'m> {
    pub fn new(model: &'m Yamnet, thresholds: Thresholds) -> Self {
        Self {
            model,
            thresholds,
            use_gate: true,
            features: FeatureExtractor::new(),
            pending: Vec::new(),
            windows: Vec::new(),
            noise_floor_db: None,
            gated: 0,
        }
    }

    /// Отключает гейт по громкости: каждое окно идёт в сеть (медленнее, для сверки).
    pub fn without_gate(mut self) -> Self {
        self.use_gate = false;
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
        if self.pending.len() >= BATCH_PATCHES {
            self.run_batch();
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
        Analysis {
            duration_ms,
            windows,
        }
    }

    fn run_batch(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let patches = std::mem::take(&mut self.pending);

        let mut gated = vec![false; patches.len()];
        for (g, p) in gated.iter_mut().zip(&patches) {
            *g = self.use_gate && self.is_quiet(p.rms_db);
        }
        let to_run: Vec<Patch> = patches
            .iter()
            .zip(&gated)
            .filter(|(_, g)| !**g)
            .map(|(p, _)| p.clone())
            .collect();
        let mut predictions = self.model.predict(&to_run).into_iter();

        for (p, g) in patches.iter().zip(gated) {
            let (class, confidence, scores) = if g {
                self.gated += 1;
                (SoundClass::Silence, 1.0, ClassScores::default())
            } else {
                let pred = predictions.next().expect("one prediction per patch");
                let scores = ClassScores::from_audioset(&pred.scores);
                let (class, conf) = decide(&scores, &self.thresholds);
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

    /// Обновляет оценку фонового шума и решает, тихое ли окно.
    /// Оценка быстро падает к тихим окнам и медленно растёт, поэтому лай её не «поднимает».
    /// Громкие окна не отсеиваются никогда — иначе запись, начавшаяся с лая, стала бы «фоном».
    fn is_quiet(&mut self, rms_db: f32) -> bool {
        let floor = match self.noise_floor_db {
            None => rms_db,
            Some(f) => rms_db.min(f + FLOOR_RISE_DB),
        };
        self.noise_floor_db = Some(floor);
        let th = &self.thresholds;
        rms_db < th.silence_db || (rms_db < th.quiet_max_db && rms_db < floor + th.gate_margin_db)
    }
}

/// Анализирует источник целиком. `progress` вызывается после каждой порции;
/// вернуть `false` — отменить анализ.
pub fn analyze(
    source: &mut dyn AudioSource,
    model: &Yamnet,
    thresholds: Thresholds,
    use_gate: bool,
    mut progress: impl FnMut(Progress) -> bool,
) -> Result<Analysis, AnalyzeError> {
    let mut analyzer = StreamAnalyzer::new(model, thresholds);
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
            if !progress(Progress { processed_ms, total_ms }) {
                return Err(AnalyzeError::Cancelled);
            }
        }
    }
    Ok(analyzer.finish())
}
