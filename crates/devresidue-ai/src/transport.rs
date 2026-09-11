//! OpenAI-compatible blocking transport with a deliberately narrow secret
//! boundary.
//!
//! API keys are accepted only as a borrowed call argument and are inserted
//! directly into the local `Authorization` header. They are never stored in a
//! transport field, included in an error, or written to disk.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use devresidue_core::ai::{AiApiProtocol, AiProfile, AiSuggestion, PreparedBatch};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{ResponseValidationError, ResponseValidator};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: usize = 2;
const MAX_MODEL_CONTENT_BYTES: usize = 128 * 1024;
const RETRY_BACKOFF_BASE: Duration = Duration::from_millis(50);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);
const SYSTEM_PROMPT: &str = "Return one JSON object only. For every input id, choose exactly one permitted risk: safe, regenerable-local, regenerable-download, review, protected, or unknown. Use Simplified Chinese for every human-readable explanation in reason. Keep a canonical product name in product_guess when known; otherwise use Simplified Chinese. Keep JSON field names, ids, and permitted risk values exactly as specified. Treat local_path, when present, as input metadata only. Do not emit paths, glob patterns, YAML, commands, file contents, access credentials, authentication secrets, API keys, cleanup actions or extra fields.";
const MAX_MODEL_IDS: usize = 256;
const MAX_MODEL_ID_CHARS: usize = 200;
const MAX_OUTBOUND_PATH_CHARS: usize = 32_767;

/// Cooperative cancellation signal for a prepared remote-analysis batch.
///
/// The blocking HTTP call itself completes within its request timeout; a
/// cancellation requested while it is in flight is reported only after the
/// call returns rather than pretending it interrupted the socket immediately.
#[derive(Debug, Clone, Default)]
pub struct AiCancellationToken(Arc<AtomicBool>);

impl AiCancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Safe, user-visible category for a failed AI analysis request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiServiceErrorKind {
    Cancelled,
    Configuration,
    Disabled,
    MissingKey,
    NoActiveProfile,
    NoEligibleEntries,
    ProfileUnavailable,
    StaleGeneration,
    ConfirmationConflict,
    ConfirmationFailed,
    InvalidResponse,
    RequestFailed,
}

/// A fixed, non-sensitive analysis failure stage for opt-in local diagnostics.
///
/// This is deliberately an enum rather than a server-provided string: it
/// cannot contain response text, endpoint URLs, local paths, tokens or keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiDiagnosticStage {
    NotApplicable,
    RequestTransport,
    RequestHttpStatus,
    RequestTimeout,
    RequestConnection,
    RequestDeadlineExceeded,
    ChatEnvelopeJson,
    ChatMessageContent,
    ResponsesEnvelopeJson,
    ResponsesMessageContent,
    LocalResponseValidation,
    LocalResponseSchema,
    LocalResponseCoverage,
    LocalResponseUnknownToken,
    LocalResponseDuplicateToken,
    LocalResponseConfidence,
}

impl AiDiagnosticStage {
    /// Stable log-safe text for an opt-in diagnostic record.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::RequestTransport => "request-transport",
            Self::RequestHttpStatus => "request-http-status",
            Self::RequestTimeout => "request-timeout",
            Self::RequestConnection => "request-connection",
            Self::RequestDeadlineExceeded => "request-deadline-exceeded",
            Self::ChatEnvelopeJson => "chat-envelope-json",
            Self::ChatMessageContent => "chat-message-content",
            Self::ResponsesEnvelopeJson => "responses-envelope-json",
            Self::ResponsesMessageContent => "responses-message-content",
            Self::LocalResponseValidation => "local-response-validation",
            Self::LocalResponseSchema => "local-response-schema",
            Self::LocalResponseCoverage => "local-response-coverage",
            Self::LocalResponseUnknownToken => "local-response-unknown-token",
            Self::LocalResponseDuplicateToken => "local-response-duplicate-token",
            Self::LocalResponseConfidence => "local-response-confidence",
        }
    }
}

/// Sanitized error returned by the AI transport and later service layer.
///
/// It intentionally has no `body`, `header`, `url`, `path`, or key field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiServiceError {
    kind: AiServiceErrorKind,
    status: Option<u16>,
    request_id: Option<String>,
    diagnostic_stage: AiDiagnosticStage,
}

