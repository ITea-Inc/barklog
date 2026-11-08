//! Выгрузка в CSV — для таблиц и для кинолога.
//!
//! Разделитель `;` и десятичная запятая, в начале — BOM: так файл без настройки
//! открывается в Excel с русской локалью, с кириллицей и цифрами на своих местах.

use std::fmt::Write;

use crate::{LabelKind, SessionReport, SessionSummary, SoundKind, StartInfo};

const BOM: &str = "\u{feff}";

/// Эпизоды одной записи.
pub fn episodes(r: &SessionReport) -> String {
    let mut out = String::from(BOM);
    row(
        &mut out,
        &[
            "№",
            if r.started_at.is_some() {
                "Начало"
            } else {
                "Начало от старта записи"
            },
            "Конец",
            "Длительность, с",
            "Звук, с",
            "Что",
            "Отметка хозяина",
            "Пик, дБ",
            "Уверенность, %",
        ],
    );
    for (i, e) in r.episodes.iter().enumerate() {
        row(
            &mut out,
            &[
                &(i + 1).to_string(),
                &clock(r.started_at.as_ref(), e.start_ms),
                &clock(r.started_at.as_ref(), e.end_ms),
                &decimal((e.end_ms - e.start_ms) as f64 / 1000.0, 1),
                &decimal(e.sound_ms as f64 / 1000.0, 1),
                kind(e.kind),
                e.label.map(label).unwrap_or(""),
                &decimal(e.peak_db as f64, 0),
                &decimal(e.confidence as f64 * 100.0, 0),
            ],
        );
    }
    out
}

/// Все записи — по строке на каждую.
pub fn sessions(list: &[SessionSummary]) -> String {
    let mut out = String::from(BOM);
    row(
        &mut out,
        &[
            "Дата",
            "Начало",
            "Конец",
            "Длительность, мин",
            "Голос собаки, мин",
            "Голос, % записи",
            "Лай, мин",
            "Скулёж, мин",
            "Вой, мин",
            "Рычание, мин",
            "Эпизодов",
            "Заметка",
            "Файл",
        ],
    );
    let min = |ms: u64| decimal(ms as f64 / 60_000.0, 1);
    for s in list {
        let (date, start, end) = match &s.started_at {
            Some(st) => (date(st), clock(Some(st), 0), clock(Some(st), s.duration_ms)),
            None => (String::new(), String::new(), String::new()),
        };
        row(
            &mut out,
            &[
                &date,
                &start,
                &end,
                &min(s.duration_ms),
                &min(s.dog_ms),
                &decimal(s.dog_percent as f64, 1),
                &min(s.bark_ms),
                &min(s.whine_ms),
                &min(s.howl_ms),
                &min(s.growl_ms),
                &s.episodes.to_string(),
                s.note.as_deref().unwrap_or(""),
                &s.source_name,
            ],
        );
    }
    out
}

fn row(out: &mut String, fields: &[&str]) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        if f.contains([';', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&f.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(f);
        }
    }
    out.push_str("\r\n");
}

fn decimal(v: f64, digits: usize) -> String {
    format!("{v:.digits$}").replace('.', ",")
}

/// Время на часах «09:14:05», если известно начало записи, иначе — от начала записи.
fn clock(start: Option<&StartInfo>, offset_ms: u64) -> String {
    let secs = match start {
        Some(st) => {
            (st.unix_ms + offset_ms as i64).div_euclid(1000) + st.utc_offset_min as i64 * 60
        }
        None => (offset_ms / 1000) as i64,
    };
    let in_day = if start.is_some() {
        secs.rem_euclid(86_400)
    } else {
        secs
    };
    let mut s = String::new();
    let _ = write!(
        s,
        "{:02}:{:02}:{:02}",
        in_day / 3600,
        in_day / 60 % 60,
        in_day % 60
    );
    s
}

