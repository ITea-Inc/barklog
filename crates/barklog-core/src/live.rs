//! Живая запись: звук с микрофона анализируется на лету, сохраняются только
//! фрагменты вокруг звуков собаки, в любой момент можно получить снимок состояния.

use std::borrow::Borrow;
use std::path::Path;

use barklog_audio::{Resampler, TARGET_RATE};
use barklog_ml::{PATCH_WINDOW_MS, Yamnet};
use serde::{Deserialize, Serialize};

use crate::checkpoint::CheckpointWriter;
use crate::clips::{ClipIndex, ClipWriter};
use crate::pipeline::{Analysis, StreamAnalyzer};
use crate::report::{Report, StartTime, build_report};
use crate::{Sensitivity, SoundClass};

/// Окна по 0,48 с: сеть запускается примерно раз в 4 секунды.
const LIVE_BATCH: usize = 8;
/// Сколько звука сохранять до и после собачьего окна.
const CLIP_PAD_MS: u64 = 1_000;
/// «Сейчас лает», если собачье окно было в последние столько миллисекунд анализа.
const NOW_WINDOW_MS: u64 = 10_000;
/// Окно для «за последние N минут».
const RECENT_MS: u64 = 15 * 60_000;
/// Индекс фрагментов и контрольная точка анализа сохраняются раз в столько звука:
/// если систему убьёт приложение, потеряется не больше этого.
const INDEX_SAVE_MS: u64 = 30_000;

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
    /// Идущий сейчас период активности (паузы до 30 с не прерывают его) —
    /// по нему второй телефон решает, не пора ли прислать уведомление.
    pub active_bout: Option<ActiveBout>,
    pub report: Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveBout {
    /// Начало периода от начала записи.
    pub start_ms: u64,
    /// Сколько он длится к моменту снимка.
    pub duration_ms: u64,
    /// Чистое время голоса внутри периода.
    pub sound_ms: u64,
}

/// Версия формата снимка для синхронизации между телефонами.
pub const SYNC_FORMAT: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("снимок записан более новой версией приложения (формат {0})")]
    UnsupportedVersion(u32),
    #[error("повреждённый снимок: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u32,
    snapshot: LiveSnapshot,
}

impl LiveSnapshot {
    /// Компактный JSON для отправки на другой телефон (без звука — только цифры).
    pub fn to_sync_json(&self) -> String {
        serde_json::to_string(&Envelope {
            v: SYNC_FORMAT,
            snapshot: self.clone(),
        })
        .expect("snapshot is always serializable")
    }

    pub fn from_sync_json(json: &str) -> Result<Self, SyncError> {
        #[derive(Deserialize)]
        struct Version {
            v: u32,
        }
        let Version { v } = serde_json::from_str(json)?;
        if v > SYNC_FORMAT {
            return Err(SyncError::UnsupportedVersion(v));
        }
        Ok(serde_json::from_str::<Envelope>(json)?.snapshot)
    }
}

pub struct LiveRecorder<M: Borrow<Yamnet>> {
    analyzer: StreamAnalyzer<M>,
    resampler: Resampler,
    input_rate: u32,
    /// Отсчётов получено с микрофона (до ресемплинга — у него есть небольшая задержка).
    input_samples: u64,
    clips: ClipWriter,
    checkpoint: CheckpointWriter,
    started_at: StartTime,
    sensitivity: Sensitivity,
    resampled: Vec<f32>,
    /// Сколько окон уже просмотрено на предмет сохранения звука.
    seen_windows: usize,
    last_index_save_ms: u64,
}

impl<M: Borrow<Yamnet>> LiveRecorder<M> {
    /// `input_rate` — частота микрофона; `clips_dir` — куда писать фрагменты звука.
    pub fn new(
        model: M,
        sensitivity: Sensitivity,
        input_rate: u32,
        started_at: StartTime,
        clips_dir: impl AsRef<Path>,
    ) -> std::io::Result<Self> {
        let checkpoint = CheckpointWriter::create(&clips_dir, started_at, sensitivity)?;
        Ok(Self {
            analyzer: StreamAnalyzer::new(model, sensitivity.thresholds()).with_batch(LIVE_BATCH),
            resampler: Resampler::new(input_rate, TARGET_RATE),
            input_rate,
            input_samples: 0,
            clips: ClipWriter::create(clips_dir)?,
            checkpoint,
            started_at,
            sensitivity,
            resampled: Vec::new(),
            seen_windows: 0,
            last_index_save_ms: 0,
        })
    }

    /// Метка в контрольной точке — по ней после сбоя понятно, чья это запись.
    pub fn with_tag(mut self, tag: Option<String>) -> Self {
        self.checkpoint.set_tag(tag);
        self
    }

    /// Личная модель для уточнения классов (см. [`crate::personal`]).
    pub fn with_personal(mut self, model: Option<std::sync::Arc<crate::PersonalModel>>) -> Self {
        self.analyzer = self.analyzer.with_personal(model);
        self
    }

    pub fn started_at(&self) -> StartTime {
        self.started_at
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
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
            self.checkpoint.write(
                self.analyzer.windows(),
                self.analyzer.embeddings(),
                self.analyzer.covered_ms(),
            )?;
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
            &self.sensitivity.report_settings(analysis.duration_ms),
            Some(self.started_at),
            None,
        );
        let windows = &analysis.windows;
        let last_dog = windows.iter().rev().find(|w| w.class.is_dog());
        let analyzed = analysis.duration_ms;
        let dog_now = last_dog.is_some_and(|w| w.start_ms + NOW_WINDOW_MS >= analyzed);
        let active_bout = report.bouts.last().filter(|_| dog_now).map(|b| ActiveBout {
            start_ms: b.start_ms,
            duration_ms: analyzed.saturating_sub(b.start_ms),
            sound_ms: b.sound_ms,
        });
        LiveSnapshot {
            elapsed_ms: self.input_samples * 1000 / self.input_rate as u64,
            analyzed_ms: analyzed,
            dog_now,
            active_bout,
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
            LiveRecorder::new(&model, Sensitivity::default(), 48_000, start, &dir).unwrap();

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

        assert!(snap.active_bout.is_none());

        // Снимок проходит через JSON без потерь, чужая версия формата отвергается.
        let json = snap.to_sync_json();
        let back = LiveSnapshot::from_sync_json(&json).unwrap();
        assert_eq!(back.elapsed_ms, snap.elapsed_ms);
        assert_eq!(back.report.timeline.len(), snap.report.timeline.len());
        let future = json.replacen(&format!("\"v\":{SYNC_FORMAT}"), "\"v\":99", 1);
        assert!(matches!(
            LiveSnapshot::from_sync_json(&future),
            Err(SyncError::UnsupportedVersion(99))
        ));
        assert!(
            json.len() < 20_000,
            "снимок 20 с должен быть маленьким: {}",
            json.len()
        );

        // Через 30 с звука появляется контрольная точка — запись переживёт сбой.
        for _ in 0..150 {
            rec.push_pcm16(&chunk).unwrap();
        }
        let recovered = crate::checkpoint::recover(&dir)
            .unwrap()
            .expect("точка записана");
        assert!(recovered.analysis.duration_ms >= 25_000);
        assert!(!recovered.analysis.windows.is_empty());

        let (analysis, index) = rec.finish().unwrap();
        assert_eq!(analysis.duration_ms, 35_000);
        assert!(index.segments.is_empty(), "тишину не сохраняем");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