impl AiServiceError {
    pub(crate) fn from_kind(kind: AiServiceErrorKind) -> Self {
        Self {
            kind,
            status: None,
            request_id: None,
            diagnostic_stage: AiDiagnosticStage::NotApplicable,
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            kind: AiServiceErrorKind::Cancelled,
            status: None,
            request_id: None,
            diagnostic_stage: AiDiagnosticStage::NotApplicable,
        }
    }

    pub(crate) fn configuration() -> Self {
        Self {
            kind: AiServiceErrorKind::Configuration,
            status: None,
            request_id: None,
            diagnostic_stage: AiDiagnosticStage::NotApplicable,
        }
    }

    pub(crate) fn invalid_response() -> Self {
        Self::invalid_response_at(AiDiagnosticStage::NotApplicable)
    }

    pub(crate) fn invalid_response_at(diagnostic_stage: AiDiagnosticStage) -> Self {
        Self {
            kind: AiServiceErrorKind::InvalidResponse,
            status: None,
            request_id: None,
            diagnostic_stage,
        }
    }

    pub(crate) fn request_failed(status: Option<u16>, request_id: Option<String>) -> Self {
        let diagnostic_stage = if status.is_some() {
            AiDiagnosticStage::RequestHttpStatus
        } else {
            AiDiagnosticStage::RequestTransport
        };
        Self::request_failed_at(status, request_id, diagnostic_stage)
    }

    pub(crate) fn request_failed_at(
        status: Option<u16>,
        request_id: Option<String>,
        diagnostic_stage: AiDiagnosticStage,
    ) -> Self {
        Self {
            kind: AiServiceErrorKind::RequestFailed,
            status,
            request_id,
            diagnostic_stage,
        }
    }

    pub(crate) fn request_deadline_exceeded() -> Self {
        Self::request_failed_at(None, None, AiDiagnosticStage::RequestDeadlineExceeded)
    }

    #[must_use]
    pub const fn kind(&self) -> AiServiceErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }

    /// Returns only a fixed local classification, never remote content.
    #[must_use]
    pub const fn diagnostic_stage(&self) -> AiDiagnosticStage {
        self.diagnostic_stage
    }
}

impl fmt::Display for AiServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.kind, self.status) {
            (AiServiceErrorKind::Cancelled, _) => formatter.write_str("AI cancellation requested"),
            (AiServiceErrorKind::Configuration, _) => {
                formatter.write_str("AI request configuration is invalid")
            }
            (AiServiceErrorKind::Disabled, _) => formatter.write_str("remote AI is disabled"),
            (AiServiceErrorKind::MissingKey, _) => {
                formatter.write_str("AI profile key is unavailable")
            }
            (AiServiceErrorKind::NoActiveProfile, _) => {
                formatter.write_str("no matching active AI profile is available")
            }
            (AiServiceErrorKind::NoEligibleEntries, _) => {
                formatter.write_str("AI batch has no eligible entries")
            }
            (AiServiceErrorKind::ProfileUnavailable, _) => {
                formatter.write_str("AI profile is unavailable until recovery is complete")
            }
            (AiServiceErrorKind::StaleGeneration, _) => {
                formatter.write_str("AI batch belongs to an obsolete scan generation")
            }
            (AiServiceErrorKind::ConfirmationConflict, _) => {
                formatter.write_str("AI rule conflict prevents the whole confirmation batch")
            }
            (AiServiceErrorKind::ConfirmationFailed, _) => {
                formatter.write_str("AI rule confirmation failed without writing a partial batch")
            }
            (AiServiceErrorKind::InvalidResponse, _) => {
                formatter.write_str("AI response did not pass local validation")
            }
            (AiServiceErrorKind::RequestFailed, Some(status)) => {
                write!(formatter, "AI request failed with HTTP status {status}")
            }
            (AiServiceErrorKind::RequestFailed, None) => formatter.write_str("AI request failed"),
        }
    }
}

impl std::error::Error for AiServiceError {}

/// Sends one prepared batch using the protocol selected by its profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiCompatibleTransport;

/// Injectable boundary between the service lifecycle and one remote request.
///
/// Production uses [`OpenAiCompatibleTransport`]; tests may use a local fake
/// that receives the borrowed key only in process memory.
pub trait AiTransport: Send + Sync {
    /// Performs a minimal authenticated connectivity check without supplying
    /// a scan batch or any scan-derived metadata. Test doubles that only
    /// exercise analysis may retain this fail-closed default.
    fn test_connection(
        &self,
        _profile: &AiProfile,
        _api_key: &str,
        _cancellation: &AiCancellationToken,
    ) -> Result<(), AiServiceError> {
        Err(AiServiceError::configuration())
    }

