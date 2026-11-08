//! Настройки чувствительности, которые меняет пользователь.

use serde::{Deserialize, Serialize};

use crate::classify::Thresholds;
use crate::pipeline::CANDIDATE_DOG;
use crate::report::ReportSettings;

/// Порог «это собака» и правила сборки эпизодов. Меняются задним числом:
/// все записи с сохранёнными окнами пересчитываются без повторного анализа.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sensitivity {
    /// Минимальная «собачья» вероятность окна, 0–1. Меньше — чувствительнее.
    pub dog_min: f32,
    /// Паузы короче этого не разрывают эпизод.
    pub merge_gap_ms: u64,
    /// Эпизоды короче этого не считаются.
    pub min_episode_ms: u64,
}

impl Default for Sensitivity {
    fn default() -> Self {
        let r = ReportSettings::default();
        Self {
            dog_min: Thresholds::default().dog_min,
            merge_gap_ms: r.merge_gap_ms,
            min_episode_ms: r.min_episode_ms,
        }
    }
}

impl Sensitivity {
    /// Ниже `CANDIDATE_DOG` у окон нет сохранённых эмбеддингов — пересчитать их нельзя.
    pub const DOG_MIN: (f32, f32) = (CANDIDATE_DOG, 0.9);
    pub const MAX_GAP_MS: u64 = 30_000;
    pub const MAX_MIN_EPISODE_MS: u64 = 30_000;

    /// Приводит значения к допустимым пределам.
    pub fn clamped(self) -> Self {
        let dog_min = if self.dog_min.is_finite() {
            self.dog_min
        } else {
            Self::default().dog_min
        };
        Self {
            dog_min: dog_min.clamp(Self::DOG_MIN.0, Self::DOG_MIN.1),
            merge_gap_ms: self.merge_gap_ms.min(Self::MAX_GAP_MS),
            min_episode_ms: self.min_episode_ms.min(Self::MAX_MIN_EPISODE_MS),
        }
    }

    pub fn thresholds(&self) -> Thresholds {
        Thresholds {
            dog_min: self.dog_min,
            ..Thresholds::default()
        }
    }

    /// Параметры отчёта: свои правила эпизодов поверх `base` (ширина таймлайна и прочее).
    pub fn apply(&self, base: &ReportSettings) -> ReportSettings {
        ReportSettings {
            merge_gap_ms: self.merge_gap_ms,
            min_episode_ms: self.min_episode_ms,
            ..base.clone()
        }
    }

    /// Параметры отчёта для записи, которую только что разобрали с этими порогами.
    pub fn report_settings(&self, duration_ms: u64) -> ReportSettings {
        ReportSettings {
            windows_dog_min: self.dog_min,
            ..self.apply(&ReportSettings::for_duration(duration_ms))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_core_defaults() {
        let s = Sensitivity::default();
        assert_eq!(s.thresholds().dog_min, Thresholds::default().dog_min);
        let r = s.report_settings(60_000);
        assert_eq!(r.merge_gap_ms, ReportSettings::default().merge_gap_ms);
        assert_eq!(
            r.timeline_bin_ms,
            ReportSettings::for_duration(60_000).timeline_bin_ms
        );
    }

    #[test]
    fn clamps_out_of_range() {
        let s = Sensitivity {
            dog_min: 0.0,
            merge_gap_ms: 1_000_000,
            min_episode_ms: 1_000_000,
        }
        .clamped();
        assert_eq!(s.dog_min, CANDIDATE_DOG);
        assert_eq!(s.merge_gap_ms, Sensitivity::MAX_GAP_MS);
        assert_eq!(s.min_episode_ms, Sensitivity::MAX_MIN_EPISODE_MS);
        let s = Sensitivity {
            dog_min: f32::NAN,
            ..Sensitivity::default()
        }
        .clamped();
        assert_eq!(s.dog_min, Sensitivity::default().dog_min);
    }
}
