//! Потоковое декодирование аудиофайлов через symphonia.
//!
//! Файл никогда не читается в память целиком: 8 часов записи в f32 — это
//! около 1.8 ГБ, поэтому отдаём моно-сигнал порциями по мере декодирования.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use crate::AudioError;

/// Сведения об исходном файле.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub sample_rate: u32,
    pub channels: usize,
    /// Длительность по данным контейнера, если она известна.
    pub duration_ms: Option<u64>,
}

/// Декодер, выдающий моно-сигнал с исходной частотой дискретизации.
pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    info: SourceInfo,
    interleaved: Vec<f32>,
    finished: bool,
}

impl Decoder {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AudioError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(map_open_error)?;

        let track = format
            .default_track(TrackType::Audio)
            .ok_or(AudioError::NoAudioTrack)?;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else {
            return Err(AudioError::NoAudioTrack);
        };
        let track_id = track.id;

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(map_open_error)?;

        let sample_rate = params.sample_rate.ok_or(AudioError::Unsupported(
            "неизвестная частота дискретизации".into(),
        ))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(1)
            .max(1);
        let duration_ms = track
            .num_frames
            .map(|n| n * 1000 / sample_rate as u64)
            .or_else(|| {
                let tb = track.time_base?;
                let t = tb.calc_duration(track.duration?)?;
                Some((t.as_secs_f64() * 1000.0) as u64)
            });

        Ok(Self {
            format,
            decoder,
            track_id,
            info: SourceInfo {
                sample_rate,
                channels,
                duration_ms,
            },
            interleaved: Vec::new(),
            finished: false,
        })
    }

    pub fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Декодирует следующий пакет и дописывает моно-отсчёты в `out`.
    /// Возвращает `false`, когда поток закончился.
    pub fn read_mono(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        while !self.finished {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.finished = true;
                    break;
                }
                // Обрезанный в конце файл (например, диктофон выключился) — не ошибка.
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.finished = true;
                    break;
                }
                Err(SymError::ResetRequired) => {
                    self.finished = true;
                    break;
                }
                Err(e) => return Err(AudioError::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                // Битый пакет пропускаем, остальное декодируем дальше.
                Err(SymError::DecodeError(_)) => continue,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.finished = true;
                    break;
                }
                Err(e) => return Err(AudioError::Decode(e.to_string())),
            };

            let channels = buf.spec().channels().count().max(1);
            buf.copy_to_vec_interleaved(&mut self.interleaved);
            if channels == 1 {
                out.extend_from_slice(&self.interleaved);
            } else {
                let scale = 1.0 / channels as f32;
                out.extend(
                    self.interleaved
                        .chunks_exact(channels)
                        .map(|f| f.iter().sum::<f32>() * scale),
                );
            }
            return Ok(true);
        }
        Ok(false)
    }
}

fn map_open_error(e: SymError) -> AudioError {
    match e {
        SymError::IoError(e) => AudioError::Io(e),
        SymError::Unsupported(what) => AudioError::Unsupported(what.to_string()),
        other => AudioError::Decode(other.to_string()),
    }
}
