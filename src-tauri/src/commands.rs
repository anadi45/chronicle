use crate::capture::CaptureSettings;
use crate::db::{Database, RawEvent};
use std::sync::Mutex;
use tauri::State;

pub struct AppState {
    pub database: Mutex<Database>,
    pub settings: Mutex<CaptureSettings>,
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
            database: Mutex::new(database),
            settings: Mutex::new(settings),
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
