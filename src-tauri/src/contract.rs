//! The frontend↔backend contract: command argument DTOs, result DTOs, the
//! structured error type and the event payloads.
//!
//! Safety shape (SPEC §20 / INV-013): every *request* is either an opaque id
//! or a structured scope — there is no path-typed command parameter anywhere
//! in this shell. Paths only ever travel **out** (in scan items, plans and
//! session results produced by the trusted core).

use devresidue_core::cleanup::planner::SkipReason;
use devresidue_core::domain::plan::ConfirmRequirement;
use devresidue_core::{CleanupMode, CleanupResult, CleanupSession, CleanupStatus, ScanItem};
use serde::{Deserialize, Serialize};

// ---- Event channel names ---------------------------------------------------

/// Coarse scan progress (`scan_id`, `provider`, `stage`).
pub const EV_SCAN_PROGRESS: &str = "scan://progress";
/// One discovered item (`scan_id`, `item`).
pub const EV_SCAN_ITEM: &str = "scan://item";
/// Scan finished (`scan_id`, `cancelled`, `total`, `generation`).
pub const EV_SCAN_DONE: &str = "scan://done";
/// Scan failed (`scan_id`, `message`) — emitted on error paths instead of
/// `scan://done` (R10: a failed scan never looks finished).
pub const EV_SCAN_ERROR: &str = "scan://error";
/// Warning collected during a scan (`scan_id`, `message`).
pub const EV_SCAN_WARNING: &str = "scan://warning";
/// One cleanup result, in plan order (`plan_id`, `item`).
pub const EV_CLEANUP_ITEM: &str = "cleanup://item";

// ---- Request DTOs ----------------------------------------------------------

/// Which providers a scan should run — the same semantics as the CLI flags
/// (`--dev-cache`, `--projects <roots>`, `--agents`, `--unknown`, and the
/// default).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ScanScope {
    /// AI agent data providers only (SPEC §9).
    Agents,
    /// Developer-cache providers only (npm/pip/uv/cargo/nuget/bun).
    DevCache,
    /// kondo project discovery over the given workspace roots.
    Projects { roots: Vec<String> },
    /// Unknown developer-data provider only (SPEC §25).
    Unknown,
    /// Everything: dev caches + kondo (default roots) + agents + unknown.
    Default,
}

/// The confirmation level a plan build / execution is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfirmPolicyArg {
    /// Safe / RegenerableLocal only.
    Default,
    /// Additionally allow RegenerableDownload.
    Redownload,
    /// Additionally allow Review.
    Review,
    /// Confirm everything.
    All,
}

/// Result of `scan`: an opaque handle (the UI waits on the event stream).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanHandleDto {
    pub scan_id: u64,
}

// ---- Result DTOs -----------------------------------------------------------

/// One planned item (create_cleanup_plan result, human vocabulary mirrors the
/// CLI's plan summary).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedItemDto {
    pub scan_item_id: u64,
    /// recycle | delete | execute
    pub action: String,
    /// none | redownload | review
    pub confirmation: String,
    pub estimated_size: u64,
    pub path: String,
}

/// A selected item the planner intentionally left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedItemDto {
    pub scan_item_id: u64,
    pub path: String,
    /// Human reason (mirrors the CLI plan summary, e.g. "protected-risk").
    pub reason: String,
}

/// The created plan (create_cleanup_plan result).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPlanDto {
    pub plan_id: u64,
    pub dry_run: bool,
    pub total_estimated_bytes: u64,
    pub items: Vec<PlannedItemDto>,
    pub skipped: Vec<SkippedItemDto>,
}

/// Totals of a cleanup session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTotalsDto {
    pub planned_bytes: u64,
    pub completed_bytes: u64,
    pub succeeded: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// One per-item outcome (execute_cleanup_plan result / cleanup://item event).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionItemDto {
    pub scan_item_id: u64,
    pub path: String,
    pub product: Option<String>,
    /// recycle | delete | execute
    pub action: String,
    pub estimated_size: u64,
    /// ok | would | skipped | failed (machine-ish verb)
    pub status: String,
    /// Human detail (skip reason / failure message).
    pub detail: Option<String>,
}

