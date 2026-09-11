//! Remote-AI Tauri commands and their process-local review state.
//!
//! This module is intentionally a thin, ID-only shell over devresidue-ai.
//! API keys flow only into profile upsert and the Windows environment-key
//! port; they are never placed in a DTO, command result or session state.
//! Remote suggestions stay in the service's in-memory binding and this module
//! has no cleanup-plan or deletion import.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use devresidue_ai::{
    AiAdvisorService, AiCancellationToken, AiConfirmationConfig, AiConfirmationSelection,
    AiProfileInput, AiProfileStore, AiReviewBatch, AiServiceError, AiServiceErrorKind,
    OpenAiCompatibleTransport, StoredProfiles,
};
use devresidue_core::ai::{AiApiProtocol, AiProfile, AiProfileId, StructuredOutputMode};
use devresidue_core::{ResidueCategory, RiskLevel, ScanItem, ScanItemId};
use devresidue_platform_windows::env_key::WindowsUserEnvKeyStore;
use devresidue_platform_windows::profile_file::WindowsAiProfileFilePort;
use devresidue_platform_windows::user_rule_tx::WindowsUserRuleTransactionPort;
use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};
use tauri::State;

use crate::contract::{
    AiApiProtocolArg, AiConfirmResultDto, AiPreparedBatchDto, AiPreparedEntryDto, AiProfileDto,
    AiProfileStateDto, AiSuggestionDto, CommandError, ErrorCode, RiskLevelArg,
    StructuredOutputModeArg,
};
use crate::state::{AppModel, AppState};
use crate::support;

const MAX_REVIEW_BATCHES: usize = 4;
const AI_DIAGNOSTICS_ENV: &str = "DEVRESIDUE_AI_DIAGNOSTICS";
const AI_DIAGNOSTICS_FILE: &str = "remote-ai-diagnostics.log";
const AI_DIAGNOSTICS_PREFIX: &str = "[AI-DIAG-9F4B]";
const MAX_AI_DIAGNOSTIC_LOG_BYTES: usize = 32 * 1024;

/// Lists all non-secret profile metadata and the independent remote-AI master
/// switch. The API Key and generated environment-variable name are absent.
#[tauri::command]
pub fn ai_list_profiles(state: State<'_, AppState>) -> Result<AiProfileStateDto, CommandError> {
    let data_dir = data_dir(&state);
    profile_state(&open_profile_store(&data_dir)?)
}

/// Creates or updates a profile. api_key is the one input-only secret field:
/// it is borrowed by the Windows environment-key port then dropped before a
/// DTO is returned.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn ai_upsert_profile(
    state: State<'_, AppState>,
    profile_id: Option<String>,
    name: String,
    base_url: String,
    model: String,
    api_protocol: AiApiProtocolArg,
    structured_output: StructuredOutputModeArg,
    timeout_secs: u64,
    enabled: bool,
    api_key: String,
) -> Result<AiProfileDto, CommandError> {
    let data_dir = data_dir(&state);
    let store = open_profile_store(&data_dir)?;
    let input = AiProfileInput {
        name,
        base_url,
        model,
        api_protocol: ai_api_protocol(api_protocol),
        structured_output_mode: structured_output_mode(structured_output),
        timeout_secs,
        enabled,
    };
    let keys = WindowsUserEnvKeyStore::new();
    let profile = match profile_id {
        Some(id) => store.update(parse_profile_id(&id)?, input, &api_key, &keys),
        None => store.upsert(input, &api_key, &keys),
    }
    .map_err(profile_error)?;
    drop(api_key);

    // A changed endpoint/model/key must never reuse an earlier prepared
    // request or suggestion, even though neither is persisted.
    state.model.lock().unwrap().ai.clear();
    let active = store
        .load()
        .map_err(profile_error)?
        .active_profile_id()
        .is_some_and(|id| id == profile.id());
    Ok(profile_dto(&profile, active))
}

/// Deletes exactly one profile through the platform key port and invalidates
/// every process-local review batch afterwards.
#[tauri::command]
pub fn ai_delete_profile(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), CommandError> {
    let data_dir = data_dir(&state);
    let store = open_profile_store(&data_dir)?;
    let keys = WindowsUserEnvKeyStore::new();
    store
        .delete(parse_profile_id(&profile_id)?, &keys)
        .map_err(profile_error)?;
    state.model.lock().unwrap().ai.clear();
    Ok(())
}

