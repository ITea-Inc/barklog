//! barklog — анализ записи с диктофона на компьютере.
//!
//!     barklog analyze recording.m4a
//!     barklog analyze recording.m4a --start "2026-11-07 09:30" --clips 30

mod fmt;
mod html;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use barklog_audio::{AudioSource, FileSource, TARGET_RATE};
use barklog_core::{
    Report, ReportSettings, SoundClass, StartSource, StartTime, Thresholds, analyze, build_report,
    start_from_modified, start_from_name,
};
use barklog_ml::Yamnet;
use chrono::{Local, NaiveDateTime, TimeZone};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Parser)]
#[command(
    name = "barklog",
    version,
    about = "Анализ записей: когда и сколько собака лаяла, скулила, выла"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Проанализировать запись и собрать отчёт (HTML + JSON + фрагменты)
    Analyze(AnalyzeArgs),
    /// Показать топ классов AudioSet по каждому окну (для отладки и калибровки)
    Inspect(InspectArgs),
}

#[derive(Parser)]
struct InspectArgs {
    file: PathBuf,
    #[arg(long, env = "BARKLOG_MODEL")]
    model: Option<PathBuf>,
    /// Сколько классов показывать на окно
    #[arg(long, default_value_t = 5)]
    top: usize,
}

#[derive(Parser)]
struct AnalyzeArgs {
    /// Аудиофайл: m4a, mp3, wav, flac, ogg
    file: PathBuf,
    /// Папка для отчёта (по умолчанию — рядом с файлом, <имя>_barklog)
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Модель YAMNet (.safetensors)
    #[arg(long, env = "BARKLOG_MODEL")]
    model: Option<PathBuf>,
    /// Время начала записи, «2026-11-07 09:30» (по умолчанию — из имени или даты файла)
    #[arg(long)]
    start: Option<String>,
    /// Сколько самых длинных эпизодов сохранить фрагментами .wav (0 — не сохранять)
    #[arg(long, default_value_t = 20)]
    clips: usize,
    /// Порог «собачьей» вероятности, 0–1
    #[arg(long, default_value_t = Thresholds::default().dog_min)]
    dog_min: f32,
    /// Паузы короче этого (с) не разрывают эпизод
    #[arg(long, default_value_t = 3)]
    merge_gap: u64,
    /// Прогонять через сеть все окна, без отсева тишины (медленнее)
    #[arg(long)]
    no_gate: bool,
    /// Сохранить в JSON результат по каждому окну (для калибровки)
    #[arg(long)]
    windows: bool,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Analyze(args) => run_analyze(args),
        Command::Inspect(args) => run_inspect(args),
    }
}

fn run_inspect(args: InspectArgs) -> Result<()> {
    let model = Yamnet::load(find_model(args.model.as_deref())?)?;
    let mut source = FileSource::open(&args.file)?;
    let mut fx = barklog_ml::FeatureExtractor::new();
    let mut patches = Vec::new();
    let mut buf = Vec::new();
    let names = barklog_ml::class_names();
    let flush = |patches: &mut Vec<barklog_ml::Patch>| {
        for (p, pred) in patches.iter().zip(model.predict(patches)) {
            let mut idx: Vec<usize> = (0..pred.scores.len()).collect();
            idx.sort_by(|&a, &b| pred.scores[b].total_cmp(&pred.scores[a]));
            let top: Vec<String> = idx
                .iter()
                .take(args.top)
                .map(|&i| format!("{} {:.2}", names[i], pred.scores[i]))
                .collect();
            let ms = p.start_ms();
            println!(
                "{}.{}  {:>6.1} дБ  {}",
                fmt::counter(ms),
                ms % 1000 / 100,
                p.rms_db,
                top.join(" · ")
            );
        }
        patches.clear();
    };
    loop {
        buf.clear();
        if !source.read(&mut buf)? {
            break;
        }
        fx.push(&buf, &mut patches);
        if patches.len() >= 64 {
            flush(&mut patches);
        }
    }
    fx.finish(&mut patches);
    flush(&mut patches);
    Ok(())
}

