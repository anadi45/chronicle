//! Windows activity capture providers.
//!
//! This module owns the boundary between Windows activity APIs and Chronicle's
//! normalized raw-event model. Capture runs on a background thread and must
//! never wait for semantic AI processing. Privacy-sensitive providers such as
//! keyboard capture belong here, behind explicit opt-in settings and exclusion
//! checks.

// Platform-specific implementations belong in sibling modules such as
// `windows.rs`, `macos.rs`, and `linux.rs`. The current foreground provider is
// kept behind `cfg(windows)` while the normalized contract remains shared.

use crate::local_sqlite_event_database::RawEvent;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CaptureSettings {
    pub enabled: bool,
    pub mouse_enabled: bool,
    pub keyboard_enabled: bool,
    pub keyboard_mode: KeyboardMode,
    #[serde(default)]
    pub keyboard_text_allowlist: Vec<String>,
    pub excluded_applications: Vec<String>,
    #[serde(default)]
    pub excluded_paths: Vec<String>,
    pub watched_folders: Vec<String>,
    pub screenshots_enabled: bool,
}

impl CaptureSettings {
    pub fn excludes_application(&self, executable_path: &str, app_name: &str) -> bool {
        self.excluded_applications.iter().any(|excluded| {
            let pattern = excluded.to_ascii_lowercase();
            executable_path.to_ascii_lowercase().contains(&pattern)
                || app_name.to_ascii_lowercase() == pattern
        })
    }

    pub fn excludes_path(&self, path: &str) -> bool {
        let candidate = path.to_ascii_lowercase();
        self.excluded_paths.iter().any(|excluded| candidate.contains(&excluded.to_ascii_lowercase()))
    }

    pub fn allows_keyboard_text(&self, app_name: &str) -> bool {
        matches!(self.keyboard_mode, KeyboardMode::AllowlistedText) && self.keyboard_text_allowlist.iter().any(|allowed| allowed.eq_ignore_ascii_case(app_name))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMode {
    #[default]
    MetadataOnly,
    AllowlistedText,
    FullText,
}

pub trait CaptureProvider: Send {
    fn name(&self) -> &'static str;
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self);
    fn is_available(&self) -> bool;
}

pub struct ForegroundWindowProvider {
    running: bool,
}
impl ForegroundWindowProvider {
    pub fn new() -> Self {
        Self { running: false }
    }
}
impl CaptureProvider for ForegroundWindowProvider {
    fn name(&self) -> &'static str {
        "foreground_window"
    }
    fn start(&mut self) -> Result<(), String> {
        self.running = true;
        Ok(())
    }
    fn stop(&mut self) {
        self.running = false;
    }
    fn is_available(&self) -> bool {
        cfg!(windows)
    }
}

pub fn normalize_window_event(
    app_name: String,
    window_title: String,
    executable_path: Option<String>,
    process_id: Option<u32>,
) -> RawEvent {
    RawEvent {
        id: Uuid::new_v4().to_string(),
        timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        event_type: "window_focused".into(),
        source: "foreground_window".into(),
        app_name: Some(app_name),
        executable_path,
        process_id,
        window_handle: None,
        window_title: Some(window_title),
        element_name: None,
        text: None,
        file_path: None,
        metadata_json: "{}".into(),
        privacy_class: "metadata".into(),
        confidence: 1.0,
        created_at: Utc::now().to_rfc3339(),
    }
}

