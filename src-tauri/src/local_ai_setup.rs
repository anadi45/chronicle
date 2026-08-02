//! One-time local AI setup, driven from inside the app.
//!
//! Chronicle's semantic analysis and embeddings run on a bundled llama.cpp
//! engine (`llama-server`) rather than a separately installed application:
//! nothing here shows up in the Start Menu, the system tray, or Windows'
//! installed-apps list. This module downloads the `llama-server` binary
//! (from llama.cpp's own GitHub releases) and the GGUF model files (Gemma 3
//! for chat/vision, EmbeddingGemma for embeddings, both from their official
//! Hugging Face repos) into `%LOCALAPPDATA%\Chronicle\llama`, starts/stops
//! the two local servers, and removes any of those files again on request.
//! Every step is UI-triggered and streams real, byte-accurate progress back
//! as `llama-setup-progress` events (also mirrored to `tracing`, so the same
//! information is visible in the `npm run dev` terminal and the app UI).
//! Nothing here runs automatically or silently in the background.

use crate::local_model_provider::{engine_paths, shared_agent, LlamaCppProvider};
use crate::tauri_application_commands::AppState;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize)]
pub struct LlamaSetupStatus {
    pub runtime_installed: bool,
    pub chat_model_installed: bool,
    pub embed_model_installed: bool,
    pub chat_running: bool,
    pub embed_running: bool,
    pub chat_model_name: String,
    pub embed_model_name: String,
}

fn setup_status_blocking() -> LlamaSetupStatus {
    let engine = LlamaCppProvider::default();
    LlamaSetupStatus {
        runtime_installed: engine_paths::runtime_installed(),
        chat_model_installed: engine_paths::chat_model_installed(),
        embed_model_installed: engine_paths::embed_model_installed(),
        chat_running: engine.chat_reachable(),
        embed_running: engine.embed_reachable(),
        chat_model_name: engine.chat_model.clone(),
        embed_model_name: engine.embedding_model.clone(),
    }
}

#[tauri::command]
pub async fn local_ai_setup_status() -> LlamaSetupStatus {
    tauri::async_runtime::spawn_blocking(setup_status_blocking)
        .await
        .unwrap_or(LlamaSetupStatus {
            runtime_installed: false,
            chat_model_installed: false,
            embed_model_installed: false,
            chat_running: false,
            embed_running: false,
            chat_model_name: String::new(),
            embed_model_name: String::new(),
        })
}

#[derive(Debug, Clone, Serialize)]
struct DownloadProgress {
    label: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    percent: Option<f32>,
}

fn emit_download_progress(
    app: &AppHandle,
    label: &str,
    downloaded: u64,
    total: Option<u64>,
    force: bool,
    last_emit: &mut Instant,
) {
    if !force && last_emit.elapsed() < Duration::from_millis(200) {
        return;
    }
    *last_emit = Instant::now();
    let percent = total
        .filter(|&total| total > 0)
        .map(|total| (downloaded as f64 / total as f64 * 100.0) as f32);
    let percent_display = percent
        .map(|percent| format!(" ({percent:.0}%)"))
        .unwrap_or_default();
    tracing::info!(target: "chronicle::local_ai_setup", "{label}: {downloaded} bytes{percent_display}");
    let _ = app.emit(
        "llama-setup-progress",
        DownloadProgress {
            label: label.to_string(),
            downloaded_bytes: downloaded,
            total_bytes: total,
            percent,
        },
    );
}

