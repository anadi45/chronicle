mod commands;
mod db;

use commands::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("chronicle=info")
        .with_target(false)
        .init();

    let state = AppState::initialize().expect("database initialization failed");
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::health_check,
            commands::recent_event_count
        ])
        .run(tauri::generate_context!())
        .expect("error while running Chronicle");
}
