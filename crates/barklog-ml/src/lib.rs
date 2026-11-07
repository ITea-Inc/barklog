//! ML-часть BarkLog: признаки YAMNet и инференс сети на чистом Rust
//! (без TensorFlow/ONNX — одинаково собирается под Android, iOS и десктоп).

pub mod features;
pub mod yamnet;

use std::sync::OnceLock;

pub use features::{FeatureExtractor, PATCH_HOP_MS, PATCH_WINDOW_MS, Patch};
pub use yamnet::{EMBEDDING_SIZE, NUM_CLASSES, Prediction, Yamnet};

#[derive(Debug, thiserror::Error)]
pub enum MlError {
    #[error("не удалось прочитать модель: {0}")]
    Io(#[from] std::io::Error),
    #[error("некорректный файл модели: {0}")]
    Model(String),
}

/// Индексы классов AudioSet, относящихся к собакам (см. `yamnet_class_map.csv`).
pub mod audioset {
    pub const ANIMAL: usize = 67;
    pub const DOMESTIC_ANIMALS: usize = 68;
    pub const DOG: usize = 69;
    pub const BARK: usize = 70;
    pub const YIP: usize = 71;
    pub const HOWL: usize = 72;
    pub const BOW_WOW: usize = 73;
    pub const GROWLING: usize = 74;
    pub const WHIMPER_DOG: usize = 75;
    pub const CANIDAE: usize = 117;
    pub const WHIMPER: usize = 21;
    pub const SILENCE: usize = 494;
}

/// Названия 521 класса AudioSet в порядке выходов YAMNet.
pub fn class_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        include_str!("../assets/yamnet_class_map.csv")
            .lines()
            .skip(1)
            .filter(|l| !l.is_empty())
            .map(|line| {
                // index,mid,display_name — имя может быть в кавычках и содержать запятые.
                let name = line.splitn(3, ',').nth(2).unwrap_or_default();
                name.trim().trim_matches('"').to_string()
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_map_is_complete() {
        let names = class_names();
        assert_eq!(names.len(), NUM_CLASSES);
        assert_eq!(names[audioset::DOG], "Dog");
        assert_eq!(names[audioset::BARK], "Bark");
        assert_eq!(names[audioset::WHIMPER_DOG], "Whimper (dog)");
        assert_eq!(names[audioset::CANIDAE], "Canidae, dogs, wolves");
        assert_eq!(names[audioset::SILENCE], "Silence");
    }
}
