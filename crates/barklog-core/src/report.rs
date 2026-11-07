//! Из последовательности окон — эпизоды, статистика, таймлайн и почасовая разбивка.

use serde::{Deserialize, Serialize};

use crate::SoundClass;
use crate::pipeline::{Analysis, WindowResult};
use barklog_ml::PATCH_HOP_MS;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSettings {
    /// Паузы короче этого внутри эпизода одного класса не разрывают его.
    pub merge_gap_ms: u64,
    /// Паузы короче этого не разрывают период активности собаки.
    pub activity_gap_ms: u64,
    /// Ширина столбца таймлайна.
    pub timeline_bin_ms: u64,
}

impl Default for ReportSettings {
    fn default() -> Self {
        Self {
            merge_gap_ms: 3_000,
            activity_gap_ms: 30_000,
            timeline_bin_ms: 60_000,
        }
    }
}

/// Когда началась запись (для почасовой разбивки по часам на стене).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StartTime {
    pub unix_ms: i64,
    /// Смещение локального времени от UTC в минутах.
    pub utc_offset_min: i32,
    /// Откуда известно время начала.
    pub source: crate::start::StartSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub class: SoundClass,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Чистое время звука внутри эпизода (без пауз).
    pub sound_ms: u64,
    pub peak_db: f32,
    pub confidence: f32,
}

impl Episode {
    pub fn span_ms(&self) -> u64 {
        self.end_ms - self.start_ms
    }
}

