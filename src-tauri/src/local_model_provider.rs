//! Local inference via a bundled `llama-server` (llama.cpp) engine.
//!
//! Chronicle runs two `llama-server` instances on localhost — one serving a
//! Gemma 3 chat/vision model for semantic analysis, one serving EmbeddingGemma
//! for embeddings — rather than depending on a separately installed
//! application. Both the `llama-server` binary and the GGUF model files live
//! under `<data dir>\llama` (see `engine_paths`), where `<data dir>` is the
//! folder the user chose on first run (see `data_directory`), and are
//! downloaded once by `local_inference_setup`; nothing here downloads anything
//! itself. Both servers speak llama.cpp's OpenAI-compatible HTTP API
//! (`/v1/chat/completions` for text and vision, `/v1/embeddings` for
//! embeddings), so this module is a thin, stable HTTP client over that API,
//! not a reimplementation of inference itself.

use crate::embedding_provider::TextEmbedder;
use crate::local_semantic_processing::{
    parse_and_validate_model_json, validate_image_input, LocalSemanticAnalyzer, SemanticModelOutput,
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

/// Context window size (in tokens) both bundled servers are started with.
/// `analyze_text_batch` concatenates up to `MAX_MODEL_BATCH_SIZE` event
/// contexts plus `analyze_image`'s base64-embedded screenshots into a single
/// prompt; llama.cpp's own default context (often 4096 or the model's
/// training context) is not reliably large enough for that, and a prompt
/// that overflows it is rejected outright rather than truncated.
const SERVER_CONTEXT_SIZE: u32 = 8192;

/// Upper bound on generated tokens per chat/vision request. Without this,
/// `n_predict` defaults to `-1` (generate until end-of-sequence or the
/// context fills), so a single request the model can't naturally terminate
/// — a common failure mode for small quantized models asked for strict JSON
/// — pins one of the server's slots and the calling worker thread for as
/// long as the whole remaining context takes to fill. Capping it bounds
/// worst-case per-task latency, which is what keeps the queue moving under
/// load rather than stalling behind one bad generation. The structured
/// output this provider asks for (category/summary/entities/relationships/
/// confidence, optionally for up to `MAX_MODEL_BATCH_SIZE` items) fits well
/// inside this budget.
const MAX_RESPONSE_TOKENS: u32 = 1024;

/// Opens (creating/truncating) a log file under `<data dir>\llama\logs` for a
/// spawned server's stdout/stderr. Both streams were previously discarded
/// (`Stdio::null()`), which made every startup and inference failure from
/// `llama-server.exe` invisible — the process stays up and its port stays
/// reachable even when, for example, it can't apply the model's chat
/// template, so `chat_reachable()` reports healthy while every real request
/// fails. Logging to a file makes that diagnosable without changing the
/// "pending, not failed" behavior when the engine isn't installed yet.
/// Returns one `Stdio` per stream (stdout, stderr), both appending to the
/// same log file, so interleaved output stays in one place per server.
fn open_server_log(name: &str) -> (Stdio, Stdio) {
    let Some(path) = server_log_path(name) else {
        return (Stdio::null(), Stdio::null());
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match File::create(&path).and_then(|file| Ok((file.try_clone()?, file))) {
        Ok((out, err)) => (Stdio::from(out), Stdio::from(err)),
        Err(_) => (Stdio::null(), Stdio::null()),
    }
}

/// Builds the argument list for the chat/vision `llama-server`, factored out
/// so the exact flags can be asserted on in tests without spawning a real
/// process. `--jinja` enables llama.cpp's Jinja chat-template engine, which
/// Gemma 3's chat template requires — without it, `/v1/chat/completions`
/// fails (empty/unsupported template) even though the server process stays
/// up and the port stays reachable, which is what made this failure
/// invisible before. `-c` raises the context window past llama.cpp's
/// default so a batched multi-event prompt (see `analyze_text_batch`) or an
/// embedded screenshot doesn't get rejected for overflowing it.
fn chat_server_args(chat_model: &Path, mmproj: &Path, host: &str, port: u16) -> Vec<String> {
    vec![
        "-m".into(),
        chat_model.to_string_lossy().into_owned(),
        "--mmproj".into(),
        mmproj.to_string_lossy().into_owned(),
        "--host".into(),
        host.into(),
        "--port".into(),
        port.to_string(),
        "--jinja".into(),
        "-c".into(),
        SERVER_CONTEXT_SIZE.to_string(),
        "-t".into(),
        inference_thread_count().to_string(),
    ]
}

/// Threads llama.cpp is told to use for generation. llama.cpp's own default
/// (`-1`) already resolves to the host's core count, but pinning it
/// explicitly makes the number visible/tunable here instead of buried in
/// the engine's own heuristics, and avoids over-subscribing on hybrid
/// (performance + efficiency core) CPUs where llama.cpp's auto-detection is
/// not always the count you'd actually pick. One core is held back for the
/// rest of Chronicle (capture hooks, the Tauri UI thread, SQLite) so local
/// inference never fully starves the app it's running inside.
fn inference_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(4)
}

/// Builds the argument list for the embedding `llama-server`. See
/// `chat_server_args` for why `--jinja` and `-c` are present; `--jinja` is
/// harmless for a pure-embedding model but keeps the two spawn paths
/// consistent, and `-c` matters here too since `embed_batch` sends one
/// request per batch of inputs.
fn embed_server_args(embed_model: &Path, host: &str, port: u16) -> Vec<String> {
    vec![
        "-m".into(),
        embed_model.to_string_lossy().into_owned(),
        "--host".into(),
        host.into(),
        "--port".into(),
        port.to_string(),
        "--embeddings".into(),
        "-c".into(),
        SERVER_CONTEXT_SIZE.to_string(),
        "-t".into(),
        inference_thread_count().to_string(),
    ]
}

fn server_log_path(name: &str) -> Option<PathBuf> {
    Some(
        crate::data_directory::current()?
            .join("llama")
            .join("logs")
            .join(format!("{name}.log")),
    )
}

/// Where the bundled engine (binary + models) lives and what its pieces are
/// named. A single source of truth shared by the provider (to run
/// inference) and `local_inference_setup` (to download/remove these same files).
pub mod engine_paths {
    use std::path::PathBuf;

    /// Display name for the chat/vision model file — also its filename.
    ///
    /// Sourced from `bartowski`'s GGUF re-upload rather than Google's
    /// official `google/gemma-3-4b-it-qat-q4_0-gguf` repo: Google's repo is
    /// access-gated (requires a Hugging Face login and accepting a license
    /// agreement), which returns HTTP 401 for the anonymous download this
    /// setup flow does. `bartowski`'s re-upload of the same weights is
    /// openly downloadable and is the community-standard mirror llama.cpp
    /// users are pointed to for exactly this reason.
    pub const CHAT_MODEL_FILE: &str = "google_gemma-3-4b-it-Q4_K_M.gguf";
    /// Multimodal projector required alongside the chat model for vision input.
    pub const MMPROJ_FILE: &str = "mmproj-google_gemma-3-4b-it-f16.gguf";
    /// Display name for the embedding model file — also its filename.
    pub const EMBED_MODEL_FILE: &str = "embeddinggemma-300M-Q8_0.gguf";

    pub const CHAT_MODEL_URL: &str = "https://huggingface.co/bartowski/google_gemma-3-4b-it-GGUF/resolve/main/google_gemma-3-4b-it-Q4_K_M.gguf";
    pub const MMPROJ_URL: &str = "https://huggingface.co/bartowski/google_gemma-3-4b-it-GGUF/resolve/main/mmproj-google_gemma-3-4b-it-f16.gguf";
    pub const EMBED_MODEL_URL: &str = "https://huggingface.co/ggml-org/embeddinggemma-300M-GGUF/resolve/main/embeddinggemma-300M-Q8_0.gguf";

    /// `None` until the user has chosen a data directory from Settings —
    /// there is nowhere to put model files yet.
    fn base_dir() -> Option<PathBuf> {
        Some(crate::data_directory::current()?.join("llama"))
    }

    /// Where the `llama-server` binary and its DLLs live. Unlike the model
    /// weights below, the engine itself is bundled into the app install
    /// (see `tauri.conf.json`'s `bundle.resources` and
    /// `src-tauri/resources/llama/`) rather than downloaded at runtime, so
    /// this looks next to the running executable instead of under the data
    /// directory. Falls back to the source tree's `resources/llama` when
    /// running un-bundled (`cargo run` / `tauri dev`), where Tauri doesn't
    /// copy resources next to the dev binary.
    pub fn runtime_dir() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                for candidate in [exe_dir.join("llama"), exe_dir.join("resources").join("llama")] {
                    if candidate.join("llama-server.exe").is_file() {
                        return candidate;
                    }
                }
            }
        }
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("llama")
    }
    pub fn models_dir() -> Option<PathBuf> {
        Some(base_dir()?.join("models"))
    }
    pub fn server_binary() -> PathBuf {
        runtime_dir().join("llama-server.exe")
    }
    pub fn chat_model() -> Option<PathBuf> {
        Some(models_dir()?.join(CHAT_MODEL_FILE))
    }
    pub fn mmproj() -> Option<PathBuf> {
        Some(models_dir()?.join(MMPROJ_FILE))
    }
    pub fn embed_model() -> Option<PathBuf> {
        Some(models_dir()?.join(EMBED_MODEL_FILE))
    }
    pub fn runtime_installed() -> bool {
        server_binary().is_file()
    }
    pub fn chat_model_installed() -> bool {
        chat_model().is_some_and(|path| path.is_file()) && mmproj().is_some_and(|path| path.is_file())
    }
    pub fn embed_model_installed() -> bool {
        embed_model().is_some_and(|path| path.is_file())
    }
}

