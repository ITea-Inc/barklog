//! Компактное хранение результатов по окнам и применение разметки пользователя.
//!
//! Чтобы пересчитать отчёт после «это не собака» или после обучения личной модели,
//! нужны результаты по всем окнам. В JSON 8 часов окон — мегабайты, поэтому храним
//! 4 байта на окно: класс + флаг гейта, уверенность, громкость, пик.

use serde::{Deserialize, Serialize};

use crate::SoundClass;
use crate::classify::{ClassScores, Thresholds, decide};
use crate::personal::{Label, PersonalModel};
use crate::pipeline::{Analysis, NoiseFloor, WindowResult, background_as_silence};
use barklog_ml::{PATCH_HOP_MS, Yamnet};

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

/// Чем пересчитывать окна сохранённой записи.
pub struct Reclassify<'a> {
    /// YAMNet: по эмбеддингу заново считает вероятности и решает с `thresholds`.
    pub yamnet: Option<&'a Yamnet>,
    pub thresholds: &'a Thresholds,
    /// Порог, с которым окна решены при анализе: при том же пороге YAMNet не нужен.
    pub analyzed_dog_min: f32,
    pub personal: Option<&'a PersonalModel>,
}

/// Пересчитывает классы окон: окна с эмбеддингами — личной моделью или YAMNet с
/// текущими порогами, остальные собачьи окна — по сохранённой уверенности; потом
/// поверх — явные ответы пользователя. Ответы всегда главнее модели.
///
/// Эмбеддинги есть у всех окон, где YAMNet хоть немного слышал собаку
/// (см. [`crate::pipeline::CANDIDATE_DOG`]), пока не кончился лимит на сессию.
///
/// Возвращает, сколько окон пришлось на чужих собак: в голос своей собаки они не
/// входят (класс — «другие звуки»), но хозяину полезно знать, что соседи лаяли.
pub fn reclassify(analysis: &mut Analysis, with: &Reclassify, labels: &[EpisodeLabel]) -> usize {
    // Та же оценка фона, что и при анализе: по ней «не собака» становится тишиной.
    let mut floor = NoiseFloor::default();
    let quiet: Vec<bool> = analysis
        .windows
        .iter()
        .map(|w| floor.level(w.rms_db, with.thresholds).1)
        .collect();
    let yamnet = with
        .yamnet
        .filter(|_| (with.thresholds.dog_min - with.analyzed_dog_min).abs() > 1e-4);
    let mut has_embedding = vec![false; analysis.windows.len()];
    for (idx, _) in &analysis.embeddings {
        if let Some(h) = has_embedding.get_mut(*idx as usize) {
            *h = true;
        }
    }
    let mut foreign = vec![false; analysis.windows.len()];
    // (класс, уверенность, чужая ли собака)
    let decisions: Vec<(SoundClass, f32, bool)> = match (with.personal, yamnet) {
        (Some(m), _) => analysis
            .embeddings
            .iter()
            .map(|(_, e)| {
                let (label, p) = m.decide_label(&e.to_f32());
                (label.class(), p, label == Label::ForeignDog)
            })
            .collect(),
        (None, Some(y)) => {
            // Одним пакетом: по одному окну на телефоне выходит в разы дольше.
            let flat: Vec<f32> = analysis
                .embeddings
                .iter()
                .flat_map(|(_, e)| e.to_f32())
                .collect();
            y.scores_from_embeddings(&flat)
                .as_chunks::<{ barklog_ml::NUM_CLASSES }>()
                .0
                .iter()
                .map(|s| {
                    let (class, conf) = decide(&ClassScores::from_audioset(s), with.thresholds);
                    (class, conf, false)
                })
                .collect()
        }
        (None, None) => Vec::new(),
    };
    for ((idx, _), (class, conf, is_foreign)) in analysis.embeddings.iter().zip(decisions) {
        let i = *idx as usize;
        if let Some(w) = analysis.windows.get_mut(i) {
            // Чужой лай — не фон, даже тихий: тишиной его не считаем.
            w.class = if is_foreign {
                class
            } else {
                background_as_silence(class, quiet[i])
            };
            w.confidence = conf;
            foreign[i] = is_foreign;
        }
    }
    // Окна сверх лимита эмбеддингов: уверенность собачьего окна — вероятность «собака»,
    // поэтому поднятый порог применяется и к ним.
    if with.personal.is_none() {
        for (i, w) in analysis.windows.iter_mut().enumerate() {
            if !has_embedding[i] && w.class.is_dog() && w.confidence < with.thresholds.dog_min {
                w.class = background_as_silence(SoundClass::Other, quiet[i]);
                w.confidence = 1.0 - w.confidence;
            }
        }
    }
    for l in labels {
        for (w, f) in analysis
            .windows
            .iter_mut()
            .zip(foreign.iter_mut())
            .filter(|(w, _)| l.covers(w))
        {
            // «Не собака» и «чужая собака» снимают собачьи окна; «лай/скулёж/…»
            // переназначает собачьи окна.
            if w.class.is_dog() {
                w.class = l.label.class();
                *f = l.label == Label::ForeignDog;
            }
        }
    }
    foreign.iter().filter(|&&f| f).count()
}

