//! Local Ollama-compatible Gemma and Nomic model discovery and inference.
use crate::embedding_provider::TextEmbedder;
use crate::local_semantic_processing::{
    parse_and_validate_model_json, validate_image_input, LocalSemanticAnalyzer, SemanticModelOutput,
};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelStatus {
    pub endpoint: String,
    pub gemma_model: String,
    pub nomic_model: String,
    pub gemma_available: bool,
    pub nomic_available: bool,
}
#[derive(Debug, Clone)]
pub struct OllamaLocalModelProvider {
    pub endpoint: String,
    pub gemma_model: String,
    pub nomic_model: String,
}
#[derive(Debug, Deserialize)]
struct Tags {
    models: Vec<Model>,
}
#[derive(Debug, Deserialize)]
struct Model {
    name: String,
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Generate {
    response: String,
}
#[derive(Debug, Deserialize)]
struct Embed {
    embeddings: Vec<Vec<f32>>,
}
impl Default for OllamaLocalModelProvider {
    fn default() -> Self {
        Self {
            endpoint: std::env::var("CHRONICLE_OLLAMA_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".into()),
            gemma_model: std::env::var("CHRONICLE_GEMMA_MODEL")
                .unwrap_or_else(|_| "gemma3:4b".into()),
            nomic_model: std::env::var("CHRONICLE_NOMIC_MODEL")
                .unwrap_or_else(|_| "nomic-embed-text".into()),
        }
    }
}
impl OllamaLocalModelProvider {
    pub fn status(&self) -> LocalModelStatus {
        let names = self.tags().unwrap_or_default();
        LocalModelStatus {
            endpoint: self.endpoint.clone(),
            gemma_model: self.gemma_model.clone(),
            nomic_model: self.nomic_model.clone(),
            gemma_available: names.contains(&self.gemma_model),
            nomic_available: names.contains(&self.nomic_model),
        }
    }
    fn tags(&self) -> Result<Vec<String>, String> {
        Ok(self
            .request::<Tags>("GET", "/api/tags", "")?
            .models
            .into_iter()
            .map(|m| m.name)
            .collect())
    }
    #[allow(dead_code)]
    pub fn analyze_text(&self, input: &str) -> Result<SemanticModelOutput, String> {
        let body = serde_json::json!({"model": self.gemma_model, "prompt": format!("Return JSON only with category, summary, entities, relationships, confidence (0..1). Interpret:\n{input}"), "stream": false, "format": "json"});
        let output = self.request::<Generate>("POST", "/api/generate", &body.to_string())?;
        parse_and_validate_model_json(&output.response)
    }
    pub fn analyze_image(&self, bytes: &[u8]) -> Result<SemanticModelOutput, String> {
        validate_image_input(bytes)?;
        let body = serde_json::json!({"model": self.gemma_model, "prompt": "Return JSON only with category, summary, entities, relationships, confidence (0..1). Interpret this screenshot.", "images": [base64_encode(bytes)], "stream": false, "format": "json"});
        let output = self.request::<Generate>("POST", "/api/generate", &body.to_string())?;
        parse_and_validate_model_json(&output.response)
    }
    fn request<T: for<'a> Deserialize<'a>>(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<T, String> {
        let host = self
            .endpoint
            .strip_prefix("http://")
            .ok_or("only local HTTP model endpoints are supported")?;
        let address = host
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or("model endpoint unavailable")?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .map_err(|e| format!("local model unavailable: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        let request = if method == "GET" {
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
        } else {
            format!("POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        };
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let payload = String::from_utf8_lossy(&bytes)
            .split("\r\n\r\n")
            .nth(1)
            .ok_or("invalid model response")?
            .to_string();
        serde_json::from_str(&payload).map_err(|e| format!("invalid model JSON: {e}"))
    }
}
impl LocalSemanticAnalyzer for OllamaLocalModelProvider {
    fn analyze_text(&self, input: &str) -> Result<SemanticModelOutput, String> {
        self.analyze_text(input)
    }
    fn analyze_image(&self, bytes: &[u8]) -> Result<SemanticModelOutput, String> {
        self.analyze_image(bytes)
    }
}
impl TextEmbedder for OllamaLocalModelProvider {
    fn dimensions(&self) -> usize {
        768
    }
    fn embed(&self, input: &str) -> Result<Vec<f32>, String> {
        let body = serde_json::json!({"model": self.nomic_model, "input": input});
        self.request::<Embed>("POST", "/api/embed", &body.to_string())?
            .embeddings
            .into_iter()
            .next()
            .ok_or("Nomic returned no embedding".into())
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
    fn defaults_use_local_models() {
        let p = OllamaLocalModelProvider::default();
        assert!(p.endpoint.starts_with("http://"));
        assert!(!p.gemma_model.is_empty());
        assert!(!p.nomic_model.is_empty());
    }
}
