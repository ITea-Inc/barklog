//! Самодостаточный HTML-отчёт с SVG-графиками. Дизайн — docs/DESIGN.md.

use std::collections::HashMap;
use std::fmt::Write;

use barklog_core::{Report, SoundClass};

use crate::fmt::{clock, counter, date_ru, duration, escape, local_time, percent};

const MAX_EPISODE_ROWS: usize = 300;

pub struct HtmlInput<'a> {
    pub report: &'a Report,
    pub file_name: &'a str,
    /// Индекс эпизода → относительный путь к клипу.
    pub clips: &'a HashMap<usize, String>,
}

fn css_class(c: SoundClass) -> &'static str {
    match c {
        SoundClass::Bark => "bark",
        SoundClass::Whine => "whine",
        SoundClass::Howl => "howl",
        SoundClass::Growl => "growl",
        SoundClass::Other => "other",
        SoundClass::Silence => "silence",
    }
}

pub fn render(input: &HtmlInput) -> String {
    let r = input.report;
    let s = &r.stats;
    let start = r.started_at.as_ref();

    let (title, period) = match start.and_then(|st| local_time(st, 0)) {
        Some(t0) => {
            let t1 = local_time(start.unwrap(), s.duration_ms).unwrap_or(t0);
            (
                date_ru(&t0),
                format!("{}–{}", t0.format("%H:%M"), t1.format("%H:%M")),
            )
        }
        None => (input.file_name.to_string(), String::new()),
    };

    let mut h = String::with_capacity(64 * 1024);
    h.push_str(HEAD);
    let _ = write!(
        h,
        "<title>BarkLog · {}</title></head><body><main class=\"page\">",
        escape(&title)
    );

    // Шапка.
    let _ = write!(
        h,
        "<header class=\"top\"><span class=\"brand\">BarkLog</span>\
         <span class=\"mono muted\">{}</span></header>",
        escape(input.file_name)
    );
    let _ = write!(h, "<section class=\"head\"><h1>{}", escape(&title));
    if !period.is_empty() {
        let _ = write!(h, " <span class=\"mono period\">{period}</span>");
    }
    h.push_str("</h1>");
    let summary = if s.dog_time_ms == 0 {
        format!("Запись {}. Собаку не слышно", duration(s.duration_ms))
    } else {
        format!(
            "Запись {}. Собака подавала голос {} — {} времени",
            duration(s.duration_ms),
            duration(s.dog_time_ms),
            percent(s.dog_percent)
        )
    };
    let _ = write!(h, "<p class=\"sub\">{summary}</p></section>");

    // Ключевые цифры.
    h.push_str("<section class=\"stats\">");
    stat(
        &mut h,
        "Голос собаки",
        &duration(s.dog_time_ms),
        &format!("{} записи", percent(s.dog_percent)),
    );
    let longest = s
        .longest_episode
        .as_ref()
        .map(|e| format!("самый длинный {}", duration(e.span_ms())))
        .unwrap_or_else(|| "не было".into());
    stat(&mut h, "Эпизодов", &s.episodes.to_string(), &longest);
    match s.first_dog_sound_ms {
        Some(ms) => stat(
            &mut h,
            "Первый звук",
            &counter(ms),
            &format!("в {}", clock(start, ms)),
        ),
        None => stat(&mut h, "Первый звук", "—", "собаку не слышно"),
    }
    stat(
        &mut h,
        "Самый долгий покой",
        &duration(s.longest_calm_ms),
        "без звуков собаки",
    );
    h.push_str("</section>");

    timeline(&mut h, r);

    h.push_str("<div class=\"split\">");
    shares(&mut h, r);
    hours(&mut h, r);
    h.push_str("</div>");

    episodes(&mut h, input);

    // Подвал с параметрами анализа.
    let _ = write!(
        h,
        "<footer class=\"foot mono muted\"><span>YAMNet, окно 0,96 с, шаг 0,48 с</span>\
         <span>эпизод: паузы до {} с</span><span>без сети отсеяно {} тихих окон</span>",
        r.settings.merge_gap_ms / 1000,
        percent(s.gated_percent)
    );
    if let Some(st) = &r.started_at {
        let _ = write!(h, "<span>начало записи: {}</span>", st.source.label_ru());
    }
    h.push_str("</footer></main></body></html>");
    h
}

fn stat(h: &mut String, label: &str, value: &str, note: &str) {
    let _ = write!(
        h,
        "<div class=\"stat\"><div class=\"label\">{label}</div>\
         <div class=\"value\">{}</div><div class=\"note\">{}</div></div>",
        escape(value),
        escape(note)
    );
}

