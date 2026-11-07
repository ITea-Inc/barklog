//! Живая запись: звук с микрофона анализируется на лету, сохраняются только
//! фрагменты вокруг звуков собаки, в любой момент можно получить снимок состояния.

use std::borrow::Borrow;
use std::path::Path;

use barklog_audio::{Resampler, TARGET_RATE};
use barklog_ml::{PATCH_WINDOW_MS, Yamnet};
use serde::{Deserialize, Serialize};

use crate::clips::{ClipIndex, ClipWriter};
use crate::pipeline::{Analysis, StreamAnalyzer};
use crate::report::{Report, ReportSettings, StartTime, build_report};
use crate::{SoundClass, Thresholds};

/// Окна по 0,48 с: сеть запускается примерно раз в 4 секунды.
const LIVE_BATCH: usize = 8;
/// Сколько звука сохранять до и после собачьего окна.
const CLIP_PAD_MS: u64 = 1_000;
/// «Сейчас лает», если собачье окно было в последние столько миллисекунд анализа.
const NOW_WINDOW_MS: u64 = 10_000;
/// Окно для «за последние N минут».
const RECENT_MS: u64 = 15 * 60_000;
/// Индекс фрагментов сохраняется не реже, чем раз в столько звука.
const INDEX_SAVE_MS: u64 = 60_000;

/// Состояние живой записи — для экрана записи и для отправки на другой телефон.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSnapshot {
    /// Сколько звука получено.
    pub elapsed_ms: u64,
    /// Сколько уже прошло через сеть (отстаёт на несколько секунд).
    pub analyzed_ms: u64,
    /// Собака слышна прямо сейчас (последние ~10 с).
    pub dog_now: bool,
    /// Когда собака последний раз подавала голос (смещение от начала записи).
    pub last_dog_sound_ms: Option<u64>,
    /// Голос собаки за последние 15 минут.
    pub recent_dog_ms: u64,
    /// Громкость последнего окна, дБFS.
    pub level_db: f32,
    pub report: Report,
}

pub struct LiveRecorder<M: Borrow<Yamnet>> {
    analyzer: StreamAnalyzer<M>,
    resampler: Resampler,
    input_rate: u32,
    /// Отсчётов получено с микрофона (до ресемплинга — у него есть небольшая задержка).
    input_samples: u64,
    clips: ClipWriter,
    started_at: StartTime,
    resampled: Vec<f32>,
    /// Сколько окон уже просмотрено на предмет сохранения звука.
    seen_windows: usize,
    last_index_save_ms: u64,
}

