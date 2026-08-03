//! Where Chronicle stores its data — chosen by the user on first run.
//!
//! Everything Chronicle writes to disk (the sqlite event database, downloaded
//! llama.cpp runtime + model files) lives under a single user-chosen "data
//! directory" rather than being scattered directly into the install folder or
//! a fixed `%LOCALAPPDATA%` path the user never sees or agreed to. The choice
//! itself is remembered in a small pointer file under `%APPDATA%\Chronicle`
//! (fixed — this is just a pointer, not the data itself), so the picker only
//! ever runs once.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn pointer_file() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Chronicle").join("data_dir.txt")
}

fn default_data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Chronicle").join("Data")
}

fn read_pointer() -> Option<PathBuf> {
    let contents = std::fs::read_to_string(pointer_file()).ok()?;
    let path = PathBuf::from(contents.trim());
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn write_pointer(path: &Path) -> std::io::Result<()> {
    let pointer = pointer_file();
    if let Some(parent) = pointer.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(pointer, path.to_string_lossy().as_bytes())
}

/// Resolves the data directory, asking the user to pick one (via a native
/// folder-choose dialog) the first time Chronicle runs. If the user cancels
/// the picker, falls back to a sensible default under `%LOCALAPPDATA%` rather
/// than blocking startup entirely.
///
/// Runs before the Tauri event loop starts, so it uses `rfd`'s standalone
/// blocking dialog (no window handle required) instead of the
/// window-scoped `tauri-plugin-dialog`.
fn resolve_data_dir() -> PathBuf {
    if let Some(existing) = read_pointer() {
        if existing.is_dir() {
            return existing;
        }
        // Pointer exists but the directory itself is gone (e.g. removable
        // drive unplugged, folder deleted); fall through and ask again.
    }

    let chosen = rfd::FileDialog::new()
        .set_title("Choose a folder for Chronicle to store its data and downloaded models")
        .pick_folder()
        .unwrap_or_else(default_data_dir);

    if let Err(error) = std::fs::create_dir_all(&chosen) {
        tracing::error!(%error, path = %chosen.display(), "failed to create chosen data directory; falling back to default");
        let fallback = default_data_dir();
        let _ = std::fs::create_dir_all(&fallback);
        let _ = write_pointer(&fallback);
        return fallback;
    }

    let _ = write_pointer(&chosen);
    chosen
}

/// The user-chosen (or default-fallback) root directory Chronicle stores all
/// of its data under. Resolved once per process and cached.
pub fn data_dir() -> &'static Path {
    static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
    DATA_DIR.get_or_init(resolve_data_dir)
}

/// Path to the sqlite event database, under the chosen data directory.
pub fn database_file() -> PathBuf {
    data_dir().join("chronicle.db")
}
