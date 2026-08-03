//! Where Chronicle stores its data — chosen explicitly by the user.
//!
//! Everything Chronicle writes to disk (the sqlite event database, the
//! downloaded llama.cpp runtime and model files) lives under this one
//! directory instead of being scattered into the install folder or a fixed
//! path the user never agreed to. There is deliberately no default: if no
//! directory has been chosen yet (no pointer file, or the previously chosen
//! folder no longer exists), Chronicle blocks startup on a folder-choose
//! dialog and asks again on cancel rather than silently picking one for the
//! user. The choice itself is remembered in a small pointer file whose OS
//! location is platform-specific (see `windows.rs`, `mac.rs`).

#[cfg(not(windows))]
mod mac;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
use windows as platform;
#[cfg(not(windows))]
use mac as platform;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn read_pointer() -> Option<PathBuf> {
    let contents = std::fs::read_to_string(platform::pointer_file()).ok()?;
    let path = PathBuf::from(contents.trim());
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn write_pointer(path: &Path) -> std::io::Result<()> {
    let pointer = platform::pointer_file();
    if let Some(parent) = pointer.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(pointer, path.to_string_lossy().as_bytes())
}

/// Blocks on a native folder-choose dialog until the user picks a directory
/// Chronicle can actually create/use. Cancelling asks again — there is no
/// fallback default, since a silently-chosen location is exactly what the
/// user directory exists to avoid.
fn ask_user_to_choose() -> PathBuf {
    loop {
        let Some(chosen) = rfd::FileDialog::new()
            .set_title("Chronicle needs a folder to store its data and downloaded models — choose one to continue")
            .pick_folder()
        else {
            tracing::warn!("no data directory chosen; Chronicle cannot start without one — asking again");
            continue;
        };
        if std::fs::create_dir_all(&chosen).is_ok() {
            return chosen;
        }
        tracing::error!(path = %chosen.display(), "failed to create the chosen data directory; choose another");
    }
}

fn resolve_data_dir() -> PathBuf {
    if let Some(existing) = read_pointer() {
        if existing.is_dir() {
            return existing;
        }
        tracing::warn!(path = %existing.display(), "previously chosen data directory no longer exists; asking again");
    }
    let chosen = ask_user_to_choose();
    if let Err(error) = write_pointer(&chosen) {
        tracing::error!(%error, "failed to remember the chosen data directory; Chronicle will ask again next launch");
    }
    chosen
}

/// The user-chosen root directory Chronicle stores all of its data under.
/// Resolved once per process (asking the user if necessary) and cached.
pub fn data_dir() -> &'static Path {
    static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
    DATA_DIR.get_or_init(resolve_data_dir)
}

/// Path to the sqlite event database, under the chosen data directory.
pub fn database_file() -> PathBuf {
    data_dir().join("chronicle.db")
}

/// Recursively copies every entry under `src` into `dest` (which must
/// already exist), preserving relative structure.
fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Moves Chronicle's data from the current data directory to `new_dir` and
/// remembers `new_dir` as the new choice. `new_dir` must be a real,
/// explicitly chosen path — same "no default" rule as first-run resolution.
///
/// Copies rather than renames (a rename fails outright across drives, which
/// is a completely ordinary choice here — e.g. moving from `C:` to a `D:`
/// data disk) and only removes the old copy after every file has landed
/// safely in the new location. The caller is responsible for having stopped
/// anything that holds these files open (capture threads, the llama.cpp
/// servers, the database connection) before calling this — copying files
/// still being written by a live connection would race.
pub fn relocate(new_dir: &Path) -> Result<(), String> {
    if new_dir.as_os_str().is_empty() {
        return Err("no destination directory was provided".into());
    }
    let current = data_dir().to_path_buf();
    if new_dir == current {
        return Ok(());
    }
    std::fs::create_dir_all(new_dir)
        .map_err(|error| format!("failed to create {}: {error}", new_dir.display()))?;
    copy_dir_recursive(&current, new_dir)
        .map_err(|error| format!("failed to copy data to {}: {error}", new_dir.display()))?;
    write_pointer(new_dir)
        .map_err(|error| format!("failed to remember the new data directory: {error}"))?;
    if let Err(error) = std::fs::remove_dir_all(&current) {
        tracing::warn!(%error, path = %current.display(), "moved data to the new directory but failed to remove the old copy; remove it manually if disk space matters");
    }
    Ok(())
}
