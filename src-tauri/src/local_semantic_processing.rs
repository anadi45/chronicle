//! Local semantic-processing adapter contracts.
//!
//! Model runtimes are optional and machine-specific. This module validates the
//! stable JSON shape before it reaches `semantic_events`, keeping capture and
//! persistence independent from Gemma or any future local model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticModelOutput {
    pub category: String,
    pub summary: String,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub relationships: Vec<String>,
    pub confidence: f32,
}

pub trait LocalSemanticAnalyzer: Send + Sync {
    fn analyze_text(&self, input: &str) -> Result<SemanticModelOutput, String>;
    fn analyze_image(&self, bytes: &[u8]) -> Result<SemanticModelOutput, String>;
}

pub fn parse_and_validate_model_json(json: &str) -> Result<SemanticModelOutput, String> {
    if json.len() > 64 * 1024 { return Err("semantic model output exceeds 64 KiB".into()); }
    let output: SemanticModelOutput = serde_json::from_str(json).map_err(|error| format!("invalid semantic model JSON: {error}"))?;
    validate_model_output(output)
}

pub fn validate_model_output(output: SemanticModelOutput) -> Result<SemanticModelOutput, String> {
    if output.category.trim().is_empty() {
        return Err("semantic category is empty".into());
    }
    if output.summary.trim().is_empty() {
        return Err("semantic summary is empty".into());
    }
    if !(0.0..=1.0).contains(&output.confidence) {
        return Err("semantic confidence must be between 0 and 1".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_valid_output() {
        assert!(validate_model_output(SemanticModelOutput {
            category: "work".into(),
            summary: "Edited a file".into(),
            entities: vec![],
            relationships: vec![],
            confidence: 0.8
        })
        .is_ok());
    }
    #[test]
    fn rejects_invalid_confidence() {
        assert!(validate_model_output(SemanticModelOutput {
            category: "work".into(),
            summary: "summary".into(),
            entities: vec![],
            relationships: vec![],
            confidence: 2.0
        })
        .is_err());
    }
    #[test]
    fn parses_structured_model_json() {
        let output = parse_and_validate_model_json(r#"{"category":"work","summary":"Edited a file","confidence":0.7}"#).unwrap();
        assert_eq!(output.category, "work");
    }
    #[test]
    fn rejects_oversized_model_json() {
        assert!(parse_and_validate_model_json(&"x".repeat(65 * 1024)).is_err());
    }
}