fn legend(h: &mut String, classes: &[SoundClass]) {
    h.push_str("<div class=\"legend\">");
    for &c in classes {
        let _ = write!(
            h,
            "<span><i class=\"dot {}\"></i>{}</span>",
            css_class(c),
            c.label_ru()
        );
    }
    h.push_str("</div>");
}

/// Главный график: столбец на интервал, высота — доля интервала со звуками собаки.
fn timeline(h: &mut String, r: &Report) {
    let bins = &r.timeline;
    let bin_ms = r.settings.timeline_bin_ms;
    let unit = if bin_ms >= 60_000 {
        format!("{} мин", bin_ms / 60_000)
    } else {
        format!("{} с", bin_ms / 1000)
    };

    let fraction = |b: &barklog_core::report::TimelineBin| {
        let len = (b.end_ms - b.start_ms).max(1) as f32;
        SoundClass::DOG
            .iter()
            .map(|c| b.class_ms[c.index()])
            .sum::<u64>() as f32
            / len
    };
    let max = bins.iter().map(fraction).fold(0f32, f32::max);
    let scale = [0.1, 0.25, 0.5, 1.0]
        .into_iter()
        .find(|&s| max <= s)
        .unwrap_or(1.0);

    let _ = write!(
        h,
        "<section class=\"card timeline\"><div class=\"card-head\"><div><h2>Сессия</h2>\
         <p class=\"muted\">Столбец — {unit}, высота — сколько из них звучала собака, \
         шкала до {}</p></div>",
        percent(scale * 100.0).replace(",0", "")
    );
    legend(h, &SoundClass::DOG);
    h.push_str("</div><div class=\"plot\">");
    for frac in [0.5, 1.0] {
        let _ = write!(
            h,
            "<i class=\"grid\" style=\"bottom:{}%\"></i>",
            frac * 100.0
        );
    }

    let n = bins.len().max(1);
    let gap = if n <= 240 { 0.18 } else { 0.0 };
    let _ = write!(
        h,
        "<svg viewBox=\"0 0 {n} 100\" preserveAspectRatio=\"none\" shape-rendering=\"crispEdges\" \
         role=\"img\" aria-label=\"Звуки собаки по времени\">"
    );
    for (i, b) in bins.iter().enumerate() {
        let len = (b.end_ms - b.start_ms).max(1) as f32;
        let mut y = 100.0;
        let mut tip = format!("{} · ", clock(r.started_at.as_ref(), b.start_ms));
        let mut any = false;
        let _ = write!(h, "<g>");
        for c in SoundClass::DOG {
            let ms = b.class_ms[c.index()];
            if ms == 0 {
                continue;
            }
            let hgt = (ms as f32 / len / scale * 100.0).min(y);
            y -= hgt;
            let _ = write!(
                h,
                "<rect class=\"{}\" x=\"{:.2}\" y=\"{y:.2}\" width=\"{:.2}\" height=\"{hgt:.2}\"/>",
                css_class(c),
                i as f32 + gap / 2.0,
                1.0 - gap
            );
            let _ = write!(
                tip,
                "{}{} {}",
                if any { ", " } else { "" },
                c.label_ru().to_lowercase(),
                duration(ms)
            );
            any = true;
        }
        if !any {
            tip.push_str("тихо");
        }
        // Прозрачная полоса во всю высоту — чтобы подсказка работала и над пустыми местами.
        let _ = write!(
            h,
            "<rect class=\"hit\" x=\"{i}\" y=\"0\" width=\"1\" height=\"100\"><title>{}</title></rect></g>",
            escape(&tip)
        );
    }
    h.push_str("</svg></div><div class=\"axis mono\">");
    let total = r.stats.duration_ms.max(1);
    for (t, label) in axis_ticks(r.started_at.as_ref(), total) {
        let pos = t as f64 * 100.0 / total as f64;
        // Крайние подписи не вылезают за край графика.
        let align = if pos < 4.0 {
            "start"
        } else if pos > 96.0 {
            "end"
        } else {
            "mid"
        };
        let _ = write!(
            h,
            "<span class=\"{align}\" style=\"left:{pos:.3}%\">{label}</span>"
        );
    }
    h.push_str("</div></section>");
}

