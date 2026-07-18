//! User-selected filesystem activity capture.
//!
//! This provider watches only folders explicitly selected by the user and
//! records filesystem evidence rather than claiming who edited a file.

use crate::activity_capture::CaptureSettings;
use crate::local_sqlite_event_database::{Database, RawEvent};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub fn start_filesystem_loop(
    database: Arc<Mutex<Database>>,
    stop: Arc<AtomicBool>,
    settings: Arc<Mutex<CaptureSettings>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut previous: HashMap<String, u128> = HashMap::new();
        while !stop.load(Ordering::Relaxed) {
            let folders = settings
                .lock()
                .map(|settings| settings.watched_folders.clone())
                .unwrap_or_default();
            let excluded_paths = settings.lock().map(|settings| settings.excluded_paths.clone()).unwrap_or_default();
            let current = snapshot(&folders, &excluded_paths);
            let removed: Vec<(String, u128)> = previous.iter().filter(|(path, _)| !current.contains_key(path.as_str())).map(|(path, modified)| (path.clone(), *modified)).collect();
            let added: Vec<(String, u128)> = current.iter().filter(|(path, _)| !previous.contains_key(path.as_str())).map(|(path, modified)| (path.clone(), *modified)).collect();
            for ((old_path, old_modified), (new_path, new_modified)) in removed.iter().zip(added.iter()).filter(|((_, old), (_, new))| old == new && *old > 0) {
                persist(&database, "file_renamed", new_path, *new_modified);
                let _ = old_path;
            }
            for (path, modified) in &current {
                let event_type = match previous.get(path.as_str()) {
                    None if !removed.iter().zip(added.iter()).any(|((_, old_modified), (new, new_modified))| new == path && old_modified == new_modified) => Some("file_created"),
                    Some(old) if old != modified => Some("file_modified"),
                    _ => None,
                };
                if let Some(event_type) = event_type {
                    persist(&database, event_type, path, *modified);
                }
            }
            for path in previous
                .keys()
                .filter(|path| !current.contains_key(path.as_str()))
            {
                if !removed.iter().zip(added.iter()).any(|((old, old_modified), (new, new_modified))| old == path && old_modified == new_modified) { persist(&database, "file_deleted", path, 0); }
            }
            previous = current;
            thread::sleep(Duration::from_secs(2));
        }
    })
}
fn snapshot(folders: &[String], excluded_paths: &[String]) -> HashMap<String, u128> {
    let mut result = HashMap::new();
    for folder in folders {
        collect_files(Path::new(folder), excluded_paths, &mut result);
    }
    result
}
fn collect_files(path: &Path, excluded_paths: &[String], result: &mut HashMap<String, u128>) {
    if excluded_paths.iter().any(|excluded| path.to_string_lossy().to_ascii_lowercase().contains(&excluded.to_ascii_lowercase())) { return; }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, excluded_paths, result);
        } else if let Ok(metadata) = entry.metadata() {
            let modified = metadata
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            result.insert(path.to_string_lossy().to_string(), modified);
        }
    }
}
fn persist(database: &Arc<Mutex<Database>>, event_type: &str, path: &str, modified: u128) {
    let event = RawEvent {
        id: Uuid::new_v4().to_string(),
        timestamp_ns: Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        event_type: event_type.into(),
        source: "filesystem_polling".into(),
        app_name: None,
        executable_path: None,
        process_id: None,
        window_handle: None,
        window_title: None,
        element_name: None,
        text: None,
        file_path: Some(path.into()),
        metadata_json: format!("{{\"modified_ns\":{modified}}}"),
        privacy_class: "filesystem_metadata".into(),
        confidence: 1.0,
        created_at: Utc::now().to_rfc3339(),
    };
    if let Ok(database) = database.lock() {
        let _ = database.insert_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::snapshot;
    use std::fs;

    #[test]
    fn snapshot_recursively_finds_files_and_ignores_missing_folders() {
        let root = std::env::temp_dir().join(format!("chronicle-fs-{}", std::process::id()));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("note.txt");
        fs::write(&file, "hello").unwrap();

        let result = snapshot(&[root.to_string_lossy().into_owned(), "missing-folder".into()], &["nested/skip".into()]);
        assert!(result.contains_key(&file.to_string_lossy().to_string()));
        assert!(result.values().all(|modified| *modified > 0));

        fs::remove_dir_all(root).unwrap();
    }
}
