use crate::db::RawEvent;
use chrono::Utc;
use serde::{Deserialize, Serialize};
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
