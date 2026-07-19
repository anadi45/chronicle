//! Asynchronous post-capture processing contracts.
//!
//! Queue tasks are deliberately separate from capture. A slow or unavailable
//! model must not block persistence of raw evidence. Workers will claim bounded
//! batches, retry transient failures, and retain model/version metadata.

use crate::local_sqlite_event_database::Database;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;
use std::panic::{catch_unwind, AssertUnwindSafe};
use crate::local_model_provider::OllamaLocalModelProvider;
use crate::embedding_provider::TextEmbedder;
use crate::local_sqlite_event_database::SemanticEvent;
use chrono::Utc;
use uuid::Uuid;

pub const MAX_RETRY_ATTEMPTS: u32 = 3;
pub const MAX_PENDING_TASKS: u32 = 10_000;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProcessingMetrics { pub completed: u64, pub failed: u64, pub panicked: u64, pub total_latency_ms: u128, pub last_model_name: Option<String>, pub last_model_version: Option<String> }
impl ProcessingMetrics {
    pub fn snapshot(&self) -> Self { self.clone() }
    pub fn reset(&mut self) { *self = Self::default(); }
    pub fn record_completed(&mut self) { self.completed += 1; }
    pub fn record_completed_with_latency(&mut self, latency: Duration) { self.record_completed(); self.total_latency_ms += latency.as_millis(); }
    pub fn record_failed(&mut self) { self.failed += 1; }
    pub fn record_panicked(&mut self) { self.panicked += 1; }
    pub fn record_model(&mut self, name: impl Into<String>, version: impl Into<String>) { self.last_model_name = Some(name.into()); self.last_model_version = Some(version.into()); }
    pub fn average_latency_ms(&self) -> Option<f64> { (self.completed > 0).then(|| self.total_latency_ms as f64 / self.completed as f64) }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    SemanticTextAnalysis,
    SemanticImageAnalysis,
    EmbeddingGeneration,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueStatus {
    Pending,
    Processing,
    Complete,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueTask {
    pub id: String,
    pub raw_event_id: String,
    pub task_type: TaskType,
    pub status: QueueStatus,
    pub attempts: u32,
    pub priority: i32,
}
pub trait SemanticAnalyzer: Send + Sync {
    fn analyze_text(&self, input: &str) -> Result<String, String>;
    fn analyze_image(&self, bytes: &[u8]) -> Result<String, String>;
}
pub trait Embedder: Send + Sync {
    fn embed(&self, input: &str) -> Result<Vec<f32>, String>;
}
pub fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(250u64.saturating_mul(2u64.saturating_pow(attempt.min(8))))
}

pub trait QueueTaskProcessor: Send + Sync {
    fn process(&self, task: &QueueTask) -> Result<(), String>;
}

pub struct LocalModelQueueProcessor { pub database: Arc<Mutex<Database>> }
impl QueueTaskProcessor for LocalModelQueueProcessor {
    fn process(&self, task: &QueueTask) -> Result<(), String> {
        let provider = OllamaLocalModelProvider::default();
        let database = self.database.clone();
        let event = database.lock().map_err(|_| "database lock poisoned")?.event_by_id(&task.raw_event_id).map_err(|e| e.to_string())?.ok_or("raw event not found")?;
        let context = format!("application: {:?}\nwindow: {:?}\nevent: {}\ntext: {:?}", event.app_name, event.window_title, event.event_type, event.text);
        match task.task_type {
            TaskType::SemanticTextAnalysis => { let output = provider.analyze_text(&context)?; let semantic_id = Uuid::new_v4().to_string(); database.lock().map_err(|_| "database lock poisoned")?.insert_semantic_event(&SemanticEvent { id: semantic_id, raw_event_id: task.raw_event_id.clone(), category: output.category, summary: output.summary, entities_json: serde_json::to_string(&output.entities).unwrap_or_default(), relationships_json: serde_json::to_string(&output.relationships).unwrap_or_default(), confidence: output.confidence, model_name: provider.gemma_model.clone(), model_version: "ollama".into(), created_at: Utc::now().to_rfc3339() }).map_err(|e| e.to_string())?; database.lock().map_err(|_| "database lock poisoned")?.enqueue_task(&QueueTask { id: Uuid::new_v4().to_string(), raw_event_id: task.raw_event_id.clone(), task_type: TaskType::EmbeddingGeneration, status: QueueStatus::Pending, attempts: 0, priority: -1 }).map_err(|e| e.to_string())?; Ok(()) }
            TaskType::EmbeddingGeneration => { let embedding = provider.embed(&context)?; let semantic_id = database.lock().map_err(|_| "database lock poisoned")?.semantic_for_raw_event(&task.raw_event_id).map_err(|e| e.to_string())?.ok_or("semantic event not found")?.id; database.lock().map_err(|_| "database lock poisoned")?.insert_embedding(&semantic_id, &provider.nomic_model, "ollama", &embedding).map_err(|e| e.to_string()) }
            TaskType::SemanticImageAnalysis => Err("image processing is not wired to a native image provider".into()),
        }
    }
}


pub fn run_processing_worker(
    database: Arc<Mutex<Database>>,
    stop: Arc<AtomicBool>,
    processor: Arc<dyn QueueTaskProcessor>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Ok(database) = database.lock() {
            let _ = database.recover_stale_processing_tasks(10);
        }
        while !stop.load(Ordering::Relaxed) {
            let task = database
                .lock()
                .ok()
                .and_then(|database| database.claim_next_task().ok())
                .flatten();
            let Some(task) = task else {
                thread::sleep(Duration::from_millis(250));
                continue;
            };
            if stop.load(Ordering::Relaxed) {
                if let Ok(database) = database.lock() { let _ = database.requeue_processing_tasks(); }
                break;
            }
            let processing_result = catch_unwind(AssertUnwindSafe(|| processor.process(&task)))
                .unwrap_or_else(|_| Err("processing provider panicked".into()));
            match processing_result {
                Ok(()) => {
                    if let Ok(database) = database.lock() {
                        let _ = database.finish_task(&task.id);
                    }
                }
                Err(error) => {
                    let retry = task.attempts < MAX_RETRY_ATTEMPTS;
                    if let Ok(database) = database.lock() {
                        let _ = database.fail_task(&task.id, &error, retry, task.attempts);
                    }
                    if retry {
                        thread::sleep(retry_delay(task.attempts));
                    }
                }
            }
        }
        if let Ok(database) = database.lock() { let _ = database.requeue_processing_tasks(); }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_sqlite_event_database::RawEvent;
    use std::sync::atomic::AtomicUsize;
    #[test]
    fn retries_back_off() {
        assert_eq!(MAX_RETRY_ATTEMPTS, 3);
        assert!(retry_delay(2) > retry_delay(1));
        assert_eq!(retry_delay(0), Duration::from_millis(250));
    }

    #[test]
    fn provider_panics_are_convertible_to_failures() {
        let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> { panic!("model failure") }));
        assert!(result.is_err());
    }

    #[test]
    fn processing_metrics_start_empty() {
        let mut metrics = ProcessingMetrics::default();
        metrics.record_completed_with_latency(Duration::from_millis(25)); metrics.record_failed(); metrics.record_panicked(); metrics.record_model("test-model", "1");
        assert_eq!(metrics.average_latency_ms(), Some(25.0));
        assert_eq!(metrics.snapshot(), ProcessingMetrics { completed: 1, failed: 1, panicked: 1, total_latency_ms: 25, last_model_name: Some("test-model".into()), last_model_version: Some("1".into()) });
        metrics.reset();
        assert_eq!(metrics, ProcessingMetrics::default());
        assert_eq!(metrics.average_latency_ms(), None);
    }

    #[test]
    fn busy_worker_processes_bounded_work_and_stops() {
        struct BusyProcessor { calls: AtomicUsize }
        impl QueueTaskProcessor for BusyProcessor {
            fn process(&self, _task: &QueueTask) -> Result<(), String> { std::thread::sleep(Duration::from_millis(10)); self.calls.fetch_add(1, Ordering::Relaxed); Ok(()) }
        }
        let database = Arc::new(Mutex::new(Database::in_memory().unwrap()));
        database.lock().unwrap().insert_event(&RawEvent { id: "busy-event".into(), timestamp_ns: 1, event_type: "test".into(), source: "test".into(), app_name: None, executable_path: None, process_id: None, window_handle: None, window_title: None, element_name: None, text: None, file_path: None, metadata_json: "{}".into(), privacy_class: "test".into(), confidence: 1.0, created_at: "2026-01-01T00:00:00Z".into() }).unwrap();
        database.lock().unwrap().enqueue_task(&QueueTask { id: "busy-task".into(), raw_event_id: "busy-event".into(), task_type: TaskType::SemanticTextAnalysis, status: QueueStatus::Pending, attempts: 0, priority: 0 }).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let processor = Arc::new(BusyProcessor { calls: AtomicUsize::new(0) });
        let worker = run_processing_worker(database.clone(), stop.clone(), processor.clone());
        std::thread::sleep(Duration::from_millis(5));
        database.lock().unwrap().insert_event(&RawEvent { id: "capture-while-busy".into(), timestamp_ns: 2, event_type: "window_focused".into(), source: "foreground_window".into(), app_name: Some("Editor".into()), executable_path: None, process_id: None, window_handle: None, window_title: Some("Notes".into()), element_name: None, text: None, file_path: None, metadata_json: "{}".into(), privacy_class: "metadata".into(), confidence: 1.0, created_at: "2026-01-01T00:00:01Z".into() }).unwrap();
        let processing_started = std::time::Instant::now();
        std::thread::sleep(Duration::from_millis(50)); stop.store(true, Ordering::Relaxed); worker.join().unwrap();
        assert!(processing_started.elapsed() < Duration::from_secs(2));
        assert_eq!(processor.calls.load(Ordering::Relaxed), 1);
        assert_eq!(database.lock().unwrap().count_events().unwrap(), 2);
        assert_eq!(database.lock().unwrap().queue_counts().unwrap().get("complete"), Some(&1));
    }
}
