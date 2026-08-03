//! Local inference via a bundled `llama-server` (llama.cpp) engine.
//!
//! Chronicle runs two `llama-server` instances on localhost — one serving a
//! Gemma 3 chat/vision model for semantic analysis, one serving EmbeddingGemma
//! for embeddings — rather than depending on a separately installed
//! application. Both the `llama-server` binary and the GGUF model files live
//! under `<data dir>\llama` (see `engine_paths`), where `<data dir>` is the
//! folder the user chose on first run (see `data_directory`), and are
//! downloaded once by `local_ai_setup`; nothing here downloads anything
//! itself. Both servers speak llama.cpp's OpenAI-compatible HTTP API
//! (`/v1/chat/completions` for text and vision, `/v1/embeddings` for
//! embeddings), so this module is a thin, stable HTTP client over that API,
//! not a reimplementation of inference itself.

use crate::embedding_provider::TextEmbedder;
use crate::local_semantic_processing::{
    parse_and_validate_model_json, validate_image_input, LocalSemanticAnalyzer, SemanticModelOutput,
};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

/// Where the bundled engine (binary + models) lives and what its pieces are
/// named. A single source of truth shared by the provider (to run
/// inference) and `local_ai_setup` (to download/remove these same files).
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

    fn base_dir() -> PathBuf {
        crate::data_directory::data_dir().join("llama")
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
    pub fn models_dir() -> PathBuf {
        base_dir().join("models")
    }
    pub fn server_binary() -> PathBuf {
        runtime_dir().join("llama-server.exe")
    }
    pub fn chat_model() -> PathBuf {
        models_dir().join(CHAT_MODEL_FILE)
    }
    pub fn mmproj() -> PathBuf {
        models_dir().join(MMPROJ_FILE)
    }
    pub fn embed_model() -> PathBuf {
        models_dir().join(EMBED_MODEL_FILE)
    }
    pub fn runtime_installed() -> bool {
        server_binary().is_file()
    }
    pub fn chat_model_installed() -> bool {
        chat_model().is_file() && mmproj().is_file()
    }
    pub fn embed_model_installed() -> bool {
        embed_model().is_file()
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
        Command::new(engine_paths::server_binary())
            .arg("-m")
            .arg(engine_paths::chat_model())
            .arg("--mmproj")
            .arg(engine_paths::mmproj())
            .arg("--host")
            .arg(&self.host)
            .arg("--port")
            .arg(self.chat_port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
        Command::new(engine_paths::server_binary())
            .arg("-m")
            .arg(engine_paths::embed_model())
            .arg("--host")
            .arg(&self.host)
            .arg("--port")
            .arg(self.embed_port.to_string())
            .arg("--embeddings")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
            "temperature": 0.2
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
            "temperature": 0.2
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
            "temperature": 0.2
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_use_local_engine_ports() {
        let p = LlamaCppProvider::default();
        assert_eq!(p.host, "127.0.0.1");
        assert!(p.chat_port > 0);
        assert!(p.embed_port > 0);
        assert_ne!(p.chat_port, p.embed_port);
        assert!(!p.chat_model.is_empty());
        assert!(!p.embedding_model.is_empty());
    }
}
