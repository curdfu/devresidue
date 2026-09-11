//! In-memory lifecycle gate for prepared AI analysis and explicit review.
//!
//! Model output remains transient and never authors rule syntax. After an
//! explicit user selection, this module passes trusted snapshot items to
//! Core's all-or-nothing local rule transaction. It never creates a cleanup
//! plan, imports cleanup code, or invokes any deletion capability.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use devresidue_core::ai::{
    AiBatchId, AiEntryToken, AiSuggestion, AiSuggestionId, EnvKeyStore, PreparedBatch, RuleOrigin,
    RuleProvenance,
};
use devresidue_core::rules::commit_ai_rule_batch;
use devresidue_core::{ResidueCategory, RiskLevel, ScanItem, ScanItemId};
use uuid::Uuid;

use crate::confirm::FullRuleRematchVerifier;
use crate::sanitize::MAX_BATCH_ITEMS;
use crate::{
    AiAuditEvent, AiAuditResult, AiAuditWarning, AiCancellationToken, AiConfirmationConfig,
    AiConfirmationResult, AiConfirmationSelection, AiProfileStore, AiServiceError,
    AiServiceErrorKind, AiTransport, MetadataSanitizer,
};

/// Coordinates profile readiness, a transport request and ephemeral
/// suggestions for the current process.
pub struct AiAdvisorService {
    profile_store: AiProfileStore,
    keys: Arc<dyn EnvKeyStore>,
    transport: Arc<dyn AiTransport>,
    suggestions: Mutex<HashMap<AiBatchId, Vec<AiSuggestion>>>,
    candidates: Mutex<HashMap<AiBatchId, BoundBatch>>,
    confirmation: Option<AiConfirmationConfig>,
}

#[derive(Clone)]
struct BoundBatch {
    profile_id: devresidue_core::ai::AiProfileId,
    scan_generation: u64,
    candidates: HashMap<ScanItemId, BoundCandidate>,
    outbound_paths: Option<Vec<PathBuf>>,
}

#[derive(Clone)]
struct BoundCandidate {
    token: AiEntryToken,
    item: ScanItem,
}

/// A process-local prepared review batch and its trusted scan-item mapping.
/// The mapping is never serialized; callers use it only to render and confirm
/// the ids already registered in the snapshot that supplied `batch`.
#[derive(Debug, Clone)]
pub struct AiReviewBatch {
    batch: PreparedBatch,
    item_ids: Vec<ScanItemId>,
    includes_paths: bool,
}

impl AiReviewBatch {
    #[must_use]
    pub const fn batch(&self) -> &PreparedBatch {
        &self.batch
    }

    #[must_use]
    pub fn item_ids(&self) -> &[ScanItemId] {
        &self.item_ids
    }

    /// Whether this single review batch has explicit consent to send its
    /// trusted snapshot-derived paths. The paths themselves never serialize.
    #[must_use]
    pub const fn includes_paths(&self) -> bool {
        self.includes_paths
    }
}

impl AiAdvisorService {
    #[must_use]
    pub fn new(
        profile_store: AiProfileStore,
        keys: Arc<dyn EnvKeyStore>,
        transport: Arc<dyn AiTransport>,
    ) -> Self {
        Self {
            profile_store,
            keys,
            transport,
            suggestions: Mutex::new(HashMap::new()),
            candidates: Mutex::new(HashMap::new()),
            confirmation: None,
        }
    }

    /// Builds a service that can atomically confirm reviewed suggestions into
    /// local user rules through an injected platform transaction port.
    #[must_use]
    pub fn with_confirmation(
        profile_store: AiProfileStore,
        keys: Arc<dyn EnvKeyStore>,
        transport: Arc<dyn AiTransport>,
        confirmation: AiConfirmationConfig,
    ) -> Self {
        Self {
            profile_store,
            keys,
            transport,
            suggestions: Mutex::new(HashMap::new()),
            candidates: Mutex::new(HashMap::new()),
            confirmation: Some(confirmation),
        }
    }

    /// Filters an already verified current-snapshot slice, sanitizes only its
    /// eligible metadata and binds that exact candidate order in local memory.
    ///
    /// This is the single preparation path for shells: it prevents a caller
    /// from independently filtering items for the request and confirmation
    /// sides, which could otherwise mismatch entry tokens and `ScanItemId`s.
    pub fn prepare_review_batch(
        &self,
        items: &[ScanItem],
        scan_generation: u64,
        profile_id: devresidue_core::ai::AiProfileId,
    ) -> Result<AiReviewBatch, AiServiceError> {
        self.prepare_review_batch_with_paths(items, scan_generation, profile_id, false)
    }

