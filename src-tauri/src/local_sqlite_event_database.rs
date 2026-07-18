//! Local SQLite persistence for append-only raw evidence and derived records.
//!
//! The database is the reliability boundary: capture writes compact normalized
//! events first, while semantic processing may be retried or regenerated. FTS5
//! is maintained from raw-event triggers so search remains useful without AI.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: String,
    pub timestamp_ns: i64,
    pub event_type: String,
    pub source: String,
    pub app_name: Option<String>,
    pub executable_path: Option<String>,
    pub process_id: Option<u32>,
    pub window_title: Option<String>,
    pub element_name: Option<String>,
    pub text: Option<String>,
    pub file_path: Option<String>,
    pub metadata_json: String,
    pub privacy_class: String,
    pub confidence: f32,
    pub created_at: String,
}

pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open() -> Result<Self> {
        Self::from_connection(Connection::open("chronicle.db")?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(include_str!("../migrations/001_initial.sql"))?;
        Ok(Self { connection })
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn count_events(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
    }

    pub fn insert_event(&self, event: &RawEvent) -> Result<()> {
        self.connection.execute(
            "INSERT INTO raw_events (id, timestamp_ns, event_type, source, app_name, executable_path, process_id, window_title, element_name, text, file_path, metadata_json, privacy_class, confidence, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![event.id, event.timestamp_ns, event.event_type, event.source, event.app_name, event.executable_path, event.process_id, event.window_title, event.element_name, event.text, event.file_path, event.metadata_json, event.privacy_class, event.confidence, event.created_at],
        )?;
        Ok(())
    }

    pub fn recent_events(&self, limit: u32, query: Option<&str>) -> Result<Vec<RawEvent>> {
        let mut statement = if query.is_some() {
            self.connection.prepare("SELECT r.id, r.timestamp_ns, r.event_type, r.source, r.app_name, r.executable_path, r.process_id, r.window_title, r.element_name, r.text, r.file_path, r.metadata_json, r.privacy_class, r.confidence, r.created_at FROM raw_events r JOIN raw_events_fts f ON f.rowid = r.rowid WHERE raw_events_fts MATCH ?1 ORDER BY r.timestamp_ns DESC LIMIT ?2")?
        } else {
            self.connection.prepare("SELECT id, timestamp_ns, event_type, source, app_name, executable_path, process_id, window_title, element_name, text, file_path, metadata_json, privacy_class, confidence, created_at FROM raw_events ORDER BY timestamp_ns DESC LIMIT ?1")?
        };
        let rows = if let Some(query) = query {
            statement.query_map(params![query, limit], map_event)?
        } else {
            statement.query_map(params![limit], map_event)?
        };
        rows.collect()
    }

    pub fn delete_all(&self) -> Result<()> {
        self.connection.execute_batch(
            "DELETE FROM processing_queue; DELETE FROM semantic_events; DELETE FROM raw_events;",
        )
    }

    pub fn save_setting(&self, key: &str, value_json: &str) -> Result<()> {
        self.connection.execute("INSERT INTO app_settings(key, value_json, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at=excluded.updated_at", params![key, value_json, Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub fn load_setting(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn export_json(&self) -> Result<String> {
        let events = self.recent_events(100_000, None)?;
        serde_json::to_string_pretty(&HashMap::from([("events", events)]))
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
    }

    pub fn seed_ready_event(&self) -> Result<()> {
        if self.count_events()? == 0 {
            self.insert_event(&RawEvent {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or_default(),
                event_type: "system_ready".into(),
                source: "chronicle".into(),
                app_name: Some("Chronicle".into()),
                executable_path: None,
                process_id: None,
                window_title: Some("Desktop shell initialized".into()),
                element_name: None,
                text: None,
                file_path: None,
                metadata_json: "{}".into(),
                privacy_class: "safe".into(),
                confidence: 1.0,
                created_at: Utc::now().to_rfc3339(),
            })?;
        }
        Ok(())
    }
}

fn map_event(row: &rusqlite::Row<'_>) -> Result<RawEvent> {
    Ok(RawEvent {
        id: row.get(0)?,
        timestamp_ns: row.get(1)?,
        event_type: row.get(2)?,
        source: row.get(3)?,
        app_name: row.get(4)?,
        executable_path: row.get(5)?,
        process_id: row.get(6)?,
        window_title: row.get(7)?,
        element_name: row.get(8)?,
        text: row.get(9)?,
        file_path: row.get(10)?,
        metadata_json: row.get(11)?,
        privacy_class: row.get(12)?,
        confidence: row.get(13)?,
        created_at: row.get(14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, timestamp_ns: i64, title: &str, text: Option<&str>) -> RawEvent {
        RawEvent {
            id: id.into(),
            timestamp_ns,
            event_type: "window_focused".into(),
            source: "test".into(),
            app_name: Some("Test App".into()),
            executable_path: None,
            process_id: Some(42),
            window_title: Some(title.into()),
            element_name: None,
            text: text.map(str::to_owned),
            file_path: None,
            metadata_json: "{}".into(),
            privacy_class: "safe".into(),
            confidence: 1.0,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn creates_schema_and_starts_empty() {
        let database = Database::in_memory().unwrap();
        assert_eq!(database.count_events().unwrap(), 0);
    }

    #[test]
    fn inserts_and_returns_newest_events_first() {
        let database = Database::in_memory().unwrap();
        database
            .insert_event(&event("old", 10, "Older", None))
            .unwrap();
        database
            .insert_event(&event("new", 20, "Newer", None))
            .unwrap();
        let events = database.recent_events(10, None).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "old"]
        );
    }

    #[test]
    fn fts_search_finds_window_title_and_text() {
        let database = Database::in_memory().unwrap();
        database
            .insert_event(&event(
                "rust",
                10,
                "Rust compiler",
                Some("cargo test passed"),
            ))
            .unwrap();
        database
            .insert_event(&event(
                "notes",
                20,
                "Meeting notes",
                Some("project planning"),
            ))
            .unwrap();
        let results = database.recent_events(10, Some("compiler")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "rust");
    }

    #[test]
    fn seed_is_idempotent() {
        let database = Database::in_memory().unwrap();
        database.seed_ready_event().unwrap();
        database.seed_ready_event().unwrap();
        assert_eq!(database.count_events().unwrap(), 1);
    }

    #[test]
    fn delete_all_removes_raw_and_derived_records() {
        let database = Database::in_memory().unwrap();
        database
            .insert_event(&event("one", 10, "One", None))
            .unwrap();
        database.delete_all().unwrap();
        assert_eq!(database.count_events().unwrap(), 0);
        assert!(database.recent_events(10, Some("One")).unwrap().is_empty());
    }
}
