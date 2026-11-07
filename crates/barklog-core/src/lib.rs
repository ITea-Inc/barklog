//! Ядро BarkLog: пайплайн анализа, эпизоды и статистика.

pub mod classify;
pub mod clips;
pub mod live;
pub mod pipeline;
pub mod report;
pub mod start;

use serde::{Deserialize, Serialize};

pub use classify::{ClassScores, Thresholds};
pub use live::{LiveRecorder, LiveSnapshot};
pub use pipeline::{Analysis, AnalyzeError, Progress, StreamAnalyzer, WindowResult, analyze};
pub use report::{Episode, Report, ReportSettings, StartTime, Stats, build_report};
pub use start::{StartSource, start_from_modified, start_from_name};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SoundClass {
    Bark,
    Whine,
    Howl,
    Growl,
    Other,
    Silence,
}

impl SoundClass {
    pub const ALL: [SoundClass; 6] = [
        SoundClass::Bark,
        SoundClass::Whine,
        SoundClass::Howl,
        SoundClass::Growl,
        SoundClass::Other,
        SoundClass::Silence,
    ];
    pub const DOG: [SoundClass; 4] = [
        SoundClass::Bark,
        SoundClass::Whine,
        SoundClass::Howl,
        SoundClass::Growl,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn is_dog(self) -> bool {
        matches!(
            self,
            SoundClass::Bark | SoundClass::Whine | SoundClass::Howl | SoundClass::Growl
        )
    }

    pub fn label_ru(self) -> &'static str {
        match self {
            SoundClass::Bark => "Лай",
            SoundClass::Whine => "Скулёж",
            SoundClass::Howl => "Вой",
            SoundClass::Growl => "Рычание",
            SoundClass::Other => "Другие звуки",
            SoundClass::Silence => "Тишина",
        }
    }
}
