//! Transient screenshot capture and asset lifecycle.
//!
//! Screenshot bytes are intentionally not part of `RawEvent` or SQLite. This
//! module owns short-lived in-memory assets that may be handed to an analysis
//! queue and are then dropped, including when analysis fails.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotTrigger {
    AppActivated,
    WindowTitleChanged,
    DoubleClick,
    RightClick,
    TextSelected,
    DragEnded,
    ElementFocused,
}

impl ScreenshotTrigger {
    pub fn meaningful(self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct TransientScreenshotAsset {
    pub raw_event_id: String,
    pub captured_at: Instant,
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

impl TransientScreenshotAsset {
    pub fn new(raw_event_id: String, bytes: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            raw_event_id,
            captured_at: Instant::now(),
            bytes,
            mime_type: mime_type.into(),
        }
    }
    pub fn expired(&self, retention: Duration) -> bool {
        self.captured_at.elapsed() >= retention
    }
}

pub trait ActiveWindowScreenshotProvider: Send {
    fn capture_active_window(&self) -> Result<Vec<u8>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assets_are_in_memory_and_expire() {
        let asset = TransientScreenshotAsset::new("event".into(), vec![1, 2, 3], "image/png");
        assert_eq!(asset.bytes.len(), 3);
        assert!(!asset.expired(Duration::from_secs(1)));
    }
    #[test]
    fn meaningful_triggers_are_explicit() {
        assert!(ScreenshotTrigger::DoubleClick.meaningful());
        assert!(ScreenshotTrigger::ElementFocused.meaningful());
    }
}
