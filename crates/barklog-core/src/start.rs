//! Определение времени начала записи — общее для CLI и мобильных приложений.
//!
//! Порядок: указано вручную → дата в имени файла → время изменения файла минус длительность
//! (диктофон обычно сохраняет файл в момент остановки записи).

use serde::{Deserialize, Serialize};

use crate::report::StartTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartSource {
    Manual,
    FileName,
    FileModified,
    /// Записано в приложении — время точное.
    Recorded,
}

impl StartSource {
    pub fn label_ru(self) -> &'static str {
        match self {
            StartSource::Manual => "указано вручную",
            StartSource::FileName => "из имени файла",
            StartSource::FileModified => "по времени изменения файла, может быть неточным",
            StartSource::Recorded => "записано в приложении",
        }
    }
}

/// Ищет в имени «ГГГГММДД_ЧЧММСС» или «ГГГГММДД-ЧЧММСС» (так называют файлы многие диктофоны)
/// и трактует его как местное время со смещением `utc_offset_min`.
pub fn start_from_name(name: &str, utc_offset_min: i32) -> Option<StartTime> {
    let b = name.as_bytes();
    (0..b.len().saturating_sub(14)).find_map(|i| {
        let w = &b[i..i + 15];
        let num = |r: std::ops::Range<usize>| -> Option<i64> {
            w[r].iter().try_fold(0i64, |acc, c| {
                c.is_ascii_digit().then(|| acc * 10 + (c - b'0') as i64)
            })
        };
        if w[8] != b'_' && w[8] != b'-' {
            return None;
        }
        let (y, mo, d) = (num(0..4)?, num(4..6)?, num(6..8)?);
        let (h, mi, s) = (num(9..11)?, num(11..13)?, num(13..15)?);
        let valid = (2000..=2100).contains(&y)
            && (1..=12).contains(&mo)
            && (1..=31).contains(&d)
            && h < 24
            && mi < 60
            && s < 60;
        if !valid {
            return None;
        }
        let local_ms = (days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s) * 1000;
        Some(StartTime {
            unix_ms: local_ms - utc_offset_min as i64 * 60_000,
            utc_offset_min,
            source: StartSource::FileName,
        })
    })
}

/// Начало = время изменения файла минус длительность записи.
pub fn start_from_modified(
    modified_unix_ms: i64,
    duration_ms: u64,
    utc_offset_min: i32,
) -> StartTime {
    StartTime {
        unix_ms: modified_unix_ms - duration_ms as i64,
        utc_offset_min,
        source: StartSource::FileModified,
    }
}

/// Дни от 1970-01-01 для григорианской даты (алгоритм Говарда Хиннанта).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 11, 7), 20_764);
    }

    #[test]
    fn parses_recorder_file_names() {
        let st = start_from_name("Voice_20261107_093012", 180).unwrap();
        // 09:30:12 по UTC+3 = 06:30:12 UTC.
        let utc_ms = (20_764 * 86_400 + 6 * 3600 + 30 * 60 + 12) * 1000;
        assert_eq!(st.unix_ms, utc_ms);
        assert_eq!(st.source, StartSource::FileName);
        assert!(start_from_name("REC-20261107-093012", 0).is_some());
        assert!(start_from_name("Запись 12", 0).is_none());
        assert!(start_from_name("19991399_256199", 0).is_none());
    }

    #[test]
    fn modified_minus_duration() {
        let st = start_from_modified(10_000_000, 3_000_000, 60);
        assert_eq!(st.unix_ms, 7_000_000);
        assert_eq!(st.source, StartSource::FileModified);
    }
}