/// Selects the profile used by later remote analysis. Passing null clears the
/// active selection; neither form accepts an environment-variable name.
#[tauri::command]
pub fn ai_set_active_profile(
    state: State<'_, AppState>,
    profile_id: Option<String>,
) -> Result<AiProfileStateDto, CommandError> {
    let data_dir = data_dir(&state);
    let store = open_profile_store(&data_dir)?;
    let id = profile_id.as_deref().map(parse_profile_id).transpose()?;
    let profiles = store.set_active_profile(id).map_err(profile_error)?;
    state.model.lock().unwrap().ai.clear();
    Ok(profile_state_dto(&profiles))
}

/// Changes the separate remote-AI master switch. Disabling immediately
/// invalidates pending review/session state; it does not alter any user rule.
#[tauri::command]
pub fn ai_set_master_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<AiProfileStateDto, CommandError> {
    let data_dir = data_dir(&state);
    let store = open_profile_store(&data_dir)?;
    let profiles = store.set_master_enabled(enabled).map_err(profile_error)?;
    if !enabled {
        state.model.lock().unwrap().ai.clear();
    }
    Ok(profile_state_dto(&profiles))
}

/// Performs a minimal authenticated endpoint check for a profile. It sends no
/// scan items, metadata batch or prompt.
#[tauri::command]
pub fn ai_test_connection(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<(), CommandError> {
    let data_dir = data_dir(&state);
    let service = Arc::new(advisor_service(&data_dir)?);
    let cancellation = AiCancellationToken::new();
    service
        .test_connection(&parse_profile_id(&profile_id)?, &cancellation)
        .map_err(ai_error)
}

/// Lists safe model ids for one saved profile without sending scan metadata.
/// This configuration operation remains available while the analysis master
/// switch is off, so users can configure and validate a profile first.
#[tauri::command]
pub fn ai_list_models(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<Vec<String>, CommandError> {
    let data_dir = data_dir(&state);
    let service = Arc::new(advisor_service(&data_dir)?);
    let cancellation = AiCancellationToken::new();
    service
        .list_models(&parse_profile_id(&profile_id)?, &cancellation)
        .map_err(ai_error)
}

/// Prepares a consent preview from an HMAC-verified, completed real scan. The
/// response has only sanitized metadata plus ScanItemId; the service's entry
/// token remains process-local.
#[tauri::command]
pub async fn ai_prepare_batch(
    state: State<'_, AppState>,
    profile_id: String,
    scan_generation: u64,
    item_ids: Vec<u64>,
    include_paths: bool,
) -> Result<AiPreparedBatchDto, CommandError> {
    let data_dir = data_dir(&state);
    let model = Arc::clone(&state.model);
    run_ai_blocking(move || {
        prepare_batch(
            &model,
            &data_dir,
            profile_id,
            scan_generation,
            item_ids,
            include_paths,
        )
    })
    .await
}

/// Performs synchronous snapshot/profile preparation away from the window
/// thread. It only creates an in-memory review session; no metadata is sent.
fn prepare_batch(
    model: &Arc<Mutex<AppModel>>,
    data_dir: &Path,
    profile_id: String,
    scan_generation: u64,
    item_ids: Vec<u64>,
    include_paths: bool,
) -> Result<AiPreparedBatchDto, CommandError> {
    let profile_id = parse_profile_id(&profile_id)?;
    let store = open_profile_store(data_dir)?;
    ensure_profile_ready_for_analysis(&store, &profile_id)?;

    let snapshot = latest_real_snapshot(data_dir, scan_generation)?;
    let selected = select_snapshot_items(&snapshot, &item_ids)?;
    let service = Arc::new(advisor_service(data_dir)?);
    let review = service
        .prepare_review_batch_with_paths(&selected, snapshot.generation, profile_id, include_paths)
        .map_err(ai_error)?;
    let dto = prepared_batch_dto(&review);

    let mut model = model.lock().unwrap();
    if model
        .latest()
        .is_none_or(|latest| latest.generation != snapshot.generation)
    {
        service.clear_suggestions();
        return Err(batch_expired());
    }
    model.ai.insert_review(service, review);
    Ok(dto)
}

/// Sends exactly one previously prepared batch. Only its opaque in-memory
/// batch id crosses IPC; a second request is rejected until the first ends.
#[tauri::command]
pub async fn ai_analyze(
    state: State<'_, AppState>,
    batch_id: String,
) -> Result<Vec<AiSuggestionDto>, CommandError> {
    let data_dir = data_dir(&state);
    let model = Arc::clone(&state.model);
    let (service, review, cancellation) = {
        let mut model = model.lock().unwrap();
        model.ai.begin_analysis(&batch_id).map_err(session_error)?
    };

    let worker_model = Arc::clone(&model);
    let worker_batch_id = batch_id.clone();
    let result = run_ai_blocking(move || {
        analyze_started_batch(
            &worker_model,
            &data_dir,
            &worker_batch_id,
            service,
            review,
            cancellation,
        )
    })
    .await;

    // A worker panic or runtime shutdown cannot leave the UI in a permanent
    // "analyzing" state. Normal analysis errors already finish the request.
    if result.is_err() {
        model.lock().unwrap().ai.finish_analysis(&batch_id);
    }
    result
}

/// Runs one already-registered analysis. The caller creates the session and
/// token before dispatching this synchronous work so cancellation is available
/// immediately, even while the worker waits on a remote endpoint.
fn analyze_started_batch(
    model: &Arc<Mutex<AppModel>>,
    data_dir: &Path,
    batch_id: &str,
    service: Arc<AiAdvisorService>,
    review: AiReviewBatch,
    cancellation: AiCancellationToken,
) -> Result<Vec<AiSuggestionDto>, CommandError> {
    if let Err(error) = latest_real_snapshot(data_dir, review.batch().scan_generation) {
        let mut model = model.lock().unwrap();
        model.ai.finish_analysis(&batch_id);
        model.ai.remove_batch(&batch_id);
        return Err(error);
    }

    let result = service.analyze(
        review.batch(),
        review.batch().scan_generation,
        &cancellation,
    );
    let batch_still_registered = {
        let mut model = model.lock().unwrap();
        model.ai.finish_analysis(&batch_id);
        model.ai.has_batch(&batch_id)
    };
    if !batch_still_registered {
        service.clear_suggestions();
        return Err(batch_expired());
    }

    let suggestions = match result {
        Ok(suggestions) => suggestions,
        Err(error) => {
            record_ai_diagnostic(data_dir, &error);
            if error.kind() == AiServiceErrorKind::StaleGeneration {
                model.lock().unwrap().ai.remove_batch(batch_id);
            }
            return Err(ai_error(error));
        }
    };

    if let Err(error) = latest_real_snapshot(data_dir, review.batch().scan_generation) {
        model.lock().unwrap().ai.remove_batch(batch_id);
        return Err(error);
    }
    suggestion_dtos(&review, &suggestions)
}

/// Confirms user-selected final risks and categories through the Task 7 Core
/// transaction. No path, rule text, cleanup plan or model token is accepted
/// from IPC.
#[tauri::command]
pub fn ai_confirm(
    state: State<'_, AppState>,
    batch_id: String,
    scan_generation: u64,
    items: Vec<AiConfirmItemArg>,
) -> Result<AiConfirmResultDto, CommandError> {
    let data_dir = data_dir(&state);
    let (service, review) = {
        let model = state.model.lock().unwrap();
        model
            .ai
            .review_for_confirm(&batch_id)
            .map_err(session_error)?
    };
    if review.batch().scan_generation != scan_generation {
        return Err(batch_expired());
    }
    latest_real_snapshot(&data_dir, scan_generation)?;
    let selections = confirmation_selections(items)?;

    match service.confirm_suggestions(&review.batch().id, scan_generation, &selections) {
        Ok(result) => {
            state.model.lock().unwrap().ai.remove_batch(&batch_id);
            Ok(AiConfirmResultDto {
                confirmed_count: result.written_rule_ids.len(),
                audit_warning: result.audit_warning.is_some(),
            })
        }
        Err(error) => {
            if error.kind() == AiServiceErrorKind::StaleGeneration {
                state.model.lock().unwrap().ai.remove_batch(&batch_id);
            }
            Err(ai_error(error))
        }
    }
}

/// Cooperatively cancels only the currently running analysis for batch_id.
/// The remote socket is bounded by its request timeout; no rule is written.
#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>, batch_id: String) -> bool {
    state.model.lock().unwrap().ai.cancel(&batch_id)
}

/// Discards one prepared, not-currently-running batch so the user can return
/// to candidate selection. It has no cleanup or rule-writing effect.
#[tauri::command]
pub fn ai_discard_batch(state: State<'_, AppState>, batch_id: String) -> bool {
    state.model.lock().unwrap().ai.discard(&batch_id)
}

/// Actual item argument for ai_confirm. It has closed risk/category enums and
/// no path field; the test-only macro mirror in ipc_contract pins its wire
/// form.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiConfirmItemArg {
    item_id: u64,
    final_risk: RiskLevelArg,
    final_category: ResidueCategory,
}

