//! macOS-specific location for the pointer file that remembers the
//! user-chosen data directory. Reserved for a future macOS build, mirroring
//! `windows.rs`.

use std::path::PathBuf;

pub(super) fn pointer_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Chronicle")
        .join("data_dir.txt")
}