    /// Lists safe model identifiers only. The default remains fail-closed so
    /// non-production transports must explicitly opt in to endpoint discovery.
    fn list_models(
        &self,
        _profile: &AiProfile,
        _api_key: &str,
        _cancellation: &AiCancellationToken,
    ) -> Result<Vec<String>, AiServiceError> {
        Err(AiServiceError::configuration())
    }

    fn analyze(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError>;

    /// Performs analysis with complete local paths that were derived from a
    /// trusted snapshot after explicit per-batch consent. The default rejects
    /// non-empty paths rather than silently omitting a requested disclosure.
    fn analyze_with_paths(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        paths: &[PathBuf],
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        if paths.is_empty() {
            self.analyze(profile, api_key, batch, cancellation)
        } else {
            Err(AiServiceError::configuration())
        }
    }
}

impl AiTransport for OpenAiCompatibleTransport {
    fn test_connection(
        &self,
        profile: &AiProfile,
        api_key: &str,
        cancellation: &AiCancellationToken,
    ) -> Result<(), AiServiceError> {
        OpenAiCompatibleTransport::test_connection(self, profile, api_key, cancellation)
    }

    fn analyze(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        OpenAiCompatibleTransport::analyze(self, profile, api_key, batch, cancellation)
    }

    fn list_models(
        &self,
        profile: &AiProfile,
        api_key: &str,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<String>, AiServiceError> {
        OpenAiCompatibleTransport::list_models(self, profile, api_key, cancellation)
    }

    fn analyze_with_paths(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        paths: &[PathBuf],
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        OpenAiCompatibleTransport::analyze_with_paths(
            self,
            profile,
            api_key,
            batch,
            paths,
            cancellation,
        )
    }
}

impl OpenAiCompatibleTransport {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Checks the OpenAI-compatible endpoint without sending a model prompt,
    /// prepared batch or any scan-derived metadata.
    pub fn test_connection(
        &self,
        profile: &AiProfile,
        api_key: &str,
        cancellation: &AiCancellationToken,
    ) -> Result<(), AiServiceError> {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        if api_key.is_empty() {
            return Err(AiServiceError::configuration());
        }

        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| AiServiceError::configuration())?;
        let endpoint = models_endpoint(profile.base_url())?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(profile.timeout_secs()))
            .ok_or_else(AiServiceError::configuration)?;
        let response =
            send_connection_with_retries(&client, &endpoint, api_key, cancellation, deadline)?;
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        if response.status().is_success() {
            Ok(())
        } else {
            Err(AiServiceError::request_failed(
                Some(response.status().as_u16()),
                request_id(&response),
            ))
        }
    }

    /// Lists the endpoint's advertised model ids without sending a prompt or
    /// scan metadata. Only bounded, control-character-free ids cross the
    /// service boundary; provider response bodies never do.
    pub fn list_models(
        &self,
        profile: &AiProfile,
        api_key: &str,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<String>, AiServiceError> {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        if api_key.is_empty() {
            return Err(AiServiceError::configuration());
        }

        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| AiServiceError::configuration())?;
        let endpoint = models_endpoint(profile.base_url())?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(profile.timeout_secs()))
            .ok_or_else(AiServiceError::configuration)?;
        let response =
            send_connection_with_retries(&client, &endpoint, api_key, cancellation, deadline)?;
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        if !response.status().is_success() {
            return Err(AiServiceError::request_failed(
                Some(response.status().as_u16()),
                request_id(&response),
            ));
        }