/// One keep-alive `ureq` agent shared by every provider instance and every
/// worker thread. Reusing pooled connections instead of opening a fresh TCP
/// connection per inference call removes a full connect + slow-start round
/// trip from every request, and `ureq` correctly handles chunked transfer
/// encoding and HTTP status codes instead of guessing from a raw byte split.
pub(crate) fn shared_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(2)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .build()
            .into()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelStatus {
    pub chat_endpoint: String,
    pub embedding_endpoint: String,
    pub chat_model: String,
    pub embedding_model: String,
    pub chat_available: bool,
    pub embedding_available: bool,
}

#[derive(Debug, Clone)]
pub struct LlamaCppProvider {
    pub host: String,
    pub chat_port: u16,
    pub embed_port: u16,
    pub chat_model: String,
    pub embedding_model: String,
}

impl Default for LlamaCppProvider {
    fn default() -> Self {
        Self {
            host: std::env::var("CHRONICLE_LLAMA_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            chat_port: std::env::var("CHRONICLE_LLAMA_CHAT_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(8090),
            embed_port: std::env::var("CHRONICLE_LLAMA_EMBED_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(8091),
            chat_model: engine_paths::CHAT_MODEL_FILE.to_string(),
            embedding_model: engine_paths::EMBED_MODEL_FILE.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}
#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}
#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}
#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}
#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}
#[derive(Debug, Deserialize)]
struct BatchSemanticResponse {
    results: Vec<BatchSemanticItem>,
}
#[derive(Debug, Deserialize)]
struct BatchSemanticItem {
    index: usize,
    category: String,
    summary: String,
    entities: Vec<String>,
    relationships: Vec<String>,
    confidence: f32,
}

impl LlamaCppProvider {
    fn socket_address(host: &str, port: u16) -> Result<SocketAddr, String> {
        (host, port)
            .to_socket_addrs()
            .map_err(|error| format!("invalid llama.cpp endpoint {host}:{port}: {error}"))?
            .next()
            .ok_or_else(|| format!("llama.cpp endpoint {host}:{port} unavailable"))
    }

    fn is_port_reachable(host: &str, port: u16) -> bool {
        Self::socket_address(host, port)
            .map(|address| TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok())
            .unwrap_or(false)
    }

    pub fn chat_reachable(&self) -> bool {
        Self::is_port_reachable(&self.host, self.chat_port)
    }
    pub fn embed_reachable(&self) -> bool {
        Self::is_port_reachable(&self.host, self.embed_port)
    }

    /// Starts the chat/vision `llama-server` if the binary and model files
    /// are present and it isn't already listening. Returns `Ok(None)` (not
    /// an error) when setup isn't complete yet — capture and the rest of
    /// Chronicle must keep working with local AI simply pending setup.
    pub fn start_chat_server_if_needed(&self) -> Result<Option<Child>, String> {
        if self.chat_reachable() {
            return Ok(None);
        }
        if !engine_paths::runtime_installed() || !engine_paths::chat_model_installed() {
            return Ok(None);
        }
        let (Some(chat_model), Some(mmproj)) = (engine_paths::chat_model(), engine_paths::mmproj())
        else {
            return Ok(None);
        };
        let (out, err) = open_server_log("chat-server");
        Command::new(engine_paths::server_binary())
            .args(chat_server_args(&chat_model, &mmproj, &self.host, self.chat_port))
            .stdout(out)
            .stderr(err)
            .spawn()
            .map(Some)
            .map_err(|error| format!("unable to start the chat/vision engine: {error}"))
    }

    /// Starts the embedding `llama-server` if the binary and model file are
    /// present and it isn't already listening. Same "pending, not failed"
    /// behavior as `start_chat_server_if_needed` when setup isn't complete.
    pub fn start_embed_server_if_needed(&self) -> Result<Option<Child>, String> {
        if self.embed_reachable() {
            return Ok(None);
        }
        if !engine_paths::runtime_installed() || !engine_paths::embed_model_installed() {
            return Ok(None);
        }
        let Some(embed_model) = engine_paths::embed_model() else {
            return Ok(None);
        };
        let (out, err) = open_server_log("embed-server");
        Command::new(engine_paths::server_binary())
            .args(embed_server_args(&embed_model, &self.host, self.embed_port))
            .stdout(out)
            .stderr(err)
            .spawn()
            .map(Some)
            .map_err(|error| format!("unable to start the embedding engine: {error}"))
    }

    pub fn status(&self) -> LocalModelStatus {
        LocalModelStatus {
            chat_endpoint: format!("http://{}:{}", self.host, self.chat_port),
            embedding_endpoint: format!("http://{}:{}", self.host, self.embed_port),
            chat_model: self.chat_model.clone(),
            embedding_model: self.embedding_model.clone(),
            chat_available: self.chat_reachable(),
            embedding_available: self.embed_reachable(),
        }
    }

    fn chat_completion(&self, body: &serde_json::Value) -> Result<String, String> {
        let url = format!("http://{}:{}/v1/chat/completions", self.host, self.chat_port);
        let mut response = shared_agent()
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|error| format!("local chat engine unavailable: {error}"))?;
        let payload = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("invalid chat engine response: {error}"))?;
        let parsed: ChatCompletionResponse = serde_json::from_str(&payload)
            .map_err(|error| format!("invalid chat engine JSON: {error}"))?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| "chat engine returned no choices".into())
    }

    #[allow(dead_code)]
    pub fn analyze_text(&self, input: &str) -> Result<SemanticModelOutput, String> {
        let prompt = format!("Return JSON only with category, summary, entities, relationships, confidence (0..1). Interpret:\n{input}");
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": prompt}],
            "response_format": {"type": "json_object"},
            "temperature": 0.2,
            "max_tokens": MAX_RESPONSE_TOKENS
        });
        let content = self.chat_completion(&body)?;
        parse_and_validate_model_json(&content)
    }

    /// Analyze several contexts in one chat request. The indexed response
    /// prevents an item from being silently assigned to the wrong event.
    /// This is the same numbered-prompt technique used with every backend
    /// this provider has had — it's a prompting strategy, not something the
    /// server needs to support natively, since chat completion APIs don't
    /// offer "batch of independent prompts" as a primitive.
    pub fn analyze_text_batch(
        &self,
        inputs: &[String],
    ) -> Result<Vec<SemanticModelOutput>, String> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let numbered = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| format!("ITEM {index}:\n{input}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let prompt = format!("Return JSON only as {{\"results\":[{{\"index\":0,\"category\":\"...\",\"summary\":\"...\",\"entities\":[],\"relationships\":[],\"confidence\":0.0}}]}}. Include exactly one result for every item, preserving its index.\n{numbered}");
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": prompt}],
            "response_format": {"type": "json_object"},
            "temperature": 0.2,
            "max_tokens": MAX_RESPONSE_TOKENS.saturating_mul(inputs.len() as u32)
        });
        let content = self.chat_completion(&body)?;
        let response: BatchSemanticResponse = serde_json::from_str(&content)
            .map_err(|e| format!("invalid batch semantic JSON: {e}"))?;
        if response.results.len() != inputs.len() {
            return Err("batch semantic response count mismatch".into());
        }
        let mut ordered = vec![None; inputs.len()];
        for item in response.results {
            if item.index >= inputs.len() || ordered[item.index].is_some() {
                return Err("batch semantic response index mismatch".into());
            }
            ordered[item.index] = Some(SemanticModelOutput {
                category: item.category,
                summary: item.summary,
                entities: item.entities,
                relationships: item.relationships,
                confidence: item.confidence,
            });
        }
        ordered
            .into_iter()
            .map(|item| item.ok_or_else(|| "batch semantic response missing item".into()))
            .collect()
    }

    pub fn analyze_image(&self, bytes: &[u8]) -> Result<SemanticModelOutput, String> {
        validate_image_input(bytes)?;
        let data_url = format!("data:image/png;base64,{}", base64_encode(bytes));
        let body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": data_url}},
                    {"type": "text", "text": "Return JSON only with category, summary, entities, relationships, confidence (0..1). Interpret this screenshot."}
                ]
            }],
            "response_format": {"type": "json_object"},
            "temperature": 0.2,
            "max_tokens": MAX_RESPONSE_TOKENS
        });
        let content = self.chat_completion(&body)?;
        parse_and_validate_model_json(&content)
    }

    /// Embeds a batch of inputs in one request. Unlike the text-analysis
    /// batching above, this is a real server-side batch: llama.cpp's
    /// OpenAI-compatible `/v1/embeddings` accepts an array `input` and
    /// returns one vector per item, so this doesn't need a prompting trick.
    pub fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("http://{}:{}/v1/embeddings", self.host, self.embed_port);
        let body = serde_json::json!({ "input": inputs });
        let mut response = shared_agent()
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|error| format!("local embedding engine unavailable: {error}"))?;
        let payload = response
            .body_mut()
            .read_to_string()
            .map_err(|error| format!("invalid embedding engine response: {error}"))?;
        let parsed: EmbeddingsResponse = serde_json::from_str(&payload)
            .map_err(|error| format!("invalid embedding engine JSON: {error}"))?;
        if parsed.data.len() != inputs.len() {
            return Err("embedding engine returned an incomplete batch".into());
        }
        let mut ordered: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
        for item in parsed.data {
            if item.index >= inputs.len() {
                return Err("embedding engine returned an out-of-range index".into());
            }
            ordered[item.index] = Some(item.embedding);
        }
        ordered
            .into_iter()
            .enumerate()
            .map(|(index, embedding)| {
                embedding.ok_or_else(|| format!("embedding engine response missing item {index}"))
            })
            .collect()
    }
}
impl LocalSemanticAnalyzer for LlamaCppProvider {
    fn analyze_text(&self, input: &str) -> Result<SemanticModelOutput, String> {
        self.analyze_text(input)
    }
    fn analyze_image(&self, bytes: &[u8]) -> Result<SemanticModelOutput, String> {
        self.analyze_image(bytes)
    }
}
impl TextEmbedder for LlamaCppProvider {
    fn dimensions(&self) -> usize {
        768
    }
    fn embed(&self, input: &str) -> Result<Vec<f32>, String> {
        self.embed_batch(&[input.to_string()])?
            .into_iter()
            .next()
            .ok_or("embedding engine returned no embedding".into())
    }
}
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (a << 16) | (b << 8) | c;
        output.push(TABLE[((value >> 18) & 63) as usize] as char);
        output.push(TABLE[((value >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}
/// `LlamaCppProvider::default()` reads `CHRONICLE_LLAMA_*` env vars, which
/// are process-global. Tests that set them (to point the provider at a mock
/// server) and tests that assert on the unset defaults would otherwise race
/// when `cargo test` runs them on different threads of the same process.
/// This lock — shared with `asynchronous_processing_queue`'s end-to-end test
/// — makes any such env-var-touching test mutually exclusive.
#[cfg(test)]
pub(crate) fn env_var_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// A one-shot HTTP server standing in for one `llama-server.exe` endpoint:
/// accepts a single connection, reads the request (headers + body via
/// Content-Length), hands the request body to `on_request`, and writes back
/// whatever body it returns as a 200 JSON response. Shared by this module's
/// own HTTP-client tests and by `asynchronous_processing_queue`'s full
/// capture-to-database pipeline test, so both can drive `LlamaCppProvider`
/// exactly as it would a real `llama-server` instance without needing the
/// multi-gigabyte model download this pipeline ultimately depends on.
#[cfg(test)]
pub(crate) fn mock_http_server(
    on_request: impl Fn(String) -> String + Send + 'static,
) -> (u16, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut content_length = None;
        loop {
            let n = stream.read(&mut chunk).expect("read request");
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(header_end) = text.find("\r\n\r\n") {
                if content_length.is_none() {
                    content_length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().to_string())
                        })
                        .and_then(|v| v.parse::<usize>().ok());
                }
                let body_len = buf.len() - (header_end + 4);
                if let Some(expected) = content_length {
                    if body_len >= expected {
                        break;
                    }
                } else {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let header_end = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(text.len());
        let request_body = text[header_end..].to_string();
        let response_body = on_request(request_body);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream.write_all(response.as_bytes()).expect("write response");
        stream.flush().ok();
    });
    (addr.port(), handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_local_engine_ports() {
        let _guard = env_var_lock().lock().unwrap();
        let p = LlamaCppProvider::default();
        assert_eq!(p.host, "127.0.0.1");
        assert!(p.chat_port > 0);
        assert!(p.embed_port > 0);
        assert_ne!(p.chat_port, p.embed_port);
        assert!(!p.chat_model.is_empty());
        assert!(!p.embedding_model.is_empty());
    }

    #[test]
    fn chat_server_args_enable_jinja_and_wide_context() {
        let args = chat_server_args(
            Path::new("model.gguf"),
            Path::new("mmproj.gguf"),
            "127.0.0.1",
            8090,
        );
        assert!(
            args.contains(&"--jinja".to_string()),
            "chat server must run with --jinja so Gemma 3's chat template is applied; \
             without it /v1/chat/completions fails while the port stays reachable, \
             which is exactly the silent-failure mode reported: {args:?}"
        );
        let ctx_index = args.iter().position(|a| a == "-c").expect("-c flag present");
        assert_eq!(args[ctx_index + 1], SERVER_CONTEXT_SIZE.to_string());
    }

    #[test]
    fn embed_server_args_include_wide_context() {
        let args = embed_server_args(Path::new("embed.gguf"), "127.0.0.1", 8091);
        assert!(args.contains(&"--embeddings".to_string()));
        let ctx_index = args.iter().position(|a| a == "-c").expect("-c flag present");
        assert_eq!(args[ctx_index + 1], SERVER_CONTEXT_SIZE.to_string());
    }

    #[test]
    fn server_args_pin_an_explicit_thread_count() {
        let expected = inference_thread_count().to_string();
        let chat_args = chat_server_args(Path::new("m.gguf"), Path::new("mm.gguf"), "127.0.0.1", 8090);
        let chat_index = chat_args.iter().position(|a| a == "-t").expect("-t flag present");
        assert_eq!(chat_args[chat_index + 1], expected);

        let embed_args = embed_server_args(Path::new("e.gguf"), "127.0.0.1", 8091);
        let embed_index = embed_args.iter().position(|a| a == "-t").expect("-t flag present");
        assert_eq!(embed_args[embed_index + 1], expected);
    }

    #[test]
    fn inference_thread_count_leaves_a_core_for_the_rest_of_the_app() {
        let available = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let threads = inference_thread_count();
        assert!(threads >= 1);
        if available > 1 {
            assert_eq!(threads, available - 1);
        } else {
            assert_eq!(threads, 1);
        }
    }

    fn provider_for(port: u16) -> LlamaCppProvider {
        LlamaCppProvider {
            host: "127.0.0.1".into(),
            chat_port: port,
            embed_port: port,
            chat_model: "test-chat".into(),
            embedding_model: "test-embed".into(),
        }
    }

    #[test]
    fn analyze_text_bounds_generation_with_max_tokens() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (port, handle) = mock_http_server(move |request| {
            sender.send(request).unwrap();
            let content = serde_json::json!({
                "category": "coding",
                "summary": "s",
                "entities": [],
                "relationships": [],
                "confidence": 0.5
            })
            .to_string();
            serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
        });
        let provider = provider_for(port);
        provider.analyze_text("hello").expect("analyze_text should succeed");
        handle.join().unwrap();
        let request: serde_json::Value = serde_json::from_str(&receiver.recv().unwrap()).unwrap();
        let max_tokens = request["max_tokens"]
            .as_u64()
            .expect("request must bound generation with max_tokens, or a bad generation can pin a worker thread indefinitely");
        assert_eq!(max_tokens, MAX_RESPONSE_TOKENS as u64);
    }

    #[test]
    fn analyze_text_parses_llama_server_chat_response() {
        let (port, handle) = mock_http_server(|_request| {
            let content = serde_json::json!({
                "category": "coding",
                "summary": "Editing Rust source",
                "entities": ["chronicle"],
                "relationships": [],
                "confidence": 0.9
            })
            .to_string();
            serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
        });
        let provider = provider_for(port);
        let result = provider.analyze_text("editing a rust file").expect("analyze_text should succeed");
        assert_eq!(result.category, "coding");
        assert_eq!(result.summary, "Editing Rust source");
        handle.join().unwrap();
    }

    #[test]
    fn analyze_text_batch_reorders_by_response_index() {
        let (port, handle) = mock_http_server(|_request| {
            let content = serde_json::json!({
                "results": [
                    {"index": 1, "category": "b", "summary": "second", "entities": [], "relationships": [], "confidence": 0.5},
                    {"index": 0, "category": "a", "summary": "first", "entities": [], "relationships": [], "confidence": 0.5}
                ]
            })
            .to_string();
            serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
        });
        let provider = provider_for(port);
        let results = provider
            .analyze_text_batch(&["first input".into(), "second input".into()])
            .expect("batch should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].summary, "first");
        assert_eq!(results[1].summary, "second");
        handle.join().unwrap();
    }

    #[test]
    fn analyze_text_batch_rejects_mismatched_result_count() {
        let (port, handle) = mock_http_server(|_request| {
            let content = serde_json::json!({
                "results": [
                    {"index": 0, "category": "a", "summary": "only one", "entities": [], "relationships": [], "confidence": 0.5}
                ]
            })
            .to_string();
            serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
        });
        let provider = provider_for(port);
        let err = provider
            .analyze_text_batch(&["first".into(), "second".into()])
            .expect_err("count mismatch must error, not silently misassign results");
        assert!(err.contains("count mismatch"));
        handle.join().unwrap();
    }

    #[test]
    fn embed_batch_parses_llama_server_embeddings_response() {
        let (port, handle) = mock_http_server(|_request| {
            serde_json::json!({
                "data": [
                    {"index": 1, "embedding": [0.4, 0.5]},
                    {"index": 0, "embedding": [0.1, 0.2]}
                ]
            })
            .to_string()
        });
        let provider = provider_for(port);
        let results = provider
            .embed_batch(&["first".into(), "second".into()])
            .expect("embed_batch should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], vec![0.1, 0.2]);
        assert_eq!(results[1], vec![0.4, 0.5]);
        handle.join().unwrap();
    }

    #[test]
    fn chat_completion_surfaces_engine_errors_instead_of_panicking() {
        let provider = provider_for(1);
        let err = provider
            .analyze_text("anything")
            .expect_err("unreachable port must error, not panic");
        assert!(err.contains("unavailable") || err.contains("engine"));
    }
}
