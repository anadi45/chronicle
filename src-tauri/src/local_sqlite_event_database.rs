//! Local SQLite persistence for append-only raw evidence and derived records.
//!
//! The database is the reliability boundary: capture writes compact normalized
//! events first, while semantic processing may be retried or regenerated. FTS5
//! is maintained from raw-event triggers so search remains useful without AI.

use crate::asynchronous_processing_queue::{QueueStatus, QueueTask, TaskType};
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
    pub window_handle: Option<u64>,
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
            "INSERT INTO raw_events (id, timestamp_ns, event_type, source, app_name, executable_path, process_id, window_handle, window_title, element_name, text, file_path, metadata_json, privacy_class, confidence, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![event.id, event.timestamp_ns, event.event_type, event.source, event.app_name, event.executable_path, event.process_id, event.window_handle, event.window_title, event.element_name, event.text, event.file_path, event.metadata_json, event.privacy_class, event.confidence, event.created_at],
        )?;
        Ok(())
    }

    pub fn recent_events(&self, limit: u32, query: Option<&str>) -> Result<Vec<RawEvent>> {
        let mut statement = if query.is_some() {
            self.connection.prepare("SELECT r.id, r.timestamp_ns, r.event_type, r.source, r.app_name, r.executable_path, r.process_id, r.window_handle, r.window_title, r.element_name, r.text, r.file_path, r.metadata_json, r.privacy_class, r.confidence, r.created_at FROM raw_events r JOIN raw_events_fts f ON f.rowid = r.rowid WHERE raw_events_fts MATCH ?1 ORDER BY r.timestamp_ns DESC LIMIT ?2")?
        } else {
            self.connection.prepare("SELECT id, timestamp_ns, event_type, source, app_name, executable_path, process_id, window_handle, window_title, element_name, text, file_path, metadata_json, privacy_class, confidence, created_at FROM raw_events ORDER BY timestamp_ns DESC LIMIT ?1")?
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

    pub fn enqueue_task(&self, task: &QueueTask) -> Result<()> {
        self.connection.execute("INSERT INTO processing_queue (id, raw_event_id, task_type, status, priority, attempts, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))", params![task.id, task.raw_event_id, serde_json::to_string(&task.task_type).unwrap_or_default().trim_matches('"'), serde_json::to_string(&task.status).unwrap_or_default().trim_matches('"'), task.priority, task.attempts])?;
        Ok(())
    }

    pub fn claim_next_task(&self) -> Result<Option<QueueTask>> {
        let transaction = self.connection.unchecked_transaction()?;
        let candidate = transaction.query_row("SELECT id, raw_event_id, task_type, attempts, priority FROM processing_queue WHERE status = 'pending' ORDER BY priority DESC, created_at ASC LIMIT 1", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, u32>(3)?, row.get::<_, i32>(4)?))).optional()?;
        let Some((id, raw_event_id, task_type, attempts, priority)) = candidate else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute("UPDATE processing_queue SET status = 'processing', started_at = datetime('now'), attempts = attempts + 1 WHERE id = ?1", [&id])?;
        transaction.commit()?;
        let task_type = match task_type.as_str() {
            "SemanticTextAnalysis" | "semantic_text_analysis" => TaskType::SemanticTextAnalysis,
            "SemanticImageAnalysis" | "semantic_image_analysis" => TaskType::SemanticImageAnalysis,
            _ => TaskType::EmbeddingGeneration,
        };
        Ok(Some(QueueTask {
            id,
            raw_event_id,
            task_type,
            status: QueueStatus::Processing,
            attempts: attempts + 1,
            priority,
        }))
    }

    pub fn finish_task(&self, task_id: &str) -> Result<()> {
        self.connection.execute("UPDATE processing_queue SET status = 'complete', completed_at = datetime('now') WHERE id = ?1", [task_id])?;
        Ok(())
    }
    pub fn fail_task(&self, task_id: &str, error: &str, retry: bool) -> Result<()> {
        let status = if retry { "pending" } else { "failed" };
        self.connection.execute("UPDATE processing_queue SET status = ?1, error = ?, completed_at = CASE WHEN ?1 = 'failed' THEN datetime('now') ELSE NULL END WHERE id = ?3", params![status, error, task_id])?;
        Ok(())
    }

    pub fn queue_counts(&self) -> Result<HashMap<String, i64>> {
        let mut counts = HashMap::new();
        let mut statement = self
            .connection
            .prepare("SELECT status, COUNT(*) FROM processing_queue GROUP BY status")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            counts.insert(status, count);
        }
        Ok(counts)
    }

    pub fn recover_stale_processing_tasks(&self, stale_minutes: u32) -> Result<usize> {
        let changed = self.connection.execute("UPDATE processing_queue SET status = 'pending', started_at = NULL, error = 'requeued after interrupted processing' WHERE status = 'processing' AND started_at < datetime('now', ?1)", [format!("-{} minutes", stale_minutes)])?;
        Ok(changed)
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
                window_handle: None,
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
        window_handle: row.get(7)?,
        window_title: row.get(8)?,
        element_name: row.get(9)?,
        text: row.get(10)?,
        file_path: row.get(11)?,
        metadata_json: row.get(12)?,
        privacy_class: row.get(13)?,
        confidence: row.get(14)?,
        created_at: row.get(15)?,
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
            window_handle: None,
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

    #[test]
    fn queue_claim_and_finish_round_trip() {
        let database = Database::in_memory().unwrap();
        database
            .insert_event(&event("event-1", 1, "Queue source", None))
            .unwrap();
        let task = QueueTask {
            id: "task-1".into(),
            raw_event_id: "event-1".into(),
            task_type: TaskType::SemanticTextAnalysis,
            status: QueueStatus::Pending,
            attempts: 0,
            priority: 5,
        };
        database.enqueue_task(&task).unwrap();
        let claimed = database.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.id, "task-1");
        assert_eq!(claimed.status, QueueStatus::Processing);
        database.finish_task("task-1").unwrap();
        assert!(database.claim_next_task().unwrap().is_none());
    }

    #[test]
    fn stale_processing_tasks_are_requeued() {
        let database = Database::in_memory().unwrap();
        database
            .insert_event(&event("event-stale", 1, "Stale", None))
            .unwrap();
        database
            .enqueue_task(&QueueTask {
                id: "task-stale".into(),
                raw_event_id: "event-stale".into(),
                task_type: TaskType::EmbeddingGeneration,
                status: QueueStatus::Pending,
                attempts: 0,
                priority: 0,
            })
            .unwrap();
        let claimed = database.claim_next_task().unwrap().unwrap();
        assert_eq!(claimed.status, QueueStatus::Processing);
        database.connection.execute("UPDATE processing_queue SET started_at = datetime('now', '-20 minutes') WHERE id = 'task-stale'", []).unwrap();
        assert_eq!(database.recover_stale_processing_tasks(10).unwrap(), 1);
        assert!(database.claim_next_task().unwrap().is_some());
    }
}