/// Подписи оси на круглых значениях времени: шаг 1–120 мин, не больше 8 подписей.
/// Если известно время начала — по часам на стене, иначе от начала записи.
fn axis_ticks(start: Option<&barklog_core::StartTime>, total_ms: u64) -> Vec<(u64, String)> {
    const MIN: u64 = 60_000;
    let step = [
        MIN,
        2 * MIN,
        5 * MIN,
        10 * MIN,
        15 * MIN,
        30 * MIN,
        60 * MIN,
        120 * MIN,
    ]
    .into_iter()
    .find(|s| total_ms / s <= 7)
    .unwrap_or(240 * MIN);
    // Смещение начала записи относительно круглого времени.
    let phase = start
        .map(|s| (s.unix_ms + s.utc_offset_min as i64 * MIN as i64).rem_euclid(step as i64) as u64)
        .unwrap_or(0);
    let first = if phase == 0 { 0 } else { step - phase };
    (0..)
        .map(|k| first + k * step)
        .take_while(|&t| t <= total_ms)
        .map(|t| {
            let label = match start {
                Some(_) => clock(start, t).get(..5).unwrap_or_default().to_string(),
                None if total_ms >= 60 * MIN => format!("{}:{:02}", t / (60 * MIN), t / MIN % 60),
                None => format!("{} мин", t / MIN),
            };
            (t, label)
        })
        .collect()
}

/// Доли: одна полоса на всю запись и список с тонкими полосками.
fn shares(h: &mut String, r: &Report) {
    let s = &r.stats;
    h.push_str(
        "<section class=\"card shares\"><div class=\"card-head\"><div><h2>Из чего состояла запись</h2>\
         <p class=\"muted\">Доля от всего времени и среди звуков собаки</p></div></div>\
         <div class=\"stack\">",
    );
    for c in &s.classes {
        if c.percent > 0.0 {
            let _ = write!(
                h,
                "<i class=\"{}\" style=\"width:{:.3}%\" title=\"{} · {}\"></i>",
                css_class(c.class),
                c.percent,
                c.class.label_ru(),
                percent(c.percent)
            );
        }
    }
    h.push_str("</div><div class=\"scroll\"><table class=\"rows\"><thead><tr><th>Звук</th><th class=\"num\">Время</th>\
                <th class=\"num\">От записи</th><th class=\"num\">Среди голоса</th></tr></thead><tbody>");
    for c in &s.classes {
        let dog = c.dog_percent.map(percent).unwrap_or_else(|| "—".into());
        let _ = write!(
            h,
            "<tr><td><i class=\"dot {}\"></i>{}</td><td class=\"num mono\">{}</td>\
             <td class=\"num mono\">{}</td><td class=\"num mono\">{dog}</td></tr>",
            css_class(c.class),
            c.class.label_ru(),
            duration(c.time_ms),
            percent(c.percent)
        );
    }
    h.push_str("</tbody></table></div></section>");
}

fn hours(h: &mut String, r: &Report) {
    let known = r.started_at.is_some();
    let _ = write!(
        h,
        "<section class=\"card hours\"><div class=\"card-head\"><div><h2>По часам</h2>\
         <p class=\"muted\">{}</p></div></div><div class=\"scroll\"><table class=\"rows\"><tbody>",
        if known {
            "Сколько звучала собака в каждый час"
        } else {
            "Часы от начала записи"
        }
    );
    for b in &r.hourly {
        let len = (b.end_ms - b.start_ms).max(1);
        let dog: u64 = SoundClass::DOG.iter().map(|c| b.class_ms[c.index()]).sum();
        let label = if known {
            format!("{:02}:00", b.hour)
        } else {
            format!("{}-й час", b.hour + 1)
        };
        let _ = write!(
            h,
            "<tr><td class=\"mono\">{label}</td><td class=\"barcell\"><div class=\"thin\">"
        );
        for c in SoundClass::DOG {
            let ms = b.class_ms[c.index()];
            if ms > 0 {
                let _ = write!(
                    h,
                    "<i class=\"{}\" style=\"width:{:.3}%\"></i>",
                    css_class(c),
                    ms as f64 * 100.0 / len as f64
                );
            }
        }
        let _ = write!(
            h,
            "</div></td><td class=\"num mono\">{}</td></tr>",
            if dog == 0 {
                "—".into()
            } else {
                duration(dog)
            }
        );
    }
    h.push_str("</tbody></table></div></section>");
}