impl<M: Borrow<Yamnet>> LiveRecorder<M> {
    /// `input_rate` — частота микрофона; `clips_dir` — куда писать фрагменты звука.
    pub fn new(
        model: M,
        thresholds: Thresholds,
        input_rate: u32,
        started_at: StartTime,
        clips_dir: impl AsRef<Path>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            analyzer: StreamAnalyzer::new(model, thresholds).with_batch(LIVE_BATCH),
            resampler: Resampler::new(input_rate, TARGET_RATE),
            input_rate,
            input_samples: 0,
            clips: ClipWriter::create(clips_dir)?,
            started_at,
            resampled: Vec::new(),
            seen_windows: 0,
            last_index_save_ms: 0,
        })
    }

    pub fn started_at(&self) -> StartTime {
        self.started_at
    }

    pub fn clips_dir(&self) -> &Path {
        self.clips.dir()
    }

    /// Моно PCM 16 бит little-endian с частотой `input_rate`.
    pub fn push_pcm16(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let samples: Vec<f32> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b) as f32 / 32_768.0)
            .collect();
        self.push(&samples)
    }

    pub fn push(&mut self, samples: &[f32]) -> std::io::Result<()> {
        self.input_samples += samples.len() as u64;
        self.resampled.clear();
        self.resampler.process(samples, &mut self.resampled);
        self.analyzer.push(&self.resampled);
        // Сначала помечаем, что сохранить, потом отдаём звук писателю: так он успеет
        // записать фрагмент, пока звук ещё в буфере.
        self.mark_dog_windows();
        self.clips.push(&self.resampled)?;

        let processed = self.analyzer.processed_ms();
        if processed >= self.last_index_save_ms + INDEX_SAVE_MS {
            self.last_index_save_ms = processed;
            self.clips.save_index()?;
        }
        Ok(())
    }

    fn mark_dog_windows(&mut self) {
        let windows = self.analyzer.windows();
        for w in &windows[self.seen_windows..] {
            if w.class.is_dog() {
                self.clips.keep(
                    w.start_ms.saturating_sub(CLIP_PAD_MS),
                    w.start_ms + PATCH_WINDOW_MS + CLIP_PAD_MS,
                );
            }
        }
        self.seen_windows = windows.len();
    }

    pub fn snapshot(&self) -> LiveSnapshot {
        let analysis = self.analyzer.snapshot();
        let report = build_report(
            &analysis,
            &ReportSettings::for_duration(analysis.duration_ms),
            Some(self.started_at),
            None,
        );
        let windows = &analysis.windows;
        let last_dog = windows.iter().rev().find(|w| w.class.is_dog());
        let analyzed = analysis.duration_ms;
        LiveSnapshot {
            elapsed_ms: self.input_samples * 1000 / self.input_rate as u64,
            analyzed_ms: analyzed,
            dog_now: last_dog.is_some_and(|w| w.start_ms + NOW_WINDOW_MS >= analyzed),
            last_dog_sound_ms: last_dog.map(|w| w.start_ms + PATCH_WINDOW_MS),
            recent_dog_ms: recent_dog_ms(&analysis, RECENT_MS),
            level_db: windows.last().map(|w| w.rms_db).unwrap_or(-120.0),
            report,
        }
    }

    /// Завершает запись: дорабатывает хвост, дописывает фрагменты.
    pub fn finish(mut self) -> std::io::Result<(Analysis, ClipIndex)> {
        self.resampled.clear();
        self.resampler.flush(&mut self.resampled);
        self.analyzer.push(&self.resampled);
        self.clips.push(&self.resampled)?;
        let analysis = self.analyzer.finish();
        // Окна, появившиеся при завершении, тоже сохраняем — пока звук в буфере.
        for w in analysis.windows.iter().skip(self.seen_windows) {
            if w.class.is_dog() {
                self.clips.keep(
                    w.start_ms.saturating_sub(CLIP_PAD_MS),
                    w.start_ms + PATCH_WINDOW_MS + CLIP_PAD_MS,
                );
            }
        }
        let index = self.clips.finish()?;
        Ok((analysis, index))
    }
}

/// Сколько звучала собака за последние `span_ms` анализа (по шагу окна).
fn recent_dog_ms(analysis: &Analysis, span_ms: u64) -> u64 {
    let from = analysis.duration_ms.saturating_sub(span_ms);
    analysis
        .windows
        .iter()
        .filter(|w| w.start_ms >= from && w.class.is_dog())
        .count() as u64
        * barklog_ml::PATCH_HOP_MS
}

/// Удобный фасад: сколько времени по классам в снимке.
impl LiveSnapshot {
    pub fn class_ms(&self, c: SoundClass) -> u64 {
        self.report.stats.classes[c.index()].time_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StartSource;

    fn model() -> Option<Yamnet> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/yamnet.safetensors");
        p.exists().then(|| Yamnet::load(p).unwrap())
    }

    #[test]
    fn records_silence_and_finishes() {
        let Some(model) = model() else { return };
        let dir = std::env::temp_dir().join(format!("barklog-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let start = StartTime {
            unix_ms: 0,
            utc_offset_min: 0,
            source: StartSource::Recorded,
        };
        let mut rec =
            LiveRecorder::new(&model, Thresholds::default(), 48_000, start, &dir).unwrap();

        // 20 секунд тишины с микрофона 48 кГц порциями по 100 мс.
        let chunk = vec![0u8; 4800 * 2];
        for _ in 0..200 {
            rec.push_pcm16(&chunk).unwrap();
        }
        let snap = rec.snapshot();
        assert_eq!(snap.elapsed_ms, 20_000);
        assert!(
            snap.analyzed_ms > 10_000 && snap.analyzed_ms <= 20_000,
            "{}",
            snap.analyzed_ms
        );
        assert!(!snap.dog_now);
        assert_eq!(snap.report.stats.dog_time_ms, 0);

        let (analysis, index) = rec.finish().unwrap();
        assert_eq!(analysis.duration_ms, 20_000);
        assert!(index.segments.is_empty(), "тишину не сохраняем");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
