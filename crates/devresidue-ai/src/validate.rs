//! Strict local validation for one OpenAI-compatible response body.
//!
//! The validator accepts only the exact response shape promised to the remote
//! service. It never returns raw request or response text in an error, and it
//! keeps only a bounded, conservatively redacted display copy of model text.

use std::collections::HashSet;

use devresidue_core::ai::{AiEntryToken, AiProfileId, AiSuggestion, PreparedBatch};
use devresidue_core::RiskLevel;
use serde::Deserialize;

const MAX_REASON_CHARS: usize = 240;
const MAX_PRODUCT_GUESS_CHARS: usize = 80;
const REDACTED_MODEL_TEXT: &str = "<redacted-model-text>";

/// Validates a response against exactly one prepared, consented batch.
#[derive(Debug, Clone)]
pub struct ResponseValidator {
    expected_tokens: HashSet<AiEntryToken>,
    profile_id: AiProfileId,
    scan_generation: u64,
}

impl ResponseValidator {
    /// Builds a validator that accepts every prepared token exactly once.
    #[must_use]
    pub fn for_batch(batch: &PreparedBatch) -> Self {
        Self {
            expected_tokens: batch.entries.iter().map(|entry| entry.id.clone()).collect(),
            profile_id: batch.profile_id.clone(),
            scan_generation: batch.scan_generation,
        }
    }

    /// Validates one model-content JSON object and returns in-memory-only
    /// suggestions tied to the prepared profile and scan generation.
    ///
    /// Errors deliberately identify only the validation category. They never
    /// include the raw model response, an entry token, a path, or a key.
    pub fn validate(&self, body: &str) -> Result<Vec<AiSuggestion>, String> {
        let response: ModelResponse = serde_json::from_str(body)
            .map_err(|_| "AI response has an invalid or unsupported schema".to_string())?;

        if response.suggestions.len() != self.expected_tokens.len() {
            return Err("AI response does not cover the prepared batch exactly once".to_string());
        }

        let mut seen = HashSet::with_capacity(response.suggestions.len());
        let mut suggestions = Vec::with_capacity(response.suggestions.len());
        for suggestion in response.suggestions {
            if !self.expected_tokens.contains(&suggestion.id) {
                return Err("AI response contains an unregistered entry token".to_string());
            }
            if !seen.insert(suggestion.id.clone()) {
                return Err("AI response contains a duplicate entry token".to_string());
            }
            if !suggestion.confidence.is_finite()
                || !(0.0_f64..=1.0_f64).contains(&suggestion.confidence)
            {
                return Err("AI response confidence is outside the permitted range".to_string());
            }

            suggestions.push(AiSuggestion {
                token: suggestion.id,
                suggested_risk: suggestion.suggested_risk,
                confidence: suggestion.confidence as f32,
                reason: redact_model_text(&suggestion.reason, MAX_REASON_CHARS),
                product_guess: suggestion
                    .product_guess
                    .as_deref()
                    .map(|text| redact_model_text(text, MAX_PRODUCT_GUESS_CHARS)),
                profile_id: self.profile_id.clone(),
                scan_generation: self.scan_generation,
            });
        }

        if seen != self.expected_tokens {
            return Err("AI response does not cover the prepared batch exactly once".to_string());
        }

        Ok(suggestions)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelResponse {
    suggestions: Vec<ModelSuggestion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSuggestion {
    id: AiEntryToken,
    suggested_risk: RiskLevel,
    confidence: f64,
    reason: String,
    product_guess: Option<String>,
}

fn redact_model_text(text: &str, max_chars: usize) -> String {
    if text.trim().is_empty() || contains_sensitive_model_text(text) {
        return REDACTED_MODEL_TEXT.to_string();
    }

    truncate_chars(text, max_chars)
}

fn contains_sensitive_model_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if text.chars().any(char::is_control)
        || text.contains('/')
        || text.contains('\\')
        || text.contains("://")
        || has_drive_prefix(text)
    {
        return true;
    }

    [
        "authorization",
        "bearer",
        "api key",
        "apikey",
        "token",
        "secret",
        "credential",
        "password",
        "sk-",
        "ghp_",
        "github_pat_",
        "xox",
        "akia",
        "-----begin",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn has_drive_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let value: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let keep = max_chars.saturating_sub(1);
        format!("{}…", value.chars().take(keep).collect::<String>())
    } else {
        value
    }
}
