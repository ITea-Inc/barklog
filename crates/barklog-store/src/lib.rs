//! Хранение проанализированных сессий в SQLite.
//!
//! Полный отчёт лежит JSON-ом, а для списка сессий продублированы ключевые
//! цифры в колонках — список строится без разбора JSON.

use std::path::Path;

use barklog_core::{Report, SoundClass, StartSource, StartTime};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("ошибка базы данных: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("повреждённый отчёт: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Строка списка сессий.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: i64,
    pub created_at_ms: i64,
    pub source_name: String,
    pub started_at: Option<StartTime>,
    pub duration_ms: u64,
    pub dog_ms: u64,
    pub bark_ms: u64,
    pub whine_ms: u64,
    pub howl_ms: u64,
    pub growl_ms: u64,
    pub episodes: u32,
}

impl SessionSummary {
    pub fn dog_percent(&self) -> f32 {
        if self.duration_ms == 0 {
            0.0
        } else {
            self.dog_ms as f32 * 100.0 / self.duration_ms as f32
        }
    }
}

/// Миграции по порядку; номер версии схемы = число применённых миграций.
const MIGRATIONS: &[&str] = &[
    // 1: сессии.
    "CREATE TABLE sessions (
        id              INTEGER PRIMARY KEY,
        created_at_ms   INTEGER NOT NULL,
        source_name     TEXT    NOT NULL,
        started_at_ms   INTEGER,
        utc_offset_min  INTEGER,
        start_source    TEXT,
        duration_ms     INTEGER NOT NULL,
        dog_ms          INTEGER NOT NULL,
        bark_ms         INTEGER NOT NULL,
        whine_ms        INTEGER NOT NULL,
        howl_ms         INTEGER NOT NULL,
        growl_ms        INTEGER NOT NULL,
        episodes        INTEGER NOT NULL,
        report_json     TEXT    NOT NULL
    );
    CREATE INDEX sessions_by_start ON sessions (coalesce(started_at_ms, created_at_ms));",
    // 2: ссылка на исходный файл (content:// на Android) — для прослушивания эпизодов.
    "ALTER TABLE sessions ADD COLUMN source_uri TEXT;",
];

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", i as i64 + 1)?;
        }
        Ok(Self { conn })
    }

    /// `source_uri` — откуда можно заново открыть запись (для прослушивания), если это возможно.
    pub fn insert(
        &self,
        report: &Report,
        created_at_ms: i64,
        source_uri: Option<&str>,
    ) -> Result<i64> {
        let s = &report.stats;
        let class = |c: SoundClass| s.classes[c.index()].time_ms as i64;
        let st = report.started_at.as_ref();
        self.conn.execute(
            "INSERT INTO sessions (created_at_ms, source_name, started_at_ms, utc_offset_min,
                start_source, duration_ms, dog_ms, bark_ms, whine_ms, howl_ms, growl_ms,
                episodes, report_json, source_uri)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                created_at_ms,
                report.source.clone().unwrap_or_default(),
                st.map(|s| s.unix_ms),
                st.map(|s| s.utc_offset_min),
                st.map(|s| source_key(s.source)),
                s.duration_ms as i64,
                s.dog_time_ms as i64,
                class(SoundClass::Bark),
                class(SoundClass::Whine),
                class(SoundClass::Howl),
                class(SoundClass::Growl),
                s.episodes,
                serde_json::to_string(report)?,
                source_uri,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Сессии от новых к старым (по времени начала записи, если оно известно).
    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at_ms, source_name, started_at_ms, utc_offset_min, start_source,
                    duration_ms, dog_ms, bark_ms, whine_ms, howl_ms, growl_ms, episodes
             FROM sessions ORDER BY coalesce(started_at_ms, created_at_ms) DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let started_at = match (r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i32>>(4)?) {
                (Some(unix_ms), Some(utc_offset_min)) => Some(StartTime {
                    unix_ms,
                    utc_offset_min,
                    source: parse_source(r.get::<_, Option<String>>(5)?.as_deref()),
                }),
                _ => None,
            };
            Ok(SessionSummary {
                id: r.get(0)?,
                created_at_ms: r.get(1)?,
                source_name: r.get(2)?,
                started_at,
                duration_ms: r.get::<_, i64>(6)? as u64,
                dog_ms: r.get::<_, i64>(7)? as u64,
                bark_ms: r.get::<_, i64>(8)? as u64,
                whine_ms: r.get::<_, i64>(9)? as u64,
                howl_ms: r.get::<_, i64>(10)? as u64,
                growl_ms: r.get::<_, i64>(11)? as u64,
                episodes: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Report>> {
        Ok(self.get_with_source(id)?.map(|(r, _)| r))
    }

    /// Отчёт вместе со ссылкой на исходный файл.
    pub fn get_with_source(&self, id: i64) -> Result<Option<(Report, Option<String>)>> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT report_json, source_uri FROM sessions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(json, uri)| Ok((serde_json::from_str(&json)?, uri)))
            .transpose()
    }

    /// `true`, если сессия была и удалена.
    pub fn delete(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?
            > 0)
    }
}