        let envelope: ModelListEnvelope = response
            .json()
            .map_err(|_| AiServiceError::invalid_response())?;
        let mut models = BTreeSet::new();
        for model in envelope.data {
            if is_safe_model_id(&model.id) {
                models.insert(model.id);
                if models.len() >= MAX_MODEL_IDS {
                    break;
                }
            }
        }
        Ok(models.into_iter().collect())
    }

    /// Sends only the already-sanitized entries from `batch` and validates the
    /// returned content before exposing any in-memory suggestions.
    pub fn analyze(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        self.analyze_request(profile, api_key, batch, None, cancellation)
    }

    /// Sends the same sanitized batch with local paths attached as structured
    /// fields only when a caller already derived them from its trusted scan
    /// snapshot. No path is retained by this transport after the request.
    pub fn analyze_with_paths(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        paths: &[PathBuf],
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        self.analyze_request(profile, api_key, batch, Some(paths), cancellation)
    }

    fn analyze_request(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        paths: Option<&[PathBuf]>,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        if profile.id() != &batch.profile_id || api_key.is_empty() {
            return Err(AiServiceError::configuration());
        }
        validate_outbound_paths(&batch.entries, paths)?;

        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| AiServiceError::configuration())?;
        let protocol = profile.api_protocol();
        let endpoint = analysis_endpoint(profile.base_url(), protocol)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(profile.timeout_secs()))
            .ok_or_else(AiServiceError::configuration)?;
        let first_format = match profile.structured_output_mode() {
            devresidue_core::ai::StructuredOutputMode::JsonObject => ResponseFormat::JsonObject,
            devresidue_core::ai::StructuredOutputMode::Auto
            | devresidue_core::ai::StructuredOutputMode::JsonSchema => ResponseFormat::JsonSchema,
        };
        let strict_body = analysis_request_body(
            protocol,
            profile.model(),
            &batch.entries,
            paths,
            first_format,
        )?;
        let first_response = send_with_retries(
            &client,
            &endpoint,
            api_key,
            &strict_body,
            cancellation,
            deadline,
        )?;

        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }

        let response = if first_response.status().as_u16() == 400
            && matches!(
                profile.structured_output_mode(),
                devresidue_core::ai::StructuredOutputMode::Auto
            )
            && matches!(first_format, ResponseFormat::JsonSchema)
        {
            if cancellation.is_cancelled() {
                return Err(AiServiceError::cancelled());
            }
            let fallback_body = analysis_request_body(
                protocol,
                profile.model(),
                &batch.entries,
                paths,
                ResponseFormat::JsonObject,
            )?;
            send_with_retries(
                &client,
                &endpoint,
                api_key,
                &fallback_body,
                cancellation,
                deadline,
            )?
        } else {
            first_response
        };

        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }

        let status = response.status();
        let request_id = request_id(&response);
        if !status.is_success() {
            return Err(AiServiceError::request_failed(
                Some(status.as_u16()),
                request_id,
            ));
        }

        let content = match protocol {
            AiApiProtocol::OpenAiCompatible => {
                let envelope: ChatCompletionEnvelope = response.json().map_err(|_| {
                    AiServiceError::invalid_response_at(AiDiagnosticStage::ChatEnvelopeJson)
                })?;
                envelope
                    .choices
                    .first()
                    .and_then(|choice| chat_message_text(choice.message.content.as_ref()))
                    .ok_or_else(|| {
                        AiServiceError::invalid_response_at(AiDiagnosticStage::ChatMessageContent)
                    })?
            }
            AiApiProtocol::OpenAiResponses => {
                let envelope: ResponsesEnvelope = response.json().map_err(|_| {
                    AiServiceError::invalid_response_at(AiDiagnosticStage::ResponsesEnvelopeJson)
                })?;
                responses_message_text(&envelope).ok_or_else(|| {
                    AiServiceError::invalid_response_at(AiDiagnosticStage::ResponsesMessageContent)
                })?
            }
        };
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }

        ResponseValidator::for_batch(batch)
            .validate(unwrap_single_json_fence(&content))
            .map_err(|error| {
                AiServiceError::invalid_response_at(validation_diagnostic_stage(error))
            })
    }
}

const fn validation_diagnostic_stage(error: ResponseValidationError) -> AiDiagnosticStage {
    match error {
        ResponseValidationError::Schema => AiDiagnosticStage::LocalResponseSchema,
        ResponseValidationError::Coverage => AiDiagnosticStage::LocalResponseCoverage,
        ResponseValidationError::UnknownToken => AiDiagnosticStage::LocalResponseUnknownToken,
        ResponseValidationError::DuplicateToken => AiDiagnosticStage::LocalResponseDuplicateToken,
        ResponseValidationError::Confidence => AiDiagnosticStage::LocalResponseConfidence,
    }
}