/// Обучающие примеры из одной сессии: окна с эмбеддингами внутри отмеченных эпизодов.
pub fn training_samples(analysis: &Analysis, labels: &[EpisodeLabel]) -> Vec<(Vec<f32>, Label)> {
    analysis
        .embeddings
        .iter()
        .filter_map(|(idx, emb)| {
            let w = analysis.windows.get(*idx as usize)?;
            let l = labels.iter().find(|l| l.covers(w))?;
            Some((emb.to_f32(), l.label))
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
        let th = Thresholds::default();
        let with = Reclassify {
            yamnet: None,
            thresholds: &th,
            analyzed_dog_min: Thresholds::default().dog_min,
            personal: None,
        };
        reclassify(&mut a, &with, &labels);
        let classes: Vec<_> = a.windows.iter().map(|w| w.class).collect();
        use SoundClass::*;
        assert_eq!(
            classes,
            [
                Other, Silence, Other, Silence, Bark, Silence, Whine, Silence, Whine, Silence
            ]
        );
    }

    #[test]
    fn raised_threshold_drops_unsure_dog_windows() {
        // Ровный фон −40 дБ, на нём два собачьих окна: уверенное и сомнительное.
        let mut ws: Vec<_> = (0..6).map(|i| window(i, SoundClass::Other)).collect();
        for w in &mut ws {
            w.rms_db = -40.0;
            w.gated = false;
        }
        ws[2].class = SoundClass::Bark;
        ws[2].confidence = 0.9;
        ws[2].rms_db = -20.0;
        ws[4].class = SoundClass::Whine;
        ws[4].confidence = 0.2;
        let mut a = Analysis {
            duration_ms: 6 * PATCH_HOP_MS,
            windows: ws,
            embeddings: Vec::new(),
        };
        let th = Thresholds {
            dog_min: 0.5,
            ..Thresholds::default()
        };
        let with = Reclassify {
            yamnet: None,
            thresholds: &th,
            analyzed_dog_min: Thresholds::default().dog_min,
            personal: None,
        };
        reclassify(&mut a, &with, &[]);
        assert_eq!(a.windows[2].class, SoundClass::Bark);
        // Тихое сомнительное окно — уже не собака и лежит на уровне фона.
        assert_eq!(a.windows[4].class, SoundClass::Silence);
    }

    #[test]
    fn foreign_dog_label_counts_separately() {
        let mut a = Analysis {
            duration_ms: 6 * PATCH_HOP_MS,
            windows: (0..6).map(|i| window(i, SoundClass::Bark)).collect(),
            embeddings: Vec::new(),
        };
        let labels = [EpisodeLabel {
            start_ms: 0,
            end_ms: 2 * PATCH_HOP_MS,
            label: Label::ForeignDog,
        }];
        let th = Thresholds::default();
        let with = Reclassify {
            yamnet: None,
            thresholds: &th,
            analyzed_dog_min: th.dog_min,
            personal: None,
        };
        assert_eq!(reclassify(&mut a, &with, &labels), 2);
        assert_eq!(a.windows[0].class, SoundClass::Other);
        assert_eq!(a.windows[2].class, SoundClass::Bark);
    }
}