/// «2026-11-07» — так даты сортируются и в Excel, и где угодно.
fn date(st: &StartInfo) -> String {
    let days = (st.unix_ms.div_euclid(1000) + st.utc_offset_min as i64 * 60).div_euclid(86_400);
    let d = barklog_core::trends::civil_from_days(days);
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

fn kind(k: SoundKind) -> &'static str {
    match k {
        SoundKind::Bark => "лай",
        SoundKind::Whine => "скулёж",
        SoundKind::Howl => "вой",
        SoundKind::Growl => "рычание",
        SoundKind::Other => "другие звуки",
        SoundKind::Silence => "тишина",
    }
}

fn label(l: LabelKind) -> &'static str {
    match l {
        LabelKind::Bark => "лай",
        LabelKind::Whine => "скулёж",
        LabelKind::Howl => "вой",
        LabelKind::Growl => "рычание",
        LabelKind::NotDog => "не собака",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EpisodeInfo, StartSource};

    fn start() -> StartInfo {
        // 2026-11-07 09:30:00 по Москве.
        StartInfo {
            unix_ms: 1_794_033_000_000,
            utc_offset_min: 180,
            source: StartSource::FileName,
        }
    }

    #[test]
    fn escapes_and_formats() {
        let mut out = String::new();
        row(&mut out, &["a;b", "say \"hi\"", "plain"]);
        assert_eq!(out, "\"a;b\";\"say \"\"hi\"\"\";plain\r\n");
        assert_eq!(decimal(1.25, 1), "1,2");
        assert_eq!(clock(Some(&start()), 65_000), "09:31:05");
        assert_eq!(clock(None, 3_725_000), "01:02:05");
        assert_eq!(date(&start()), "2026-11-07");
    }

    #[test]
    fn sessions_table() {
        let s = SessionSummary {
            id: 1,
            created_at_ms: 0,
            source_name: "rec.m4a".into(),
            started_at: Some(start()),
            duration_ms: 40 * 60_000,
            dog_ms: 90_000,
            dog_percent: 3.75,
            bark_ms: 60_000,
            whine_ms: 30_000,
            howl_ms: 0,
            growl_ms: 0,
            episodes: 4,
            note: Some("конг; радио".into()),
        };
        let csv = sessions(&[s]);
        assert!(csv.starts_with(BOM));
        let line = csv.lines().nth(1).unwrap();
        assert_eq!(
            line,
            "2026-11-07;09:30:00;10:10:00;40,0;1,5;3,8;1,0;0,5;0,0;0,0;4;\"конг; радио\";rec.m4a"
        );
    }

    #[test]
    fn episodes_table() {
        let e = EpisodeInfo {
            kind: SoundKind::Whine,
            start_ms: 61_000,
            end_ms: 64_500,
            sound_ms: 2_880,
            peak_db: -12.4,
            confidence: 0.42,
            label: Some(LabelKind::Bark),
        };
        let r = SessionReport {
            id: 1,
            source_name: "x".into(),
            source_uri: None,
            editable: true,
            labels: Vec::new(),
            note: None,
            started_at: None,
            duration_ms: 100_000,
            dog_ms: 2_880,
            dog_percent: 2.9,
            episodes_count: 1,
            first_dog_sound_ms: Some(61_000),
            last_dog_sound_ms: Some(64_500),
            longest_episode_ms: Some(3_500),
            longest_calm_ms: 61_000,
            gated_percent: 0.0,
            merge_gap_ms: 3_000,
            timeline_bin_ms: 10_000,
            classes: Vec::new(),
            episodes: vec![e],
            timeline: Vec::new(),
            hourly: Vec::new(),
        };
        let csv = episodes(&r);
        let mut lines = csv.lines();
        assert!(lines.next().unwrap().contains("Начало от старта записи"));
        assert_eq!(
            lines.next().unwrap(),
            "1;00:01:01;00:01:04;3,5;2,9;скулёж;лай;-12;42"
        );
    }
}
