//! Собирает тестовую «сессию»: тихая квартира (слабый шум) с фрагментами,
//! вставленными в заданные моменты. Нужна, чтобы проверять отчёт без настоящей записи.
//!
//!     cargo run -p barklog-cli --example make_session -- out.wav 40 bark.ogg@2.5 bark.ogg@17 howl.ogg@30
//!
//! Аргументы: выходной WAV, длительность в минутах, затем `файл@минута`.

use barklog_audio::{TARGET_RATE, read_all};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: make_session <out.wav> <minutes> [file@minute ...]");
        std::process::exit(2);
    }
    let minutes: f64 = args[1].parse().expect("minutes");
    let n = (minutes * 60.0 * TARGET_RATE as f64) as usize;

    // Фон около −62 дБFS: шум, слегка сглаженный, чтобы не был чисто белым.
    let mut state: u64 = 12345;
    let mut prev = 0f32;
    let mut out: Vec<f32> = (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let white = ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0;
            prev = 0.8 * prev + 0.2 * white;
            prev * 0.004
        })
        .collect();

    for spec in &args[2..] {
        let (file, at) = spec.rsplit_once('@').expect("file@minute");
        let at: f64 = at.parse().expect("minute");
        let clip = read_all(file).unwrap_or_else(|e| panic!("{file}: {e}"));
        let start = (at * 60.0 * TARGET_RATE as f64) as usize;
        for (i, x) in clip.iter().enumerate() {
            if let Some(o) = out.get_mut(start + i) {
                *o += x;
            }
        }
        eprintln!(
            "{file} @ {at} мин ({:.1} с)",
            clip.len() as f64 / TARGET_RATE as f64
        );
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&args[0], spec).expect("create wav");
    for x in out {
        w.write_sample((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
}