/// One cleanup session (execute_cleanup_plan result).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupSessionDto {
    pub session_id: u64,
    pub plan_id: u64,
    pub dry_run: bool,
    pub totals: SessionTotalsDto,
    pub items: Vec<SessionItemDto>,
    pub journal_degraded: bool,
}

/// One journal entry (get_journal result; time in epoch seconds for easy
/// rendering).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryDto {
    pub session_id: u64,
    pub time_secs: u64,
    pub phase: String,
    pub product: Option<String>,
    pub rule: Option<u64>,
    pub provider: Option<u64>,
    pub path: Option<String>,
    pub action: Option<String>,
    pub estimated_size: u64,
    pub result: Option<String>,
    pub error: Option<String>,
}

// ---- Unknown dispositions DTOs (Phase 13) -----------------------------------

/// User disposition for one scan item (SPEC §25 actions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DispositionArg {
    /// Never report this path again.
    Ignore,
    /// Report it as Protected from now on.
    Protect,
}

/// Result of applying a disposition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispositionResultDto {
    pub item_id: u64,
    pub rule_id: String,
    pub path: String,
    /// Next scan will reflect this (ignore → absent, protect → Protected).
    pub effect: String,
}

// ---- Remote-AI DTOs (no secret / no path) ---------------------------------

/// Closed structured-output preference accepted when a remote-AI profile is
/// created or updated. The profile response deliberately omits this and every
/// secret-adjacent field; the frontend keeps the submitted preference only for
/// the one-shot edit request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuredOutputModeArg {
    Auto,
    JsonSchema,
    JsonObject,
}

/// The only final risks the remote-AI confirmation surface accepts. `Unknown`
/// is intentionally absent: a user must choose a concrete local disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskLevelArg {
    Safe,
    RegenerableLocal,
    RegenerableDownload,
    Review,
    Protected,
}

/// A stored remote-AI profile as it may cross IPC. Neither the API key nor the
/// generated environment-variable name is part of this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfileDto {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub enabled: bool,
    pub is_active: bool,
}

/// Non-secret remote-AI profile state for the settings UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProfileStateDto {
    pub master_enabled: bool,
    pub profiles: Vec<AiProfileDto>,
}

/// One sanitized entry shown for explicit metadata-send consent. `item_id` is
/// the sole local identity; the in-memory entry token never crosses IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPreparedEntryDto {
    pub item_id: u64,
    pub zone: String,
    pub relative_depth: u8,
    pub display_name: String,
    pub source_kind: String,
    pub category_hint: String,
    pub product_hint: Option<String>,
    pub size_bucket: String,
    pub age_bucket: String,
    pub signals: Vec<String>,
}

/// A process-local batch preview. The opaque batch id is usable only until a
/// scan/profile/session transition invalidates the in-memory state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPreparedBatchDto {
    pub batch_id: String,
    pub scan_generation: u64,
    pub profile_id: String,
    /// Whether this batch will include full snapshot-derived paths in the
    /// remote request. The paths themselves never cross IPC.
    pub includes_paths: bool,
    pub entries: Vec<AiPreparedEntryDto>,
}

/// Validated remote suggestion rendered without the model's opaque entry
/// token, raw request or raw response. The final-risk choices are closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSuggestionDto {
    pub item_id: u64,
    pub suggested_risk: String,
    pub confidence: f32,
    pub reason: String,
    pub product_guess: Option<String>,
    pub final_risk_options: Vec<RiskLevelArg>,
}

/// Summary of a successful all-or-nothing local confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfirmResultDto {
    pub confirmed_count: usize,
    pub audit_warning: bool,
}

