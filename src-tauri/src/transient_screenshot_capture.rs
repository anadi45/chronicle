//! Transient screenshot capture and asset lifecycle.
//!
//! Screenshot bytes are intentionally not part of `RawEvent` or SQLite. This
//! module owns short-lived in-memory assets that may be handed to an analysis
//! queue and are then dropped, including when analysis fails.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use std::collections::HashMap;

pub const DEFAULT_SCREENSHOT_RETENTION: Duration = Duration::from_secs(30);

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
    pub queue_task_id: Option<String>,
    pub captured_at: Instant,
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

impl TransientScreenshotAsset {
    pub fn new(raw_event_id: String, bytes: Vec<u8>, mime_type: impl Into<String>) -> Self {
        Self {
            raw_event_id,
            queue_task_id: None,
            captured_at: Instant::now(),
            bytes,
            mime_type: mime_type.into(),
        }
    }
    pub fn expired(&self, retention: Duration) -> bool {
        self.captured_at.elapsed() >= retention
    }
    pub fn is_valid(&self) -> bool { !self.bytes.is_empty() && self.mime_type.starts_with("image/") }
}

#[derive(Default)]
pub struct TransientScreenshotStore {
    assets: HashMap<String, TransientScreenshotAsset>,
}

impl TransientScreenshotStore {
    pub fn insert(&mut self, asset: TransientScreenshotAsset) -> bool { if !asset.is_valid() { return false; } self.assets.insert(asset.raw_event_id.clone(), asset); true }
    pub fn associate_queue_task(&mut self, raw_event_id: &str, queue_task_id: String) -> bool { if let Some(asset) = self.assets.get_mut(raw_event_id) { asset.queue_task_id = Some(queue_task_id); true } else { false } }
    pub fn take(&mut self, raw_event_id: &str) -> Option<TransientScreenshotAsset> { self.assets.remove(raw_event_id) }
    pub fn purge_expired(&mut self, retention: Duration) { self.assets.retain(|_, asset| !asset.expired(retention)); }
    pub fn purge_default_retention(&mut self) { self.purge_expired(DEFAULT_SCREENSHOT_RETENTION); }
    pub fn len(&self) -> usize { self.assets.len() }
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
    #[test]
    fn store_releases_assets_after_processing() {
        let mut store = TransientScreenshotStore::default();
        assert!(store.insert(TransientScreenshotAsset::new("event".into(), vec![1], "image/png")));
        assert_eq!(store.len(), 1);
        assert!(store.take("event").is_some());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn store_associates_asset_with_queue_task() {
        let mut store = TransientScreenshotStore::default();
        store.insert(TransientScreenshotAsset::new("event".into(), vec![1], "image/png"));
        assert!(store.associate_queue_task("event", "task".into()));
        assert_eq!(store.take("event").unwrap().queue_task_id.as_deref(), Some("task"));
    }

    #[test]
    fn store_purges_expired_assets() {
        let mut store = TransientScreenshotStore::default();
        store.insert(TransientScreenshotAsset::new("expired".into(), vec![1], "image/png"));
        store.purge_expired(Duration::ZERO);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn store_rejects_empty_and_non_image_assets() {
        let mut store = TransientScreenshotStore::default();
        assert!(!store.insert(TransientScreenshotAsset::new("empty".into(), vec![], "image/png")));
        assert!(!store.insert(TransientScreenshotAsset::new("text".into(), vec![1], "text/plain")));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn default_retention_is_short_and_explicit() {
        assert_eq!(DEFAULT_SCREENSHOT_RETENTION, Duration::from_secs(30));
    }
}
