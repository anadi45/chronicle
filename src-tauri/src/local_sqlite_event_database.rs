//! Local SQLite persistence for append-only raw evidence and derived records.
//!
//! The database is the reliability boundary: capture writes compact normalized
//! events first, while semantic processing may be retried or regenerated. FTS5
//! is maintained from raw-event triggers so search remains useful without AI.

use crate::asynchronous_processing_queue::{QueueStatus, QueueTask, TaskType};
use crate::asynchronous_processing_queue::MAX_PENDING_TASKS;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEvent {
    pub id: String,
    pub raw_event_id: String,
    pub category: String,
    pub summary: String,
    pub entities_json: String,
    pub relationships_json: String,
    pub confidence: f32,
    pub model_name: String,
    pub model_version: String,
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
        // Keep existing installations compatible with the retry timestamp added after v1.
        let _ = connection.execute("ALTER TABLE processing_queue ADD COLUMN retry_at TEXT", []);
        Ok(Self { connection })
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn count_events(&self) -> Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
    }

    pub fn storage_counts(&self) -> Result<HashMap<String, i64>> {
        let mut counts = HashMap::new();
        for (name, table) in [("raw_events", "raw_events"), ("semantic_events", "semantic_events"), ("embeddings", "semantic_event_embeddings"), ("queue_tasks", "processing_queue")] {
            let count: i64 = self.connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
            counts.insert(name.to_owned(), count);
        }
        Ok(counts)
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
        let pending: i64 = self.connection.query_row("SELECT COUNT(*) FROM processing_queue WHERE status = 'pending'", [], |row| row.get(0))?;
        if pending >= MAX_PENDING_TASKS as i64 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute("INSERT INTO processing_queue (id, raw_event_id, task_type, status, priority, attempts, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))", params![task.id, task.raw_event_id, serde_json::to_string(&task.task_type).unwrap_or_default().trim_matches('"'), serde_json::to_string(&task.status).unwrap_or_default().trim_matches('"'), task.priority, task.attempts])?;
        Ok(())
    }

    pub fn claim_next_task(&self) -> Result<Option<QueueTask>> {
        let transaction = self.connection.unchecked_transaction()?;
        let candidate = transaction.query_row("SELECT id, raw_event_id, task_type, attempts, priority FROM processing_queue WHERE status = 'pending' AND (retry_at IS NULL OR retry_at <= datetime('now')) ORDER BY priority DESC, created_at ASC LIMIT 1", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, u32>(3)?, row.get::<_, i32>(4)?))).optional()?;
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
    pub fn fail_task(&self, task_id: &str, error: &str, retry: bool, attempt: u32) -> Result<()> {
        if retry {
            let retry_seconds = (250u64.saturating_mul(2u64.saturating_pow(attempt.min(8))) / 1000).max(1);
            self.connection.execute("UPDATE processing_queue SET status = 'pending', error = ?1, retry_at = datetime('now', '+' || ?2 || ' seconds'), completed_at = NULL WHERE id = ?3", params![error, retry_seconds.max(1), task_id])?;
        } else {
            self.connection.execute("UPDATE processing_queue SET status = 'failed', error = ?1, retry_at = NULL, completed_at = datetime('now') WHERE id = ?2", params![error, task_id])?;
        }
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

    pub fn cancel_pending_tasks(&self) -> Result<usize> {
        Ok(self.connection.execute("UPDATE processing_queue SET status = 'cancelled', completed_at = datetime('now') WHERE status = 'pending'", [])?)
    }

    pub fn requeue_processing_tasks(&self) -> Result<usize> {
        Ok(self.connection.execute("UPDATE processing_queue SET status = 'pending', started_at = NULL WHERE status = 'processing'", [])?)
    }

    pub fn processing_status_for_raw_event(
        &self,
        raw_event_id: &str,
    ) -> Result<Vec<(String, String, u32, Option<String>)>> {
        let mut statement = self.connection.prepare("SELECT task_type, status, attempts, error FROM processing_queue WHERE raw_event_id = ?1 ORDER BY created_at ASC")?;
        let rows = statement.query_map([raw_event_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    pub fn recover_stale_processing_tasks(&self, stale_minutes: u32) -> Result<usize> {
        let changed = self.connection.execute("UPDATE processing_queue SET status = 'pending', started_at = NULL, error = 'requeued after interrupted processing' WHERE status = 'processing' AND started_at < datetime('now', ?1)", [format!("-{} minutes", stale_minutes)])?;
        Ok(changed)
    }

    pub fn insert_semantic_event(&self, event: &SemanticEvent) -> Result<()> {
        self.connection.execute("INSERT INTO semantic_events (id, raw_event_id, category, summary, entities_json, relationships_json, confidence, model_name, model_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)", params![event.id, event.raw_event_id, event.category, event.summary, event.entities_json, event.relationships_json, event.confidence, event.model_name, event.model_version, event.created_at])?;
        Ok(())
    }

    pub fn semantic_for_raw_event(&self, raw_event_id: &str) -> Result<Option<SemanticEvent>> {
        self.connection.query_row("SELECT id, raw_event_id, category, summary, entities_json, relationships_json, confidence, model_name, model_version, created_at FROM semantic_events WHERE raw_event_id = ?1 ORDER BY created_at DESC LIMIT 1", [raw_event_id], |row| Ok(SemanticEvent { id: row.get(0)?, raw_event_id: row.get(1)?, category: row.get(2)?, summary: row.get(3)?, entities_json: row.get(4)?, relationships_json: row.get(5)?, confidence: row.get(6)?, model_name: row.get(7)?, model_version: row.get(8)?, created_at: row.get(9)? })).optional()
    }

    pub fn insert_embedding(
        &self,
        semantic_event_id: &str,
        model_name: &str,
        model_version: &str,
        embedding: &[f32],
    ) -> Result<()> {
        self.connection.execute("INSERT INTO semantic_event_embeddings (semantic_event_id, model_name, model_version, dimensions, embedding_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) ON CONFLICT(semantic_event_id) DO UPDATE SET model_name=excluded.model_name, model_version=excluded.model_version, dimensions=excluded.dimensions, embedding_json=excluded.embedding_json, created_at=excluded.created_at", params![semantic_event_id, model_name, model_version, embedding.len() as i64, serde_json::to_string(embedding).unwrap_or_else(|_| "[]".into())])?;
        Ok(())
    }

    pub fn search_embeddings(&self, query: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        let mut statement = self
            .connection
            .prepare("SELECT semantic_event_id, embedding_json FROM semantic_event_embeddings")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut scored = Vec::new();
        for row in rows {
            let (id, json) = row?;
            let embedding: Vec<f32> = serde_json::from_str(&json).unwrap_or_default();
            if embedding.len() == query.len() {
                scored.push((id, cosine_similarity(query, &embedding)));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    pub fn hybrid_rank(&self, text_ids: &[String], vector_scores: &[(String, f32)], limit: usize) -> Vec<String> {
        let text_rank: HashMap<&String, f32> = text_ids.iter().enumerate().map(|(index, id)| (id, 1.0 / (index as f32 + 1.0))).collect();
        let vector_rank: HashMap<&String, f32> = vector_scores.iter().map(|(id, score)| (id, *score)).collect();
        let mut ids: Vec<String> = text_ids.iter().chain(vector_scores.iter().map(|(id, _)| id)).cloned().collect();
        ids.sort();
        ids.dedup();
        ids.sort_by(|left, right| {
            let left_score = text_rank.get(left).copied().unwrap_or(0.0) * 0.4 + vector_rank.get(left).copied().unwrap_or(0.0) * 0.6;
            let right_score = text_rank.get(right).copied().unwrap_or(0.0) * 0.4 + vector_rank.get(right).copied().unwrap_or(0.0) * 0.6;
            right_score.partial_cmp(&left_score).unwrap_or(std::cmp::Ordering::Equal)
        });
        ids.truncate(limit);
        ids
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

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let dot: f32 = left.iter().zip(right).map(|(a, b)| a * b).sum();
    let left_norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f32>().sqrt();
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm * right_norm)
    }
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
        let counts = database.storage_counts().unwrap();
        assert_eq!(counts.get("raw_events"), Some(&0));
        assert_eq!(counts.get("semantic_events"), Some(&0));
        assert_eq!(counts.get("embeddings"), Some(&0));
        assert_eq!(counts.get("queue_tasks"), Some(&0));
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
    fn failed_retry_persists_a_future_retry_timestamp() {
        let database = Database::in_memory().unwrap();
        database.insert_event(&event("event-retry", 1, "Retry", None)).unwrap();
        database.enqueue_task(&QueueTask { id: "task-retry".into(), raw_event_id: "event-retry".into(), task_type: TaskType::SemanticTextAnalysis, status: QueueStatus::Pending, attempts: 0, priority: 0 }).unwrap();
        database.claim_next_task().unwrap().unwrap();
        database.fail_task("task-retry", "temporary failure", true, 3).unwrap();
        let retry_at: Option<String> = database.connection.query_row("SELECT retry_at FROM processing_queue WHERE id = 'task-retry'", [], |row| row.get(0)).unwrap();
        assert!(retry_at.is_some());
        assert!(database.claim_next_task().unwrap().is_none());
    }

    #[test]
    fn cancellation_marks_only_pending_tasks() {
        let database = Database::in_memory().unwrap();
        database.insert_event(&event("event-cancel", 1, "Cancel", None)).unwrap();
        database.enqueue_task(&QueueTask { id: "task-cancel".into(), raw_event_id: "event-cancel".into(), task_type: TaskType::EmbeddingGeneration, status: QueueStatus::Pending, attempts: 0, priority: 0 }).unwrap();
        assert_eq!(database.cancel_pending_tasks().unwrap(), 1);
        assert_eq!(database.queue_counts().unwrap().get("cancelled"), Some(&1));
        assert!(database.claim_next_task().unwrap().is_none());
    }

    #[test]
    fn processing_tasks_can_be_requeued_on_shutdown() {
        let database = Database::in_memory().unwrap();
        database.insert_event(&event("event-shutdown", 1, "Shutdown", None)).unwrap();
        database.enqueue_task(&QueueTask { id: "task-shutdown".into(), raw_event_id: "event-shutdown".into(), task_type: TaskType::EmbeddingGeneration, status: QueueStatus::Pending, attempts: 0, priority: 0 }).unwrap();
        database.claim_next_task().unwrap().unwrap();
        assert_eq!(database.requeue_processing_tasks().unwrap(), 1);
        assert!(database.claim_next_task().unwrap().is_some());
    }

    #[test]
    fn persists_one_thousand_events_without_losing_count() {
        let database = Database::in_memory().unwrap();
        for index in 0..1_000 { database.insert_event(&event(&format!("bulk-{index}"), index, "Bulk", None)).unwrap(); }
        assert_eq!(database.count_events().unwrap(), 1_000);
        assert_eq!(database.recent_events(10, None).unwrap().len(), 10);
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

    #[test]
    fn semantic_event_requires_existing_raw_event() {
        let database = Database::in_memory().unwrap();
        let semantic = SemanticEvent {
            id: "semantic-1".into(),
            raw_event_id: "missing".into(),
            category: "test".into(),
            summary: "summary".into(),
            entities_json: "[]".into(),
            relationships_json: "[]".into(),
            confidence: 0.9,
            model_name: "test-model".into(),
            model_version: "1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        assert!(database.insert_semantic_event(&semantic).is_err());
    }

    #[test]
    fn hybrid_rank_combines_text_and_vector_results() {
        let database = Database::in_memory().unwrap();
        let ranked = database.hybrid_rank(&["text-only".into(), "shared".into()], &[("vector-only".into(), 0.9), ("shared".into(), 0.8)], 3);
        assert_eq!(ranked[0], "shared");
        assert_eq!(ranked.len(), 3);
    }

    #[test]
    fn embedding_fallback_search_ranks_similar_vectors() {
        let database = Database::in_memory().unwrap();
        database
            .insert_event(&event("event-embed", 1, "Embedding source", None))
            .unwrap();
        database
            .insert_semantic_event(&SemanticEvent {
                id: "semantic-embed".into(),
                raw_event_id: "event-embed".into(),
                category: "test".into(),
                summary: "vector".into(),
                entities_json: "[]".into(),
                relationships_json: "[]".into(),
                confidence: 1.0,
                model_name: "test".into(),
                model_version: "1".into(),
                created_at: "now".into(),
            })
            .unwrap();
        database
            .insert_embedding("semantic-embed", "test", "1", &[1.0, 0.0])
            .unwrap();
        assert_eq!(
            database.search_embeddings(&[0.9, 0.1], 1).unwrap()[0].0,
            "semantic-embed"
        );
    }
}