// ---- Errors ----------------------------------------------------------------

/// Stable machine-readable error codes the UI can switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// Plan execution needs a higher confirmation level than granted.
    ConfirmationRequired,
    /// No finished scan available (or the requested scan id is unknown).
    ScanNotFound,
    /// The plan id does not exist.
    PlanNotFound,
    /// A scan is already running / partial results are not yet finalised.
    PartialScan,
    /// A referenced scan-item id does not belong to the latest scan.
    InvalidItem,
    /// Remote AI is disabled by its separate master switch.
    AiDisabled,
    /// The selected profile or its process-local Key is not ready.
    AiNotConfigured,
    /// A remote request for another batch is already in progress.
    AiBatchInProgress,
    /// The requested batch no longer belongs to the current real scan.
    AiBatchExpired,
    /// A sanitized remote-AI request, validation or local confirmation failed.
    AiRequestFailed,
    /// Any other engine/planner/store failure.
    Engine,
}

/// Structured command error: every command returns `Result<_, CommandError>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    /// Confirmation level the plan requires when `code == ConfirmationRequired`
    /// (none | redownload | review). Lets the UI render the right dialog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<String>,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            required: None,
        }
    }

    pub fn confirmation_required(required: ConfirmRequirement) -> Self {
        Self {
            code: ErrorCode::ConfirmationRequired,
            message: format!(
                "plan requires {required:?} confirmation; re-run with a matching \
                 confirmation level"
            ),
            required: Some(confirm_label(required).to_string()),
        }
    }
}

// ---- Event payloads (serialised under the EV_* names) ----------------------

/// scan://progress payload.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressPayload {
    pub scan_id: u64,
    pub provider: String,
    /// started | done | project
    pub stage: String,
}

/// scan://item payload.
#[derive(Debug, Clone, Serialize)]
pub struct ScanItemPayload {
    pub scan_id: u64,
    pub item: Box<ScanItem>,
}

/// scan://warning payload.
#[derive(Debug, Clone, Serialize)]
pub struct WarningPayload {
    pub scan_id: u64,
    pub message: String,
}

/// scan://done payload.
///
/// `generation` lets the frontend correlate the done event with the exact
/// persisted snapshot: it is assigned before saving and stamped into the
/// snapshot, so a late done event can never be mistaken for an older scan's
/// finish (R10).
#[derive(Debug, Clone, Serialize)]
pub struct ScanDonePayload {
    pub scan_id: u64,
    pub cancelled: bool,
    pub total: usize,
    pub generation: u64,
}

/// scan://error payload (R10): emitted when the worker failed before a
/// snapshot could be finalised. The scan is never reported as `done`.
#[derive(Debug, Clone, Serialize)]
pub struct ScanErrorPayload {
    pub scan_id: u64,
    pub message: String,
}

// ---- Rule-inspection DTOs (R11) --------------------------------------------

/// One rule of the merged registry (built-in + user), as shown by `get_rules`.
///
/// Rules that failed validation are still returned with `valid: false` and the
/// failure messages in `issues`; their metadata (source/risk/category/
/// description) is `None` because the declaration did not load.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDto {
    /// Rule slug (e.g. `builtin-protected/ssh`, `user-protected/…`).
    pub rule_id: String,
    /// Source label (`builtin-protected` / `user-protected` / `user` /
    /// `community` / `builtin-detection`); `None` for invalid rules.
    pub source: Option<String>,
    /// Kebab-case risk; `None` for invalid rules.
    pub risk: Option<String>,
    /// Kebab-case category; `None` for invalid rules.
    pub category: Option<String>,
    pub description: Option<String>,
    pub valid: bool,
    /// Validation findings that reference this rule (messages).
    pub issues: Vec<String>,
}

/// One validation finding inside [`RulesValidationDto`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleIssueDto {
    /// Rule slug when the finding is rule-specific (`None` = file-level).
    pub rule_id: Option<String>,
    pub message: String,
    /// `error` | `warning`
    pub severity: String,
}

