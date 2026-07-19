//! Tauri IPC commands exposed to the desktop UI.
//!
//! Commands validate/clamp user-facing inputs and delegate work to the
//! database, capture lifecycle, and settings services. Long-running capture
//! work is launched in a background thread so invoke handlers stay responsive.

use crate::activity_capture::CaptureSettings;
use crate::asynchronous_processing_queue::MAX_RETRY_ATTEMPTS;
use serde::Serialize;
use crate::local_sqlite_event_database::{Database, RawEvent, SemanticEvent};
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use tauri::State;

pub struct AppState {
    pub database: Arc<Mutex<Database>>,
    pub settings: Arc<Mutex<CaptureSettings>>,
    pub capture_stop: Mutex<Option<Arc<AtomicBool>>>,
    pub capture_threads: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureStatus {
    pub enabled: bool,
    pub foreground_provider_available: bool,
    pub active: bool,
    pub persisted_event_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessingQueueStatus {
    pub pending: i64,
    pub processing: i64,
    pub complete: i64,
    pub failed: i64,
    pub cancelled: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EventProcessingStatus {
    pub task_type: String,
    pub status: String,
    pub attempts: u32,
    pub error: Option<String>,
}

impl AppState {
    pub fn initialize() -> rusqlite::Result<Self> {
        let database = Database::open()?;
        database.seed_ready_event()?;
        let settings = database
            .load_setting("capture")?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        Ok(Self {
            database: Arc::new(Mutex::new(database)),
            settings: Arc::new(Mutex::new(settings)),
            capture_stop: Mutex::new(None),
            capture_threads: Mutex::new(Vec::new()),
        })
    }
}

#[tauri::command]
pub fn health_check() -> &'static str {
    "ok"
}

#[tauri::command]
pub fn recent_event_count(state: State<'_, AppState>) -> Result<i64, String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .count_events()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_events(
    state: State<'_, AppState>,
    limit: u32,
    query: Option<String>,
) -> Result<Vec<RawEvent>, String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .recent_events(limit.clamp(1, 500), query.as_deref())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn record_event(state: State<'_, AppState>, event: RawEvent) -> Result<(), String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .insert_event(&event)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn record_semantic_event(
    state: State<'_, AppState>,
    event: SemanticEvent,
) -> Result<(), String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .insert_semantic_event(&event)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn semantic_for_event(
    state: State<'_, AppState>,
    raw_event_id: String,
) -> Result<Option<SemanticEvent>, String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .semantic_for_raw_event(&raw_event_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_all_data(state: State<'_, AppState>) -> Result<(), String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .delete_all()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_capture_settings(state: State<'_, AppState>) -> Result<CaptureSettings, String> {
    Ok(state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?
        .clone())
}

#[tauri::command]
pub fn update_capture_settings(
    state: State<'_, AppState>,
    settings: CaptureSettings,
) -> Result<CaptureSettings, String> {
    let json = serde_json::to_string(&settings).map_err(|error| error.to_string())?;
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .save_setting("capture", &json)
        .map_err(|error| error.to_string())?;
    *state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())? = settings.clone();
    Ok(settings)
}

#[tauri::command]
pub fn set_input_permission(
    state: State<'_, AppState>,
    input: String,
    enabled: bool,
) -> Result<CaptureSettings, String> {
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?;
    match input.as_str() {
        "keyboard" => settings.keyboard_enabled = enabled,
        "mouse" => settings.mouse_enabled = enabled,
        _ => return Err("input must be keyboard or mouse".to_owned()),
    }
    let json = serde_json::to_string(&*settings).map_err(|error| error.to_string())?;
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .save_setting("capture", &json)
        .map_err(|error| error.to_string())?;
    Ok(settings.clone())
}

#[tauri::command]
pub fn set_excluded_applications(
    state: State<'_, AppState>,
    applications: Vec<String>,
) -> Result<CaptureSettings, String> {
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?;
    let mut normalized = Vec::new();
    for application in applications {
        let value = application.trim().to_ascii_lowercase();
        if !value.is_empty() && !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    settings.excluded_applications = normalized;
    let json = serde_json::to_string(&*settings).map_err(|error| error.to_string())?;
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .save_setting("capture", &json)
        .map_err(|error| error.to_string())?;
    Ok(settings.clone())
}

#[tauri::command]
pub fn set_excluded_paths(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<CaptureSettings, String> {
    let mut settings = state.settings.lock().map_err(|_| "settings lock poisoned".to_owned())?;
    settings.excluded_paths = paths.into_iter().map(|path| path.trim().to_owned()).filter(|path| !path.is_empty()).collect();
    let json = serde_json::to_string(&*settings).map_err(|error| error.to_string())?;
    state.database.lock().map_err(|_| "database lock poisoned".to_owned())?.save_setting("capture", &json).map_err(|error| error.to_string())?;
    Ok(settings.clone())
}

#[tauri::command]
pub fn set_watched_folders(
    state: State<'_, AppState>,
    folders: Vec<String>,
) -> Result<CaptureSettings, String> {
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?;
    let mut normalized = Vec::new();
    for folder in folders {
        let value = folder.trim().to_owned();
        if !value.is_empty()
            && std::path::Path::new(&value).is_dir()
            && !normalized.contains(&value)
        {
            normalized.push(value);
        }
    }
    settings.watched_folders = normalized;
    let json = serde_json::to_string(&*settings).map_err(|error| error.to_string())?;
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .save_setting("capture", &json)
        .map_err(|error| error.to_string())?;
    Ok(settings.clone())
}

#[tauri::command]
pub fn export_data(state: State<'_, AppState>) -> Result<String, String> {
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .export_json()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn start_capture(state: State<'_, AppState>) -> Result<(), String> {
    let mut stop_slot = state
        .capture_stop
        .lock()
        .map_err(|_| "capture lock poisoned".to_owned())?;
    if stop_slot.is_some() {
        return Ok(());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let keyboard_enabled = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?
        .keyboard_enabled;
    let mouse_enabled = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?
        .mouse_enabled;
    let thread = crate::activity_capture::start_foreground_loop(
        state.database.clone(),
        stop.clone(),
        state.settings.clone(),
    );
    *stop_slot = Some(stop.clone());
    let mut threads = state
        .capture_threads
        .lock()
        .map_err(|_| "capture thread lock poisoned".to_owned())?;
    threads.push(thread);
    threads.push(crate::filesystem_activity_capture::start_filesystem_loop(
        state.database.clone(),
        stop.clone(),
        state.settings.clone(),
    ));
    #[cfg(windows)]
    if mouse_enabled {
        threads.push(crate::input_capture::windows::start_mouse_hook(
            state.database.clone(),
            stop.clone(),
        ));
    }
    #[cfg(windows)]
    if keyboard_enabled {
        threads.push(crate::input_capture::windows::start_keyboard_hook(
            state.database.clone(),
            stop.clone(),
        ));
    }
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?;
    settings.enabled = true;
    let settings_json = serde_json::to_string(&*settings).map_err(|error| error.to_string())?;
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .save_setting("capture", &settings_json)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn capture_status(state: State<'_, AppState>) -> Result<CaptureStatus, String> {
    let enabled = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?
        .enabled;
    let active = state
        .capture_stop
        .lock()
        .map_err(|_| "capture lock poisoned".to_owned())?
        .is_some();
    let persisted_event_count = state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .count_events()
        .map_err(|error| error.to_string())?;
    Ok(CaptureStatus {
        enabled,
        active,
        foreground_provider_available: cfg!(windows),
        persisted_event_count,
    })
}

#[tauri::command]
pub fn processing_queue_status(
    state: State<'_, AppState>,
) -> Result<ProcessingQueueStatus, String> {
    let counts = state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .queue_counts()
        .map_err(|error| error.to_string())?;
    Ok(ProcessingQueueStatus {
        pending: *counts.get("pending").unwrap_or(&0),
        processing: *counts.get("processing").unwrap_or(&0),
        complete: *counts.get("complete").unwrap_or(&0),
        failed: *counts.get("failed").unwrap_or(&0),
        cancelled: *counts.get("cancelled").unwrap_or(&0),
    })
}

#[tauri::command]
pub fn storage_usage(state: State<'_, AppState>) -> Result<std::collections::HashMap<String, i64>, String> {
    state.database.lock().map_err(|_| "database lock poisoned".to_owned())?.storage_counts().map_err(|error| error.to_string())
}

#[derive(Debug, Serialize)]
pub struct ModelProviderStatus { pub semantic_provider: String, pub embedding_provider: String, pub semantic_available: bool, pub embedding_available: bool }

#[tauri::command]
pub fn model_provider_status() -> ModelProviderStatus {
    ModelProviderStatus { semantic_provider: "local-contract".into(), embedding_provider: "sqlite-fallback".into(), semantic_available: false, embedding_available: true }
}

#[derive(Debug, Serialize)]
pub struct ProcessingQueueLimits { pub max_retry_attempts: u32, pub max_pending_tasks: u32 }

#[tauri::command]
pub fn processing_queue_limits() -> ProcessingQueueLimits { ProcessingQueueLimits { max_retry_attempts: MAX_RETRY_ATTEMPTS, max_pending_tasks: 10_000 } }

#[tauri::command]
pub fn cancel_pending_processing_tasks(state: State<'_, AppState>) -> Result<usize, String> {
    state.database.lock().map_err(|_| "database lock poisoned".to_owned())?.cancel_pending_tasks().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn processing_status_for_event(
    state: State<'_, AppState>,
    raw_event_id: String,
) -> Result<Vec<EventProcessingStatus>, String> {
    let rows = state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .processing_status_for_raw_event(&raw_event_id)
        .map_err(|error| error.to_string())?;
    Ok(rows
        .into_iter()
        .map(
            |(task_type, status, attempts, error)| EventProcessingStatus {
                task_type,
                status,
                attempts,
                error,
            },
        )
        .collect())
}

pub fn stop_capture_state(state: &AppState) {
    if let Ok(mut stop_slot) = state.capture_stop.lock() {
        if let Some(stop) = stop_slot.take() {
            stop.store(true, Ordering::Relaxed);
        }
    }
    if let Ok(mut thread_slot) = state.capture_threads.lock() {
        for thread in thread_slot.drain(..) {
            let _ = thread.join();
        }
    }
}

#[tauri::command]
pub fn stop_capture(state: State<'_, AppState>) -> Result<(), String> {
    stop_capture_state(&state);
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| "settings lock poisoned".to_owned())?;
    settings.enabled = false;
    let settings_json = serde_json::to_string(&*settings).map_err(|error| error.to_string())?;
    state
        .database
        .lock()
        .map_err(|_| "database lock poisoned".to_owned())?
        .save_setting("capture", &settings_json)
        .map_err(|error| error.to_string())?;
    Ok(())
}
