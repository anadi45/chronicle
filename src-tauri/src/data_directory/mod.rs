//! Where Chronicle stores its data — chosen explicitly by the user.
//!
//! Everything Chronicle writes to disk (the sqlite event database, the
//! downloaded llama.cpp model files) lives under this one directory instead
//! of being scattered into the install folder or a fixed path the user never
//! agreed to. There is deliberately no default: if no directory has been
//! chosen yet (no pointer file, or the previously chosen folder no longer
//! exists), Chronicle blocks startup on a folder-choose dialog and asks
//! again on cancel rather than silently picking one for the user. The
//! choice itself is remembered in a small pointer file whose OS location is
//! platform-specific (see `windows.rs`, `mac.rs`).
//!
//! Whatever folder the user picks, Chronicle never writes directly into it:
//! it creates (and, for both storage and retrieval, only ever operates on) a
//! `chronicle` subfolder underneath. The picked folder is often a general
//! one — a user's existing "Data" or "Documents" drive root, say — and
//! writing loose files straight into it, or later deleting siblings the
//! user didn't expect deleted, would be careless.

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

const CHRONICLE_SUBFOLDER: &str = "chronicle";

fn read_pointer() -> Option<PathBuf> {
    let contents = std::fs::read_to_string(platform::pointer_file()).ok()?;
    let path = PathBuf::from(contents.trim());
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn write_pointer(root: &Path) -> std::io::Result<()> {
    let pointer = platform::pointer_file();
    if let Some(parent) = pointer.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(pointer, root.to_string_lossy().as_bytes())
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
        if std::fs::create_dir_all(chronicle_subfolder(&chosen)).is_ok() {
            return chosen;
        }
        tracing::error!(path = %chosen.display(), "failed to create the chosen data directory; choose another");
    }
}

fn chronicle_subfolder(root: &Path) -> PathBuf {
    root.join(CHRONICLE_SUBFOLDER)
}

fn resolve_data_dir() -> PathBuf {
    let root = if let Some(existing_root) = read_pointer() {
        if chronicle_subfolder(&existing_root).is_dir() {
            existing_root
        } else {
            tracing::warn!(path = %existing_root.display(), "previously chosen data directory no longer exists; asking again");
            let chosen = ask_user_to_choose();
            if let Err(error) = write_pointer(&chosen) {
                tracing::error!(%error, "failed to remember the chosen data directory; Chronicle will ask again next launch");
            }
            chosen
        }
    } else {
        let chosen = ask_user_to_choose();
        if let Err(error) = write_pointer(&chosen) {
            tracing::error!(%error, "failed to remember the chosen data directory; Chronicle will ask again next launch");
        }
        chosen
    };
    chronicle_subfolder(&root)
}

/// The `chronicle` subfolder under the user-chosen root directory — every
/// file Chronicle stores lives under here. Resolved once per process
/// (asking the user if necessary) and cached.
pub fn data_dir() -> &'static Path {
    static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
    DATA_DIR.get_or_init(resolve_data_dir)
}

/// Path to the sqlite event database, under the chosen data directory.
pub fn database_file() -> PathBuf {
    data_dir().join("chronicle.db")
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Total size, in bytes, of every file under `path`.
fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => directory_size(&entry.path()),
            Ok(file_type) if file_type.is_file() => {
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            }
            _ => 0,
        })
        .sum()
}

/// Recursively copies every entry under `src` into `dest` (which must
/// already exist), preserving relative structure, reporting cumulative
/// bytes copied so far against `total` after every file.
fn copy_dir_recursive(
    src: &Path,
    dest: &Path,
    copied: &mut u64,
    total: u64,
    on_progress: &mut dyn FnMut(u64, u64),
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_dir_recursive(&entry.path(), &target, copied, total, on_progress)?;
        } else if file_type.is_file() {
            *copied += std::fs::copy(entry.path(), &target)?;
            on_progress(*copied, total);
        }
    }
    Ok(())
}

/// Moves Chronicle's data from the current data directory into a
/// `chronicle` subfolder under `new_root` (the user-picked destination —
/// same "must be a real, explicitly chosen path, no default" rule as
/// first-run resolution) and remembers `new_root` as the new choice.
///
/// Checks free space at the destination before copying a single byte, so a
/// too-small target fails fast with a clear message instead of partway
/// through a multi-gigabyte copy. Copies rather than renames (a rename
/// fails outright across drives, which is a completely ordinary choice
/// here — e.g. moving from `C:` to a `D:` data disk) and only removes the
/// old copy after every file has landed safely in the new location. The
/// caller is responsible for having stopped anything that holds these files
/// open (capture threads, the llama.cpp servers, the database connection)
/// before calling this — copying files still being written by a live
/// connection would race.
pub fn relocate(new_root: &Path, mut on_progress: impl FnMut(u64, u64)) -> Result<(), String> {
    if new_root.as_os_str().is_empty() {
        return Err("no destination directory was provided".into());
    }
    let current = data_dir().to_path_buf();
    let dest = chronicle_subfolder(new_root);
    if dest == current {
        return Ok(());
    }
    std::fs::create_dir_all(&dest)
        .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;

    let total = directory_size(&current);
    if let Some(available) = platform::available_space(new_root) {
        if available < total {
            return Err(format!(
                "the chosen folder doesn't have enough free space: needs {}, only {} available",
                format_bytes(total),
                format_bytes(available)
            ));
        }
    }

    copy_dir_recursive(&current, &dest, &mut 0, total, &mut on_progress)
        .map_err(|error| format!("failed to copy data to {}: {error}", dest.display()))?;
    write_pointer(new_root)
        .map_err(|error| format!("failed to remember the new data directory: {error}"))?;
    if let Err(error) = std::fs::remove_dir_all(&current) {
        tracing::warn!(%error, path = %current.display(), "moved data to the new directory but failed to remove the old copy; remove it manually if disk space matters");
    }
    Ok(())
}