/// Whole-registry validation result (`validate_rules`, CLI `rules validate`
/// semantics).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesValidationDto {
    /// Rules that loaded and validated.
    pub total: usize,
    pub errors: usize,
    pub warnings: usize,
    /// Every finding (blocking errors first, then warnings).
    pub issues: Vec<RuleIssueDto>,
}

/// cleanup://item payload.
#[derive(Debug, Clone, Serialize)]
pub struct CleanupItemPayload {
    pub plan_id: u64,
    pub item: SessionItemDto,
}

// ---- Shared vocabulary helpers ---------------------------------------------

/// `recycle | delete | execute` for a mode. The wire value stays `recycle`
/// (the plan/authorisation vocabulary), but the frontend renders it as a
/// permanent deletion: the engine executes RecycleBin as a verified
/// handle-bound permanent delete on Windows.
pub fn mode_label(mode: CleanupMode) -> &'static str {
    match mode {
        CleanupMode::RecycleBin => "recycle",
        CleanupMode::DirectDelete => "delete",
        CleanupMode::ExternalCommand => "execute",
    }
}

/// `none | redownload | review` for a confirmation requirement.
pub fn confirm_label(req: ConfirmRequirement) -> &'static str {
    match req {
        ConfirmRequirement::None => "none",
        ConfirmRequirement::Redownload => "redownload",
        ConfirmRequirement::Review => "review",
    }
}

/// Human skip reason, mirroring the CLI's plan summary vocabulary.
pub fn skip_reason_label(reason: &SkipReason) -> String {
    match reason {
        SkipReason::ProtectedRisk => "skip: protected-risk".into(),
        SkipReason::UnknownRisk => "skip: unknown-risk".into(),
        SkipReason::ActionNone => "skip: action none".into(),
        SkipReason::ActionDeferred { reason } => format!("skip: deferred ({reason})"),
        SkipReason::ConfirmationRequired { required } => {
            format!("skip: requires {} confirmation", confirm_label(*required))
        }
        SkipReason::Unverifiable { detail } => format!("skip: unverifiable ({detail})"),
        SkipReason::UnknownSelection => "skip: unknown selection".into(),
        SkipReason::ProtectedDescendant { area } => format!(
            "skip: protected descendant at {} inside the target tree",
            area.display()
        ),
        SkipReason::DiscoverySourceInvalidated { rule_id } => format!(
            "skip: discovery source invalidated (rule {:?} no longer matches)",
            rule_id
        ),
        SkipReason::MissingScanSnapshot => {
            "skip: item lacks a scan-time snapshot (re-scan required)".to_string()
        }
        SkipReason::CurrentProtectedRoot { root } => format!(
            "skip: target hits the current protected-root set ({}) — protected at plan time",
            root.display()
        ),
    }
}

/// Maps one session result onto its DTO (also used by the cleanup://item
/// event stream).
pub fn session_item_dto(result: &CleanupResult) -> SessionItemDto {
    let (status, detail) = match &result.status {
        CleanupStatus::Success => ("ok", None),
        CleanupStatus::WouldExecute { mode } => {
            ("would", Some(format!("would {}", mode_label(*mode))))
        }
        CleanupStatus::Skipped { reason } => ("skipped", Some(reason.clone())),
        CleanupStatus::Failed { error } => ("failed", Some(error.clone())),
    };
    SessionItemDto {
        scan_item_id: result.scan_item_id.raw(),
        path: result.path.display().to_string(),
        product: result.product.clone(),
        action: mode_label(result.action).to_string(),
        estimated_size: result.estimated_size,
        status: status.to_string(),
        detail,
    }
}