fn run_analyze(args: AnalyzeArgs) -> Result<()> {
    let model_path = find_model(args.model.as_deref())?;
    let model = Yamnet::load(&model_path)
        .with_context(|| format!("не удалось загрузить модель {}", model_path.display()))?;

    let mut source = FileSource::open(&args.file)
        .with_context(|| format!("не удалось открыть {}", args.file.display()))?;
    let info = source.info().clone();
    let file_name = args
        .file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    eprintln!(
        "{file_name}: {} Гц, каналов {}, {}",
        info.sample_rate,
        info.channels,
        info.duration_ms
            .map(fmt::duration)
            .unwrap_or_else(|| "длительность неизвестна".into())
    );

    let bar = ProgressBar::new(info.duration_ms.unwrap_or(0).max(1));
    bar.set_style(
        ProgressStyle::with_template("{bar:40} {percent:>3}%  {msg}  осталось {eta}")
            .unwrap()
            .progress_chars("━━ "),
    );
    let started = Instant::now();
    let thresholds = Thresholds {
        dog_min: args.dog_min,
        ..Thresholds::default()
    };
    let analysis = analyze(&mut source, &model, thresholds, !args.no_gate, None, |p| {
        bar.set_position(p.processed_ms);
        bar.set_message(fmt::counter(p.processed_ms));
        true
    })?;
    bar.finish_and_clear();
    let elapsed = started.elapsed();

    let started_at = resolve_start(&args, analysis.duration_ms)?;
    let settings = ReportSettings {
        merge_gap_ms: args.merge_gap * 1000,
        ..ReportSettings::for_duration(analysis.duration_ms)
    };
    let mut report = build_report(&analysis, &settings, started_at, Some(file_name.clone()));

    let out_dir = args
        .out
        .clone()
        .unwrap_or_else(|| default_out_dir(&args.file));
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("не удалось создать {}", out_dir.display()))?;

    let clips = if args.clips > 0 && !report.episodes.is_empty() {
        write_clips(&args.file, &report, args.clips, &out_dir.join("clips"))?
    } else {
        HashMap::new()
    };

    let html = html::render(&html::HtmlInput {
        report: &report,
        file_name: &file_name,
        clips: &clips,
    });
    std::fs::write(out_dir.join("report.html"), html)?;

    if args.windows {
        report.windows = analysis.windows;
    }
    std::fs::write(
        out_dir.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;

    print_summary(&report, elapsed.as_secs_f64());
    println!("\nОтчёт: {}", out_dir.join("report.html").display());
    Ok(())
}

fn print_summary(r: &Report, secs: f64) {
    let s = &r.stats;
    println!(
        "\nЗапись {} · проанализирована за {:.0} с ({:.0}× быстрее реального времени)",
        fmt::duration(s.duration_ms),
        secs,
        s.duration_ms as f64 / 1000.0 / secs.max(0.001)
    );
    println!(
        "Голос собаки: {} ({})\n",
        fmt::duration(s.dog_time_ms),
        fmt::percent(s.dog_percent)
    );
    for c in &s.classes {
        println!(
            "  {:<14}{:>16}{:>10}{:>10}",
            c.class.label_ru(),
            fmt::duration(c.time_ms),
            fmt::percent(c.percent),
            if c.class.is_dog() {
                format!("эп. {}", c.episodes)
            } else {
                String::new()
            }
        );
    }
    if let Some(ms) = s.first_dog_sound_ms {
        println!("\nПервый звук через {}", fmt::duration(ms));
    }
    println!("Самый долгий покой {}", fmt::duration(s.longest_calm_ms));
}

/// Модель ищется: аргумент/переменная → ./models → рядом с исполняемым файлом → корень репозитория.
fn find_model(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let name = Path::new("models").join("yamnet.safetensors");
    let mut candidates = vec![name.clone()];
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(4) {
            candidates.push(dir.join(&name));
        }
    }
    candidates.into_iter().find(|p| p.exists()).ok_or_else(|| {
        anyhow::anyhow!(
            "не найдена модель models/yamnet.safetensors — укажите --model или BARKLOG_MODEL"
        )
    })
}

fn default_out_dir(file: &Path) -> PathBuf {
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or("recording".into());
    file.with_file_name(format!("{stem}_barklog"))
}