/// Process-local remote-AI state behind AppModel. It is deliberately not
/// serializable and stores neither an API Key nor a raw request/response.
#[derive(Default)]
pub struct AiSessionState {
    batches: HashMap<String, AiReviewSession>,
    active_request: Option<ActiveAiRequest>,
}

struct AiReviewSession {
    service: Arc<AiAdvisorService>,
    review: AiReviewBatch,
}

struct ActiveAiRequest {
    batch_id: String,
    cancellation: AiCancellationToken,
}

impl fmt::Debug for AiSessionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiSessionState")
            .field("prepared_batch_count", &self.batches.len())
            .field("has_active_request", &self.active_request.is_some())
            .finish()
    }
}

impl AiSessionState {
    pub fn clear(&mut self) {
        if let Some(active) = &self.active_request {
            active.cancellation.cancel();
        }
        for session in self.batches.values() {
            session.service.clear_suggestions();
        }
        self.batches.clear();
        self.active_request = None;
    }

    fn insert_review(&mut self, service: Arc<AiAdvisorService>, review: AiReviewBatch) {
        if self.batches.len() >= MAX_REVIEW_BATCHES {
            self.clear();
        }
        self.batches.insert(
            review.batch().id.as_str().to_string(),
            AiReviewSession { service, review },
        );
    }

