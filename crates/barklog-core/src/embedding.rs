//! Компактный эмбеддинг YAMNet для хранения: 1024 значения по байту.
//!
//! Эмбеддинг — среднее выходов ReLU, поэтому все значения неотрицательны. Храним
//! √x, линейно растянутый на 0..255 с одним множителем на вектор: корень даёт
//! больше точности малым значениям, которых в векторе большинство. Ошибка в
//! вероятностях AudioSet после такого сжатия — меньше 0,005; на 4-часовой записи
//! решение «собака / не собака» поменялось у 0,1 % окон, и все они пограничные.

use barklog_ml::EMBEDDING_SIZE;
use half::f16;

#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    /// Шаг квантования в единицах √x.
    scale: f32,
    codes: Vec<u8>,
}

impl Embedding {
    /// Размер в базе: множитель f32 + по байту на значение.
    pub const BYTES: usize = 4 + EMBEDDING_SIZE;
    /// Старый формат: f16 на значение.
    const LEGACY_BYTES: usize = 2 * EMBEDDING_SIZE;

    pub fn quantize(v: &[f32]) -> Self {
        let max = v.iter().fold(0f32, |m, &x| m.max(x.max(0.0).sqrt()));
        let scale = if max > 0.0 { max / 255.0 } else { 1.0 };
        let codes = v
            .iter()
            .map(|&x| (x.max(0.0).sqrt() / scale).round().clamp(0.0, 255.0) as u8)
            .collect();
        Self { scale, codes }
    }

    pub fn to_f32(&self) -> Vec<f32> {
        self.codes
            .iter()
            .map(|&c| {
                let r = c as f32 * self.scale;
                r * r
            })
            .collect()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.codes.len());
        out.extend_from_slice(&self.scale.to_le_bytes());
        out.extend_from_slice(&self.codes);
        out
    }

    /// Читает и новый формат, и f16 из записей прежних версий.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        match b.len() {
            Self::BYTES => {
                let scale = f32::from_le_bytes(b[..4].try_into().ok()?);
                (scale.is_finite() && scale > 0.0).then(|| Self {
                    scale,
                    codes: b[4..].to_vec(),
                })
            }
            Self::LEGACY_BYTES => {
                let v: Vec<f32> = b
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| f16::from_le_bytes(*c).to_f32())
                    .collect();
                Some(Self::quantize(&v))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<f32> {
        (0..EMBEDDING_SIZE)
            .map(|i| ((i * 37) % 101) as f32 / 40.0 * if i % 3 == 0 { 0.01 } else { 1.0 })
            .collect()
    }

    #[test]
    fn roundtrip_is_close() {
        let v = sample();
        let e = Embedding::from_bytes(&Embedding::quantize(&v).to_bytes()).unwrap();
        let max = v.iter().cloned().fold(0f32, f32::max);
        for (a, b) in v.iter().zip(e.to_f32()) {
            // Ошибка в √x не больше полшага, значит в x — не больше √max/255·√x + мелочь.
            assert!(
                (a - b).abs() <= max.sqrt() / 255.0 * (a.sqrt() + 0.01) + 1e-6,
                "{a} {b}"
            );
        }
    }

    #[test]
    fn reads_legacy_f16() {
        let v = sample();
        let legacy: Vec<u8> = v
            .iter()
            .flat_map(|&x| f16::from_f32(x).to_le_bytes())
            .collect();
        let e = Embedding::from_bytes(&legacy).unwrap();
        assert_eq!(e.to_f32().len(), EMBEDDING_SIZE);
        assert!((e.to_f32()[1] - v[1]).abs() < 0.02);
    }

    #[test]
    fn zeros_and_garbage() {
        let z = Embedding::quantize(&vec![0.0; EMBEDDING_SIZE]);
        assert!(z.to_f32().iter().all(|&x| x == 0.0));
        assert_eq!(Embedding::from_bytes(&z.to_bytes()), Some(z));
        assert_eq!(Embedding::from_bytes(&[1, 2, 3]), None);
    }
}