/// Streams `url` to `dest`, byte by byte, emitting real (not estimated)
/// progress from the response's `Content-Length` header. Writes to a
/// `.part` sibling file and renames on success, so a failed/cancelled
/// download never leaves a file that looks complete but isn't.
fn download_with_progress(app: &AppHandle, label: &str, url: &str, dest: &Path) -> Result<(), String> {
    tracing::info!(target: "chronicle::local_ai_setup", "downloading {label} from {url}");
    let response = shared_agent()
        .get(url)
        .call()
        .map_err(|error| format!("failed to start download of {label}: {error}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let mut reader = response.into_body().into_reader();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let tmp_path = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp_path)
        .map_err(|error| format!("failed to create {}: {error}", tmp_path.display()))?;
    let mut buffer = [0u8; 65536];
    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("download of {label} was interrupted: {error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("failed writing {label}: {error}"))?;
        downloaded += read as u64;
        emit_download_progress(app, label, downloaded, total, false, &mut last_emit);
    }
    emit_download_progress(app, label, downloaded, total, true, &mut last_emit);
    drop(file);
    std::fs::rename(&tmp_path, dest)
        .map_err(|error| format!("failed to finalize {label}: {error}"))?;
    tracing::info!(target: "chronicle::local_ai_setup", "{label} downloaded ({downloaded} bytes)");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}
#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Finds the Windows CPU build in llama.cpp's latest GitHub release. CPU-only
/// is the safe universal default — every Windows machine can run it — at the
/// cost of speed; picking a CUDA/Vulkan build automatically based on detected
/// hardware is tracked as follow-up work, not attempted here.
fn find_windows_cpu_asset() -> Result<GithubAsset, String> {
    let url = "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest";
    let mut response = shared_agent()
        .get(url)
        .header("User-Agent", "Chronicle-App")
        .call()
        .map_err(|error| format!("failed to query llama.cpp releases: {error}"))?;
    let payload = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("invalid llama.cpp release response: {error}"))?;
    let release: GithubRelease = serde_json::from_str(&payload)
        .map_err(|error| format!("invalid llama.cpp release JSON: {error}"))?;
    release
        .assets
        .into_iter()
        .find(|asset| asset.name.contains("bin-win-cpu-x64") && asset.name.ends_with(".zip"))
        .ok_or_else(|| format!("no Windows CPU build found in llama.cpp release {}", release.tag_name))
}

/// Extracts every file in `zip_path` into `dest_dir`. llama.cpp's release
/// zips sometimes wrap their contents in a single top-level folder and
/// sometimes don't; this strips one leading path component only when an
/// entry actually has one, so both layouts land the binary and its DLLs
/// directly under `dest_dir`.
fn extract_zip(zip_path: &Path, dest_dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|error| format!("failed to open downloaded archive: {error}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| format!("invalid archive: {error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("invalid archive entry: {error}"))?;
        let Some(entry_path) = entry.enclosed_name() else {
            continue;
        };
        let mut components: Vec<_> = entry_path.components().collect();
        if components.len() > 1 {
            components.remove(0);
        }
        let relative: std::path::PathBuf = components.iter().collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        let out_path = dest_dir.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|error| format!("failed to create {}: {error}", out_path.display()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        let mut out_file = std::fs::File::create(&out_path)
            .map_err(|error| format!("failed to write {}: {error}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out_file)
            .map_err(|error| format!("failed extracting {}: {error}", out_path.display()))?;
    }
    Ok(())
}

/// Downloads and extracts the `llama-server` runtime (binary + DLLs).
#[tauri::command]
pub async fn setup_download_runtime(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let asset = find_windows_cpu_asset()?;
        let zip_path = engine_paths::runtime_dir().join("llama-runtime.part.zip");
        download_with_progress(
            &app,
            "llama.cpp runtime",
            &asset.browser_download_url,
            &zip_path,
        )?;
        extract_zip(&zip_path, &engine_paths::runtime_dir())?;
        let _ = std::fs::remove_file(&zip_path);
        if !engine_paths::runtime_installed() {
            return Err(
                "llama-server.exe was not found after extracting the downloaded archive".into(),
            );
        }
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Downloads the Gemma 3 chat/vision model and its multimodal projector.
#[tauri::command]
pub async fn setup_download_chat_model(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        download_with_progress(
            &app,
            "Gemma 3 chat model",
            engine_paths::CHAT_MODEL_URL,
            &engine_paths::chat_model(),
        )?;
        download_with_progress(
            &app,
            "Gemma 3 vision projector",
            engine_paths::MMPROJ_URL,
            &engine_paths::mmproj(),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Downloads the EmbeddingGemma model.
#[tauri::command]
pub async fn setup_download_embed_model(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        download_with_progress(
            &app,
            "EmbeddingGemma model",
            engine_paths::EMBED_MODEL_URL,
            &engine_paths::embed_model(),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Starts both local servers (chat/vision and embedding) if their files are
/// present and they aren't already listening, registering any child this
/// call starts in `AppState` so it's stopped the same way as one started at
/// application launch (see `shutdown_llama_engine`).
#[tauri::command]
pub async fn setup_start_engine(state: State<'_, AppState>) -> Result<(), String> {
    let engine = LlamaCppProvider::default();
    let chat_engine = engine.clone();
    let chat_child = tauri::async_runtime::spawn_blocking(move || chat_engine.start_chat_server_if_needed())
        .await
        .map_err(|error| error.to_string())??;
    if let Some(child) = chat_child {
        if let Ok(mut slot) = state.llama_chat_process.lock() {
            if slot.is_none() {
                *slot = Some(child);
            }
        }
    }
    let embed_engine = engine.clone();
    let embed_child = tauri::async_runtime::spawn_blocking(move || embed_engine.start_embed_server_if_needed())
        .await
        .map_err(|error| error.to_string())??;
    if let Some(child) = embed_child {
        if let Ok(mut slot) = state.llama_embed_process.lock() {
            if slot.is_none() {
                *slot = Some(child);
            }
        }
    }
    Ok(())
}

fn stop_process(slot: &std::sync::Mutex<Option<std::process::Child>>) {
    if let Ok(mut process_slot) = slot.lock() {
        if let Some(mut process) = process_slot.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to remove {}: {error}", path.display())),
    }
}

/// Removes the `llama-server` runtime. Stops both servers first: on Windows
/// a running process keeps its own executable file locked, so deleting it
/// out from under a live server would fail.
#[tauri::command]
pub async fn setup_remove_runtime(state: State<'_, AppState>) -> Result<(), String> {
    stop_process(&state.llama_chat_process);
    stop_process(&state.llama_embed_process);
    tracing::info!(target: "chronicle::local_ai_setup", "removing llama.cpp runtime");
    tauri::async_runtime::spawn_blocking(|| {
        match std::fs::remove_dir_all(engine_paths::runtime_dir()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("failed to remove the llama.cpp runtime: {error}")),
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Removes the Gemma 3 chat/vision model and its projector. Stops the chat
/// server first for the same file-locking reason as `setup_remove_runtime`.
#[tauri::command]
pub async fn setup_remove_chat_model(state: State<'_, AppState>) -> Result<(), String> {
    stop_process(&state.llama_chat_process);
    tracing::info!(target: "chronicle::local_ai_setup", "removing Gemma 3 chat model");
    tauri::async_runtime::spawn_blocking(|| {
        remove_file_if_exists(&engine_paths::chat_model())?;
        remove_file_if_exists(&engine_paths::mmproj())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Removes the EmbeddingGemma model. Stops the embedding server first for
/// the same file-locking reason as `setup_remove_runtime`.
#[tauri::command]
pub async fn setup_remove_embed_model(state: State<'_, AppState>) -> Result<(), String> {
    stop_process(&state.llama_embed_process);
    tracing::info!(target: "chronicle::local_ai_setup", "removing EmbeddingGemma model");
    tauri::async_runtime::spawn_blocking(|| remove_file_if_exists(&engine_paths::embed_model()))
        .await
        .map_err(|error| error.to_string())?
}
