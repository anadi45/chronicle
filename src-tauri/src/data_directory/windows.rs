//! Windows-specific location for the pointer file that remembers the
//! user-chosen data directory. This is just a pointer — a few bytes of text
//! — not the data itself, so it lives in the fixed per-user `%APPDATA%`
//! location rather than needing a choice of its own.

use std::path::PathBuf;

pub(super) fn pointer_file() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Chronicle").join("data_dir.txt")
}