/// Период активности — любые звуки собаки с паузами короче `activity_gap_ms`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bout {
    pub start_ms: u64,
    pub end_ms: u64,
    pub sound_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassStat {
    pub class: SoundClass,
    pub time_ms: u64,
    /// Доля от всей записи, %.
    pub percent: f32,
    /// Доля среди звуков собаки, % (для не-собачьих классов — `None`).
    pub dog_percent: Option<f32>,
    pub episodes: u32,
    pub longest_episode_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub duration_ms: u64,
    pub dog_time_ms: u64,
    pub dog_percent: f32,
    pub classes: Vec<ClassStat>,
    pub episodes: u32,
    pub bouts: u32,
    /// Через сколько после начала записи собака впервые подала голос.
    pub first_dog_sound_ms: Option<u64>,
    /// Когда звуки собаки прекратились (конец последнего эпизода).
    pub last_dog_sound_ms: Option<u64>,
    pub longest_episode: Option<Episode>,
    /// Самый длинный промежуток без звуков собаки.
    pub longest_calm_ms: u64,
    /// Доля окон, отсеянных по громкости (сеть не запускалась), %.
    pub gated_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineBin {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Время по классам в порядке `SoundClass::ALL`.
    pub class_ms: [u64; 6],
    /// Самое громкое окно столбца, дБFS.
    pub peak_db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourBucket {
    /// Час на часах (0–23), если известно время начала, иначе номер часа от начала.
    pub hour: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub class_ms: [u64; 6],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub source: Option<String>,
    pub started_at: Option<StartTime>,
    pub settings: ReportSettings,
    pub stats: Stats,
    pub episodes: Vec<Episode>,
    pub bouts: Vec<Bout>,
    pub timeline: Vec<TimelineBin>,
    pub hourly: Vec<HourBucket>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub windows: Vec<WindowResult>,
}

/// Временной отрезок окна: [start, start + шаг), последний тянется до конца записи.
#[derive(Clone, Copy)]
struct Slot {
    start: u64,
    end: u64,
    class: SoundClass,
    peak_db: f32,
    confidence: f32,
}

fn slots(analysis: &Analysis) -> Vec<Slot> {
    let classes = smooth(&analysis.windows);
    let n = analysis.windows.len();
    analysis
        .windows
        .iter()
        .zip(classes)
        .enumerate()
        .map(|(i, (w, class))| {
            let end = if i + 1 == n {
                analysis.duration_ms.max(w.start_ms)
            } else {
                (w.start_ms + PATCH_HOP_MS).min(analysis.duration_ms)
            };
            Slot {
                start: w.start_ms,
                end,
                class,
                peak_db: w.peak_db,
                confidence: w.confidence,
            }
        })
        .filter(|s| s.end > s.start)
        .collect()
}

/// Убирает одиночные «перескоки» между собачьими классами: лай-скулёж-лай → лай,
/// лай-скулёж-тишина → лай. Одиночное окно другого собачьего класса рядом с
/// лаем почти всегда ошибка классификатора, а не отдельный эпизод.
/// Одиночные собачьи окна среди тишины не трогаем — короткий одиночный гав реален.
fn smooth(windows: &[WindowResult]) -> Vec<SoundClass> {
    // Серии одинаковых классов: (класс, длина).
    let mut runs: Vec<(SoundClass, usize)> = Vec::new();
    for w in windows {
        match runs.last_mut() {
            Some((c, n)) if *c == w.class => *n += 1,
            _ => runs.push((w.class, 1)),
        }
    }
    // Одиночное собачье окно забирает класс соседней собачьей серии из 2+ окон
    // (более длинной; при равенстве — предыдущей). Решение по исходным сериям,
    // чтобы два одиночных окна не поменялись классами друг с другом.
    let mut out = Vec::with_capacity(windows.len());
    for (k, &(class, n)) in runs.iter().enumerate() {
        let established = |j: Option<usize>| {
            j.and_then(|j| runs.get(j))
                .filter(|(c, len)| c.is_dog() && *c != class && *len >= 2)
                .copied()
        };
        let mut class = class;
        if n == 1 && class.is_dog() {
            match (established(k.checked_sub(1)), established(Some(k + 1))) {
                (Some(p), Some(nx)) => class = if nx.1 > p.1 { nx.0 } else { p.0 },
                (Some(p), None) => class = p.0,
                (None, Some(nx)) => class = nx.0,
                (None, None) => {}
            }
        }
        out.extend(std::iter::repeat_n(class, n));
    }
    out
}

/// Склеивает отрезки, для которых `pred` истинно, с паузами не длиннее `gap`.
fn group(slots: &[Slot], gap: u64, pred: impl Fn(&Slot) -> bool) -> Vec<Vec<Slot>> {
    let mut groups: Vec<Vec<Slot>> = Vec::new();
    for s in slots.iter().filter(|s| pred(s)) {
        match groups.last_mut() {
            Some(g) if s.start <= g.last().unwrap().end + gap => g.push(*s),
            _ => groups.push(vec![*s]),
        }
    }
    groups
}

pub fn build_report(
    analysis: &Analysis,
    settings: &ReportSettings,
    started_at: Option<StartTime>,
    source: Option<String>,
) -> Report {
    let slots = slots(analysis);
    let duration = analysis.duration_ms;

    let mut episodes: Vec<Episode> = SoundClass::DOG
        .iter()
        .flat_map(|&class| {
            group(&slots, settings.merge_gap_ms, |s| s.class == class)
                .into_iter()
                .map(move |g| {
                    let sound_ms = g.iter().map(|s| s.end - s.start).sum();
                    Episode {
                        class,
                        start_ms: g[0].start,
                        end_ms: g.last().unwrap().end,
                        sound_ms,
                        peak_db: g.iter().map(|s| s.peak_db).fold(f32::MIN, f32::max),
                        confidence: g.iter().map(|s| s.confidence).sum::<f32>() / g.len() as f32,
                    }
                })
        })
        .collect();
    episodes.sort_by_key(|e| e.start_ms);

    let bouts: Vec<Bout> = group(&slots, settings.activity_gap_ms, |s| s.class.is_dog())
        .into_iter()
        .map(|g| Bout {
            start_ms: g[0].start,
            end_ms: g.last().unwrap().end,
            sound_ms: g.iter().map(|s| s.end - s.start).sum(),
        })
        .collect();

    let mut class_ms = [0u64; 6];
    for s in &slots {
        class_ms[s.class.index()] += s.end - s.start;
    }
    let dog_time_ms: u64 = SoundClass::DOG.iter().map(|c| class_ms[c.index()]).sum();
    let pct = |ms: u64, of: u64| {
        if of == 0 {
            0.0
        } else {
            ms as f32 * 100.0 / of as f32
        }
    };

    let classes = SoundClass::ALL
        .iter()
        .map(|&class| {
            let own: Vec<&Episode> = episodes.iter().filter(|e| e.class == class).collect();
            ClassStat {
                class,
                time_ms: class_ms[class.index()],
                percent: pct(class_ms[class.index()], duration),
                dog_percent: class
                    .is_dog()
                    .then(|| pct(class_ms[class.index()], dog_time_ms)),
                episodes: own.len() as u32,
                longest_episode_ms: own.iter().map(|e| e.span_ms()).max().unwrap_or(0),
            }
        })
        .collect();

    // Самый длинный промежуток без собаки, включая начало и конец записи.
    let mut longest_calm_ms = 0;
    let mut calm_from = 0;
    for b in &bouts {
        longest_calm_ms = longest_calm_ms.max(b.start_ms - calm_from);
        calm_from = b.end_ms;
    }
    longest_calm_ms = longest_calm_ms.max(duration.saturating_sub(calm_from));

    let gated = analysis.windows.iter().filter(|w| w.gated).count();
    let stats = Stats {
        duration_ms: duration,
        dog_time_ms,
        dog_percent: pct(dog_time_ms, duration),
        classes,
        episodes: episodes.len() as u32,
        bouts: bouts.len() as u32,
        first_dog_sound_ms: episodes.first().map(|e| e.start_ms),
        last_dog_sound_ms: episodes.iter().map(|e| e.end_ms).max(),
        longest_episode: episodes.iter().max_by_key(|e| e.span_ms()).cloned(),
        longest_calm_ms,
        gated_percent: pct(gated as u64, analysis.windows.len() as u64),
    };

    Report {
        source,
        started_at,
        settings: settings.clone(),
        stats,
        timeline: timeline(&slots, duration, settings.timeline_bin_ms),
        hourly: hourly(&slots, duration, started_at),
        episodes,
        bouts,
        windows: Vec::new(),
    }
}

/// Раскладывает отрезки по корзинам с границами `edges` (по возрастанию).
fn bucketize(slots: &[Slot], edges: &[u64]) -> Vec<([u64; 6], f32)> {
    let mut out = vec![([0u64; 6], f32::MIN); edges.len().saturating_sub(1)];
    let mut b = 0;
    for s in slots {
        while b + 1 < out.len() && edges[b + 1] <= s.start {
            b += 1;
        }
        // Отрезок может пересекать границу корзины.
        let mut k = b;
        while k < out.len() && edges[k] < s.end {
            let overlap = s
                .end
                .min(edges[k + 1])
                .saturating_sub(s.start.max(edges[k]));
            out[k].0[s.class.index()] += overlap;
            if overlap > 0 {
                out[k].1 = out[k].1.max(s.peak_db);
            }
            k += 1;
        }
    }
    out
}

fn timeline(slots: &[Slot], duration: u64, bin_ms: u64) -> Vec<TimelineBin> {
    let bin_ms = bin_ms.max(PATCH_HOP_MS);
    let mut edges: Vec<u64> = (0..)
        .map(|i| i * bin_ms)
        .take_while(|&t| t < duration)
        .collect();
    edges.push(duration);
    bucketize(slots, &edges)
        .into_iter()
        .zip(edges.windows(2))
        .map(|((class_ms, peak_db), e)| TimelineBin {
            start_ms: e[0],
            end_ms: e[1],
            class_ms,
            peak_db: if peak_db == f32::MIN { -120.0 } else { peak_db },
        })
        .collect()
}

const HOUR_MS: u64 = 3_600_000;

fn hourly(slots: &[Slot], duration: u64, started_at: Option<StartTime>) -> Vec<HourBucket> {
    // Смещение начала записи внутри часа и сам час по локальному времени.
    let (offset_in_hour, first_hour) = match started_at {
        Some(st) => {
            let local = st.unix_ms + st.utc_offset_min as i64 * 60_000;
            let in_day = local.rem_euclid(24 * HOUR_MS as i64) as u64;
            (in_day % HOUR_MS, (in_day / HOUR_MS) as u32)
        }
        None => (0, 0),
    };
    let mut edges = vec![0];
    let mut t = HOUR_MS - offset_in_hour;
    while t < duration {
        edges.push(t);
        t += HOUR_MS;
    }
    edges.push(duration.max(1));
    let wrap = started_at.is_some();
    bucketize(slots, &edges)
        .into_iter()
        .zip(edges.windows(2))
        .enumerate()
        .map(|(i, ((class_ms, _), e))| HourBucket {
            hour: if wrap {
                (first_hour + i as u32) % 24
            } else {
                i as u32
            },
            start_ms: e[0],
            end_ms: e[1],
            class_ms,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::ClassScores;

    /// Строит анализ из строки: один символ — одно окно 0.48 с.
    /// b — лай, w — скулёж, h — вой, g — рычание, . — тишина, o — прочее.
    fn analysis(pattern: &str) -> Analysis {
        let windows: Vec<WindowResult> = pattern
            .chars()
            .enumerate()
            .map(|(i, c)| WindowResult {
                start_ms: i as u64 * PATCH_HOP_MS,
                class: match c {
                    'b' => SoundClass::Bark,
                    'w' => SoundClass::Whine,
                    'h' => SoundClass::Howl,
                    'g' => SoundClass::Growl,
                    'o' => SoundClass::Other,
                    _ => SoundClass::Silence,
                },
                confidence: 0.9,
                rms_db: -30.0,
                peak_db: -20.0 - i as f32 * 0.01,
                gated: c == '.',
                scores: ClassScores::default(),
            })
            .collect();
        Analysis {
            duration_ms: windows.len() as u64 * PATCH_HOP_MS,
            windows,
        }
    }

    fn report(pattern: &str) -> Report {
        build_report(&analysis(pattern), &ReportSettings::default(), None, None)
    }

    fn stat(r: &Report, c: SoundClass) -> &ClassStat {
        &r.stats.classes[c.index()]
    }

    #[test]
    fn percentages_sum_to_100() {
        let r = report("..bbb..www..oo....hg");
        let total: f32 = r.stats.classes.iter().map(|c| c.percent).sum();
        assert!((total - 100.0).abs() < 1e-3);
        let dog: f32 = r.stats.classes.iter().filter_map(|c| c.dog_percent).sum();
        assert!((dog - 100.0).abs() < 1e-3);
        assert_eq!(stat(&r, SoundClass::Bark).time_ms, 3 * PATCH_HOP_MS);
    }

    #[test]
    fn short_pauses_merge_into_one_episode() {
        // Пауза 4 окна = 1.92 с < 3 с — один эпизод лая.
        let r = report("bb....bb");
        assert_eq!(r.episodes.len(), 1);
        assert_eq!(r.episodes[0].span_ms(), 8 * PATCH_HOP_MS);
        assert_eq!(r.episodes[0].sound_ms, 4 * PATCH_HOP_MS);
        // Пауза 8 окон = 3.84 с > 3 с — два эпизода.
        assert_eq!(report("bb........bb").episodes.len(), 2);
    }

    #[test]
    fn single_flip_between_dog_classes_is_smoothed() {
        let r = report("bbwbb");
        assert_eq!(stat(&r, SoundClass::Whine).time_ms, 0);
        // Одиночный скулёж на краю лая — тоже лай.
        assert_eq!(
            stat(&report("..wbbb..bbbw.."), SoundClass::Whine).time_ms,
            0
        );
        // Два одиночных окна рядом не меняются классами: лай(1) скулёж(1) лай(4).
        let r = report("..bwbbbb..");
        assert_eq!(stat(&r, SoundClass::Whine).time_ms, 0);
        assert_eq!(stat(&r, SoundClass::Bark).time_ms, 6 * PATCH_HOP_MS);
        // А настоящий скулёж из нескольких окон остаётся.
        assert_eq!(
            stat(&report("bbbwww"), SoundClass::Whine).time_ms,
            3 * PATCH_HOP_MS
        );
        // Одиночный гав в тишине остаётся.
        let r = report("....b....");
        assert_eq!(stat(&r, SoundClass::Bark).episodes, 1);
    }

    #[test]
    fn first_last_and_calm() {
        let r = report("..........bb..........ww");
        assert_eq!(r.stats.first_dog_sound_ms, Some(10 * PATCH_HOP_MS));
        assert_eq!(r.stats.last_dog_sound_ms, Some(24 * PATCH_HOP_MS));
        assert_eq!(r.stats.longest_calm_ms, 10 * PATCH_HOP_MS);
        assert_eq!(r.stats.bouts, 1); // пауза 4.8 с < 30 с
    }

    #[test]
    fn no_dog_at_all() {
        let r = report("....oo....");
        assert_eq!(r.stats.dog_time_ms, 0);
        assert_eq!(r.stats.first_dog_sound_ms, None);
        assert_eq!(r.stats.longest_calm_ms, r.stats.duration_ms);
        assert!(
            r.stats
                .classes
                .iter()
                .all(|c| c.dog_percent.is_none_or(|p| p == 0.0))
        );
    }

    #[test]
    fn timeline_and_hours_cover_whole_recording() {
        // 2 часа 10 минут тишины с лаем посередине.
        let n = (2 * HOUR_MS + 600_000) / PATCH_HOP_MS;
        let mut p: String = ".".repeat(n as usize);
        p.replace_range(8000..8010, "bbbbbbbbbb");
        let a = analysis(&p);
        let total: u64 = a.duration_ms;

        let r = build_report(&a, &ReportSettings::default(), None, None);
        let tl: u64 = r
            .timeline
            .iter()
            .map(|b| b.class_ms.iter().sum::<u64>())
            .sum();
        assert_eq!(tl, total);
        assert_eq!(r.hourly.len(), 3);
        let hours: u64 = r
            .hourly
            .iter()
            .map(|b| b.class_ms.iter().sum::<u64>())
            .sum();
        assert_eq!(hours, total);

        // Начало в 09:45 по местному времени (UTC+3): первый час короткий.
        let start = StartTime {
            unix_ms: (6 * HOUR_MS + 45 * 60_000) as i64,
            utc_offset_min: 180,
            source: crate::start::StartSource::Manual,
        };
        let r = build_report(&a, &ReportSettings::default(), Some(start), None);
        assert_eq!(r.hourly[0].hour, 9);
        assert_eq!(r.hourly[0].end_ms, 15 * 60_000);
        assert_eq!(r.hourly[1].hour, 10);
        let hours: u64 = r
            .hourly
            .iter()
            .map(|b| b.class_ms.iter().sum::<u64>())
            .sum();
        assert_eq!(hours, total);
    }
}
