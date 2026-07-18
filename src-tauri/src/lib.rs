#[allow(dead_code)]
mod activity_capture;
#[allow(dead_code)]
mod asynchronous_processing_queue;
#[allow(dead_code)]
mod input_capture;
mod local_sqlite_event_database;
mod tauri_application_commands;

use tauri::Manager;
use tauri_application_commands::AppState;

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
            tauri_application_commands::health_check,
            tauri_application_commands::recent_event_count,
            tauri_application_commands::list_events,
            tauri_application_commands::record_event,
            tauri_application_commands::delete_all_data,
            tauri_application_commands::get_capture_settings,
            tauri_application_commands::update_capture_settings,
            tauri_application_commands::export_data,
            tauri_application_commands::start_capture,
            tauri_application_commands::stop_capture,
            tauri_application_commands::capture_status
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let state = window.app_handle().state::<AppState>();
                tauri_application_commands::stop_capture_state(&state);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Chronicle");
}