/// Maps a whole session onto its DTO (kept separate so item events can reuse
/// [`session_item_dto`]).
pub fn session_dto(session: &CleanupSession) -> CleanupSessionDto {
    CleanupSessionDto {
        session_id: session.session_id.raw(),
        plan_id: session.plan_id.raw(),
        dry_run: session.dry_run,
        totals: SessionTotalsDto {
            planned_bytes: session.totals.planned_bytes,
            completed_bytes: session.totals.completed_bytes,
            succeeded: session.totals.succeeded,
            skipped: session.totals.skipped,
            failed: session.totals.failed,
        },
        items: session.items.iter().map(session_item_dto).collect(),
        journal_degraded: session.journal_degraded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::ScanItemId;

    fn sample_result(id: u64) -> CleanupResult {
        CleanupResult {
            scan_item_id: ScanItemId::from_raw(id),
            path: format!(r"C:\residue\{id}").into(),
            product: Some("npm".into()),
            category: devresidue_core::ResidueCategory::PackageCache,
            source: devresidue_core::SourceKind::DeveloperCacheProvider,
            rule_id: None,
            provider_id: None,
            action: CleanupMode::RecycleBin,
            estimated_size: 2048,
            status: CleanupStatus::Success,
        }
    }

    #[test]
    fn scope_serde_round_trips_each_variant() {
        for scope in [
            ScanScope::Agents,
            ScanScope::DevCache,
            ScanScope::Projects {
                roots: vec!["D:\\Code".into()],
            },
            ScanScope::Unknown,
            ScanScope::Default,
        ] {
            let json = serde_json::to_string(&scope).unwrap();
            let back: ScanScope = serde_json::from_str(&json).unwrap();
            assert!(matches!(
                (back, scope),
                (ScanScope::Agents, ScanScope::Agents)
                    | (ScanScope::DevCache, ScanScope::DevCache)
                    | (ScanScope::Projects { .. }, ScanScope::Projects { .. })
                    | (ScanScope::Unknown, ScanScope::Unknown)
                    | (ScanScope::Default, ScanScope::Default)
            ));
        }
        let json = serde_json::to_string(&ScanScope::Projects {
            roots: vec!["D:\\Code".into()],
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"projects","roots":["D:\\Code"]}"#);
    }

    #[test]
    fn confirmation_policy_arg_serde_kebab() {
        assert_eq!(
            serde_json::to_string(&ConfirmPolicyArg::Redownload).unwrap(),
            r#""redownload""#
        );
    }

    #[test]
    fn session_item_dto_keeps_status_and_detail() {
        let ok = session_item_dto(&sample_result(1));
        assert_eq!(ok.status, "ok");
        assert_eq!(ok.action, "recycle");
        assert!(ok.detail.is_none());

        let mut skipped = sample_result(2);
        skipped.status = CleanupStatus::Skipped {
            reason: "deny:protected-risk".into(),
        };
        let skipped = session_item_dto(&skipped);
        assert_eq!(skipped.status, "skipped");
        assert_eq!(skipped.detail.as_deref(), Some("deny:protected-risk"));
    }

    #[test]
    fn skip_reason_labels_match_cli_vocabulary() {
        assert_eq!(
            skip_reason_label(&SkipReason::ProtectedRisk),
            "skip: protected-risk"
        );
        assert_eq!(
            skip_reason_label(&SkipReason::ActionNone),
            "skip: action none"
        );
        assert_eq!(
            skip_reason_label(&SkipReason::ConfirmationRequired {
                required: ConfirmRequirement::Review
            }),
            "skip: requires review confirmation"
        );
    }

    #[test]
    fn command_error_serde_shapes() {
        let err = CommandError::confirmation_required(ConfirmRequirement::Review);
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], "confirmation-required");
        assert_eq!(json["required"], "review");

        let plain = CommandError::new(ErrorCode::ScanNotFound, "nothing scanned yet");
        let json = serde_json::to_value(&plain).unwrap();
        assert_eq!(json["code"], "scan-not-found");
        assert!(json.get("required").is_none(), "required must be omitted");
    }
}
