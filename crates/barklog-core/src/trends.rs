//! Как меняется картина от дня ко дню: сессии, сгруппированные по дням или неделям.
//!
//! Сессии бывают разной длины, поэтому главная цифра — доля времени, когда звучала
//! собака: 20 минут лая за 2 часа и за 8 часов — очень разные дни.

use serde::{Deserialize, Serialize};

use crate::SoundClass;

/// Что нужно от сессии для трендов.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrendSession {
    /// Начало записи (или время добавления, если начало неизвестно).
    pub start_unix_ms: i64,
    /// Смещение местного времени, в котором считать календарные дни.
    pub utc_offset_min: i32,
    pub duration_ms: u64,
    /// Время по собачьим классам в порядке [`SoundClass::DOG`].
    pub dog_class_ms: [u64; 4],
    pub episodes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrendPeriod {
    Day,
    /// Неделя с понедельника.
    Week,
}

/// Календарная дата.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendBucket {
    /// Первый день периода (для недели — понедельник).
    pub first_day: Date,
    pub sessions: u32,
    pub duration_ms: u64,
    pub dog_ms: u64,
    /// В порядке [`SoundClass::DOG`].
    pub dog_class_ms: [u64; 4],
    pub episodes: u32,
    /// Доля голоса от времени записи, %.
    pub dog_percent: f32,
}

/// Группирует сессии по периодам; только периоды, где были записи, от старых к новым.
pub fn trends(sessions: &[TrendSession], period: TrendPeriod) -> Vec<TrendBucket> {
    let mut keyed: Vec<(i64, &TrendSession)> = sessions
        .iter()
        .map(|s| {
            let day = (s.start_unix_ms + s.utc_offset_min as i64 * 60_000).div_euclid(DAY_MS);
            let key = match period {
                TrendPeriod::Day => day,
                // 1970-01-01 — четверг: +3 сдвигает начало к понедельнику.
                TrendPeriod::Week => day - (day + 3).rem_euclid(7),
            };
            (key, s)
        })
        .collect();
    keyed.sort_by_key(|(k, _)| *k);

    let mut out: Vec<(i64, TrendBucket)> = Vec::new();
    for (key, s) in keyed {
        let b = match out.last_mut() {
            Some((k, b)) if *k == key => b,
            _ => {
                out.push((
                    key,
                    TrendBucket {
                        first_day: civil_from_days(key),
                        sessions: 0,
                        duration_ms: 0,
                        dog_ms: 0,
                        dog_class_ms: [0; 4],
                        episodes: 0,
                        dog_percent: 0.0,
                    },
                ));
                &mut out.last_mut().unwrap().1
            }
        };
        b.sessions += 1;
        b.duration_ms += s.duration_ms;
        for (acc, ms) in b.dog_class_ms.iter_mut().zip(s.dog_class_ms) {
            *acc += ms;
        }
        b.dog_ms = b.dog_class_ms.iter().sum();
        b.episodes += s.episodes;
    }
    out.into_iter()
        .map(|(_, mut b)| {
            b.dog_percent = if b.duration_ms == 0 {
                0.0
            } else {
                b.dog_ms as f32 * 100.0 / b.duration_ms as f32
            };
            b
        })
        .collect()
}

const DAY_MS: i64 = 86_400_000;

// Порядок полей `dog_class_ms` совпадает с `SoundClass::DOG`.
const _: () = assert!(SoundClass::DOG.len() == 4);

/// Дата по дням от 1970-01-01 (алгоритм Говарда Хиннанта, обратный `days_from_civil`).
pub fn civil_from_days(z: i64) -> Date {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + if month <= 2 { 1 } else { 0 }) as i32;
    Date { year, month, day }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOSCOW: i32 = 180;

    /// Сессия, начатая в `hh:mm` местного (UTC+3) времени даты `y-m-d`.
    fn session(y: i64, m: i64, d: i64, hh: i64, hours: u64, bark_min: u64) -> TrendSession {
        let local_ms = (crate::start::days_from_civil(y, m, d) * 24 + hh) * 3_600_000;
        TrendSession {
            start_unix_ms: local_ms - MOSCOW as i64 * 60_000,
            utc_offset_min: MOSCOW,
            duration_ms: hours * 3_600_000,
            dog_class_ms: [bark_min * 60_000, 60_000, 0, 0],
            episodes: 3,
        }
    }

    #[test]
    fn dates_roundtrip() {
        for days in [-1, 0, 11_017, 20_765, 20_766] {
            let d = civil_from_days(days);
            assert_eq!(
                crate::start::days_from_civil(d.year as i64, d.month as i64, d.day as i64),
                days
            );
        }
        assert_eq!(
            civil_from_days(0),
            Date {
                year: 1970,
                month: 1,
                day: 1
            }
        );
    }

    #[test]
    fn groups_by_local_day() {
        // 2 ноября в 01:00 по Москве — это ещё 1 ноября по UTC, но день — 2 ноября.
        let s = [
            session(2026, 11, 2, 1, 2, 9),
            session(2026, 11, 2, 18, 2, 3),
            session(2026, 11, 4, 9, 4, 0),
        ];
        let t = trends(&s, TrendPeriod::Day);
        assert_eq!(t.len(), 2);
        assert_eq!(
            t[0].first_day,
            Date {
                year: 2026,
                month: 11,
                day: 2
            }
        );
        assert_eq!(t[0].sessions, 2);
        assert_eq!(t[0].duration_ms, 4 * 3_600_000);
        assert_eq!(t[0].dog_ms, 14 * 60_000);
        assert!((t[0].dog_percent - 14.0 / 240.0 * 100.0).abs() < 1e-4);
        assert_eq!(t[1].episodes, 3);
    }

    #[test]
    fn weeks_start_on_monday() {
        // 2026-11-01 — воскресенье, 2026-11-02 — понедельник.
        let s = [
            session(2026, 11, 8, 10, 1, 1),
            session(2026, 11, 1, 10, 1, 1),
            session(2026, 11, 2, 10, 1, 1),
            session(2026, 11, 7, 23, 1, 1),
        ];
        let t = trends(&s, TrendPeriod::Week);
        let firsts: Vec<_> = t
            .iter()
            .map(|b| (b.first_day.month, b.first_day.day, b.sessions))
            .collect();
        assert_eq!(firsts, [(10, 26, 1), (11, 2, 3)]);
    }

    #[test]
    fn empty() {
        assert!(trends(&[], TrendPeriod::Week).is_empty());
    }
}