#[cfg(windows)]
pub fn start_foreground_loop(
    database: Arc<std::sync::Mutex<crate::local_sqlite_event_database::Database>>,
    stop: Arc<AtomicBool>,
    settings: Arc<std::sync::Mutex<CaptureSettings>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut previous: Option<(isize, String)> = None;
        while !stop.load(Ordering::Relaxed) {
            if let Some((handle, title, process_id, executable_path, app_name)) =
                current_foreground_window()
            {
                if settings
                    .lock()
                    .map(|settings| settings.excludes_application(&executable_path, &app_name))
                    .unwrap_or(false)
                {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                let changed = previous
                    .as_ref()
                    .map(|(old_handle, old_title)| *old_handle != handle || old_title != &title)
                    .unwrap_or(true);
                if changed {
                    let event_type = if previous
                        .as_ref()
                        .map(|(old_handle, _)| *old_handle == handle)
                        .unwrap_or(false)
                    {
                        "window_title_changed"
                    } else {
                        "window_focused"
                    };
                    let mut event = normalize_window_event(
                        app_name,
                        title.clone(),
                        Some(executable_path),
                        Some(process_id),
                    );
                    event.window_handle = Some(handle as u64);
                    event.event_type = event_type.into();
                    event.metadata_json = format!("{{\"window_handle\":{handle}}}");
                    match database.lock() {
                        Ok(database) => if let Err(error) = database.insert_event_and_enqueue(&event) { tracing::warn!(%error, event_id = %event.id, "failed to persist foreground event"); },
                        Err(error) => tracing::warn!(%error, "failed to lock database for foreground event"),
                    }
                    previous = Some((handle, title));
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    })
}

#[cfg(not(windows))]
pub fn start_foreground_loop(
    _database: Arc<std::sync::Mutex<crate::local_sqlite_event_database::Database>>,
    stop: Arc<AtomicBool>,
    _settings: Arc<std::sync::Mutex<CaptureSettings>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(500));
        }
    })
}

#[cfg(windows)]
fn current_foreground_window() -> Option<(isize, String, u32, String, String)> {
    use ::windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    };
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return None;
    }
    let length = unsafe { GetWindowTextLengthW(window) };
    let mut buffer = vec![0u16; (length + 1) as usize];
    let written = unsafe { GetWindowTextW(window, &mut buffer) };
    let title = String::from_utf16_lossy(&buffer[..written as usize]);
    let mut process_id = 0u32;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut process_id));
    }
    let executable_path = process_executable_path(process_id).unwrap_or_default();
    let app_name = executable_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("unknown")
        .to_string();
    Some((
        window.0 as isize,
        title,
        process_id,
        executable_path,
        app_name,
    ))
}

#[cfg(windows)]
fn process_executable_path(process_id: u32) -> Option<String> {
    use ::windows::Win32::Foundation::CloseHandle;
    use ::windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()? };
    let mut buffer = vec![0u16; 1024];
    let mut length = buffer.len() as u32;
    let success = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            ::windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .is_ok()
    };
    unsafe {
        let _ = CloseHandle(process);
    }
    success.then(|| String::from_utf16_lossy(&buffer[..length as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_event_has_evidence() {
        let e = normalize_window_event("Editor".into(), "main.rs".into(), None, Some(7));
        assert_eq!(e.event_type, "window_focused");
        assert_eq!(e.process_id, Some(7));
        assert!(!e.id.is_empty());
    }
    #[test]
    fn defaults_are_privacy_safe() {
        let s = CaptureSettings::default();
        assert!(!s.enabled);
        assert!(matches!(s.keyboard_mode, KeyboardMode::MetadataOnly));
        assert!(!s.screenshots_enabled);
    }

    #[test]
    fn path_exclusions_match_case_insensitive_fragments() {
        let settings = CaptureSettings { excluded_paths: vec!["secrets".into()], ..Default::default() };
        assert!(settings.excludes_path("C:\\Projects\\Secrets\\notes.txt"));
        assert!(!settings.excludes_path("C:\\Projects\\Public\\notes.txt"));
    }

    #[test]
    fn legacy_settings_default_path_exclusions_to_empty() {
        let settings: CaptureSettings = serde_json::from_str(r#"{"enabled":true,"mouse_enabled":false,"keyboard_enabled":false,"keyboard_mode":"metadata_only","excluded_applications":[],"watched_folders":[],"screenshots_enabled":false}"#).unwrap();
        assert!(settings.excluded_paths.is_empty());
        assert!(settings.keyboard_text_allowlist.is_empty());
    }
}