    /// Same as [`Self::prepare_review_batch`], with an explicit, one-batch
    /// authorization to attach full paths derived from the trusted snapshot.
    /// No caller can supply a path string through this API.
    pub fn prepare_review_batch_with_paths(
        &self,
        items: &[ScanItem],
        scan_generation: u64,
        profile_id: devresidue_core::ai::AiProfileId,
        include_paths: bool,
    ) -> Result<AiReviewBatch, AiServiceError> {
        let candidates: Vec<ScanItem> = items
            .iter()
            .filter(|item| is_bindable_candidate(item))
            .take(MAX_BATCH_ITEMS)
            .cloned()
            .collect();
        if candidates.is_empty() {
            return Err(service_error(AiServiceErrorKind::NoEligibleEntries));
        }
        let batch = MetadataSanitizer::new()
            .prepare(&candidates, scan_generation, profile_id)
            .map_err(|_| AiServiceError::configuration())?;
        self.bind_trusted_candidates_with_paths(&batch, &candidates, include_paths)?;
        Ok(AiReviewBatch {
            item_ids: candidates.iter().map(|item| item.id).collect(),
            batch,
            includes_paths: include_paths,
        })
    }

    /// Performs a minimal endpoint check for a usable profile without
    /// preparing or transmitting any scan-derived metadata.
    pub fn test_connection(
        &self,
        profile_id: &devresidue_core::ai::AiProfileId,
        cancellation: &AiCancellationToken,
    ) -> Result<(), AiServiceError> {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        let state = self
            .profile_store
            .load()
            .map_err(|_| AiServiceError::configuration())?;
        if !state.master_enabled() {
            return Err(service_error(AiServiceErrorKind::Disabled));
        }
        let profile = self
            .profile_store
            .resolve_usable_profile(profile_id)
            .map_err(|_| AiServiceError::configuration())?
            .ok_or_else(|| service_error(AiServiceErrorKind::ProfileUnavailable))?;
        let api_key = self
            .keys
            .get_process(profile.api_key_env())
            .map_err(|_| AiServiceError::configuration())?
            .filter(|key| !key.is_empty())
            .ok_or_else(|| service_error(AiServiceErrorKind::MissingKey))?;
        self.transport
            .test_connection(&profile, &api_key, cancellation)
    }

