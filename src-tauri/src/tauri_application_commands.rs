//! Tauri IPC commands exposed to the desktop UI.
//!
//! Commands validate/clamp user-facing inputs and delegate work to the
//! database, capture lifecycle, and settings services. Long-running capture
//! work is launched in a background thread so invoke handlers stay responsive.

use crate::local_sqlite_event_database::{Database, RawEvent};
use crate::windows_activity_capture::CaptureSettings;
use std::sync::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use tauri::State;

pub struct AppState {
    pub database: Arc<Mutex<Database>>,
    pub settings: Mutex<CaptureSettings>,
    pub capture_stop: Mutex<Option<Arc<AtomicBool>>>,
    pub capture_thread: Mutex<Option<JoinHandle<()>>>,
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
            settings: Mutex::new(settings),
            capture_stop: Mutex::new(None),
            capture_thread: Mutex::new(None),
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
    let thread = crate::windows_activity_capture::start_foreground_loop(
        state.database.clone(),
        stop.clone(),
    );
    *stop_slot = Some(stop);
    *state
        .capture_thread
        .lock()
        .map_err(|_| "capture thread lock poisoned".to_owned())? = Some(thread);
    Ok(())
}

#[tauri::command]
pub fn stop_capture(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(stop) = state
        .capture_stop
        .lock()
        .map_err(|_| "capture lock poisoned".to_owned())?
        .take()
    {
        stop.store(true, Ordering::Relaxed);
    }
    if let Some(thread) = state
        .capture_thread
        .lock()
        .map_err(|_| "capture thread lock poisoned".to_owned())?
        .take()
    {
        let _ = thread.join();
    }
    Ok(())
}
