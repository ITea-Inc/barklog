//! Аудио-вход BarkLog: декодирование файлов и приведение к 16 кГц моно.

mod decode;
mod resample;

use std::path::Path;

pub use decode::{Decoder, SourceInfo};
pub use resample::Resampler;

/// Частота, с которой работает весь анализ (формат YAMNet).
pub const TARGET_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("ошибка чтения файла: {0}")]
    Io(#[from] std::io::Error),
    #[error("формат не поддерживается: {0}")]
    Unsupported(String),
    #[error("в файле нет аудиодорожки")]
    NoAudioTrack,
    #[error("ошибка декодирования: {0}")]
    Decode(String),
}

/// Источник моно-сигнала 16 кГц, читаемый порциями.
pub trait AudioSource {
    /// Дописывает очередную порцию в `out`. `false` — поток закончился.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError>;
    /// Ожидаемая длительность, если известна (для прогресса).
    fn duration_ms(&self) -> Option<u64>;
}

/// Файл, декодируемый и ресемплируемый в 16 кГц моно на лету.
pub struct FileSource {
    decoder: Decoder,
    resampler: Resampler,
    raw: Vec<f32>,
    flushed: bool,
}

impl FileSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AudioError> {
        let decoder = Decoder::open(path)?;
        let resampler = Resampler::new(decoder.info().sample_rate, TARGET_RATE);
        Ok(Self {
            decoder,
            resampler,
            raw: Vec::new(),
            flushed: false,
        })
    }

    pub fn info(&self) -> &SourceInfo {
        self.decoder.info()
    }
}

impl AudioSource for FileSource {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        if self.flushed {
            return Ok(false);
        }
        self.raw.clear();
        if self.decoder.read_mono(&mut self.raw)? {
            self.resampler.process(&self.raw, out);
        } else {
            self.resampler.flush(out);
            self.flushed = true;
        }
        Ok(true)
    }

    fn duration_ms(&self) -> Option<u64> {
        self.decoder.info().duration_ms
    }
}

/// Источник из готового буфера 16 кГц (тесты, синтетика, живая запись).
pub struct BufferSource {
    samples: Vec<f32>,
    pos: usize,
    chunk: usize,
}

impl BufferSource {
    pub fn new(samples: Vec<f32>) -> Self {
        Self {
            samples,
            pos: 0,
            chunk: TARGET_RATE as usize,
        }
    }
}

impl AudioSource for BufferSource {
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        if self.pos >= self.samples.len() {
            return Ok(false);
        }
        let end = (self.pos + self.chunk).min(self.samples.len());
        out.extend_from_slice(&self.samples[self.pos..end]);
        self.pos = end;
        Ok(true)
    }

    fn duration_ms(&self) -> Option<u64> {
        Some(self.samples.len() as u64 * 1000 / TARGET_RATE as u64)
    }
}

/// Читает весь файл в 16 кГц моно. Только для коротких фрагментов!
pub fn read_all(path: impl AsRef<Path>) -> Result<Vec<f32>, AudioError> {
    let mut src = FileSource::open(path)?;
    let mut out = Vec::new();
    while src.read(&mut out)? {}
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_stereo_wav_to_16k_mono() {
        let dir = std::env::temp_dir().join("barklog-audio-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo_44k.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..44_100 {
            let v = ((i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 16_000.0) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();

        let src = FileSource::open(&path).unwrap();
        assert_eq!(src.info().sample_rate, 44_100);
        assert_eq!(src.info().channels, 2);
        assert_eq!(src.duration_ms(), Some(1000));

        let samples = read_all(&path).unwrap();
        assert_eq!(samples.len(), 16_000);
        let peak = samples.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((peak - 16_000.0 / 32_768.0).abs() < 0.02, "peak {peak}");
    }
}
