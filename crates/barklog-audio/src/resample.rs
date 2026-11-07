//! Потоковый полифазный ресемплер (windowed sinc) для моно-сигнала.
//!
//! Работает с любым рациональным отношением частот: 48000 → 16000 (3:1),
//! 44100 → 16000 (441:160), 8000 → 16000 (1:2) и т.д. Задержки нет —
//! выходной отсчёт `n` соответствует входной позиции `n * in_rate / out_rate`.

use std::f64::consts::PI;

/// Число пересечений нуля sinc-ядра с каждой стороны. Для классификации
/// звуков этого с запасом хватает, а стоимость остаётся низкой.
const ZERO_CROSSINGS: f64 = 12.0;
/// Срез фильтра относительно новой частоты Найквиста.
const CUTOFF: f64 = 0.92;

pub struct Resampler {
    /// Числитель / знаменатель отношения: out = in * up / down.
    up: u64,
    down: u64,
    /// Половина длины ядра во входных отсчётах.
    half: usize,
    /// `table[phase * taps + tap]`, `taps = 2 * half`.
    table: Vec<f32>,
    /// Хвост входного сигнала; `hist[0]` имеет абсолютный индекс `hist_start`.
    hist: Vec<f32>,
    hist_start: i64,
    /// Сколько входных отсчётов получено всего.
    total_in: u64,
    /// Индекс следующего выходного отсчёта.
    next_out: u64,
    passthrough: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        assert!(in_rate > 0 && out_rate > 0, "sample rate must be positive");
        let g = gcd(in_rate as u64, out_rate as u64);
        let up = out_rate as u64 / g;
        let down = in_rate as u64 / g;
        let passthrough = up == down;

        // Ширина полосы относительно входной частоты дискретизации (1.0 = без фильтрации).
        let band = CUTOFF * (up as f64 / down as f64).min(1.0);
        let half = if passthrough {
            0
        } else {
            (ZERO_CROSSINGS / band).ceil() as usize
        };
        let taps = 2 * half;

        let mut table = vec![0f32; up as usize * taps];
        for phase in 0..up as usize {
            let frac = phase as f64 / up as f64;
            let row = &mut table[phase * taps..(phase + 1) * taps];
            let mut sum = 0.0;
            for (tap, w) in row.iter_mut().enumerate() {
                // Расстояние от позиции выходного отсчёта до входного отсчёта `tap`.
                let d = (half as f64 - 1.0 - tap as f64) + frac;
                let v = band * sinc(band * d) * blackman(d / half as f64);
                *w = v as f32;
                sum += v;
            }
            // Единичное усиление на постоянной составляющей для каждой фазы.
            if sum.abs() > 1e-12 {
                row.iter_mut().for_each(|w| *w = (*w as f64 / sum) as f32);
            }
        }

        Self {
            up,
            down,
            half,
            table,
            hist: vec![0.0; half],
            hist_start: -(half as i64),
            total_in: 0,
            next_out: 0,
            passthrough,
        }
    }

    /// Принимает очередной блок входа и дописывает результат в `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.hist.extend_from_slice(input);
        self.total_in += input.len() as u64;
        self.produce(out, false);
    }

    /// Выдаёт оставшиеся отсчёты в конце потока.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if self.passthrough {
            return;
        }
        self.hist.extend(std::iter::repeat_n(0.0, self.half + 1));
        self.produce(out, true);
    }

    fn produce(&mut self, out: &mut Vec<f32>, flushing: bool) {
        let taps = 2 * self.half;
        let available_end = self.hist_start + self.hist.len() as i64; // не включительно
        loop {
            let pos = self.next_out * self.down;
            if flushing && pos >= self.total_in * self.up {
                break;
            }
            let i = (pos / self.up) as i64;
            let phase = (pos % self.up) as usize;
            // Нужны входные отсчёты i - half + 1 ..= i + half.
            if i + self.half as i64 >= available_end {
                break;
            }
            let first = (i - self.half as i64 + 1 - self.hist_start) as usize;
            let x = &self.hist[first..first + taps];
            let h = &self.table[phase * taps..(phase + 1) * taps];
            out.push(dot(x, h));
            self.next_out += 1;
        }

        // Выбрасываем вход, который больше не понадобится.
        let i_next = (self.next_out * self.down / self.up) as i64;
        let keep_from = i_next - self.half as i64 + 1;
        let drop = (keep_from - self.hist_start).clamp(0, self.hist.len() as i64) as usize;
        if drop > 0 {
            self.hist.drain(..drop);
            self.hist_start += drop as i64;
        }
    }
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Окно Блэкмана на отрезке [-1, 1].
fn blackman(u: f64) -> f64 {
    if u.abs() >= 1.0 {
        return 0.0;
    }
    0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos()
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, secs: f64) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    fn resample_all(input: &[f32], from: u32, to: u32, chunk: usize) -> Vec<f32> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.process(c, &mut out);
        }
        r.flush(&mut out);
        out
    }

    #[test]
    fn output_length_matches_ratio() {
        for (from, to) in [
            (48000, 16000),
            (44100, 16000),
            (8000, 16000),
            (22050, 16000),
        ] {
            let input = sine(from, 440.0, 1.0);
            let out = resample_all(&input, from, to, 1000);
            let expected = (input.len() as u64 * to as u64).div_ceil(from as u64) as usize;
            assert_eq!(out.len(), expected, "{from} -> {to}");
        }
    }

    #[test]
    fn chunking_does_not_change_result() {
        let input = sine(44100, 1000.0, 0.5);
        let a = resample_all(&input, 44100, 16000, 7);
        let b = resample_all(&input, 44100, 16000, 4096);
        assert_eq!(a, b);
    }

    #[test]
    fn preserves_in_band_sine() {
        let input = sine(44100, 1000.0, 1.0);
        let out = resample_all(&input, 44100, 16000, 1024);
        let reference = sine(16000, 1000.0, 1.0);
        // Пропускаем края, где фильтр видит нули.
        let err = out[500..15500]
            .iter()
            .zip(&reference[500..15500])
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(err < 0.01, "max error {err}");
    }

    #[test]
    fn suppresses_alias() {
        // 12 кГц выше новой частоты Найквиста (8 кГц) — должен исчезнуть.
        let input = sine(48000, 12000.0, 1.0);
        let out = resample_all(&input, 48000, 16000, 1024);
        let rms = (out[500..15500].iter().map(|x| x * x).sum::<f32>() / 15000.0).sqrt();
        assert!(rms < 0.01, "alias rms {rms}");
    }

    #[test]
    fn passthrough_is_identity() {
        let input = sine(16000, 440.0, 0.1);
        assert_eq!(resample_all(&input, 16000, 16000, 100), input);
    }
}