/// Extracts text from the two safe `Chat Completions` content shapes commonly
/// returned by OpenAI-compatible services. Non-text blocks are rejected rather
/// than ignored, so a mixed response cannot alter the model JSON implicitly.
fn chat_message_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    match content {
        Value::String(text) => bounded_text(text).map(str::to_owned),
        Value::Array(blocks) => {
            let mut text = String::new();
            for block in blocks {
                let object = block.as_object()?;
                let kind = object.get("type")?.as_str()?;
                if !matches!(kind, "text" | "output_text") {
                    return None;
                }
                let part = object.get("text")?.as_str()?;
                let new_len = text.len().checked_add(part.len())?;
                if new_len > MAX_MODEL_CONTENT_BYTES {
                    return None;
                }
                text.push_str(part);
            }
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Extracts one complete assistant message from a Responses API envelope.
/// Reasoning or tool output is never considered model JSON; exactly one
/// completed assistant message made only of `output_text` blocks is accepted.
fn responses_message_text(envelope: &ResponsesEnvelope) -> Option<String> {
    if envelope.status != "completed" {
        return None;
    }

    let mut message = None;
    for item in &envelope.output {
        if item.kind != "message" || item.role.as_deref() != Some("assistant") {
            continue;
        }
        if message.is_some() {
            return None;
        }
        message = Some(responses_output_text(item.content.as_deref()?)?);
    }
    message
}

fn responses_output_text(content: &[ResponsesContent]) -> Option<String> {
    let mut text = String::new();
    for block in content {
        if block.kind != "output_text" {
            return None;
        }
        let part = block.text.as_deref()?;
        let new_len = text.len().checked_add(part.len())?;
        if new_len > MAX_MODEL_CONTENT_BYTES {
            return None;
        }
        text.push_str(part);
    }
    (!text.is_empty()).then_some(text)
}

fn bounded_text(text: &str) -> Option<&str> {
    (text.len() <= MAX_MODEL_CONTENT_BYTES).then_some(text)
}

/// Removes exactly one complete Markdown JSON fence. It deliberately does not
/// search through prose for a JSON-looking fragment, because only the model's
/// complete response may be used as the safety-validated payload.
fn unwrap_single_json_fence(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(after_opening_fence) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some(line_end) = after_opening_fence.find('\n') else {
        return trimmed;
    };
    let language = after_opening_fence[..line_end].trim();
    if !language.is_empty() && !language.eq_ignore_ascii_case("json") {
        return trimmed;
    }
    let fenced_content = &after_opening_fence[line_end + 1..];
    let Some(without_closing_fence) = fenced_content.strip_suffix("```") else {
        return trimmed;
    };

    without_closing_fence.trim()
}

fn chat_completions_endpoint(base_url: &str) -> Result<String, AiServiceError> {
    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    reqwest::Url::parse(&endpoint).map_err(|_| AiServiceError::configuration())?;
    Ok(endpoint)
}

fn responses_endpoint(base_url: &str) -> Result<String, AiServiceError> {
    let endpoint = format!("{}/responses", base_url.trim_end_matches('/'));
    reqwest::Url::parse(&endpoint).map_err(|_| AiServiceError::configuration())?;
    Ok(endpoint)
}

fn analysis_endpoint(base_url: &str, protocol: AiApiProtocol) -> Result<String, AiServiceError> {
    match protocol {
        AiApiProtocol::OpenAiResponses => responses_endpoint(base_url),
        AiApiProtocol::OpenAiCompatible => chat_completions_endpoint(base_url),
    }
}

fn models_endpoint(base_url: &str) -> Result<String, AiServiceError> {
    let endpoint = format!("{}/models", base_url.trim_end_matches('/'));
    reqwest::Url::parse(&endpoint).map_err(|_| AiServiceError::configuration())?;
    Ok(endpoint)
}

#[derive(Debug, Clone, Copy)]
enum ResponseFormat {
    JsonSchema,
    JsonObject,
}

fn analysis_request_body(
    protocol: AiApiProtocol,
    model: &str,
    entries: &[devresidue_core::ai::SanitizedEntry],
    paths: Option<&[PathBuf]>,
    response_format: ResponseFormat,
) -> Result<Value, AiServiceError> {
    match protocol {
        AiApiProtocol::OpenAiResponses => {
            responses_request_body(model, entries, paths, response_format)
        }
        AiApiProtocol::OpenAiCompatible => {
            chat_completions_request_body(model, entries, paths, response_format)
        }
    }
}

fn sanitized_entries_json(
    entries: &[devresidue_core::ai::SanitizedEntry],
    paths: Option<&[PathBuf]>,
) -> Result<String, AiServiceError> {
    let entries = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut entry =
                serde_json::to_value(entry).map_err(|_| AiServiceError::configuration())?;
            if let Some(paths) = paths {
                let object = entry
                    .as_object_mut()
                    .ok_or_else(AiServiceError::configuration)?;
                object.insert(
                    "local_path".to_string(),
                    Value::String(path_to_outbound_string(&paths[index])?),
                );
            }
            Ok(entry)
        })
        .collect::<Result<Vec<_>, AiServiceError>>()?;
    serde_json::to_string(&entries).map_err(|_| AiServiceError::configuration())
}