fn episodes(h: &mut String, input: &HtmlInput) {
    let r = input.report;
    let _ = write!(
        h,
        "<section class=\"card episodes\"><div class=\"card-head\"><div><h2>Эпизоды</h2>\
         <p class=\"muted\">{} · соседние звуки с паузой до {} с считаются одним эпизодом</p></div></div>",
        r.episodes.len(),
        r.settings.merge_gap_ms / 1000
    );
    if r.episodes.is_empty() {
        h.push_str("<p class=\"empty\">За запись собака не подавала голос</p></section>");
        return;
    }
    let with_clips = !input.clips.is_empty();
    h.push_str(
        "<div class=\"scroll\"><table class=\"rows\"><thead><tr><th>Начало</th><th>Звук</th><th class=\"num\">Длительность</th>\
         <th class=\"num\">Чистый звук</th><th class=\"num\">Пик</th>",
    );
    if with_clips {
        h.push_str("<th class=\"clip\">Фрагмент</th>");
    }
    h.push_str("</tr></thead><tbody>");
    for (i, e) in r.episodes.iter().enumerate().take(MAX_EPISODE_ROWS) {
        let _ = write!(
            h,
            "<tr><td class=\"mono\">{}</td><td><i class=\"dot {}\"></i>{}</td>\
             <td class=\"num mono\">{}</td><td class=\"num mono\">{}</td><td class=\"num mono\">{:.0} дБ</td>",
            clock(r.started_at.as_ref(), e.start_ms),
            css_class(e.class),
            e.class.label_ru(),
            duration(e.span_ms()),
            duration(e.sound_ms),
            e.peak_db
        );
        if with_clips {
            match input.clips.get(&i) {
                Some(p) => {
                    let _ = write!(
                        h,
                        "<td class=\"clip\"><audio controls preload=\"none\" src=\"{}\"></audio></td>",
                        escape(p)
                    );
                }
                None => h.push_str("<td class=\"clip\"></td>"),
            }
        }
        h.push_str("</tr>");
    }
    h.push_str("</tbody></table></div>");
    if r.episodes.len() > MAX_EPISODE_ROWS {
        let _ = write!(
            h,
            "<p class=\"empty\">Показаны первые {MAX_EPISODE_ROWS}, все — в report.json</p>"
        );
    }
    h.push_str("</section>");
}

