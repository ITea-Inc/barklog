//! Лог-мел спектрограмма и нарезка на патчи — в точности как во фронтенде YAMNet
//! (`features.py`): STFT 25 мс / 10 мс, 64 мел-полосы 125–7500 Гц,
//! патчи по 96 кадров (0.96 с) с шагом 48 кадров (0.48 с).

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

pub const SAMPLE_RATE: u32 = 16_000;
pub const MEL_BANDS: usize = 64;
pub const PATCH_FRAMES: usize = 96;
pub const PATCH_HOP_FRAMES: usize = 48;
pub const PATCH_LEN: usize = PATCH_FRAMES * MEL_BANDS;
/// Шаг патча в миллисекундах.
pub const PATCH_HOP_MS: u64 = 480;
/// Длина патча в миллисекундах.
pub const PATCH_WINDOW_MS: u64 = 960;

const WINDOW: usize = 400; // 25 мс
const HOP: usize = 160; // 10 мс
const FFT_LEN: usize = 512;
const BINS: usize = FFT_LEN / 2 + 1;
const MEL_MIN_HZ: f64 = 125.0;
const MEL_MAX_HZ: f64 = 7500.0;
const LOG_OFFSET: f32 = 0.001;
/// Минимальная длина сигнала для одного патча (см. `pad_waveform` в YAMNet).
const MIN_SAMPLES: usize = (PATCH_FRAMES - 1) * HOP + WINDOW;
const PATCH_HOP_SAMPLES: usize = PATCH_HOP_FRAMES * HOP;

/// Окно 0.96 с, готовое для подачи в нейросеть.
#[derive(Clone)]
pub struct Patch {
    /// Порядковый номер; начало патча = `index * PATCH_HOP_MS`.
    pub index: u64,
    /// `[PATCH_FRAMES][MEL_BANDS]` — лог-мел признаки.
    pub features: Box<[f32]>,
    /// Средняя громкость окна, дБ относительно полной шкалы.
    pub rms_db: f32,
    /// Самый громкий 25-мс кадр окна, дБFS.
    pub peak_db: f32,
}

impl Patch {
    pub fn start_ms(&self) -> u64 {
        self.index * PATCH_HOP_MS
    }
}

/// Потоковый извлекатель признаков: принимает 16 кГц моно, отдаёт патчи.
pub struct FeatureExtractor {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    mel: Vec<[f32; MEL_BANDS]>, // [BINS][MEL_BANDS]
    fft_in: Vec<f32>,
    fft_out: Vec<realfft::num_complex::Complex<f32>>,
    fft_scratch: Vec<realfft::num_complex::Complex<f32>>,

    samples: Vec<f32>,
    total_samples: u64,
    frames: Vec<[f32; MEL_BANDS]>,
    powers: Vec<f32>,
    next_patch: u64,
    finished: bool,
}

impl Default for FeatureExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureExtractor {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let fft_in = fft.make_input_vec();
        let fft_out = fft.make_output_vec();
        let fft_scratch = fft.make_scratch_vec();
        // Периодическое окно Ханна, как в tf.signal.stft.
        let window = (0..WINDOW)
            .map(|n| {
                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / WINDOW as f64).cos()) as f32
            })
            .collect();
        Self {
            fft,
            window,
            mel: mel_matrix(),
            fft_in,
            fft_out,
            fft_scratch,
            samples: Vec::new(),
            total_samples: 0,
            frames: Vec::new(),
            powers: Vec::new(),
            next_patch: 0,
            finished: false,
        }
    }

    /// Сколько отсчётов уже принято.
    pub fn samples_seen(&self) -> u64 {
        self.total_samples
    }

    /// Принимает порцию сигнала и дописывает готовые патчи в `out`.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Patch>) {
        assert!(!self.finished, "push after finish");
        self.total_samples += samples.len() as u64;
        self.samples.extend_from_slice(samples);
        self.consume(out);
    }

    /// Дополняет сигнал тишиной до целого числа патчей и отдаёт последние патчи.
    pub fn finish(&mut self, out: &mut Vec<Patch>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let n = self.total_samples as usize;
        let padded = if n <= MIN_SAMPLES {
            MIN_SAMPLES
        } else {
            MIN_SAMPLES + (n - MIN_SAMPLES).div_ceil(PATCH_HOP_SAMPLES) * PATCH_HOP_SAMPLES
        };
        self.samples.extend(std::iter::repeat_n(0.0, padded - n));
        self.consume(out);
    }

    fn consume(&mut self, out: &mut Vec<Patch>) {
        let mut pos = 0;
        while self.samples.len() - pos >= WINDOW {
            self.compute_frame(pos);
            pos += HOP;
        }
        self.samples.drain(..pos);

        while self.frames.len() >= PATCH_FRAMES {
            out.push(self.make_patch());
            self.frames.drain(..PATCH_HOP_FRAMES);
            self.powers.drain(..PATCH_HOP_FRAMES);
        }
    }

    fn compute_frame(&mut self, pos: usize) {
        let frame = &self.samples[pos..pos + WINDOW];
        let power = frame.iter().map(|x| x * x).sum::<f32>() / WINDOW as f32;

        for ((dst, x), w) in self.fft_in.iter_mut().zip(frame).zip(&self.window) {
            *dst = x * w;
        }
        self.fft_in[WINDOW..].fill(0.0);
        self.fft
            .process_with_scratch(&mut self.fft_in, &mut self.fft_out, &mut self.fft_scratch)
            .expect("fft buffer sizes are fixed");

        let mut mel = [0f32; MEL_BANDS];
        for (c, weights) in self.fft_out.iter().zip(&self.mel) {
            let mag = c.norm();
            if mag == 0.0 {
                continue;
            }
            for (m, w) in mel.iter_mut().zip(weights) {
                *m += mag * w;
            }
        }
        for m in &mut mel {
            *m = (*m + LOG_OFFSET).ln();
        }
        self.frames.push(mel);
        self.powers.push(power);
    }

    fn make_patch(&mut self) -> Patch {
        let mut features = Vec::with_capacity(PATCH_LEN);
        for f in &self.frames[..PATCH_FRAMES] {
            features.extend_from_slice(f);
        }
        let powers = &self.powers[..PATCH_FRAMES];
        let mean = powers.iter().sum::<f32>() / PATCH_FRAMES as f32;
        let peak = powers.iter().fold(0f32, |a, &b| a.max(b));
        let index = self.next_patch;
        self.next_patch += 1;
        Patch {
            index,
            features: features.into(),
            rms_db: power_db(mean),
            peak_db: power_db(peak),
        }
    }
}