/// Время начала: --start → дата в имени файла (20261107_093012) → время изменения файла минус длительность.
fn resolve_start(args: &AnalyzeArgs, duration_ms: u64) -> Result<Option<StartTime>> {
    let offset_min = Local::now().offset().local_minus_utc() / 60;
    if let Some(s) = &args.start {
        let naive = [
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%dT%H:%M",
        ]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok());
        let Some(naive) = naive else {
            bail!("не понял время «{s}», нужен формат 2026-11-07 09:30");
        };
        let dt = Local
            .from_local_datetime(&naive)
            .earliest()
            .context("несуществующее местное время")?;
        return Ok(Some(StartTime {
            unix_ms: dt.timestamp_millis(),
            utc_offset_min: dt.offset().local_minus_utc() / 60,
            source: StartSource::Manual,
        }));
    }
    let name = args
        .file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(st) = start_from_name(&name, offset_min) {
        return Ok(Some(st));
    }
    let modified = std::fs::metadata(&args.file)
        .and_then(|m| m.modified())
        .ok();
    Ok(modified.map(|m| {
        let ms = chrono::DateTime::<Local>::from(m).timestamp_millis();
        start_from_modified(ms, duration_ms, offset_min)
    }))
}

/// Второй проход по файлу: сохраняет самые длинные эпизоды фрагментами WAV.
fn write_clips(
    file: &Path,
    report: &Report,
    max: usize,
    dir: &Path,
) -> Result<HashMap<usize, String>> {
    const PAD_MS: u64 = 500;
    const MAX_CLIP_MS: u64 = 60_000;

    let mut chosen: Vec<usize> = (0..report.episodes.len()).collect();
    chosen.sort_by_key(|&i| std::cmp::Reverse(report.episodes[i].span_ms()));
    chosen.truncate(max);
    chosen.sort_by_key(|&i| report.episodes[i].start_ms);

    std::fs::create_dir_all(dir)?;
    let ms_to_sample = |ms: u64| ms * TARGET_RATE as u64 / 1000;
    let ranges: Vec<(usize, u64, u64)> = chosen
        .iter()
        .map(|&i| {
            let e = &report.episodes[i];
            let start = e.start_ms.saturating_sub(PAD_MS);
            let end = (e.end_ms + PAD_MS).min(start + MAX_CLIP_MS);
            (i, ms_to_sample(start), ms_to_sample(end))
        })
        .collect();

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writers: Vec<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>> =
        ranges.iter().map(|_| None).collect();
    let mut paths = HashMap::new();

    let mut source = FileSource::open(file)?;
    let mut buf = Vec::new();
    let mut pos: u64 = 0;
    let last_end = ranges.iter().map(|r| r.2).max().unwrap_or(0);
    while pos < last_end {
        buf.clear();
        if !source.read(&mut buf)? {
            break;
        }
        let chunk_end = pos + buf.len() as u64;
        for (k, &(i, start, end)) in ranges.iter().enumerate() {
            if end <= pos || start >= chunk_end {
                continue;
            }
            let w = match &mut writers[k] {
                Some(w) => w,
                slot @ None => {
                    let e = &report.episodes[i];
                    let name = format!(
                        "{:03}_{}_{}.wav",
                        i + 1,
                        clip_name(e.class),
                        fmt::counter(e.start_ms).replace(':', "-")
                    );
                    paths.insert(i, format!("clips/{name}"));
                    slot.insert(hound::WavWriter::create(dir.join(&name), spec)?)
                }
            };
            let from = start.max(pos) - pos;
            let to = end.min(chunk_end) - pos;
            for &x in &buf[from as usize..to as usize] {
                w.write_sample((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
            }
        }
        pos = chunk_end;
    }
    for w in writers.into_iter().flatten() {
        w.finalize()?;
    }
    Ok(paths)
}

fn clip_name(c: SoundClass) -> &'static str {
    match c {
        SoundClass::Bark => "bark",
        SoundClass::Whine => "whine",
        SoundClass::Howl => "howl",
        SoundClass::Growl => "growl",
        SoundClass::Other => "other",
        SoundClass::Silence => "silence",
    }
}
