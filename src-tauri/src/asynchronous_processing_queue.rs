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
            match processor.process(&task) {
                Ok(()) => {
                    if let Ok(database) = database.lock() {
                        let _ = database.finish_task(&task.id);
                    }
                }
                Err(error) => {
                    let retry = task.attempts < 3;
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
    #[test]
    fn retries_back_off() {
        assert!(retry_delay(2) > retry_delay(1));
        assert_eq!(retry_delay(0), Duration::from_millis(250));
    }
}
