//! Хранение проанализированных сессий в SQLite.
//!
//! Полный отчёт лежит JSON-ом, а для списка сессий продублированы ключевые
//! цифры в колонках — список строится без разбора JSON.

use std::path::Path;

use barklog_core::compact::{decode_windows, encode_windows};
use barklog_core::{
    Analysis, Embedding, EpisodeLabel, Label, Report, SoundClass, StartSource, StartTime,
};
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
    /// Заметка хозяина к сессии («оставил конг», «ушёл на 6 ч»).
    pub note: Option<String>,
    pub dog_id: i64,
}

/// Собака: у каждой свои записи, отметки и личная модель.
#[derive(Debug, Clone, PartialEq)]
pub struct Dog {
    pub id: i64,
    pub name: String,
    pub sessions: u32,
}

/// Собака, к которой при обновлении отнесены все прежние записи.
pub const FIRST_DOG: i64 = 1;

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
    // 4: настройки пользователя (JSON по имени).
    "CREATE TABLE settings (
        name  TEXT PRIMARY KEY,
        data  TEXT NOT NULL
    );",
    // 5: заметка к сессии.
    "ALTER TABLE sessions ADD COLUMN note TEXT;",
    // 6: несколько собак. Прежние записи и личная модель — у первой.
    "CREATE TABLE dogs (
        id             INTEGER PRIMARY KEY,
        name           TEXT    NOT NULL,
        created_at_ms  INTEGER NOT NULL
    );
    INSERT INTO dogs (id, name, created_at_ms) VALUES (1, 'Моя собака', 0);
    ALTER TABLE sessions ADD COLUMN dog_id INTEGER NOT NULL DEFAULT 1;
    CREATE INDEX sessions_by_dog ON sessions (dog_id);
    UPDATE models SET name = 'personal:1' WHERE name = 'personal';",
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
    /// Запись первой собаки — см. [`Self::insert_for_dog`].
    pub fn insert(
        &self,
        report: &Report,
        created_at_ms: i64,
        source_uri: Option<&str>,
        analysis: Option<&Analysis>,
    ) -> Result<i64> {
        self.insert_for_dog(FIRST_DOG, report, created_at_ms, source_uri, analysis)
    }

    pub fn insert_for_dog(
        &self,
        dog_id: i64,
        report: &Report,
        created_at_ms: i64,
        source_uri: Option<&str>,
        analysis: Option<&Analysis>,
    ) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let id = self.insert_row(report, created_at_ms, source_uri, analysis)?;
        tx.execute(
            "UPDATE sessions SET dog_id = ?2 WHERE id = ?1",
            params![id, dog_id],
        )?;
        if let Some(a) = analysis {
            let mut stmt =
                tx.prepare("INSERT INTO embeddings (session_id, window, vec) VALUES (?1, ?2, ?3)")?;
            for (window, vec) in &a.embeddings {
                stmt.execute(params![id, window, vec.to_bytes()])?;
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

    /// Сессии от новых к старым (по времени начала записи, если оно известно);
    /// `dog` — только записи этой собаки.
    pub fn list(&self, dog: Option<i64>) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at_ms, source_name, started_at_ms, utc_offset_min, start_source,
                    duration_ms, dog_ms, bark_ms, whine_ms, howl_ms, growl_ms, episodes, note, dog_id
             FROM sessions WHERE ?1 IS NULL OR dog_id = ?1
             ORDER BY coalesce(started_at_ms, created_at_ms) DESC, id DESC",
        )?;
        let rows = stmt.query_map([dog], |r| {
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
                note: r.get(13)?,
                dog_id: r.get(14)?,
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

    pub fn note(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT note FROM sessions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    /// `None` — убрать заметку. `false`, если такой сессии нет.
    pub fn set_note(&self, id: i64, note: Option<&str>) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE sessions SET note = ?2 WHERE id = ?1",
            params![id, note],
        )?;
        Ok(n > 0)
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
        // Вектор старого формата (f16) читается тоже; повреждённый просто пропускаем.
        let rows: Vec<(u32, Vec<u8>)> = stmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let embeddings = rows
            .into_iter()
            .filter_map(|(w, b)| Embedding::from_bytes(&b).map(|e| (w, e)))
            .collect();
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

    /// Сессии, для которых сохранены окна (их можно пересчитывать); `dog` — только этой собаки.
    pub fn sessions_with_analysis(&self, dog: Option<i64>) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM sessions WHERE windows IS NOT NULL AND (?1 IS NULL OR dog_id = ?1)
             ORDER BY id",
        )?;
        Ok(stmt
            .query_map([dog], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Чья это запись.
    pub fn session_dog(&self, id: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT dog_id FROM sessions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Переносит запись к другой собаке. `false`, если такой записи или собаки нет.
    pub fn set_session_dog(&self, id: i64, dog_id: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE sessions SET dog_id = ?2
             WHERE id = ?1 AND EXISTS (SELECT 1 FROM dogs WHERE id = ?2)",
            params![id, dog_id],
        )?;
        Ok(n > 0)
    }

    /// Собаки в порядке добавления, с числом записей.
    pub fn dogs(&self) -> Result<Vec<Dog>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.name, (SELECT count(*) FROM sessions s WHERE s.dog_id = d.id)
             FROM dogs d ORDER BY d.id",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Dog {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    sessions: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_dog(&self, name: &str, created_at_ms: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO dogs (name, created_at_ms) VALUES (?1, ?2)",
            params![name, created_at_ms],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_dog(&self, id: i64, name: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("UPDATE dogs SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(n > 0)
    }

    /// Удаляет собаку без записей вместе с её личной моделью. Собаку с записями
    /// и последнюю собаку не удаляет — тогда `false`.
    pub fn delete_dog(&self, id: i64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "DELETE FROM dogs WHERE id = ?1
               AND NOT EXISTS (SELECT 1 FROM sessions WHERE dog_id = ?1)
               AND (SELECT count(*) FROM dogs) > 1",
            [id],
        )?;
        if n > 0 {
            tx.execute(
                "DELETE FROM models WHERE name = ?1",
                [personal_model_name(id)],
            )?;
        }
        tx.commit()?;
        Ok(n > 0)
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

    /// Сколько отмечено эпизодов — во всех записях или у одной собаки.
    pub fn label_count(&self, dog: Option<i64>) -> Result<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM labels l JOIN sessions s ON s.id = l.session_id
             WHERE ?1 IS NULL OR s.dog_id = ?1",
            [dog],
            |r| r.get(0),
        )?)
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

    pub fn save_setting(&self, name: &str, json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (name, data) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET data = ?2",
            params![name, json],
        )?;
        Ok(())
    }

    pub fn load_setting(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT data FROM settings WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }
}

/// Имя личной модели собаки в таблице моделей.
pub fn personal_model_name(dog_id: i64) -> String {
    format!("personal:{dog_id}")
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

        let list = store.list(None).unwrap();
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
        assert_eq!(store.list(None).unwrap().len(), 1);
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
                (1, Embedding::quantize(&[0.5; 1024])),
                (3, Embedding::quantize(&[1.0; 1024])),
            ],
        };
        let r = report("live", None, 4);
        let id = store.insert(&r, 1, None, Some(&analysis)).unwrap();
        let back = store.analysis(id).unwrap().unwrap();
        assert_eq!(back.windows.len(), 10);
        assert_eq!(back.windows[2].class, SoundClass::Bark);
        assert_eq!(back.embeddings.len(), 2);
        assert!((back.embeddings[0].1.to_f32()[7] - 0.5).abs() < 1e-5);
        assert_eq!(store.sessions_with_analysis(None).unwrap(), vec![id]);

        store.set_label(id, 0, 960, Some(Label::NotDog)).unwrap();
        store.set_label(id, 0, 1440, Some(Label::Whine)).unwrap(); // заменяет
        assert_eq!(store.labels(id).unwrap().len(), 1);
        assert_eq!(store.labels(id).unwrap()[0].label, Label::Whine);
        assert_eq!(store.label_count(None).unwrap(), 1);
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

        assert_eq!(store.note(id).unwrap(), None);
        assert!(store.set_note(id, Some("оставил конг")).unwrap());
        assert_eq!(store.note(id).unwrap().as_deref(), Some("оставил конг"));
        assert_eq!(
            store.list(None).unwrap()[0].note.as_deref(),
            Some("оставил конг")
        );
        store.set_note(id, None).unwrap();
        assert_eq!(store.note(id).unwrap(), None);
        assert!(!store.set_note(id + 100, Some("x")).unwrap());

        assert_eq!(store.load_setting("sensitivity").unwrap(), None);
        store.save_setting("sensitivity", "{}").unwrap();
        store
            .save_setting("sensitivity", "{\"dog_min\":0.3}")
            .unwrap();
        assert_eq!(
            store.load_setting("sensitivity").unwrap().as_deref(),
            Some("{\"dog_min\":0.3}")
        );

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
        assert_eq!(store.list(None).unwrap().len(), 1);
        assert_eq!(store.list(None).unwrap()[0].started_at, None);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dogs_own_their_sessions_and_models() {
        // База версии 5 с обученной моделью — после миграции всё у первой собаки.
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute(
            "INSERT INTO models (name, data, trained_at_ms) VALUES ('personal', '{}', 7)",
            [],
        )
        .unwrap();
        let store = Store::init(conn).unwrap();
        assert_eq!(
            store.dogs().unwrap(),
            vec![Dog {
                id: FIRST_DOG,
                name: "Моя собака".into(),
                sessions: 0
            }]
        );
        assert!(
            store
                .load_model(&personal_model_name(FIRST_DOG))
                .unwrap()
                .is_some()
        );

        store
            .insert(&report("a.m4a", None, 1), 1, None, None)
            .unwrap();
        let rex = store.add_dog("Рекс", 2).unwrap();
        let second = store
            .insert_for_dog(rex, &report("b.m4a", None, 2), 2, None, None)
            .unwrap();
        assert_eq!(store.list(Some(FIRST_DOG)).unwrap().len(), 1);
        assert_eq!(store.list(Some(rex)).unwrap()[0].id, second);
        assert_eq!(store.list(None).unwrap().len(), 2);
        assert_eq!(store.session_dog(second).unwrap(), Some(rex));

        // С записями собаку не удалить; после переноса записи — можно.
        assert!(!store.delete_dog(rex).unwrap());
        assert!(store.set_session_dog(second, FIRST_DOG).unwrap());
        assert!(!store.set_session_dog(second, 999).unwrap());
        assert!(store.rename_dog(rex, "Рексик").unwrap());
        store
            .save_model(&personal_model_name(rex), "{}", 3)
            .unwrap();
        assert!(store.delete_dog(rex).unwrap());
        assert!(
            store
                .load_model(&personal_model_name(rex))
                .unwrap()
                .is_none()
        );
        // Последнюю собаку не удалить.
        assert!(!store.delete_dog(FIRST_DOG).unwrap());
        assert_eq!(store.dogs().unwrap()[0].sessions, 2);
    }
}
