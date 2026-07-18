//! Windows activity capture providers.
//!
//! This module owns the boundary between Windows activity APIs and Chronicle's
//! normalized raw-event model. Capture runs on a background thread and must
//! never wait for semantic AI processing. Privacy-sensitive providers such as
//! keyboard capture belong here, behind explicit opt-in settings and exclusion
//! checks.

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
    pub keyboard_mode: KeyboardMode,
    pub excluded_applications: Vec<String>,
    pub watched_folders: Vec<String>,
    pub screenshots_enabled: bool,
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
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut previous: Option<(isize, String)> = None;
        while !stop.load(Ordering::Relaxed) {
            if let Some((handle, title, process_id)) = current_foreground_window() {
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
                        "Unknown application".into(),
                        title.clone(),
                        None,
                        Some(process_id),
                    );
                    event.event_type = event_type.into();
                    event.metadata_json = format!("{{\"window_handle\":{handle}}}");
                    if let Ok(database) = database.lock() {
                        let _ = database.insert_event(&event);
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
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(500));
        }
    })
}

#[cfg(windows)]
fn current_foreground_window() -> Option<(isize, String, u32)> {
    use windows::Win32::UI::WindowsAndMessaging::{
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
    Some((window.0 as isize, title, process_id))
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
}
