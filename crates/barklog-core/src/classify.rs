//! Перевод 521 вероятности AudioSet в наши классы.

use barklog_ml::audioset as a;
use serde::{Deserialize, Serialize};

use crate::SoundClass;

/// Пороги принятия решения. Значения по умолчанию — стартовые, их стоит
/// подстроить на реальных записях конкретной собаки.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thresholds {
    /// Минимальная «собачья» вероятность, чтобы окно считалось звуком собаки.
    pub dog_min: f32,
    /// Окно тише оценки фонового шума + `gate_margin_db` считается тишиной без нейросети.
    pub gate_margin_db: f32,
    /// Окно тише этого уровня (дБFS) — тишина в любом случае.
    pub silence_db: f32,
    /// Окна громче этого уровня (дБFS) всегда идут в сеть, даже если близки к фону.
    pub quiet_max_db: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            dog_min: 0.15,
            gate_margin_db: 4.0,
            silence_db: -70.0,
            quiet_max_db: -50.0,
        }
    }
}

/// Сводные вероятности для наших классов (сохраняются для отладки и калибровки).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct ClassScores {
    pub dog: f32,
    pub bark: f32,
    pub whine: f32,
    pub howl: f32,
    pub growl: f32,
    pub silence: f32,
}

impl ClassScores {
    pub fn from_audioset(s: &[f32]) -> Self {
        let bark = s[a::BARK].max(s[a::YIP]).max(s[a::BOW_WOW]);
        // У YAMNet скулёж собаки часто уходит в «человеческий» Whimper.
        let whine = s[a::WHIMPER_DOG].max(s[a::WHIMPER]);
        let howl = s[a::HOWL];
        let growl = s[a::GROWLING];
        let dog = s[a::DOG]
            .max(s[a::CANIDAE])
            .max(bark)
            .max(howl)
            .max(growl)
            .max(s[a::WHIMPER_DOG]);
        Self {
            dog,
            bark,
            whine,
            howl,
            growl,
            silence: s[a::SILENCE],
        }
    }
}

/// Решение по одному окну: класс и уверенность.
pub fn decide(scores: &ClassScores, th: &Thresholds) -> (SoundClass, f32) {
    // Человеческий Whimper без признаков собаки — не скулёж собаки.
    if scores.dog < th.dog_min {
        return if scores.silence >= 0.5 {
            (SoundClass::Silence, scores.silence)
        } else {
            (SoundClass::Other, 1.0 - scores.dog)
        };
    }
    let candidates = [
        (SoundClass::Bark, scores.bark),
        (SoundClass::Whine, scores.whine),
        (SoundClass::Howl, scores.howl),
        (SoundClass::Growl, scores.growl),
    ];
    let (class, score) = candidates
        .into_iter()
        .max_by(|x, y| x.1.total_cmp(&y.1))
        .expect("non-empty");
    // «Собака» без явного подкласса — чаще всего лай.
    let class = if score < 0.02 {
        SoundClass::Bark
    } else {
        class
    };
    (class, scores.dog)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scores(f: impl FnOnce(&mut Vec<f32>)) -> ClassScores {
        let mut s = vec![0.0; barklog_ml::NUM_CLASSES];
        f(&mut s);
        ClassScores::from_audioset(&s)
    }

    #[test]
    fn picks_dog_subclass() {
        let th = Thresholds::default();
        let s = scores(|s| {
            s[a::DOG] = 0.8;
            s[a::BARK] = 0.6;
            s[a::HOWL] = 0.1;
        });
        assert_eq!(decide(&s, &th).0, SoundClass::Bark);

        let s = scores(|s| {
            s[a::DOG] = 0.4;
            s[a::WHIMPER_DOG] = 0.3;
        });
        assert_eq!(decide(&s, &th).0, SoundClass::Whine);
    }

    #[test]
    fn human_whimper_alone_is_not_dog() {
        let s = scores(|s| s[a::WHIMPER] = 0.7);
        assert_eq!(decide(&s, &Thresholds::default()).0, SoundClass::Other);
    }

    #[test]
    fn silence_and_other() {
        let th = Thresholds::default();
        assert_eq!(
            decide(&scores(|s| s[a::SILENCE] = 0.9), &th).0,
            SoundClass::Silence
        );
        assert_eq!(decide(&scores(|_| {}), &th).0, SoundClass::Other);
    }

    #[test]
    fn generic_dog_defaults_to_bark() {
        let s = scores(|s| s[a::DOG] = 0.5);
        assert_eq!(decide(&s, &Thresholds::default()).0, SoundClass::Bark);
    }
}