    /// Discovers the endpoint's advertised models for one saved profile. This
    /// configuration action is intentionally independent of the analysis
    /// master switch and never sends scan metadata.
    pub fn list_models(
        &self,
        profile_id: &devresidue_core::ai::AiProfileId,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<String>, AiServiceError> {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        let profile = self
            .profile_store
            .resolve_usable_profile(profile_id)
            .map_err(|_| AiServiceError::configuration())?
            .ok_or_else(|| service_error(AiServiceErrorKind::ProfileUnavailable))?;
        let api_key = self
            .keys
            .get_process(profile.api_key_env())
            .map_err(|_| AiServiceError::configuration())?
            .filter(|key| !key.is_empty())
            .ok_or_else(|| service_error(AiServiceErrorKind::MissingKey))?;
        self.transport.list_models(&profile, &api_key, cancellation)
    }

    /// Binds a just-prepared batch's opaque entry tokens to trusted snapshot
    /// items in process memory only. The caller has already verified the
    /// snapshot and passes no arbitrary path strings through the public UI/CLI
    /// confirmation surface.
    pub fn bind_trusted_candidates(
        &self,
        batch: &PreparedBatch,
        items: &[ScanItem],
    ) -> Result<(), AiServiceError> {
        self.bind_trusted_candidates_with_paths(batch, items, false)
    }

    fn bind_trusted_candidates_with_paths(
        &self,
        batch: &PreparedBatch,
        items: &[ScanItem],
        include_paths: bool,
    ) -> Result<(), AiServiceError> {
        if batch.entries.is_empty() || batch.entries.len() != items.len() {
            return Err(AiServiceError::configuration());
        }
        let mut candidates = HashMap::with_capacity(items.len());
        for (entry, item) in batch.entries.iter().zip(items) {
            if !is_bindable_candidate(item)
                || candidates
                    .insert(
                        item.id,
                        BoundCandidate {
                            token: entry.id.clone(),
                            item: item.clone(),
                        },
                    )
                    .is_some()
            {
                return Err(AiServiceError::configuration());
            }
        }
        let bound = BoundBatch {
            profile_id: batch.profile_id.clone(),
            scan_generation: batch.scan_generation,
            candidates,
            outbound_paths: include_paths
                .then(|| items.iter().map(|item| item.path.clone()).collect()),
        };
        self.candidates
            .lock()
            .map_err(|_| AiServiceError::configuration())?
            .insert(batch.id.clone(), bound);
        Ok(())
    }

    /// Analyzes one prepared batch only if it still belongs to the current
    /// scan, its profile is active and usable, and a current-process key is
    /// available. No failed path stores a suggestion.
    pub fn analyze(
        &self,
        batch: &PreparedBatch,
        current_scan_generation: u64,
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        if cancellation.is_cancelled() {
            return Err(AiServiceError::cancelled());
        }
        if batch.scan_generation != current_scan_generation {
            return Err(service_error(AiServiceErrorKind::StaleGeneration));
        }
        if batch.entries.is_empty() {
            return Err(service_error(AiServiceErrorKind::NoEligibleEntries));
        }

        let state = self
            .profile_store
            .load()
            .map_err(|_| AiServiceError::configuration())?;
        if !state.master_enabled() {
            return Err(service_error(AiServiceErrorKind::Disabled));
        }
        if state.active_profile_id() != Some(&batch.profile_id) {
            return Err(service_error(AiServiceErrorKind::NoActiveProfile));
        }

        let profile = self
            .profile_store
            .resolve_usable_profile(&batch.profile_id)
            .map_err(|_| AiServiceError::configuration())?
            .ok_or_else(|| service_error(AiServiceErrorKind::ProfileUnavailable))?;
        let api_key = self
            .keys
            .get_process(profile.api_key_env())
            .map_err(|_| AiServiceError::configuration())?
            .filter(|key| !key.is_empty())
            .ok_or_else(|| service_error(AiServiceErrorKind::MissingKey))?;

        let outbound_paths = {
            let bound = self
                .candidates
                .lock()
                .map_err(|_| AiServiceError::configuration())?;
            match bound.get(&batch.id) {
                Some(bound)
                    if bound.profile_id == batch.profile_id
                        && bound.scan_generation == batch.scan_generation =>
                {
                    bound.outbound_paths.clone()
                }
                Some(_) => return Err(AiServiceError::configuration()),
                None => None,
            }
        };
        let suggestions = match outbound_paths {
            Some(paths) => self.transport.analyze_with_paths(
                &profile,
                &api_key,
                batch,
                &paths,
                cancellation,
            )?,
            None => self
                .transport
                .analyze(&profile, &api_key, batch, cancellation)?,
        };
        validate_suggestions(batch, &suggestions)?;
        let mut stored = self
            .suggestions
            .lock()
            .map_err(|_| AiServiceError::configuration())?;
        stored.insert(batch.id.clone(), suggestions.clone());
        Ok(suggestions)
    }

    /// Returns only the current process's validated suggestions for `batch`.
    #[must_use]
    pub fn suggestions(&self, batch_id: &AiBatchId) -> Option<Vec<AiSuggestion>> {
        self.suggestions
            .lock()
            .ok()
            .and_then(|stored| stored.get(batch_id).cloned())
    }

    /// Invalidates every transient suggestion, for example after a new scan,
    /// profile deletion or the master switch being disabled.
    pub fn clear_suggestions(&self) {
        if let Ok(mut stored) = self.suggestions.lock() {
            stored.clear();
        }
        if let Ok(mut bound) = self.candidates.lock() {
            bound.clear();
        }
    }

    /// Confirms selected review items as one local all-or-nothing rule
    /// transaction. Only a previously bound `ScanItemId` and a closed risk
    /// value are accepted; the model's text never enters the rule draft.
    pub fn confirm_suggestions(
        &self,
        batch_id: &AiBatchId,
        current_scan_generation: u64,
        selections: &[AiConfirmationSelection],
    ) -> Result<AiConfirmationResult, AiServiceError> {
        let confirmation = self
            .confirmation
            .as_ref()
            .ok_or_else(AiServiceError::configuration)?;
        let started = Instant::now();
        if selections.is_empty() {
            return Err(AiServiceError::configuration());
        }
        let bound = self
            .candidates
            .lock()
            .map_err(|_| AiServiceError::configuration())?
            .get(batch_id)
            .cloned()
            .ok_or_else(AiServiceError::configuration)?;
        if bound.scan_generation != current_scan_generation {
            return Err(service_error(AiServiceErrorKind::StaleGeneration));
        }
        let suggestions = self
            .suggestions
            .lock()
            .map_err(|_| AiServiceError::configuration())?
            .get(batch_id)
            .cloned()
            .ok_or_else(AiServiceError::invalid_response)?;

        let mut seen = HashSet::with_capacity(selections.len());
        let created_at_epoch_secs = epoch_seconds()?;
        let confirmations = selections
            .iter()
            .map(|selection| {
                if selection.final_risk() == RiskLevel::Unknown
                    || selection.final_category() == ResidueCategory::Unknown
                    || !seen.insert(selection.item_id())
                {
                    return Err(AiServiceError::configuration());
                }
                let candidate = bound
                    .candidates
                    .get(&selection.item_id())
                    .ok_or_else(AiServiceError::configuration)?;
                if candidate.item.risk == RiskLevel::Protected
                    || !suggestions
                        .iter()
                        .any(|suggestion| suggestion.token == candidate.token)
                {
                    return Err(AiServiceError::configuration());
                }
                Ok(devresidue_core::rules::AiRuleConfirmation {
                    item: candidate.item.clone(),
                    final_risk: selection.final_risk(),
                    final_category: selection.final_category(),
                    provenance: RuleProvenance {
                        origin: RuleOrigin::AiAdvisor,
                        profile_id: bound.profile_id.clone(),
                        scan_generation: bound.scan_generation,
                        suggestion_id: new_suggestion_id()?,
                        user_final_risk: selection.final_risk(),
                        created_at_epoch_secs,
                    },
                })
            })
            .collect::<Result<Vec<_>, AiServiceError>>()?;
        let written_rule_ids = match commit_ai_rule_batch(
            &confirmation.user_rules_dir,
            &confirmations,
            &FullRuleRematchVerifier::new(&confirmation.builtin_rules_dir),
            confirmation.transaction_port.as_ref(),
        ) {
            Ok(ids) => ids,
            Err(error) => {
                let _ = confirmation.audit_log.record(
                    AiAuditEvent::ConfirmRolledBack,
                    bound.profile_id.clone(),
                    bounded_count(selections.len()),
                    AiAuditResult::RolledBack,
                    elapsed_millis(started),
                );
                return Err(confirmation_error(&error));
            }
        };

        let audit_warning = confirmation
            .audit_log
            .record(
                AiAuditEvent::ConfirmWritten,
                bound.profile_id.clone(),
                bounded_count(selections.len()),
                AiAuditResult::Ok,
                elapsed_millis(started),
            )
            .err()
            .map(|_| AiAuditWarning::WriteFailed);

        self.suggestions
            .lock()
            .map_err(|_| AiServiceError::configuration())?
            .remove(batch_id);
        self.candidates
            .lock()
            .map_err(|_| AiServiceError::configuration())?
            .remove(batch_id);
        Ok(AiConfirmationResult {
            written_rule_ids,
            audit_warning,
        })
    }
}

fn is_bindable_candidate(item: &ScanItem) -> bool {
    matches!(item.risk, RiskLevel::Unknown | RiskLevel::Review)
}

fn epoch_seconds() -> Result<i64, AiServiceError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AiServiceError::configuration())
        .and_then(|duration| {
            i64::try_from(duration.as_secs()).map_err(|_| AiServiceError::configuration())
        })
}

