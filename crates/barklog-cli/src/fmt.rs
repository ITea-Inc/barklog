//! Форматирование времени и чисел для отчёта (по-русски, в одном формате).

use barklog_core::StartTime;
use chrono::{DateTime, FixedOffset};

/// «1 ч 12 мин», «6 мин 12 с», «45 с».
pub fn duration(ms: u64) -> String {
    if ms > 0 && ms < 1000 {
        return "< 1 с".into();
    }
    let s = (ms + 500) / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    match (h, m) {
        (0, 0) => format!("{s} с"),
        (0, _) if s == 0 => format!("{m} мин"),
        (0, _) => format!("{m} мин {s} с"),
        _ if m == 0 => format!("{h} ч"),
        _ => format!("{h} ч {m} мин"),
    }
}

/// Счётчик ленты: «02:14:30».
pub fn counter(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Процент с одним знаком и запятой: «9,4 %».
pub fn percent(p: f32) -> String {
    format!("{:.1}\u{a0}%", p).replace('.', ",")
}

pub fn local_time(start: &StartTime, offset_ms: u64) -> Option<DateTime<FixedOffset>> {
    let tz = FixedOffset::east_opt(start.utc_offset_min * 60)?;
    DateTime::from_timestamp_millis(start.unix_ms + offset_ms as i64).map(|t| t.with_timezone(&tz))
}

/// Время на часах «09:44:05», если известно начало записи, иначе счётчик ленты.
pub fn clock(start: Option<&StartTime>, offset_ms: u64) -> String {
    match start.and_then(|s| local_time(s, offset_ms)) {
        Some(t) => t.format("%H:%M:%S").to_string(),
        None => counter(offset_ms),
    }
}

/// «7 ноября 2026».
pub fn date_ru(t: &DateTime<FixedOffset>) -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    format!("{} {} {}", t.day(), MONTHS[t.month0() as usize], t.year())
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(duration(0), "0 с");
        assert_eq!(duration(480), "< 1 с");
        assert_eq!(duration(45_000), "45 с");
        assert_eq!(duration(372_000), "6 мин 12 с");
        assert_eq!(duration(600_000), "10 мин");
        assert_eq!(duration(4_320_000), "1 ч 12 мин");
        assert_eq!(duration(7_200_000), "2 ч");
        assert_eq!(counter(8_070_000), "02:14:30");
        assert_eq!(percent(9.44), "9,4\u{a0}%");
    }
}