    fn begin_analysis(
        &mut self,
        batch_id: &str,
    ) -> Result<(Arc<AiAdvisorService>, AiReviewBatch, AiCancellationToken), AiSessionError> {
        if self.active_request.is_some() {
            return Err(AiSessionError::InProgress);
        }
        let session = self.batches.get(batch_id).ok_or(AiSessionError::Expired)?;
        let cancellation = AiCancellationToken::new();
        self.active_request = Some(ActiveAiRequest {
            batch_id: batch_id.to_string(),
            cancellation: cancellation.clone(),
        });
        Ok((
            Arc::clone(&session.service),
            session.review.clone(),
            cancellation,
        ))
    }

    fn finish_analysis(&mut self, batch_id: &str) {
        if self
            .active_request
            .as_ref()
            .is_some_and(|active| active.batch_id == batch_id)
        {
            self.active_request = None;
        }
    }

    fn review_for_confirm(
        &self,
        batch_id: &str,
    ) -> Result<(Arc<AiAdvisorService>, AiReviewBatch), AiSessionError> {
        if self.active_request.is_some() {
            return Err(AiSessionError::InProgress);
        }
        let session = self.batches.get(batch_id).ok_or(AiSessionError::Expired)?;
        Ok((Arc::clone(&session.service), session.review.clone()))
    }

    fn remove_batch(&mut self, batch_id: &str) {
        if let Some(session) = self.batches.remove(batch_id) {
            session.service.clear_suggestions();
        }
    }

    fn has_batch(&self, batch_id: &str) -> bool {
        self.batches.contains_key(batch_id)
    }

    fn cancel(&mut self, batch_id: &str) -> bool {
        match &self.active_request {
            Some(active) if active.batch_id == batch_id => {
                active.cancellation.cancel();
                true
            }
            _ => false,
        }
    }