fn power_db(p: f32) -> f32 {
    10.0 * (p.max(1e-12)).log10()
}

fn hz_to_mel(hz: f64) -> f64 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Повторяет `tf.signal.linear_to_mel_weight_matrix` (шкала HTK, треугольники в мел-домене,
/// нулевая строка для постоянной составляющей).
fn mel_matrix() -> Vec<[f32; MEL_BANDS]> {
    let nyquist = SAMPLE_RATE as f64 / 2.0;
    let lo = hz_to_mel(MEL_MIN_HZ);
    let hi = hz_to_mel(MEL_MAX_HZ);
    let edges: Vec<f64> = (0..MEL_BANDS + 2)
        .map(|i| lo + (hi - lo) * i as f64 / (MEL_BANDS + 1) as f64)
        .collect();

    let mut m = vec![[0f32; MEL_BANDS]; BINS];
    for (bin, row) in m.iter_mut().enumerate().skip(1) {
        let mel = hz_to_mel(nyquist * bin as f64 / (BINS - 1) as f64);
        for (band, w) in row.iter_mut().enumerate() {
            let (l, c, u) = (edges[band], edges[band + 1], edges[band + 2]);
            let lower = (mel - l) / (c - l);
            let upper = (u - mel) / (u - c);
            *w = lower.min(upper).max(0.0) as f32;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patches_for(n: usize, chunk: usize) -> Vec<Patch> {
        let signal: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let mut fx = FeatureExtractor::new();
        let mut out = Vec::new();
        for c in signal.chunks(chunk) {
            fx.push(c, &mut out);
        }
        fx.finish(&mut out);
        out
    }

    #[test]
    fn patch_count_matches_yamnet_padding() {
        assert_eq!(patches_for(100, 64).len(), 1);
        assert_eq!(patches_for(MIN_SAMPLES, 1000).len(), 1);
        assert_eq!(patches_for(MIN_SAMPLES + 1, 1000).len(), 2);
        // 3 секунды: 15600 + 4*7680 >= 48000 -> 1 + 5 = 6 патчей... проверяем формулу явно.
        let n = 48_000;
        let expected = 1 + (n - MIN_SAMPLES).div_ceil(PATCH_HOP_SAMPLES);
        assert_eq!(patches_for(n, 4096).len(), expected);
    }

    #[test]
    fn chunking_does_not_change_features() {
        let a = patches_for(40_000, 333);
        let b = patches_for(40_000, 16_000);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.features, y.features);
        }
    }

    #[test]
    fn silence_gives_log_offset() {
        let mut fx = FeatureExtractor::new();
        let mut out = Vec::new();
        fx.push(&vec![0.0; 16_000], &mut out);
        fx.finish(&mut out);
        let expected = LOG_OFFSET.ln();
        assert!(out[0].features.iter().all(|&v| (v - expected).abs() < 1e-6));
        assert!(out[0].rms_db < -100.0);
    }

    #[test]
    fn mel_rows_cover_band() {
        let m = mel_matrix();
        assert!(m[0].iter().all(|&w| w == 0.0));
        // Каждая мел-полоса получает вклад хотя бы от одного бина.
        for band in 0..MEL_BANDS {
            assert!(m.iter().any(|row| row[band] > 0.0), "empty band {band}");
        }
    }
}