const HEAD: &str = r#"<!doctype html><html lang="ru"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Instrument+Sans:wght@400;500&family=JetBrains+Mono:wght@400;500&display=swap" rel="stylesheet">
<style>
:root{
  --bg:#fafafa;--card:#fff;--raised:#f4f4f5;--line:rgba(23,23,23,.08);
  --text:#171717;--text-2:#525252;--muted:#737373;
  --bark:#c27f22;--whine:#c55a43;--howl:#4f73b3;--growl:#6b8448;--other:#d4d4d8;--silence:#ececee;
  --shadow:0 1px 2px rgba(0,0,0,.05);
  color-scheme:light;
}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){
  --bg:#141415;--card:#19191b;--raised:#232325;--line:rgba(237,237,237,.08);
  --text:#ededed;--text-2:#a1a1a6;--muted:#85858b;
  --bark:#d9973a;--whine:#d8705a;--howl:#7d9bd0;--growl:#93a872;--other:#3a3a3e;--silence:#26262a;
  --shadow:none;color-scheme:dark;
}}
:root[data-theme="dark"]{
  --bg:#141415;--card:#19191b;--raised:#232325;--line:rgba(237,237,237,.08);
  --text:#ededed;--text-2:#a1a1a6;--muted:#85858b;
  --bark:#d9973a;--whine:#d8705a;--howl:#7d9bd0;--growl:#93a872;--other:#3a3a3e;--silence:#26262a;
  --shadow:none;color-scheme:dark;
}
*{box-sizing:border-box;margin:0}
body{background:var(--bg);color:var(--text);font:400 14px/20px "Instrument Sans",system-ui,-apple-system,"Segoe UI",sans-serif;-webkit-font-smoothing:antialiased}
.mono{font-family:"JetBrains Mono",ui-monospace,"Cascadia Mono",Consolas,monospace;font-variant-numeric:tabular-nums;font-size:12px}
.muted{color:var(--muted)}
.page{max-width:1200px;margin:0 auto;padding:24px 32px 48px}
.top{display:flex;justify-content:space-between;align-items:center;gap:16px;height:40px;min-width:0}
.brand{font-weight:500;font-size:16px}
.top .mono{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.head{padding:40px 0 32px}
h1{font-weight:500;font-size:32px;line-height:1.15;letter-spacing:-.01em}
h1 .period{font-size:20px;color:var(--text-2);font-weight:400;letter-spacing:0;margin-left:8px}
.sub{margin-top:12px;font-size:18px;line-height:28px;color:var(--text-2);max-width:40rem}
h2{font-weight:500;font-size:15px;line-height:22px}
.stats{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:12px;margin-bottom:12px}
.stat,.card{background:var(--card);border:1px solid var(--line);border-radius:12px;box-shadow:var(--shadow)}
.stat{padding:20px}
.stat .label{color:var(--text-2)}
.stat .value{margin-top:12px;font-size:28px;line-height:1;font-weight:500;font-variant-numeric:tabular-nums}
.stat .note{margin-top:8px;color:var(--muted);font-size:13px}
.card{padding:24px;margin-bottom:12px;min-width:0}
.scroll{overflow-x:auto}
.card-head{display:flex;justify-content:space-between;align-items:flex-start;gap:16px;margin-bottom:20px}
.card-head p{margin-top:4px;font-size:13px}
.legend{display:flex;flex-wrap:wrap;gap:16px;color:var(--text-2);font-size:13px}
.legend span{display:inline-flex;align-items:center}
.dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:8px;flex:none;vertical-align:1px}
.bark{background:var(--bark);fill:var(--bark)}.whine{background:var(--whine);fill:var(--whine)}
.howl{background:var(--howl);fill:var(--howl)}.growl{background:var(--growl);fill:var(--growl)}
.other{background:var(--other);fill:var(--other)}.silence{background:var(--silence);fill:var(--silence)}
.plot{position:relative;height:280px;border-bottom:1px solid var(--line)}
.plot svg{position:absolute;inset:0;width:100%;height:100%;display:block}
.plot .grid{position:absolute;left:0;right:0;border-top:1px dashed var(--line)}
.hit{fill:transparent}.hit:hover{fill:var(--line)}
.axis{position:relative;height:28px;color:var(--muted)}
.axis span{position:absolute;top:8px;transform:translateX(-50%)}
.axis .start{transform:none}.axis .end{transform:translateX(-100%)}
.split{display:grid;grid-template-columns:minmax(0,3fr) minmax(0,2fr);gap:12px}
.split .card{margin-bottom:12px}
.stack{display:flex;height:12px;border-radius:4px;overflow:hidden;gap:2px;margin-bottom:16px}
.stack i{display:block;min-width:2px}
table.rows{width:100%;border-collapse:collapse}
.rows th{text-align:left;font-weight:400;color:var(--muted);font-size:12px;padding:0 8px 8px 0}
.rows td{height:36px;padding:0 8px 0 0;border-top:1px solid var(--line);white-space:nowrap}
.rows .num{text-align:right;padding-right:0;padding-left:12px}
.barcell{width:100%}
.thin{display:flex;height:6px;border-radius:3px;background:var(--raised);overflow:hidden}
.thin i{display:block}
.rows .clip{padding-left:24px}
.episodes audio{height:28px;width:220px;vertical-align:middle}
.empty{color:var(--muted);padding:12px 0 0}
.foot{display:flex;flex-wrap:wrap;gap:8px 24px;padding-top:24px}
@media (max-width:860px){
  .page{padding:16px 16px 40px}
  .stats{grid-template-columns:repeat(2,minmax(0,1fr))}
  .split{grid-template-columns:minmax(0,1fr)}
  .stat .value{font-size:24px}
  .plot{height:200px}
  .card{padding:16px}
  .card-head{flex-direction:column}
  h1{font-size:28px}
  h1 .period{display:block;margin:8px 0 0}
}
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use barklog_core::StartTime;

    #[test]
    fn ticks_are_round() {
        // Начало 09:30:00 UTC, 40 минут: шаг 10 мин -> 09:30, 09:40, ... 10:10.
        let st = StartTime {
            unix_ms: 9 * 3_600_000 + 30 * 60_000,
            utc_offset_min: 0,
            source: barklog_core::StartSource::Manual,
        };
        let t: Vec<String> = axis_ticks(Some(&st), 40 * 60_000)
            .into_iter()
            .map(|x| x.1)
            .collect();
        assert_eq!(t, ["09:30", "09:40", "09:50", "10:00", "10:10"]);
        // Начало 09:47 — первая подпись на 10:00.
        let st = StartTime {
            unix_ms: 9 * 3_600_000 + 47 * 60_000,
            utc_offset_min: 0,
            source: barklog_core::StartSource::Manual,
        };
        let t = axis_ticks(Some(&st), 8 * 3_600_000);
        assert_eq!(t[0].1, "10:00");
        assert!(t.len() <= 8);
    }
}
