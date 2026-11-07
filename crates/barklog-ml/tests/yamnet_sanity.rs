//! Те же проверки, что в оригинальном `yamnet_test.py`: тишина, белый шум, синус.
//! Требуют `models/yamnet.safetensors` (Git LFS); без модели тесты пропускаются.

use std::path::PathBuf;

use barklog_ml::{FeatureExtractor, Yamnet, class_names};

fn model() -> Option<Yamnet> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/yamnet.safetensors");
    if !path.exists() {
        eprintln!("модель не найдена: {}", path.display());
        return None;
    }
    Some(Yamnet::load(path).expect("модель загружается"))
}

/// Топ-N классов по средним по клипу вероятностям.
fn top_classes(model: &Yamnet, waveform: &[f32], n: usize) -> Vec<(String, f32)> {
    let mut fx = FeatureExtractor::new();
    let mut patches = Vec::new();
    fx.push(waveform, &mut patches);
    fx.finish(&mut patches);
    let preds = model.predict(&patches);
    let mut mean = vec![0f32; barklog_ml::NUM_CLASSES];
    for p in &preds {
        mean.iter_mut()
            .zip(p.scores.iter())
            .for_each(|(m, s)| *m += s / preds.len() as f32);
    }
    let mut idx: Vec<usize> = (0..mean.len()).collect();
    idx.sort_by(|&a, &b| mean[b].total_cmp(&mean[a]));
    idx.into_iter()
        .take(n)
        .map(|i| (class_names()[i].clone(), mean[i]))
        .collect()
}

fn assert_in_top(model: &Yamnet, waveform: &[f32], expected: &str) {
    let top = top_classes(model, waveform, 10);
    assert!(
        top.iter().any(|(name, _)| name == expected),
        "{expected} не в топ-10: {top:?}"
    );
}

#[test]
fn zeros_are_silence() {
    let Some(m) = model() else { return };
    assert_in_top(&m, &vec![0.0; 48_000], "Silence");
}

#[test]
fn uniform_noise_is_white_noise() {
    let Some(m) = model() else { return };
    // Простой LCG вместо зависимости от rand.
    let mut state: u64 = 51773;
    let noise: Vec<f32> = (0..48_000)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        })
        .collect();
    assert_in_top(&m, &noise, "White noise");
}

#[test]
fn sine_is_sine_wave() {
    let Some(m) = model() else { return };
    let sine: Vec<f32> = (0..48_000)
        .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 16_000.0).sin())
        .collect();
    assert_in_top(&m, &sine, "Sine wave");
}