    fn discard(&mut self, batch_id: &str) -> bool {
        if self
            .active_request
            .as_ref()
            .is_some_and(|active| active.batch_id == batch_id)
        {
            return false;
        }
        if self.batches.contains_key(batch_id) {
            self.remove_batch(batch_id);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AiSessionError {
    InProgress,
    Expired,
}

fn data_dir(state: &State<'_, AppState>) -> PathBuf {
    state.model.lock().unwrap().data_dir().to_path_buf()
}

/// Synchronous profile, snapshot and HTTP work must not run on the Tauri
/// window thread. The command future remains pending, but the WebView can keep
/// rendering its preparing/analyzing state and process cancellation requests.
async fn run_ai_blocking<T, F>(operation: F) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CommandError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| {
            CommandError::new(
                ErrorCode::Engine,
                format!("remote AI task join failed: {error}"),
            )
        })?
}

fn open_profile_store(data_dir: &Path) -> Result<AiProfileStore, CommandError> {
    AiProfileStore::open_with_file_port(data_dir, Arc::new(WindowsAiProfileFilePort::new()))
        .map_err(profile_error)
}

fn advisor_service(data_dir: &Path) -> Result<AiAdvisorService, CommandError> {
    let builtin_rules_dir = support::locate_rules_dir().ok_or_else(|| {
        CommandError::new(
            ErrorCode::AiNotConfigured,
            "built-in rules are unavailable for remote-AI confirmation",
        )
    })?;
    Ok(AiAdvisorService::with_confirmation(
        open_profile_store(data_dir)?,
        Arc::new(WindowsUserEnvKeyStore::new()),
        Arc::new(OpenAiCompatibleTransport::new()),
        AiConfirmationConfig::new(
            data_dir,
            Arc::new(WindowsUserRuleTransactionPort::new()),
            builtin_rules_dir,
        ),
    ))
}

fn profile_state(store: &AiProfileStore) -> Result<AiProfileStateDto, CommandError> {
    store
        .load()
        .map_err(profile_error)
        .map(|state| profile_state_dto(&state))
}

fn profile_state_dto(state: &StoredProfiles) -> AiProfileStateDto {
    let active = state.active_profile_id();
    AiProfileStateDto {
        master_enabled: state.master_enabled(),
        profiles: state
            .profiles()
            .iter()
            .map(|profile| profile_dto(profile, active == Some(profile.id())))
            .collect(),
    }
}

fn profile_dto(profile: &AiProfile, is_active: bool) -> AiProfileDto {
    AiProfileDto {
        id: profile.id().as_str().to_string(),
        name: profile.name().to_string(),
        base_url: profile.base_url().to_string(),
        model: profile.model().to_string(),
        api_protocol: ai_api_protocol_arg(profile.api_protocol()),
        structured_output: structured_output_mode_arg(profile.structured_output_mode()),
        enabled: profile.enabled(),
        is_active,
    }
}

fn ensure_profile_ready_for_analysis(
    store: &AiProfileStore,
    profile_id: &AiProfileId,
) -> Result<(), CommandError> {
    let state = store.load().map_err(profile_error)?;
    if !state.master_enabled() {
        return Err(CommandError::new(
            ErrorCode::AiDisabled,
            "remote AI is disabled",
        ));
    }
    if state.active_profile_id() != Some(profile_id) {
        return Err(CommandError::new(
            ErrorCode::AiNotConfigured,
            "the selected remote-AI profile is not active",
        ));
    }
    if store
        .resolve_usable_profile(profile_id)
        .map_err(profile_error)?
        .is_none()
    {
        return Err(CommandError::new(
            ErrorCode::AiNotConfigured,
            "the selected remote-AI profile is unavailable",
        ));
    }
    Ok(())
}

fn latest_real_snapshot(
    data_dir: &Path,
    expected_generation: u64,
) -> Result<ScanSnapshot, CommandError> {
    let snapshot = scan_store::load(data_dir).map_err(|_| {
        CommandError::new(
            ErrorCode::AiBatchExpired,
            "remote AI requires a current completed real scan",
        )
    })?;
    if !matches!(snapshot.mode, ScanMode::Real { .. })
        || snapshot.cancelled
        || snapshot.generation != expected_generation
    {
        return Err(batch_expired());
    }
    Ok(snapshot)
}

fn select_snapshot_items(
    snapshot: &ScanSnapshot,
    item_ids: &[u64],
) -> Result<Vec<ScanItem>, CommandError> {
    if item_ids.is_empty() {
        return Err(CommandError::new(
            ErrorCode::InvalidItem,
            "select at least one current scan item for remote-AI review",
        ));
    }
    let mut seen = HashSet::with_capacity(item_ids.len());
    let mut selected = Vec::with_capacity(item_ids.len());
    for raw_id in item_ids {
        let id = ScanItemId::from_raw(*raw_id);
        if !seen.insert(id) {
            return Err(CommandError::new(
                ErrorCode::InvalidItem,
                "remote-AI item ids must be unique",
            ));
        }
        let item = snapshot
            .items
            .iter()
            .find(|item| item.id == id)
            .cloned()
            .ok_or_else(|| {
                CommandError::new(
                    ErrorCode::InvalidItem,
                    "a remote-AI item does not belong to the current scan",
                )
            })?;
        selected.push(item);
    }
    Ok(selected)
}

fn prepared_batch_dto(review: &AiReviewBatch) -> AiPreparedBatchDto {
    let entries = review
        .batch()
        .entries
        .iter()
        .zip(review.item_ids())
        .map(|(entry, item_id)| AiPreparedEntryDto {
            item_id: item_id.raw(),
            zone: kebab(&entry.zone),
            relative_depth: entry.relative_depth,
            display_name: entry.display_name.clone(),
            source_kind: entry.source_kind.clone(),
            category_hint: entry.category_hint.clone(),
            product_hint: entry.product_hint.clone(),
            size_bucket: kebab(&entry.size_bucket),
            age_bucket: kebab(&entry.age_bucket),
            signals: entry.signals.clone(),
        })
        .collect();
    AiPreparedBatchDto {
        batch_id: review.batch().id.as_str().to_string(),
        scan_generation: review.batch().scan_generation,
        profile_id: review.batch().profile_id.as_str().to_string(),
        includes_paths: review.includes_paths(),
        entries,
    }
}

fn suggestion_dtos(
    review: &AiReviewBatch,
    suggestions: &[devresidue_core::ai::AiSuggestion],
) -> Result<Vec<AiSuggestionDto>, CommandError> {
    let item_by_token: HashMap<&str, u64> = review
        .batch()
        .entries
        .iter()
        .zip(review.item_ids())
        .map(|(entry, item_id)| (entry.id.as_str(), item_id.raw()))
        .collect();
    suggestions
        .iter()
        .map(|suggestion| {
            let item_id = item_by_token
                .get(suggestion.token.as_str())
                .copied()
                .ok_or_else(|| {
                    CommandError::new(
                        ErrorCode::AiRequestFailed,
                        "validated remote-AI suggestions no longer match this review",
                    )
                })?;
            Ok(AiSuggestionDto {
                item_id,
                suggested_risk: kebab(&suggestion.suggested_risk),
                confidence: suggestion.confidence,
                reason: bounded_display(&suggestion.reason, 240),
                product_guess: suggestion
                    .product_guess
                    .as_deref()
                    .map(|value| bounded_display(value, 80)),
                final_risk_options: final_risk_options(),
            })
        })
        .collect()
}

fn confirmation_selections(
    items: Vec<AiConfirmItemArg>,
) -> Result<Vec<AiConfirmationSelection>, CommandError> {
    if items.is_empty() {
        return Err(CommandError::new(
            ErrorCode::InvalidItem,
            "select at least one remote-AI suggestion to confirm",
        ));
    }
    let mut seen = HashSet::with_capacity(items.len());
    let mut selections = Vec::with_capacity(items.len());
    for item in items {
        let item_id = ScanItemId::from_raw(item.item_id);
        if !seen.insert(item_id) {
            return Err(CommandError::new(
                ErrorCode::InvalidItem,
                "remote-AI confirmation item ids must be unique",
            ));
        }
        if item.final_category == ResidueCategory::Unknown {
            return Err(CommandError::new(
                ErrorCode::InvalidItem,
                "remote-AI confirmation category must not be unknown",
            ));
        }
        selections.push(AiConfirmationSelection::new(
            item_id,
            core_risk(item.final_risk),
            item.final_category,
        ));
    }
    Ok(selections)
}

fn final_risk_options() -> Vec<RiskLevelArg> {
    vec![
        RiskLevelArg::Safe,
        RiskLevelArg::RegenerableLocal,
        RiskLevelArg::RegenerableDownload,
        RiskLevelArg::Review,
        RiskLevelArg::Protected,
    ]
}

const fn core_risk(risk: RiskLevelArg) -> RiskLevel {
    match risk {
        RiskLevelArg::Safe => RiskLevel::Safe,
        RiskLevelArg::RegenerableLocal => RiskLevel::RegenerableLocal,
        RiskLevelArg::RegenerableDownload => RiskLevel::RegenerableDownload,
        RiskLevelArg::Review => RiskLevel::Review,
        RiskLevelArg::Protected => RiskLevel::Protected,
    }
}

const fn structured_output_mode(mode: StructuredOutputModeArg) -> StructuredOutputMode {
    match mode {
        StructuredOutputModeArg::Auto => StructuredOutputMode::Auto,
        StructuredOutputModeArg::JsonSchema => StructuredOutputMode::JsonSchema,
        StructuredOutputModeArg::JsonObject => StructuredOutputMode::JsonObject,
    }
}

const fn ai_api_protocol(protocol: AiApiProtocolArg) -> AiApiProtocol {
    match protocol {
        AiApiProtocolArg::OpenAiResponses => AiApiProtocol::OpenAiResponses,
        AiApiProtocolArg::OpenAiCompatible => AiApiProtocol::OpenAiCompatible,
    }
}

const fn ai_api_protocol_arg(protocol: AiApiProtocol) -> AiApiProtocolArg {
    match protocol {
        AiApiProtocol::OpenAiResponses => AiApiProtocolArg::OpenAiResponses,
        AiApiProtocol::OpenAiCompatible => AiApiProtocolArg::OpenAiCompatible,
    }
}

const fn structured_output_mode_arg(mode: StructuredOutputMode) -> StructuredOutputModeArg {
    match mode {
        StructuredOutputMode::Auto => StructuredOutputModeArg::Auto,
        StructuredOutputMode::JsonSchema => StructuredOutputModeArg::JsonSchema,
        StructuredOutputMode::JsonObject => StructuredOutputModeArg::JsonObject,
    }
}

fn parse_profile_id(value: &str) -> Result<AiProfileId, CommandError> {
    AiProfileId::parse(value).map_err(|_| {
        CommandError::new(
            ErrorCode::AiNotConfigured,
            "remote-AI profile id must be a canonical UUID",
        )
    })
}

fn profile_error(error: String) -> CommandError {
    let message = match error.as_str() {
        "AI profile base URL must use HTTPS or explicit loopback HTTP" => {
            "远程 AI 地址必须使用 HTTPS，或使用 localhost / 127.0.0.1 的本机 HTTP 地址"
        }
        "AI profile name and model must not be empty" => "远程 AI 配置名称和模型不能为空",
        "AI profile timeout must be positive" => "远程 AI 超时必须大于 0 秒",
        "AI profile write lock is already held" => {
            "远程 AI 配置正在被另一个 DevResidue 进程修改，请稍后重试"
        }
        "unable to atomically replace AI profile file" => {
            "无法替换 AI 配置文件；配置未修改。请关闭可能占用配置文件的程序后重试"
        }
        "unable to open the user environment registry key"
        | "unable to write the user environment registry value"
        | "unable to refresh the current process environment" => {
            "无法写入当前 Windows 用户的 AI Key 环境变量；请检查 HKCU\\Environment 写入权限"
        }
        _ => "remote-AI profile configuration is unavailable",
    };

    CommandError::new(ErrorCode::AiNotConfigured, message)
}

fn ai_error(error: AiServiceError) -> CommandError {
    match error.kind() {
        AiServiceErrorKind::Disabled => {
            CommandError::new(ErrorCode::AiDisabled, "remote AI is disabled")
        }
        AiServiceErrorKind::MissingKey => CommandError::new(
            ErrorCode::AiNotConfigured,
            "联网 AI 配置缺少当前进程可读取的 Key；请重新保存该配置后重试",
        ),
        AiServiceErrorKind::NoActiveProfile | AiServiceErrorKind::ProfileUnavailable => {
            CommandError::new(ErrorCode::AiNotConfigured, "联网 AI 活动配置不可用")
        }
        AiServiceErrorKind::Configuration => CommandError::new(
            ErrorCode::AiNotConfigured,
            "联网 AI 配置无效，或该服务不支持所需的 OpenAI-compatible 接口",
        ),
        AiServiceErrorKind::StaleGeneration => batch_expired(),
        AiServiceErrorKind::Cancelled => CommandError::new(
            ErrorCode::AiRequestFailed,
            "联网 AI 分析已取消；未创建清理计划或删除数据",
        ),
        AiServiceErrorKind::NoEligibleEntries => CommandError::new(
            ErrorCode::InvalidItem,
            "所选条目中没有可进行联网 AI 研判的 Unknown 或 Review 项",
        ),
        AiServiceErrorKind::ConfirmationConflict | AiServiceErrorKind::ConfirmationFailed => CommandError::new(
            ErrorCode::AiRequestFailed,
            "联网 AI 建议未能写入本地规则；没有创建清理计划或删除数据",
        ),
        AiServiceErrorKind::InvalidResponse => CommandError::new(
            ErrorCode::AiRequestFailed,
            "联网 AI 返回内容不符合本地安全校验；请检查所选接口模式与模型是否支持 JSON 输出。未创建清理计划或删除数据",
        ),
        AiServiceErrorKind::RequestFailed => CommandError::new(
            ErrorCode::AiRequestFailed,
            remote_request_error_message(&error),
        ),
    }
}

fn remote_request_error_message(error: &AiServiceError) -> String {
    let detail = match error.status() {
        Some(400) => "服务拒绝请求（HTTP 400）。在“自动”结构化输出模式下已尝试 JSON Object 兼容格式；请检查模型名称和服务兼容性",
        Some(401 | 403) => "认证失败（HTTP 401/403）；请重新保存正确的 API Key",
        Some(404) => "服务端点或模型不存在（HTTP 404）；请检查 Base URL 是否包含 /v1 以及模型名称",
        Some(408) => "服务请求超时（HTTP 408）；请检查网络或提高超时时间",
        Some(429) => "服务限流（HTTP 429）；请稍后重试",
        Some(status) if (500..=599).contains(&status) => "服务端暂时不可用（HTTP 5xx）；请稍后重试",
        Some(status) => return format!("联网 AI 请求失败（HTTP {status}）；请检查服务、模型和配置。未创建清理计划或删除数据"),
        None => "未收到服务响应；请检查 Base URL、网络、代理和超时设置",
    };
    format!("联网 AI 请求失败：{detail}。未创建清理计划或删除数据")
}

fn session_error(error: AiSessionError) -> CommandError {
    match error {
        AiSessionError::InProgress => CommandError::new(
            ErrorCode::AiBatchInProgress,
            "another remote-AI analysis is already in progress",
        ),
        AiSessionError::Expired => batch_expired(),
    }
}

fn batch_expired() -> CommandError {
    CommandError::new(
        ErrorCode::AiBatchExpired,
        "remote-AI batch is no longer valid for the current scan",
    )
}

fn kebab<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn bounded_display(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}

/// Writes a bounded local diagnostic line only when the user explicitly
/// enables it. It intentionally omits endpoint, Key, paths, input metadata
/// and model response text.
fn record_ai_diagnostic(data_dir: &Path, error: &AiServiceError) {
    let enabled = matches!(
        std::env::var(AI_DIAGNOSTICS_ENV).as_deref(),
        Ok("1" | "true" | "TRUE")
    );
    if !enabled {
        return;
    }

    let path = data_dir.join(AI_DIAGNOSTICS_FILE);
    let _ = fs::create_dir_all(data_dir);
    let mut content = fs::read_to_string(&path).unwrap_or_default();
    content.push_str(&ai_diagnostic_line(
        error.kind(),
        error.diagnostic_stage(),
        error.status(),
    ));
    if content.len() > MAX_AI_DIAGNOSTIC_LOG_BYTES {
        let keep_from = content.len() - MAX_AI_DIAGNOSTIC_LOG_BYTES;
        let tail = content.get(keep_from..).unwrap_or_default();
        content = tail
            .split_once('\n')
            .map_or_else(String::new, |(_, remainder)| remainder.to_string());
    }
    let _ = fs::write(path, content);
}

fn ai_diagnostic_line(
    kind: AiServiceErrorKind,
    stage: devresidue_ai::AiDiagnosticStage,
    status: Option<u16>,
) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default();
    let status = status
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string());
    format!(
        "{AI_DIAGNOSTICS_PREFIX} ts={timestamp} event=analysis-failed kind={:?} stage={} http_status={status}\n",
        kind,
        stage.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ai_diagnostic_line, bounded_display, core_risk, final_risk_options, profile_error,
        run_ai_blocking, ActiveAiRequest, AiSessionState, AI_DIAGNOSTICS_PREFIX,
    };
    use crate::contract::{ErrorCode, RiskLevelArg};
    use devresidue_ai::{AiCancellationToken, AiDiagnosticStage, AiServiceErrorKind};
    use devresidue_core::RiskLevel;

    #[test]
    fn final_confirmation_risks_are_closed_and_exclude_unknown() {
        assert_eq!(
            final_risk_options(),
            vec![
                RiskLevelArg::Safe,
                RiskLevelArg::RegenerableLocal,
                RiskLevelArg::RegenerableDownload,
                RiskLevelArg::Review,
                RiskLevelArg::Protected,
            ]
        );
        assert_eq!(core_risk(RiskLevelArg::Protected), RiskLevel::Protected);
    }

    #[test]
    fn ai_diagnostic_line_contains_only_fixed_local_fields() {
        let line = ai_diagnostic_line(
            AiServiceErrorKind::InvalidResponse,
            AiDiagnosticStage::LocalResponseValidation,
            Some(200),
        );

        assert!(line.starts_with(AI_DIAGNOSTICS_PREFIX));
        assert!(line.contains("kind=InvalidResponse"));
        assert!(line.contains("stage=local-response-validation"));
        assert!(line.contains("http_status=200"));
        for forbidden in ["sk-test-key", "C:\\\\private", "model response", "https://"] {
            assert!(!line.contains(forbidden));
        }
    }

    #[test]
    fn profile_errors_show_safe_validation_guidance_only() {
        let validation = profile_error(
            "AI profile base URL must use HTTPS or explicit loopback HTTP".to_string(),
        );
        assert_eq!(validation.code, ErrorCode::AiNotConfigured);
        assert_eq!(
            validation.message,
            "远程 AI 地址必须使用 HTTPS，或使用 localhost / 127.0.0.1 的本机 HTTP 地址"
        );

        let write = profile_error("unable to atomically replace AI profile file".to_string());
        assert_eq!(
            write.message,
            "无法替换 AI 配置文件；配置未修改。请关闭可能占用配置文件的程序后重试"
        );

        let unknown = profile_error("secret-looking low-level failure".to_string());
        assert_eq!(
            unknown.message,
            "remote-AI profile configuration is unavailable"
        );
    }

    #[test]
    fn displayed_model_text_is_bounded_and_control_free() {
        assert_eq!(bounded_display("safe\nreason\u{1b}", 20), "safereason");
        assert_eq!(bounded_display("abcdef", 3), "abc");
    }

    #[test]
    fn clearing_ai_session_cancels_any_inflight_analysis() {
        let cancellation = AiCancellationToken::new();
        let mut session = AiSessionState {
            batches: Default::default(),
            active_request: Some(ActiveAiRequest {
                batch_id: "batch-1".to_string(),
                cancellation: cancellation.clone(),
            }),
        };

        session.clear();

        assert!(cancellation.is_cancelled());
        assert!(session.active_request.is_none());
        assert!(session.batches.is_empty());
    }

    #[test]
    fn remote_ai_blocking_work_does_not_run_on_the_calling_thread() {
        let calling_thread = std::thread::current().id();
        let worker_thread =
            tauri::async_runtime::block_on(run_ai_blocking(|| Ok(std::thread::current().id())))
                .unwrap();

        assert_ne!(calling_thread, worker_thread);
    }
}