fn new_suggestion_id() -> Result<AiSuggestionId, AiServiceError> {
    AiSuggestionId::parse(&Uuid::new_v4().to_string()).map_err(|_| AiServiceError::configuration())
}

fn bounded_count(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn confirmation_error(error: &str) -> AiServiceError {
    if error.to_ascii_lowercase().contains("conflict") {
        service_error(AiServiceErrorKind::ConfirmationConflict)
    } else {
        service_error(AiServiceErrorKind::ConfirmationFailed)
    }
}

fn validate_suggestions(
    batch: &PreparedBatch,
    suggestions: &[AiSuggestion],
) -> Result<(), AiServiceError> {
    if suggestions.len() != batch.entries.len() {
        return Err(AiServiceError::invalid_response());
    }
    let expected = batch
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<HashSet<_>>();
    let actual = suggestions
        .iter()
        .map(|suggestion| suggestion.token.clone())
        .collect::<HashSet<_>>();
    if expected != actual
        || suggestions.iter().any(|suggestion| {
            suggestion.profile_id != batch.profile_id
                || suggestion.scan_generation != batch.scan_generation
                || !suggestion.confidence.is_finite()
                || !(0.0_f32..=1.0_f32).contains(&suggestion.confidence)
        })
    {
        return Err(AiServiceError::invalid_response());
    }
    Ok(())
}

fn service_error(kind: AiServiceErrorKind) -> AiServiceError {
    match kind {
        AiServiceErrorKind::Cancelled => AiServiceError::cancelled(),
        AiServiceErrorKind::Configuration => AiServiceError::configuration(),
        AiServiceErrorKind::InvalidResponse => AiServiceError::invalid_response(),
        AiServiceErrorKind::RequestFailed => AiServiceError::request_failed(None, None),
        AiServiceErrorKind::Disabled
        | AiServiceErrorKind::MissingKey
        | AiServiceErrorKind::NoActiveProfile
        | AiServiceErrorKind::NoEligibleEntries
        | AiServiceErrorKind::ProfileUnavailable
        | AiServiceErrorKind::StaleGeneration
        | AiServiceErrorKind::ConfirmationConflict
        | AiServiceErrorKind::ConfirmationFailed => AiServiceError::from_kind(kind),
    }
}