fn chat_completions_request_body(
    model: &str,
    entries: &[devresidue_core::ai::SanitizedEntry],
    paths: Option<&[PathBuf]>,
    response_format: ResponseFormat,
) -> Result<Value, AiServiceError> {
    let entries_content = sanitized_entries_json(entries, paths)?;
    let response_format = match response_format {
        ResponseFormat::JsonSchema => json!({
            "type": "json_schema",
            "json_schema": {
                "name": "devresidue_ai_suggestions",
                "strict": true,
                "schema": suggestion_schema(entries.len()),
            }
        }),
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
    };
    Ok(json!({
        "model": model,
        "temperature": 0,
        "response_format": response_format,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": entries_content},
        ]
    }))
}

fn responses_request_body(
    model: &str,
    entries: &[devresidue_core::ai::SanitizedEntry],
    paths: Option<&[PathBuf]>,
    response_format: ResponseFormat,
) -> Result<Value, AiServiceError> {
    let entries_content = sanitized_entries_json(entries, paths)?;
    let format = match response_format {
        ResponseFormat::JsonSchema => json!({
            "type": "json_schema",
            "name": "devresidue_ai_suggestions",
            "strict": true,
            "schema": suggestion_schema(entries.len()),
        }),
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
    };
    Ok(json!({
        "model": model,
        "temperature": 0,
        "store": false,
        "instructions": SYSTEM_PROMPT,
        "input": entries_content,
        "text": {"format": format},
    }))
}

fn send_with_retries(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    body: &Value,
    cancellation: &AiCancellationToken,
    deadline: Instant,
) -> Result<Response, AiServiceError> {
    for attempt in 0..=MAX_RETRIES {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(AiServiceError::request_deadline_exceeded)?;
        let response = client
            .post(endpoint)
            .timeout(remaining)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .json(body)
            .send();

        match response {
            Ok(response)
                if is_retryable_status(response.status().as_u16()) && attempt < MAX_RETRIES =>
            {
                let delay = retry_delay(&response, attempt);
                wait_for_retry(delay, cancellation, deadline)?;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt < MAX_RETRIES => {
                wait_for_transport_retry(
                    exponential_backoff(attempt),
                    cancellation,
                    deadline,
                    transport_error_stage(&error),
                )?;
            }
            Err(error) => {
                return Err(AiServiceError::request_failed_at(
                    None,
                    None,
                    transport_error_stage(&error),
                ));
            }
        }
    }
    Err(AiServiceError::request_deadline_exceeded())
}

fn send_connection_with_retries(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    cancellation: &AiCancellationToken,
    deadline: Instant,
) -> Result<Response, AiServiceError> {
    for attempt in 0..=MAX_RETRIES {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(AiServiceError::request_deadline_exceeded)?;
        let response = client
            .get(endpoint)
            .timeout(remaining)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(ACCEPT, "application/json")
            .send();

        match response {
            Ok(response)
                if is_retryable_status(response.status().as_u16()) && attempt < MAX_RETRIES =>
            {
                let delay = retry_delay(&response, attempt);
                wait_for_retry(delay, cancellation, deadline)?;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt < MAX_RETRIES => {
                wait_for_transport_retry(
                    exponential_backoff(attempt),
                    cancellation,
                    deadline,
                    transport_error_stage(&error),
                )?;
            }
            Err(error) => {
                return Err(AiServiceError::request_failed_at(
                    None,
                    None,
                    transport_error_stage(&error),
                ));
            }
        }
    }
    Err(AiServiceError::request_deadline_exceeded())
}

fn transport_error_stage(error: &reqwest::Error) -> AiDiagnosticStage {
    if error.is_timeout() {
        AiDiagnosticStage::RequestTimeout
    } else if error.is_connect() {
        AiDiagnosticStage::RequestConnection
    } else {
        AiDiagnosticStage::RequestTransport
    }
}

fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429) || (500..=599).contains(&status)
}

