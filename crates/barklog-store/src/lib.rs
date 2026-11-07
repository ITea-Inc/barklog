//! Хранение проанализированных сессий в SQLite.
//!
//! Полный отчёт лежит JSON-ом, а для списка сессий продублированы ключевые
//! цифры в колонках — список строится без разбора JSON.

use std::path::Path;

use barklog_core::compact::{decode_windows, encode_windows};
use barklog_core::{Analysis, EpisodeLabel, Label, Report, SoundClass, StartSource, StartTime};
use half::f16;
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
    // 3: личная модель — окна (4 байта на окно), эмбеддинги, разметка, обученные модели.
    "ALTER TABLE sessions ADD COLUMN windows BLOB;
    CREATE TABLE embeddings (
        session_id  INTEGER NOT NULL,
        window      INTEGER NOT NULL,
        vec         BLOB    NOT NULL,
        PRIMARY KEY (session_id, window)
    );
    CREATE TABLE labels (
        session_id  INTEGER NOT NULL,
        start_ms    INTEGER NOT NULL,
        end_ms      INTEGER NOT NULL,
        label       TEXT    NOT NULL,
        PRIMARY KEY (session_id, start_ms)
    );
    CREATE TABLE models (
        name           TEXT PRIMARY KEY,
        data           TEXT    NOT NULL,
        trained_at_ms  INTEGER NOT NULL
    );",
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
    /// `analysis` — результаты по окнам и эмбеддинги: нужны для разметки и личной модели.
    pub fn insert(
        &self,
        report: &Report,
        created_at_ms: i64,
        source_uri: Option<&str>,
        analysis: Option<&Analysis>,
    ) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let id = self.insert_row(report, created_at_ms, source_uri, analysis)?;
        if let Some(a) = analysis {
            let mut stmt =
                tx.prepare("INSERT INTO embeddings (session_id, window, vec) VALUES (?1, ?2, ?3)")?;
            for (window, vec) in &a.embeddings {
                stmt.execute(params![id, window, f16_bytes(vec)])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    fn insert_row(
        &self,
        report: &Report,
        created_at_ms: i64,
        source_uri: Option<&str>,
        analysis: Option<&Analysis>,
    ) -> Result<i64> {
        let s = &report.stats;
        let class = |c: SoundClass| s.classes[c.index()].time_ms as i64;
        let st = report.started_at.as_ref();
        self.conn.execute(
            "INSERT INTO sessions (created_at_ms, source_name, started_at_ms, utc_offset_min,
                start_source, duration_ms, dog_ms, bark_ms, whine_ms, howl_ms, growl_ms,
                episodes, report_json, source_uri, windows)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                analysis.map(|a| encode_windows(&a.windows)),
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM embeddings WHERE session_id = ?1", [id])?;
        tx.execute("DELETE FROM labels WHERE session_id = ?1", [id])?;
        let n = tx.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Перезаписывает отчёт (после разметки или обучения личной модели).
    pub fn update_report(&self, id: i64, report: &Report) -> Result<()> {
        let s = &report.stats;
        let class = |c: SoundClass| s.classes[c.index()].time_ms as i64;
        self.conn.execute(
            "UPDATE sessions SET dog_ms = ?2, bark_ms = ?3, whine_ms = ?4, howl_ms = ?5,
                growl_ms = ?6, episodes = ?7, report_json = ?8 WHERE id = ?1",
            params![
                id,
                s.dog_time_ms as i64,
                class(SoundClass::Bark),
                class(SoundClass::Whine),
                class(SoundClass::Howl),
                class(SoundClass::Growl),
                s.episodes,
                serde_json::to_string(report)?,
            ],
        )?;
        Ok(())
    }

    /// Результаты по окнам и эмбеддинги сессии; `None`, если сессия создана до их хранения.
    pub fn analysis(&self, id: i64) -> Result<Option<Analysis>> {
        let row: Option<(Option<Vec<u8>>, i64)> = self
            .conn
            .query_row(
                "SELECT windows, duration_ms FROM sessions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((Some(bytes), duration_ms)) = row else {
            return Ok(None);
        };
        let mut stmt = self
            .conn
            .prepare("SELECT window, vec FROM embeddings WHERE session_id = ?1 ORDER BY window")?;
        let embeddings = stmt
            .query_map([id], |r| {
                Ok((r.get::<_, u32>(0)?, f16_vec(&r.get::<_, Vec<u8>>(1)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Analysis {
            duration_ms: duration_ms as u64,
            windows: decode_windows(&bytes),
            embeddings,
        }))
    }

    /// Сохранены ли для сессии окна (можно ли её размечать).
    pub fn has_analysis(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT windows IS NOT NULL FROM sessions WHERE id = ?1",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// Сессии, для которых сохранены окна (их можно пересчитывать).
    pub fn sessions_with_analysis(&self) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM sessions WHERE windows IS NOT NULL ORDER BY id")?;
        Ok(stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn labels(&self, id: i64) -> Result<Vec<EpisodeLabel>> {
        let mut stmt = self.conn.prepare(
            "SELECT start_ms, end_ms, label FROM labels WHERE session_id = ?1 ORDER BY start_ms",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (start, end, key) = row?;
            if let Some(label) = Label::from_key(&key) {
                out.push(EpisodeLabel {
                    start_ms: start as u64,
                    end_ms: end as u64,
                    label,
                });
            }
        }
        Ok(out)
    }

    /// Ставит ответ на эпизод или снимает его (`label = None`).
    pub fn set_label(
        &self,
        id: i64,
        start_ms: u64,
        end_ms: u64,
        label: Option<Label>,
    ) -> Result<()> {
        match label {
            Some(l) => self.conn.execute(
                "INSERT INTO labels (session_id, start_ms, end_ms, label) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (session_id, start_ms) DO UPDATE SET end_ms = ?3, label = ?4",
                params![id, start_ms as i64, end_ms as i64, l.key()],
            )?,
            None => self.conn.execute(
                "DELETE FROM labels WHERE session_id = ?1 AND start_ms = ?2",
                params![id, start_ms as i64],
            )?,
        };
        Ok(())
    }

    /// Сколько всего отмечено эпизодов (по всем сессиям).
    pub fn label_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM labels", [], |r| r.get(0))?)
    }

    pub fn save_model(&self, name: &str, json: &str, trained_at_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO models (name, data, trained_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET data = ?2, trained_at_ms = ?3",
            params![name, json, trained_at_ms],
        )?;
        Ok(())
    }

    /// (JSON модели, когда обучена).
    pub fn load_model(&self, name: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT data, trained_at_ms FROM models WHERE name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn delete_model(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM models WHERE name = ?1", [name])?;
        Ok(())
    }
}

fn f16_bytes(v: &[f16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn f16_vec(bytes: &[u8]) -> Vec<f16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f16::from_le_bytes(*b))
        .collect()
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
            embeddings: Vec::new(),
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
            .insert(
                &report("a.m4a", Some(early), 10),
                50,
                Some("content://a"),
                None,
            )
            .unwrap();
        let b = store
            .insert(&report("b.m4a", Some(late), 20), 40, None, None)
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
    fn stores_windows_embeddings_labels_and_models() {
        use barklog_core::{ClassScores, WindowResult};
        let store = Store::open_in_memory().unwrap();
        let windows: Vec<WindowResult> = (0..10)
            .map(|i| WindowResult {
                start_ms: i * 480,
                class: if i < 4 {
                    SoundClass::Bark
                } else {
                    SoundClass::Silence
                },
                confidence: 0.9,
                rms_db: -30.0,
                peak_db: -20.0,
                gated: i >= 4,
                scores: ClassScores::default(),
            })
            .collect();
        let analysis = Analysis {
            duration_ms: 4800,
            windows,
            embeddings: vec![
                (1, vec![f16::from_f32(0.5); 1024]),
                (3, vec![f16::ONE; 1024]),
            ],
        };
        let r = report("live", None, 4);
        let id = store.insert(&r, 1, None, Some(&analysis)).unwrap();
        let back = store.analysis(id).unwrap().unwrap();
        assert_eq!(back.windows.len(), 10);
        assert_eq!(back.windows[2].class, SoundClass::Bark);
        assert_eq!(back.embeddings.len(), 2);
        assert_eq!(back.embeddings[0].1[7], f16::from_f32(0.5));
        assert_eq!(store.sessions_with_analysis().unwrap(), vec![id]);

        store.set_label(id, 0, 960, Some(Label::NotDog)).unwrap();
        store.set_label(id, 0, 1440, Some(Label::Whine)).unwrap(); // заменяет
        assert_eq!(store.labels(id).unwrap().len(), 1);
        assert_eq!(store.labels(id).unwrap()[0].label, Label::Whine);
        assert_eq!(store.label_count().unwrap(), 1);
        store.set_label(id, 0, 0, None).unwrap();
        assert!(store.labels(id).unwrap().is_empty());

        store.save_model("personal", "{}", 5).unwrap();
        store.save_model("personal", "{\"v\":2}", 6).unwrap();
        assert_eq!(
            store.load_model("personal").unwrap(),
            Some(("{\"v\":2}".into(), 6))
        );
        store.delete_model("personal").unwrap();
        assert_eq!(store.load_model("personal").unwrap(), None);

        assert!(store.delete(id).unwrap());
        assert!(store.analysis(id).unwrap().is_none());
    }

    #[test]
    fn migrates_version_1_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        let store = Store::init(conn).unwrap();
        let id = store
            .insert(&report("old.m4a", None, 3), 1, Some("content://x"), None)
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
            store
                .insert(&report("x.wav", None, 5), 1, None, None)
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.list().unwrap()[0].started_at, None);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