fn source_key(s: StartSource) -> &'static str {
    match s {
        StartSource::Manual => "manual",
        StartSource::FileName => "file_name",
        StartSource::FileModified => "file_modified",
        StartSource::Recorded => "recorded",
    }
}

fn parse_source(s: Option<&str>) -> StartSource {
    match s {
        Some("manual") => StartSource::Manual,
        Some("file_name") => StartSource::FileName,
        Some("recorded") => StartSource::Recorded,
        _ => StartSource::FileModified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use barklog_core::{Analysis, ClassScores, ReportSettings, WindowResult, build_report};

    fn report(name: &str, start: Option<StartTime>, barks: usize) -> Report {
        let windows = (0..100)
            .map(|i| WindowResult {
                start_ms: i as u64 * 480,
                class: if i < barks {
                    SoundClass::Bark
                } else {
                    SoundClass::Silence
                },
                confidence: 0.9,
                rms_db: -30.0,
                peak_db: -20.0,
                gated: false,
                scores: ClassScores::default(),
            })
            .collect();
        let a = Analysis {
            duration_ms: 48_000,
            windows,
        };
        build_report(&a, &ReportSettings::default(), start, Some(name.into()))
    }

    #[test]
    fn insert_list_get_delete() {
        let store = Store::open_in_memory().unwrap();
        let early = StartTime {
            unix_ms: 1_000,
            utc_offset_min: 180,
            source: StartSource::FileName,
        };
        let late = StartTime {
            unix_ms: 9_000,
            utc_offset_min: 180,
            source: StartSource::Manual,
        };
        let a = store
            .insert(&report("a.m4a", Some(early), 10), 50, Some("content://a"))
            .unwrap();
        let b = store
            .insert(&report("b.m4a", Some(late), 20), 40, None)
            .unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.iter().map(|s| s.id).collect::<Vec<_>>(), [b, a]);
        assert_eq!(list[1].source_name, "a.m4a");
        assert_eq!(list[1].bark_ms, 10 * 480);
        assert_eq!(list[1].dog_ms, 10 * 480);
        assert_eq!(list[1].started_at, Some(early));
        assert!((list[0].dog_percent() - 20.0).abs() < 1e-3);

        let full = store.get(a).unwrap().unwrap();
        assert_eq!(full.stats.dog_time_ms, 10 * 480);
        assert_eq!(full.started_at.unwrap().source, StartSource::FileName);
        assert_eq!(
            store.get_with_source(a).unwrap().unwrap().1.as_deref(),
            Some("content://a")
        );
        assert_eq!(store.get_with_source(b).unwrap().unwrap().1, None);

        assert!(store.delete(a).unwrap());
        assert!(!store.delete(a).unwrap());
        assert!(store.get(a).unwrap().is_none());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn migrates_version_1_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let store = Store::init(conn).unwrap();
        let id = store
            .insert(&report("old.m4a", None, 3), 1, Some("content://x"))
            .unwrap();
        assert_eq!(
            store.get_with_source(id).unwrap().unwrap().1.as_deref(),
            Some("content://x")
        );
    }

    #[test]
    fn reopening_keeps_data() {
        let path = std::env::temp_dir().join(format!("barklog-store-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            store.insert(&report("x.wav", None, 5), 1, None).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.list().unwrap()[0].started_at, None);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
