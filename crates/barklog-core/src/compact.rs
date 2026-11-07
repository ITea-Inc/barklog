//! Компактное хранение результатов по окнам и применение разметки пользователя.
//!
//! Чтобы пересчитать отчёт после «это не собака» или после обучения личной модели,
//! нужны результаты по всем окнам. В JSON 8 часов окон — мегабайты, поэтому храним
//! 4 байта на окно: класс + флаг гейта, уверенность, громкость, пик.

use serde::{Deserialize, Serialize};

use crate::SoundClass;
use crate::classify::ClassScores;
use crate::personal::{Label, PersonalModel};
use crate::pipeline::{Analysis, WindowResult};
use barklog_ml::PATCH_HOP_MS;

const GATED: u8 = 0x80;

pub fn encode_windows(windows: &[WindowResult]) -> Vec<u8> {
    let mut out = Vec::with_capacity(windows.len() * 4);
    for w in windows {
        out.push(w.class.index() as u8 | if w.gated { GATED } else { 0 });
        out.push((w.confidence.clamp(0.0, 1.0) * 255.0).round() as u8);
        out.push(db_byte(w.rms_db));
        out.push(db_byte(w.peak_db));
    }
    out
}

/// Окна идут подряд с шагом 0,48 с, поэтому время начала не храним.
pub fn decode_windows(bytes: &[u8]) -> Vec<WindowResult> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .map(|(i, b)| WindowResult {
            start_ms: i as u64 * PATCH_HOP_MS,
            class: SoundClass::ALL[(b[0] & !GATED) as usize % SoundClass::ALL.len()],
            confidence: b[1] as f32 / 255.0,
            rms_db: -(b[2] as f32),
            peak_db: -(b[3] as f32),
            gated: b[0] & GATED != 0,
            scores: ClassScores::default(),
        })
        .collect()
}

/// дБFS от 0 до −255 с шагом 1 дБ.
fn db_byte(db: f32) -> u8 {
    (-db).clamp(0.0, 255.0).round() as u8
}

/// Ответ пользователя на эпизод: всё собачье внутри [start_ms, end_ms) — это `label`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeLabel {
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: Label,
}

impl EpisodeLabel {
    fn covers(&self, w: &WindowResult) -> bool {
        w.start_ms >= self.start_ms && w.start_ms < self.end_ms
    }
}

/// Пересчитывает классы окон: сначала личной моделью (по сохранённым эмбеддингам),
/// потом поверх — явные ответы пользователя. Ответы всегда главнее модели.
pub fn reclassify(analysis: &mut Analysis, model: Option<&PersonalModel>, labels: &[EpisodeLabel]) {
    if let Some(model) = model {
        for (idx, emb) in &analysis.embeddings {
            if let Some(w) = analysis.windows.get_mut(*idx as usize) {
                let (class, conf) = model.decide(&crate::personal::to_f32(emb));
                w.class = class;
                w.confidence = conf;
            }
        }
    }
    for l in labels {
        for w in analysis.windows.iter_mut().filter(|w| l.covers(w)) {
            // «Не собака» снимает собачьи окна; «лай/скулёж/…» переназначает собачьи окна.
            if w.class.is_dog() {
                w.class = l.label.class();
            }
        }
    }
}

/// Обучающие примеры из одной сессии: окна с эмбеддингами внутри отмеченных эпизодов.
pub fn training_samples(analysis: &Analysis, labels: &[EpisodeLabel]) -> Vec<(Vec<f32>, Label)> {
    analysis
        .embeddings
        .iter()
        .filter_map(|(idx, emb)| {
            let w = analysis.windows.get(*idx as usize)?;
            let l = labels.iter().find(|l| l.covers(w))?;
            Some((crate::personal::to_f32(emb), l.label))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(i: u64, class: SoundClass) -> WindowResult {
        WindowResult {
            start_ms: i * PATCH_HOP_MS,
            class,
            confidence: 0.8,
            rms_db: -31.4,
            peak_db: -12.0,
            gated: class == SoundClass::Silence,
            scores: ClassScores::default(),
        }
    }

    #[test]
    fn roundtrip_keeps_what_reports_need() {
        let ws: Vec<_> = SoundClass::ALL
            .iter()
            .enumerate()
            .map(|(i, &c)| window(i as u64, c))
            .collect();
        let back = decode_windows(&encode_windows(&ws));
        assert_eq!(back.len(), ws.len());
        for (a, b) in ws.iter().zip(&back) {
            assert_eq!(a.start_ms, b.start_ms);
            assert_eq!(a.class, b.class);
            assert_eq!(a.gated, b.gated);
            assert!((a.confidence - b.confidence).abs() < 0.01);
            assert_eq!(b.rms_db, -31.0);
        }
    }

    #[test]
    fn labels_override_dog_windows_only() {
        let mut a = Analysis {
            duration_ms: 10 * PATCH_HOP_MS,
            windows: (0..10)
                .map(|i| {
                    window(
                        i,
                        if i % 2 == 0 {
                            SoundClass::Bark
                        } else {
                            SoundClass::Silence
                        },
                    )
                })
                .collect(),
            embeddings: Vec::new(),
        };
        let labels = [
            EpisodeLabel {
                start_ms: 0,
                end_ms: 4 * PATCH_HOP_MS,
                label: Label::NotDog,
            },
            EpisodeLabel {
                start_ms: 6 * PATCH_HOP_MS,
                end_ms: 10 * PATCH_HOP_MS,
                label: Label::Whine,
            },
        ];
        reclassify(&mut a, None, &labels);
        let classes: Vec<_> = a.windows.iter().map(|w| w.class).collect();
        use SoundClass::*;
        assert_eq!(
            classes,
            [
                Other, Silence, Other, Silence, Bark, Silence, Whine, Silence, Whine, Silence
            ]
        );
    }
}
