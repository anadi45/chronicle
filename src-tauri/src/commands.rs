use crate::db::Database;
use std::sync::Mutex;
use tauri::State;

pub struct AppState {
    pub database: Mutex<Database>,
}

impl AppState {
    pub fn initialize() -> rusqlite::Result<Self> {
        Ok(Self {
            database: Mutex::new(Database::open()?),
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