fn retry_delay(response: &Response, attempt: usize) -> Duration {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .map(|delay| delay.min(MAX_RETRY_AFTER))
        .unwrap_or_else(|| exponential_backoff(attempt))
}

fn exponential_backoff(attempt: usize) -> Duration {
    let multiplier = 1_u32.checked_shl(attempt as u32).unwrap_or(u32::MAX);
    let base = RETRY_BACKOFF_BASE.saturating_mul(multiplier);
    let max_jitter_nanos = base.as_nanos() / 2;
    let elapsed_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let jitter_nanos = elapsed_nanos % (max_jitter_nanos + 1);
    base.saturating_add(Duration::from_nanos(jitter_nanos as u64))
}

fn wait_for_retry(
    delay: Duration,
    cancellation: &AiCancellationToken,
    deadline: Instant,
) -> Result<(), AiServiceError> {
    let until = Instant::now()
        .checked_add(delay)
        .ok_or_else(AiServiceError::request_deadline_exceeded)?;
    while Instant::now() < until {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        let remaining_delay = until.saturating_duration_since(Instant::now());
        let remaining_budget = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(AiServiceError::request_deadline_exceeded)?;
        thread::sleep(
            remaining_delay
                .min(remaining_budget)
                .min(Duration::from_millis(25)),
        );
    }
    if cancellation.is_cancelled() {
        Err(AiServiceError::cancelled())
    } else {
        Ok(())
    }
}

fn wait_for_transport_retry(
    delay: Duration,
    cancellation: &AiCancellationToken,
    deadline: Instant,
    failure_stage: AiDiagnosticStage,
) -> Result<(), AiServiceError> {
    wait_for_retry(delay, cancellation, deadline).map_err(|error| {
        if error.diagnostic_stage() == AiDiagnosticStage::RequestDeadlineExceeded {
            AiServiceError::request_failed_at(None, None, failure_stage)
        } else {
            error
        }
    })
}

fn request_id(response: &Response) -> Option<String> {
    response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_safe_request_id(value))
        .map(str::to_owned)
}

fn is_safe_request_id(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && ![
            "authorization",
            "bearer",
            "api-key",
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
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn validate_outbound_paths(
    entries: &[devresidue_core::ai::SanitizedEntry],
    paths: Option<&[PathBuf]>,
) -> Result<(), AiServiceError> {
    let Some(paths) = paths else {
        return Ok(());
    };
    if paths.len() != entries.len() {
        return Err(AiServiceError::configuration());
    }
    for path in paths {
        let value = path
            .to_str()
            .filter(|value| !value.is_empty() && value.len() <= MAX_OUTBOUND_PATH_CHARS)
            .ok_or_else(AiServiceError::configuration)?;
        if value.chars().any(char::is_control) {
            return Err(AiServiceError::configuration());
        }
    }
    Ok(())
}

fn path_to_outbound_string(path: &PathBuf) -> Result<String, AiServiceError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(AiServiceError::configuration)
}

fn is_safe_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_ID_CHARS
        && value.chars().all(|character| !character.is_control())
}

fn suggestion_schema(entry_count: usize) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["suggestions"],
        "properties": {
            "suggestions": {
                "type": "array",
                "minItems": entry_count,
                "maxItems": entry_count,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "suggested_risk", "confidence", "reason", "product_guess"],
                    "properties": {
                        "id": {"type": "string"},
                        "suggested_risk": {
                            "type": "string",
                            "enum": ["safe", "regenerable-local", "regenerable-download", "review", "protected", "unknown"]
                        },
                        "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                        "reason": {"type": "string", "maxLength": 240},
                        "product_guess": {"type": ["string", "null"], "maxLength": 80}
                    }
                }
            }
        }
    })
}

#[derive(Deserialize)]
struct ChatCompletionEnvelope {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<Value>,
}

#[derive(Deserialize)]
struct ResponsesEnvelope {
    status: String,
    output: Vec<ResponsesOutputItem>,
}

#[derive(Deserialize)]
struct ResponsesOutputItem {
    #[serde(rename = "type")]
    kind: String,
    role: Option<String>,
    content: Option<Vec<ResponsesContent>>,
}

#[derive(Deserialize)]
struct ResponsesContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ModelListEnvelope {
    data: Vec<ModelListEntry>,
}

#[derive(Deserialize)]
struct ModelListEntry {
    id: String,
}
