//! The `CleanupEngine` — the **single deletion authority** (INV-010).
//!
//! Executes a persisted [`CleanupPlan`] item by item:
//!
//! 1. per item, revalidates the live target against the plan snapshot through
//!    the [`SafetyValidator`] (INV-005, SPEC §16);
//! 2. `Allow` → executes the plan action **through the [`DeletePort`]** — the
//!    only call site of the port in the code base;
//! 3. `Defer` / `Deny` → skips the item with its named reason;
//! 4. port failures (locked, permission, failed external command, timeout)
//!    mark the item failed and execution continues — partial failure is
//!    resumable (PLAN Phase 6 acceptance);
//! 5. an attempt + result event is journaled for every item; a journal write
//!    failure degrades the session's audit flag but never rolls back cleanup.
//!
//! `dry_run` runs the identical validation pipeline (candidate set stays
//! consistent, SPEC §32) but never touches the port, producing
//! `WouldExecute` / `Would Skip` results (SPEC §23).

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use crate::domain::action::CleanupAction;
use crate::domain::ids::{CleanupPlanId, ScanItemId};
use crate::domain::plan::{CleanupPlan, CleanupPlanItem, ConfirmRequirement};
use crate::domain::risk_level::RiskLevel;
use crate::domain::scan_item::ScanItem;
use crate::domain::session::{CleanupResult, CleanupSession, CleanupStatus, SessionTotals};
use crate::domain::source::SourceKind;
use crate::journal::{Journal, JournalPhase, JournalRecord};
use crate::rules::{self, RuleSet};
use crate::safety::{DeferReason, DenyReason, SafetyValidator, SafetyVerdict, ValidationRequest};
use crate::CleanupMode;

use super::planner::ConfirmPolicy;
use super::port::DeletePort;

/// Authoritative item source: the engine re-derives every plan item's action /
/// risk / confirmation from the **current** scan's original [`ScanItem`]
/// (R02/R04). Callers inject a lookup over the persisted latest scan; an id
/// that is not present (older generation, R04) makes the plan item stale.
pub type ItemLookup = dyn Fn(ScanItemId) -> Option<ScanItem> + Send + Sync;

/// Options controlling one plan execution.
pub struct EngineOptions {
    /// Simulate only (SPEC §23) — validation identical, port never called.
    pub dry_run: bool,
    /// Confirmation level the user granted at the command line.
    pub confirm_policy: ConfirmPolicy,
    /// Optional audit journal (JSONL). A journaling failure degrades the
    /// session flag instead of interrupting cleanup.
    pub journal: Option<Journal>,
    /// R06 tool-scope re-query port. Present for tool-native cleanups whose
    /// frozen [`crate::domain::action::ScopeBinding`] must be re-verified
    /// before execution; `None` means no tool-scope re-verification happens
    /// (legacy callers / non-tool commands).
    pub tool_query: Option<Arc<dyn super::port::ToolQueryPort>>,
    /// R2-F03: the scan generation the caller is executing against (the
    /// current `ScanSnapshot.generation`). `Some(g)` makes the engine refuse a
    /// plan whose `scan_generation` differs (whole-plan reject before any
    /// item); `None` skips the gate (tests/legacy).
    pub expected_scan_generation: Option<u64>,
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOptions")
            .field("dry_run", &self.dry_run)
            .field("confirm_policy", &self.confirm_policy)
            .field("journal", &self.journal.is_some())
            .field("tool_query", &self.tool_query.is_some())
            .field("expected_scan_generation", &self.expected_scan_generation)
            .finish()
    }
}

impl Clone for EngineOptions {
    fn clone(&self) -> Self {
        Self {
            dry_run: self.dry_run,
            confirm_policy: self.confirm_policy,
            journal: self.journal.clone(),
            tool_query: self.tool_query.clone(),
            expected_scan_generation: self.expected_scan_generation,
        }
    }
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            confirm_policy: ConfirmPolicy::None,
            journal: None,
            tool_query: None,
            expected_scan_generation: None,
        }
    }
}

/// The engine refuses to start when the caller's confirmation is below the
/// plan's strictest item requirement (safe default, SPEC §19).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error(
        "plan requires {required:?} confirmation but the run was granted \
         {policy:?}; re-run with a matching confirmation flag \
         (--confirm-redownload / --confirm-review / --yes)"
    )]
    ConfirmationRequired {
        required: ConfirmRequirement,
        policy: ConfirmPolicy,
    },
    /// The rule gate is empty (F12): a cleanup run without rules cannot
    /// re-derive item risks, so it is refused outright.
    #[error(
        "no rules loaded — refusing to run cleanup without the rule gate \
         (risk re-derivation would be impossible, F-2-1/F12)"
    )]
    NoRules,
    /// R2-F03: the plan's scan generation does not match the current scan
    /// record — the plan was built over an older (or different) scan and its
    /// item ids may alias different objects. The whole run is refused.
    #[error(
        "plan-generation-mismatch: plan was built against scan generation \
         {plan_generation} but the current scan is generation {current_generation}; \
         re-plan from the current scan"
    )]
    GenerationMismatch {
        plan_generation: u64,
        current_generation: u64,
    },
}

/// Outcome of a whole plan run.
#[derive(Debug, Clone)]
pub struct EngineRun {
    pub session: CleanupSession,
}

/// The engine itself.
pub struct CleanupEngine {
    port: Arc<dyn DeletePort>,
    validator: SafetyValidator,
    /// The resolve rule set (built-in + user dispositions) used for the F12
    /// plan-risk re-derivation gate. Injected by the caller, assembled from
    /// the same source as the scan-time rule gate.
    rules: Arc<RuleSet>,
    /// Authoritative item source: the current scan's original items (R02/R04).
    item_lookup: Arc<ItemLookup>,
}

impl CleanupEngine {
    /// Creates the engine over the deletion port, the safety validator, the
    /// rule set and the authoritative item lookup (R02/R04). Every planned
    /// item's mode / command / confirmation is re-derived from the original
    /// scan item and compared with the (user-editable) plan record before
    /// execution.
    pub fn new(
        port: Arc<dyn DeletePort>,
        validator: SafetyValidator,
        rules: Arc<RuleSet>,
        item_lookup: Arc<ItemLookup>,
    ) -> Self {
        Self {
            port,
            validator,
            rules,
            item_lookup,
        }
    }

    /// Executes (or dry-runs) `plan`.
    ///
    /// # Errors
    /// - [`EngineError::ConfirmationRequired`] when a non-dry run is attempted
    ///   below the plan's confirmation requirement.
    /// - [`EngineError::NoRules`] when the injected rule set is empty (F12).
    pub fn run(&self, plan: &CleanupPlan, opts: EngineOptions) -> Result<EngineRun, EngineError> {
        // F12: fail-closed — no rules, no run (risk re-derivation is a hard
        // precondition of executing a persisted, user-editable plan file).
        if self.rules.rules.is_empty() {
            return Err(EngineError::NoRules);
        }

        // R2-F03: whole-plan generation gate — a plan from an older scan can
        // never authorise objects of the current one (closes cross-process id
        // reuse). Legacy plans (generation 0) are refused too when the caller
        // expects a real generation.
        if let Some(current) = opts.expected_scan_generation {
            if plan.scan_generation != current {
                return Err(EngineError::GenerationMismatch {
                    plan_generation: plan.scan_generation,
                    current_generation: current,
                });
            }
        }

        let required = plan.max_confirmation();
        if !opts.dry_run && !opts.confirm_policy.permits(required) {
            return Err(EngineError::ConfirmationRequired {
                required,
                policy: opts.confirm_policy,
            });
        }

        let mut session = CleanupSession {
            session_id: plan.id,
            plan_id: plan.id,
            started_at: SystemTime::now(),
            ended_at: None,
            dry_run: opts.dry_run,
            items: Vec::with_capacity(plan.items.len()),
            totals: SessionTotals {
                planned_bytes: plan.total_estimated_bytes(),
                ..SessionTotals::default()
            },
            journal_degraded: false,
        };

        let mut journal = opts.journal;
        let tool_query = opts.tool_query;
        let mut journal_ok = true;
        for item in &plan.items {
            let (outcome, ok) = self.run_item(
                plan.id,
                item,
                opts.dry_run,
                tool_query.as_deref(),
                &mut journal,
            );
            journal_ok &= ok;
            let status = match outcome {
                ItemOutcome::Success => {
                    session.totals.succeeded += 1;
                    session.totals.completed_bytes += item.estimated_size;
                    CleanupStatus::Success
                }
                ItemOutcome::WouldExecute => {
                    session.totals.succeeded += 1;
                    CleanupStatus::WouldExecute { mode: item.mode }
                }
                ItemOutcome::Skipped(reason) => {
                    session.totals.skipped += 1;
                    CleanupStatus::Skipped { reason }
                }
                ItemOutcome::Failed(error) => {
                    session.totals.failed += 1;
                    CleanupStatus::Failed { error }
                }
            };
            session.items.push(self.result_for(item, status));
        }
        session.journal_degraded = journal.is_some() && !journal_ok;

        session.ended_at = Some(SystemTime::now());
        Ok(EngineRun { session })
    }

    /// Handles one item: journal attempt, F12 gate, validate, (maybe) execute,
    /// journal result. Returns the outcome and whether journaling stayed
    /// healthy.
    fn run_item(
        &self,
        session_id: CleanupPlanId,
        item: &CleanupPlanItem,
        dry_run: bool,
        tool_query: Option<&dyn super::port::ToolQueryPort>,
        journal: &mut Option<Journal>,
    ) -> (ItemOutcome, bool) {
        let mut journal_ok = true;
        journal_ok &= self.journal(
            journal.as_mut(),
            JournalRecord::for_item(
                session_id.raw(),
                JournalPhase::Attempt,
                item.product.clone(),
                item.snapshot.rule_id.map(|r| r.raw()),
                item.snapshot.provider_id.map(|p| p.raw()),
                item.snapshot.path.clone(),
                item.mode,
                item.estimated_size,
                None,
                None,
            ),
        );

        // Authorization gate (R2-F01/F03/F04/F06/F09 + F12): re-derive the
        // mode, command, risk, confirmation, discovery/classification source
        // and *object* from the **current scan's original item**, compare every
        // one against the (user-editable) plan record, and refuse the item on
        // any mismatch before the validator or the deletion port is reached.
        // The original (with its scan-time snapshot) is the execution object;
        // `item.snapshot` is only comparison material.
        let outcome = match self.authorize_item(item) {
            Err(reason) => ItemOutcome::Skipped(reason),
            Ok(authorized) => {
                // Validate the AUTHORISED object (original path + its scan-time
                // snapshot when present; the plan snapshot only as a legacy
                // fallback after the path binding check above already passed).
                let validation_snapshot = authorized
                    .original
                    .scan_snapshot
                    .as_ref()
                    .unwrap_or(&item.snapshot);
                let request = ValidationRequest {
                    path: &authorized.original.path,
                    product: authorized.original.product.as_deref(),
                    snapshot: validation_snapshot,
                };
                match self.validator.validate(&request) {
                    // R05: the Allow verdict carries the freshly verified live
                    // identity; bind it to the deletion by using the port's
                    // *verified* variant so a replacement between validation
                    // and the port operation is refused.
                    SafetyVerdict::Allow { snapshot } => {
                        // R2-F09: the non-destructive tool-scope re-verification
                        // is part of the shared PREFLIGHT — dry-run and real
                        // execution both run it (or both fail with
                        // tool-scope-drift). Only the destructive port call is
                        // gated on `dry_run`.
                        if let Err(scope_err) =
                            self.preflight_tool_scope(&authorized.action, tool_query)
                        {
                            ItemOutcome::Failed(scope_err)
                        } else if dry_run {
                            ItemOutcome::WouldExecute
                        } else {
                            match self.execute_with(
                                &authorized.original.path,
                                &authorized.action,
                                snapshot.file_identity.as_ref(),
                            ) {
                                Ok(()) => ItemOutcome::Success,
                                Err(err) => {
                                    let text = err.to_string();
                                    if text.starts_with("identity mismatch at port") {
                                        // R05: the bound object changed between
                                        // validation and deletion — recorded as
                                        // a failed (not skipped) item so the
                                        // session/totals reflect the aborted
                                        // deletion attempt.
                                        ItemOutcome::Failed(text)
                                    } else {
                                        ItemOutcome::Failed(err.to_string())
                                    }
                                }
                            }
                        }
                    }
                    SafetyVerdict::Defer { reason, .. } => {
                        ItemOutcome::Skipped(format!("defer:{}", defer_label(&reason)))
                    }
                    SafetyVerdict::Deny {
                        reason,
                        explanation,
                        ..
                    } => {
                        ItemOutcome::Skipped(format!("deny:{}: {explanation}", deny_label(&reason)))
                    }
                }
            }
        };

        let (result, error) = match &outcome {
            ItemOutcome::Success => (Some("success"), None),
            ItemOutcome::WouldExecute => (Some("dry-run"), None),
            ItemOutcome::Skipped(reason) => (Some("skipped"), Some(reason.clone())),
            ItemOutcome::Failed(error) => (Some("failed"), Some(error.clone())),
        };
        journal_ok &= self.journal(
            journal.as_mut(),
            JournalRecord::for_item(
                session_id.raw(),
                JournalPhase::Result,
                item.product.clone(),
                item.snapshot.rule_id.map(|r| r.raw()),
                item.snapshot.provider_id.map(|p| p.raw()),
                item.snapshot.path.clone(),
                item.mode,
                item.estimated_size,
                result.map(str::to_string),
                error,
            ),
        );

        (outcome, journal_ok)
    }

    /// The authorisation gate (R2-F01/F04/F06 + F12). Returns the
    /// authoritative original [`ScanItem`] (the execution object) plus its
    /// frozen action, or a skip reason.
    ///
    /// The plan record (`item.snapshot`) is **only** comparison material:
    ///
    /// - its path must canonical-equal the original's path (a retargeted plan
    ///   snapshot is refused — R2-F01 evidence 1);
    /// - its recorded identity must equal the original's scan-time identity
    ///   (a rewritten snapshot cannot bypass R01 — evidence 2);
    /// - its declared risk must equal the original's scan risk (evidence 3);
    /// - every rule-backed classification (discovery Rule source or
    ///   `classification_rule_id`, R2-F06) must still resolve to the same rule.
    fn authorize_item(&self, item: &CleanupPlanItem) -> Result<Authorized, String> {
        // R04: the plan item must reference an object of the *current* scan.
        let original = (self.item_lookup)(item.scan_item_id).ok_or_else(|| {
            format!(
                "item-not-in-current-scan: item {} is not part of the latest scan \
                 (stale or re-numbered plan — re-scan and rebuild the plan)",
                item.scan_item_id.raw()
            )
        })?;
        let path = &original.path;

        // R2-F01: the plan's recorded snapshot must be the *same object* —
        // canonical path equality first.
        if !crate::safety::canonical::normalized_eq_path(&original.path, &item.snapshot.path) {
            return Err(format!(
                "plan-snapshot-mismatch: plan points at '{}' but the scanned item \
                 authorises '{}' — the plan snapshot was retargeted",
                item.snapshot.path.display(),
                original.path.display()
            ));
        }
        // Identity equality: the plan snapshot must carry the scan-time
        // identity of the original (when the original has one recorded).
        if let Some(scan_identity) = original
            .scan_snapshot
            .as_ref()
            .and_then(|s| s.file_identity.as_ref())
        {
            let plan_identity = item.snapshot.file_identity.as_ref();
            let mismatch = plan_identity.map_or(true, |id| {
                id.volume_serial != scan_identity.volume_serial
                    || id.file_index != scan_identity.file_index
            });
            if mismatch {
                return Err(format!(
                    "plan-snapshot-mismatch: plan snapshot's identity differs from the \
                     scan-time identity of '{}' (a rewritten snapshot cannot re-authorise \
                     a replaced object)",
                    path.display()
                ));
            }
        }

        // F04: a protected area *intersecting* the target tree (contains OR
        // equal — the conservative upper bound; a glob's literal root equal to
        // the target still covers it).
        let protected = self.rules.protected_areas();
        for area in &protected {
            let intersects = crate::safety::canonical::is_within(area, path)
                || crate::safety::canonical::is_within(path, area);
            if intersects {
                return Err(format!(
                    "protected-descendant: target '{}' intersects protected area '{}' — \
                     deleting the whole tree would delete the protected path",
                    path.display(),
                    area.display()
                ));
            }
        }

        // R2-F06 / R08: any rule that contributed to this item's final
        // classification must still resolve to the same rule. This covers both
        // the discovery source (SourceKind::Rule) and the structured
        // classification rule slug recorded by apply_rule_classification.
        let rule_basis: Option<String> = if original.source == SourceKind::Rule {
            Some(original.classification_rule_id.clone().unwrap_or_else(|| {
                original
                    .evidence
                    .iter()
                    .find_map(|e| e.rule_id)
                    .map(|r| format!("rule-id-{}", r.raw()))
                    .unwrap_or_default()
            }))
        } else {
            original.classification_rule_id.clone()
        };
        if let Some(basis) = rule_basis {
            if basis.is_empty() {
                return Err(format!(
                    "discovery-source-invalidated: item '{}' carries a Rule source but no \
                     rule basis — re-scan",
                    path.display()
                ));
            }
            let resolved = rules::resolve(path, &self.rules.rules);
            let invalidated = match resolved {
                // Numeric-id basis recorded as evidence.
                Some(hit) if basis.starts_with("rule-id-") => {
                    let id_text = basis.trim_start_matches("rule-id-");
                    id_text != hit.rule_id.raw().to_string()
                }
                Some(hit) => hit.id != basis,
                None => true,
            };
            if invalidated {
                return Err(format!(
                    "discovery-source-invalidated: item '{}' was classified by rule \
                     '{}' but the current rule gate no longer matches it — re-scan",
                    path.display(),
                    basis
                ));
            }
        }

        // F12 / R2-F01-risk: the plan's declared risk must equal the ORIGINAL
        // scan risk (not the rule-re-derived risk, which only gates above).
        let original_risk = original.risk;
        let plan_risk = item.snapshot.risk;
        if plan_risk != original_risk {
            return Err(format!(
                "risk-declaration-mismatch: plan declares {} but the scanned item \
                 carries {} for '{}' (tampered or stale plan)",
                risk_label(plan_risk),
                risk_label(original_risk),
                path.display()
            ));
        }

        // R2-F01-confirmation: the plan's granted level must cover what the
        // ORIGINAL risk requires (no downgrade to bypass confirmation).
        let derived = confirmation_for_risk(original_risk);
        if item.confirmation < derived {
            return Err(format!(
                "confirmation-downgraded: plan grants {:?} but the scanned item's risk \
                 requires {:?} for '{}' — re-run with the proper confirmation",
                item.confirmation,
                derived,
                path.display()
            ));
        }

        // R02-mode/command: the deletion mode and any external command come
        // exclusively from the scan item (the trusted source), never from the
        // editable plan JSON.
        match &original.cleanup_action {
            CleanupAction::RecycleBin => {
                if item.mode != CleanupMode::RecycleBin {
                    return Err(format!(
                        "mode-mismatch: plan declares {:?} but the scanned item \
                         authorises recycle for '{}'",
                        item.mode,
                        path.display()
                    ));
                }
            }
            CleanupAction::DirectDelete => {
                if item.mode != CleanupMode::DirectDelete {
                    return Err(format!(
                        "mode-mismatch: plan declares {:?} but the scanned item \
                         authorises direct-delete for '{}'",
                        item.mode,
                        path.display()
                    ));
                }
            }
            CleanupAction::ExternalCommand { command } => {
                if item.mode != CleanupMode::ExternalCommand {
                    return Err(format!(
                        "mode-mismatch: plan declares {:?} but the scanned item \
                         authorises an external command for '{}'",
                        item.mode,
                        path.display()
                    ));
                }
                match &item.external_command {
                    Some(planned) if planned == command => {}
                    _ => {
                        return Err(format!(
                            "command-mismatch: plan carries an external command that \
                             differs from the scanned item's frozen command for '{}' \
                             (re-run the scan and rebuild the plan)",
                            path.display()
                        ));
                    }
                }
            }
            CleanupAction::None | CleanupAction::Defer { .. } => {
                return Err(format!(
                    "action-mismatch: the scanned item for '{}' authorises no deletion \
                     ({}), but the plan targets it",
                    path.display(),
                    action_none_label(&original.cleanup_action)
                ));
            }
        }

        Ok(Authorized {
            original: original.clone(),
            action: original.cleanup_action.clone(),
        })
    }

    fn journal(&self, journal: Option<&mut Journal>, record: JournalRecord) -> bool {
        match journal {
            Some(journal) => journal.append(&record).is_ok(),
            None => true,
        }
    }

    /// Executes the **authorised** action (the frozen scan-item command, not
    /// the editable plan record — R02) on the **authorised object's path**
    /// (R2-F01: never the plan snapshot's path). Directory/file deletions go
    /// through the port's *verified* variants bound to the validator's live
    /// identity (R05) when one is available; external commands are tool-scope
    /// guarded by [`Self::verify_tool_scope`].
    fn execute_with(
        &self,
        path: &Path,
        action: &CleanupAction,
        live_identity: Option<&crate::safety::probe::FileIdentity>,
    ) -> Result<(), super::port::DeleteError> {
        match action {
            // RecycleBin is EXECUTED as a verified permanent deletion:
            // Windows' recycle API is path-only (no handle binding), and the
            // post-hoc staging protocol proved fragile in the field (staging
            // rename denied on in-use dirs; E_INVALIDARG on mixed-separator
            // paths). The handle-bound direct delete is the only Windows
            // operation that keeps the INV-005 object-binding guarantee, so
            // the user-facing contract is: deletion is permanent (documented
            // in the plan review UI), not recoverable. The port keeps its
            // recycle capability for any future use.
            CleanupAction::RecycleBin => match live_identity {
                Some(id) => self.port.delete_tree_verified(path, id),
                None => self.port.delete_tree(path),
            },
            CleanupAction::DirectDelete => match live_identity {
                Some(id) => self.port.delete_tree_verified(path, id),
                None => self.port.delete_tree(path),
            },
            CleanupAction::ExternalCommand { command } => self.port.execute_command(command),
            CleanupAction::None | CleanupAction::Defer { .. } => {
                Err(super::port::DeleteError::CommandFailed {
                    exit_code: None,
                    timed_out: false,
                    message: "authorised action is not executable".to_string(),
                })
            }
        }
    }

    /// R2-F09 preflight: for an ExternalCommand action with a frozen scope,
    /// re-runs the tool query and refuses on drift. Called for BOTH dry-run
    /// and real execution so the two paths agree (SPEC §32 dry-run
    /// consistency).
    fn preflight_tool_scope(
        &self,
        action: &CleanupAction,
        tool_query: Option<&dyn super::port::ToolQueryPort>,
    ) -> Result<(), String> {
        let CleanupAction::ExternalCommand { command } = action else {
            return Ok(());
        };
        self.verify_tool_scope(command, tool_query)
            .map_err(|e| e.to_string())
    }

    /// R06: re-runs the frozen scope query before a tool-native cleanup and
    /// refuses on drift — the tool may only ever clean the exact cache root
    /// that was verified at scan time.
    fn verify_tool_scope(
        &self,
        command: &crate::ExternalCommandSpec,
        tool_query: Option<&dyn super::port::ToolQueryPort>,
    ) -> Result<(), super::port::DeleteError> {
        let Some(scope) = &command.scope else {
            return Ok(()); // no frozen scope (legacy / non-tool command)
        };
        let port = tool_query.ok_or_else(|| super::port::DeleteError::CommandFailed {
            exit_code: None,
            timed_out: false,
            message: format!(
                "tool-scope-drift: command for '{}' carries a frozen scope but no \
                     tool-query port was provided — refusing",
                scope.tool
            ),
        })?;
        let answer = port.query_cache_root(&scope.verify_query).ok_or_else(|| {
            super::port::DeleteError::CommandFailed {
                exit_code: None,
                timed_out: false,
                message: format!(
                    "tool-scope-drift: re-query of '{}' cache root failed (tool \
                         missing/changed) — refusing cleanup of '{}'",
                    scope.tool,
                    scope.expected_cache_root.display()
                ),
            }
        })?;
        let live_root = crate::safety::canonical::normalize(std::path::Path::new(answer.trim()));
        let expected_root = crate::safety::canonical::normalize(&scope.expected_cache_root);
        if !crate::safety::canonical::normalized_eq_path(&live_root, &expected_root) {
            return Err(super::port::DeleteError::CommandFailed {
                exit_code: None,
                timed_out: false,
                message: format!(
                    "tool-scope-drift: '{}' now reports cache root '{}' but the frozen \
                     scope verified '{}' — configuration changed; re-scan before cleanup",
                    scope.tool,
                    live_root.display(),
                    expected_root.display()
                ),
            });
        }
        Ok(())
    }

    fn result_for(&self, item: &CleanupPlanItem, status: CleanupStatus) -> CleanupResult {
        CleanupResult {
            scan_item_id: item.scan_item_id,
            path: item.snapshot.path.clone(),
            product: item.product.clone(),
            category: item.category,
            source: item.source,
            rule_id: item.snapshot.rule_id,
            provider_id: item.snapshot.provider_id,
            action: item.mode,
            estimated_size: item.estimated_size,
            status,
        }
    }
}

/// Outcome of a single item before it is turned into a [`CleanupStatus`].
enum ItemOutcome {
    Success,
    WouldExecute,
    Skipped(String),
    Failed(String),
}

/// The authoritative deletion intent, re-derived from the current scan's
/// original item (R02) — never from the editable plan JSON. Carries the
/// original [`ScanItem`] because it (path + scan snapshot) is the execution
/// object (R2-F01).
struct Authorized {
    original: ScanItem,
    action: CleanupAction,
}

/// Standard risk → confirmation mapping (SPEC §19).
fn confirmation_for_risk(risk: RiskLevel) -> ConfirmRequirement {
    match risk {
        RiskLevel::Safe | RiskLevel::RegenerableLocal => ConfirmRequirement::None,
        RiskLevel::RegenerableDownload => ConfirmRequirement::Redownload,
        RiskLevel::Review => ConfirmRequirement::Review,
        RiskLevel::Protected | RiskLevel::Unknown => ConfirmRequirement::Review, // unreachable in practice
    }
}

/// Label for the "no deletion authorised" action variants.
fn action_none_label(action: &CleanupAction) -> &'static str {
    match action {
        CleanupAction::None => "no action",
        CleanupAction::Defer { .. } => "deferred",
        _ => "unknown",
    }
}

/// Short display label for a risk level (F12 mismatch messages).
fn risk_label(risk: crate::RiskLevel) -> &'static str {
    use crate::RiskLevel::*;
    match risk {
        Safe => "safe",
        RegenerableLocal => "regenerable-local",
        RegenerableDownload => "regenerable-download",
        Review => "review",
        Protected => "protected",
        Unknown => "unknown",
    }
}

/// Machine-readable snake name of a denial reason.
fn deny_label(reason: &DenyReason) -> String {
    let label = match reason {
        DenyReason::ProtectedRootExact { .. } => "protected-root-exact",
        DenyReason::ProtectedRootAncestor { .. } => "protected-root-ancestor",
        DenyReason::ProtectedRisk => "protected-risk",
        DenyReason::UnknownRisk => "unknown-risk",
        DenyReason::NotAbsolute { .. } => "not-absolute",
        DenyReason::PathMismatch { .. } => "path-mismatch",
        DenyReason::TargetMissing { .. } => "target-missing",
        DenyReason::Unverifiable { .. } => "unverifiable",
        DenyReason::ReparseCrossing { .. } => "reparse-crossing",
        DenyReason::ReparseTarget { .. } => "reparse-target",
        DenyReason::ReparseSubstitution { .. } => "reparse-substitution",
        DenyReason::IdentityChanged { .. } => "identity-changed",
        DenyReason::IdentityNotRecorded => "identity-not-recorded",
        DenyReason::StaleSnapshot { .. } => "stale-snapshot",
        DenyReason::ReadOnlyChanged { .. } => "read-only-changed",
        DenyReason::ProcessUnknown { .. } => "process-unknown",
    };
    label.to_string()
}

fn defer_label(reason: &DeferReason) -> String {
    match reason {
        DeferReason::ProcessRunning { .. } => "process-running",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::plan_store::PlanStore;
    use crate::cleanup::port::{DeleteError, DeletePort};
    use crate::domain::category::ResidueCategory;
    use crate::domain::evidence::Evidence;
    use crate::domain::plan::{CleanupPlanItem, ConfirmRequirement};
    use crate::domain::risk_level::RiskLevel;
    use crate::domain::snapshot::TargetSnapshot;
    use crate::domain::source::SourceKind;
    use crate::rules::loader::{CompiledPattern, CompiledRule};
    use crate::rules::priority::RuleSource;
    use crate::safety::fake::FakeProbe;
    use crate::safety::probe::{PathProbe, ProcessProbe, ProcessState};
    use crate::safety::process::ProcessGuard;
    use crate::safety::ProtectedRootRegistry;
    use crate::{CleanupPlan, ExternalCommandSpec, RuleId};
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    fn fake_env() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("USERPROFILE".into(), r"C:\Users\alice".into());
        m.insert("SYSTEMROOT".into(), r"C:\Windows".into());
        m.insert("PROGRAMFILES".into(), r"C:\Program Files".into());
        m.insert("PROGRAMFILES(X86)".into(), r"C:\Program Files (x86)".into());
        m.insert("PROGRAMDATA".into(), r"C:\ProgramData".into());
        m
    }

    fn registry() -> ProtectedRootRegistry {
        let env = fake_env();
        ProtectedRootRegistry::build(&|k| env.get(k).cloned(), vec![]).expect("fake env")
    }

    struct FixedProcess(ProcessState);

    impl ProcessProbe for FixedProcess {
        fn process_state(&self, _names: &[&str]) -> ProcessState {
            self.0
        }
    }

    fn validator_for(probe: &Arc<FakeProbe>, state: ProcessState) -> SafetyValidator {
        let path_probe: Arc<dyn PathProbe + Send + Sync> = probe.clone();
        let process_probe: Arc<dyn ProcessProbe + Send + Sync> = Arc::new(FixedProcess(state));
        SafetyValidator::new(
            path_probe,
            process_probe,
            registry(),
            ProcessGuard::default_list(),
        )
    }

    /// A rule set that never matches any test path (engine tests that do not
    /// exercise F12 still need a non-empty gate).
    fn inert_rules() -> Arc<RuleSet> {
        exact_rules(vec![(
            r"C:\__devresidue_never_matches__",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            "never",
        )])
    }

    /// Builds a rule set from `(exact_target, risk, source, id)` triples.
    fn exact_rules(entries: Vec<(&str, RiskLevel, RuleSource, &str)>) -> Arc<RuleSet> {
        let rules: Vec<CompiledRule> = entries
            .into_iter()
            .enumerate()
            .map(|(i, (target, risk, source, id))| CompiledRule {
                id: id.to_string(),
                numeric_id: RuleId::from_raw(i as u64 + 1),
                source,
                risk,
                category: ResidueCategory::Temporary,
                product: None,
                description: format!("test rule {id}"),
                pattern: CompiledPattern::Exact {
                    target: target.replace('\\', "/"),
                },
                parent_marker: None,
                requires_existing: false,
                include_raw: vec![],
                exclude_raw: vec![],
                include_matchers: vec![],
                exclude_matchers: vec![],
                order: i,
            })
            .collect();
        Arc::new(RuleSet {
            rules,
            issues: vec![],
        })
    }

    /// A protected **glob** rule whose compiled literal root is `root` and
    /// whose full glob pattern is `glob` (R2-F04: protected_areas derives the
    /// conservative root from the glob's literal prefix).
    fn glob_rule_root(root: &str, glob: &str, risk: RiskLevel, id: &str) -> CompiledRule {
        use crate::rules::loader::CompiledPattern;
        let matcher =
            crate::rules::matcher::compile_glob(&glob.replace('\\', "/")).expect("valid test glob");
        CompiledRule {
            id: id.to_string(),
            numeric_id: RuleId::from_raw(1),
            source: RuleSource::UserProtected,
            risk,
            category: ResidueCategory::Credential,
            product: None,
            description: "test glob rule".into(),
            pattern: CompiledPattern::Glob {
                matcher,
                pattern_norm: glob.replace('\\', "/"),
                root: root.replace('\\', "/"),
            },
            parent_marker: None,
            requires_existing: false,
            include_raw: vec![],
            exclude_raw: vec![],
            include_matchers: vec![],
            exclude_matchers: vec![],
            order: 0,
        }
    }

    /// An item lookup that finds nothing (engine tests that construct plans by
    /// hand and do not exercise R02/R04 re-derivation use this — those tests
    /// must build plans from items that *would* be looked up when the lookup
    /// matters).
    fn no_items() -> Arc<ItemLookup> {
        Arc::new(|_| None)
    }

    /// Builds a coherent "current scan" item for a plan item: same id/path/risk
    /// and an action mirroring the plan's mode. Tests that focus on the
    /// validator / process / journal (not the R02 tamper surface) use this so
    /// the authorisation gate passes and the intended guard decides.
    fn coherent_item(item: &CleanupPlanItem) -> ScanItem {
        let action = match item.mode {
            CleanupMode::RecycleBin => CleanupAction::RecycleBin,
            CleanupMode::DirectDelete => CleanupAction::DirectDelete,
            CleanupMode::ExternalCommand => CleanupAction::ExternalCommand {
                command: item.external_command.clone().unwrap_or_else(|| {
                    ExternalCommandSpec::new("frozen-cmd.exe".into(), vec![], None, Some(1))
                }),
            },
        };
        ScanItem {
            id: item.scan_item_id,
            path: item.snapshot.path.clone(),
            display_name: "coherent".into(),
            product: item.product.clone(),
            category: item.category,
            risk: item.snapshot.risk,
            source: SourceKind::Kondo,
            logical_size: item.estimated_size,
            file_count: 1,
            last_modified: item.snapshot.last_write_time,
            explanation: "coherent test item".into(),
            cleanup_action: action,
            evidence: item
                .snapshot
                .rule_id
                .map(|r| Evidence::new("rule", "r").with_rule(r))
                .into_iter()
                .collect(),
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    /// A lookup over the plan's own items (each made coherent) — the standard
    /// harness for non-tamper engine tests.
    fn lookup_from(plan: &CleanupPlan) -> Arc<ItemLookup> {
        let items: Vec<ScanItem> = plan.items.iter().map(coherent_item).collect();
        Arc::new(move |id| items.iter().find(|i| i.id == id).cloned())
    }

    /// Engine over the fake port/validator with an inert rule set and a lookup
    /// over the given plan's own (coherent) items — the standard harness for
    /// non-tamper engine tests.
    fn plan_engine(
        port: Arc<dyn DeletePort>,
        validator: SafetyValidator,
        plan: &CleanupPlan,
    ) -> CleanupEngine {
        CleanupEngine::new(port, validator, inert_rules(), lookup_from(plan))
    }

    /// Engine over the fake port/validator with an explicit rule set and a
    /// lookup over the plan's coherent items (tamper tests that swap the rules
    /// to observe the F12/R08 gate).
    fn tamper_engine(
        port: Arc<dyn DeletePort>,
        validator: SafetyValidator,
        rules: Arc<RuleSet>,
        plan: &CleanupPlan,
    ) -> CleanupEngine {
        CleanupEngine::new(port, validator, rules, lookup_from(plan))
    }

    /// Engine with an explicit rule set and a no-items lookup.
    #[allow(dead_code)] // retained for future tamper tests that need an empty lookup
    fn test_engine_rules(
        port: Arc<dyn DeletePort>,
        validator: SafetyValidator,
        rules: Arc<RuleSet>,
    ) -> CleanupEngine {
        CleanupEngine::new(port, validator, rules, no_items())
    }

    /// Programmable fake deletion port.
    #[derive(Default)]
    struct FakePort {
        calls: Mutex<Vec<String>>,
        doomed: Mutex<HashSet<PathBuf>>,
        /// Paths whose verified deletion must report IdentityMismatch (R05).
        swapped: Mutex<HashSet<PathBuf>>,
    }

    impl FakePort {
        fn doom(&self, path: &Path) {
            self.doomed.lock().unwrap().insert(path.to_path_buf());
        }

        fn swap(&self, path: &Path) {
            self.swapped.lock().unwrap().insert(path.to_path_buf());
        }

        fn snapshot_calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl FakePort {
        /// Shared verified-vs-plain plumbing: a swapped path always refuses
        /// with IdentityMismatch; otherwise the recorded verb runs.
        fn verified_guard(
            &self,
            path: &Path,
            expected: &crate::safety::probe::FileIdentity,
        ) -> Result<(), DeleteError> {
            if self.swapped.lock().unwrap().contains(path) {
                return Err(DeleteError::IdentityMismatch {
                    path: path.to_path_buf(),
                    expected_volume: expected.volume_serial,
                    expected_index: expected.file_index,
                    live_volume: expected.volume_serial + 1,
                    live_index: expected.file_index + 1,
                });
            }
            Ok(())
        }
    }

    impl DeletePort for FakePort {
        fn recycle(&self, path: &Path) -> Result<(), DeleteError> {
            if self.doomed.lock().unwrap().contains(path) {
                return Err(DeleteError::Locked {
                    path: path.to_path_buf(),
                });
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("recycle:{}", path.display()));
            Ok(())
        }

        fn delete_tree(&self, path: &Path) -> Result<(), DeleteError> {
            if self.doomed.lock().unwrap().contains(path) {
                return Err(DeleteError::PermissionDenied {
                    path: path.to_path_buf(),
                });
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("delete:{}", path.display()));
            Ok(())
        }

        fn delete_tree_verified(
            &self,
            path: &Path,
            expected: &crate::safety::probe::FileIdentity,
        ) -> Result<(), DeleteError> {
            self.verified_guard(path, expected)?;
            if self.doomed.lock().unwrap().contains(path) {
                return Err(DeleteError::PermissionDenied {
                    path: path.to_path_buf(),
                });
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("delete-verified:{}", path.display()));
            Ok(())
        }

        fn recycle_verified(
            &self,
            path: &Path,
            expected: &crate::safety::probe::FileIdentity,
        ) -> Result<(), DeleteError> {
            self.verified_guard(path, expected)?;
            if self.doomed.lock().unwrap().contains(path) {
                return Err(DeleteError::Locked {
                    path: path.to_path_buf(),
                });
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("recycle-verified:{}", path.display()));
            Ok(())
        }

        fn execute_command(&self, spec: &ExternalCommandSpec) -> Result<(), DeleteError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("exec:{}", spec.executable));
            match spec.executable.as_str() {
                "fail-timeout.exe" => Err(DeleteError::CommandFailed {
                    exit_code: None,
                    timed_out: true,
                    message: "timed out".into(),
                }),
                "fail-exit.exe" => Err(DeleteError::CommandFailed {
                    exit_code: Some(1),
                    timed_out: false,
                    message: "exit 1".into(),
                }),
                _ => Ok(()),
            }
        }
    }

    fn make_dir(probe: &Arc<FakeProbe>, dir: &str) {
        let marker = Path::new(dir).join("marker.txt");
        let mut fs = probe.lock();
        fs.create_file(&marker, None);
    }

    /// Captures a full identity snapshot for an existing fake-fs directory.
    fn snap(validator: &SafetyValidator, dir: &str) -> TargetSnapshot {
        validator
            .capture(Path::new(dir), None, None, RiskLevel::Safe)
            .expect("capture must succeed for existing dir")
    }

    /// A snapshot with an explicit (possibly forged) declared risk — the F12
    /// gate compares against it.
    fn snap_with_risk(validator: &SafetyValidator, dir: &str, risk: RiskLevel) -> TargetSnapshot {
        validator
            .capture(Path::new(dir), None, None, risk)
            .expect("capture must succeed for existing dir")
    }

    fn plan_item(
        id: u64,
        mode: CleanupMode,
        snap: TargetSnapshot,
        confirmation: ConfirmRequirement,
        external: Option<ExternalCommandSpec>,
    ) -> CleanupPlanItem {
        CleanupPlanItem::new(
            crate::ScanItemId::from_raw(id),
            mode,
            4096,
            snap,
            Some("demo".into()),
            ResidueCategory::Temporary,
            SourceKind::Kondo,
            confirmation,
            external,
        )
    }

    fn build_plan(items: Vec<CleanupPlanItem>) -> CleanupPlan {
        CleanupPlan {
            id: crate::CleanupPlanId::from_raw(1),
            created_at: SystemTime::now(),
            dry_run: None, // legacy field (F15)
            scan_generation: 0,
            items,
        }
    }

    #[test]
    fn no_rules_refuses_to_run() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\a\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\a\\cache"),
            ConfirmRequirement::None,
            None,
        )]);

        let port: Arc<dyn DeletePort> = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(
            port,
            validator,
            Arc::new(RuleSet {
                rules: vec![],
                issues: vec![],
            }),
            no_items(),
        );
        let err = engine
            .run(&plan, EngineOptions::default())
            .expect_err("empty rule set must refuse");
        assert!(matches!(err, EngineError::NoRules));
    }

    #[test]
    fn dry_run_shares_candidate_set_and_never_touches_the_port() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\a\\cache");
        make_dir(&probe, "C:\\proj\\b\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);

        let plan = build_plan(vec![
            plan_item(
                1,
                CleanupMode::RecycleBin,
                snap(&validator, "C:\\proj\\a\\cache"),
                ConfirmRequirement::None,
                None,
            ),
            plan_item(
                2,
                CleanupMode::DirectDelete,
                snap(&validator, "C:\\proj\\b\\cache"),
                ConfirmRequirement::None,
                None,
            ),
        ]);

        let port = Arc::new(FakePort::default());
        let engine = plan_engine(port.clone(), validator, &plan);

        let dry = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    ..EngineOptions::default()
                },
            )
            .expect("dry run ok");
        assert_eq!(dry.session.items.len(), 2);
        assert!(dry
            .session
            .items
            .iter()
            .all(|r| matches!(r.status, CleanupStatus::WouldExecute { .. })));
        assert!(port.snapshot_calls().is_empty(), "dry run must not delete");

        // Real run on the same state → same decisions, port invoked once per
        // allowed item (SPEC §32 candidate consistency).
        let real = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: false,
                    ..EngineOptions::default()
                },
            )
            .expect("real run ok");
        assert!(real
            .session
            .items
            .iter()
            .all(|r| matches!(r.status, CleanupStatus::Success)));
        assert_eq!(port.snapshot_calls().len(), 2);
        assert_eq!(real.session.totals.completed_bytes, 8192);
    }

    #[test]
    fn defer_on_running_process_and_deny_on_unknown_process() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");

        let validator_running = validator_for(&probe, ProcessState::Running);
        let snap = {
            // capture needs a NotRunning validator (the snapshot is
            // independent of process state, but reuse for clarity).
            let v = validator_for(&probe, ProcessState::NotRunning);
            snap(&v, "C:\\proj\\cache")
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap,
            ConfirmRequirement::None,
            None,
        )]);

        let engine = plan_engine(Arc::new(FakePort::default()), validator_running, &plan);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session ok");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("defer:process-running")
        ));

        let validator_unknown = validator_for(&probe, ProcessState::Unknown);
        let engine = plan_engine(Arc::new(FakePort::default()), validator_unknown, &plan);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session ok");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("deny:process-unknown")
        ));
    }

    #[test]
    fn identity_change_between_snapshot_and_run_is_denied() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let snapshot = snap(&validator, "C:\\proj\\cache");
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snapshot,
            ConfirmRequirement::None,
            None,
        )]);

        // Attacker deletes + re-creates the target after the plan was built.
        {
            let mut fs = probe.lock();
            fs.delete_and_recreate(Path::new("C:\\proj\\cache"));
        }

        let engine = plan_engine(Arc::new(FakePort::default()), validator, &plan);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("deny:identity-changed")
        ));
    }

    #[test]
    fn partial_failure_keeps_going_and_records_errors() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\one\\cache");
        make_dir(&probe, "C:\\proj\\two\\cache");
        make_dir(&probe, "C:\\proj\\three\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);

        let plan = build_plan(vec![
            plan_item(
                1,
                CleanupMode::RecycleBin,
                snap(&validator, "C:\\proj\\one\\cache"),
                ConfirmRequirement::None,
                None,
            ),
            plan_item(
                2,
                CleanupMode::RecycleBin,
                snap(&validator, "C:\\proj\\two\\cache"),
                ConfirmRequirement::None,
                None,
            ),
            plan_item(
                3,
                CleanupMode::RecycleBin,
                snap(&validator, "C:\\proj\\three\\cache"),
                ConfirmRequirement::None,
                None,
            ),
        ]);

        let port = FakePort::default();
        port.doom(Path::new("C:\\proj\\two\\cache"));
        let engine = plan_engine(Arc::new(port), validator, &plan);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");

        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Success
        ));
        assert!(matches!(
            run.session.items[1].status,
            CleanupStatus::Failed { .. }
        ));
        assert!(matches!(
            run.session.items[2].status,
            CleanupStatus::Success
        ));
        assert_eq!(run.session.totals.succeeded, 2);
        assert_eq!(run.session.totals.failed, 1);
    }

    #[test]
    fn external_command_timeout_marks_item_failed() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\npm-cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let spec = ExternalCommandSpec::new("fail-timeout.exe".into(), vec![], None, Some(1));
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\proj\\npm-cache"),
            ConfirmRequirement::None,
            Some(spec),
        )]);

        let engine = plan_engine(Arc::new(FakePort::default()), validator, &plan);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Failed { ref error } if error.contains("timed_out=true")
        ));
    }

    #[test]
    fn missing_command_spec_is_denied_as_command_mismatch() {
        // R02: the plan record declares mode ExternalCommand but carries no
        // command spec. The scanned item's frozen command (the authoritative
        // source) exists, so the plan is tampered/incomplete → the item is
        // denied as command-mismatch and the port is never reached.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // A legitimate scan would freeze a command on the item; the tampered
        // plan JSON drops it.
        let frozen = ExternalCommandSpec::new("frozen-cmd.exe".into(), vec![], None, Some(1));
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\proj\\cache"),
            ConfirmRequirement::None,
            None, // plan lost the command (tamper)
        )]);
        // Override the coherent lookup: the current scan item freezes the real
        // command, which the plan no longer carries.
        let port = Arc::new(FakePort::default());
        let mut scan_items = [coherent_item(&plan.items[0])];
        if let CleanupAction::ExternalCommand { command } = &mut scan_items[0].cleanup_action {
            *command = frozen.clone();
        }
        let lookup: Arc<ItemLookup> =
            Arc::new(move |id| scan_items.iter().find(|i| i.id == id).cloned());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("command-mismatch")
        ));
        assert!(
            port.snapshot_calls().is_empty(),
            "tampered command must never reach the port"
        );
    }

    #[test]
    fn confirmation_gate_refuses_real_run_below_requirement() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\review-cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\review-cache"),
            ConfirmRequirement::Review,
            None,
        )]);

        let engine = plan_engine(Arc::new(FakePort::default()), validator, &plan);
        // Real run with no confirmation → refuse before touching anything.
        let err = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: false,
                    ..EngineOptions::default()
                },
            )
            .expect_err("must refuse");
        assert!(matches!(
            err,
            EngineError::ConfirmationRequired {
                required: ConfirmRequirement::Review,
                ..
            }
        ));

        // Dry-run is side-effect free and may still preview.
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    ..EngineOptions::default()
                },
            )
            .expect("dry run allowed");
        assert_eq!(run.session.items.len(), 1);
    }

    #[test]
    fn journal_records_attempt_and_result_for_each_item() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\cache"),
            ConfirmRequirement::None,
            None,
        )]);

        let base = std::env::temp_dir().join(format!("devresidue-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let journal = crate::journal::Journal::open_at(&base).expect("journal");
        let engine = plan_engine(Arc::new(FakePort::default()), validator, &plan);
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    journal: Some(journal),
                    ..EngineOptions::default()
                },
            )
            .expect("run");
        assert!(!run.session.journal_degraded, "journal healthy");

        let records = crate::journal::read_last(&base, 100).expect("read journal");
        assert_eq!(records.len(), 2, "attempt + result");
        assert_eq!(records[0].phase, crate::journal::JournalPhase::Attempt);
        assert_eq!(records[1].phase, crate::journal::JournalPhase::Result);
        assert_eq!(records[0].session_id, 1);
        assert_eq!(records[0].estimated_size, 4096);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Guards that a plan store round trip preserves confirmation + spec.
    #[test]
    fn plan_store_round_trip_preserves_new_fields() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\cache"),
            ConfirmRequirement::None,
            None,
        )]);

        let base = std::env::temp_dir().join(format!("devresidue-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = PlanStore::open_at(&base).expect("store");
        let mut plan = plan;
        // Serialisation floors sub-second precision: quantise created_at so
        // the round trip is exact (the domain does the same in its tests).
        let secs = plan
            .created_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        plan.created_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let id = store.save(&mut plan).expect("save");
        let loaded = store.load(id).expect("load");
        assert_eq!(loaded, plan);
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].confirmation, ConfirmRequirement::None);
        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- F12: plan-file tamper gate (risk re-derivation) --------------------

    #[test]
    fn tampered_plan_risking_protected_path_is_denied() {
        // A plan item whose JSON was edited to point at a protected rule path
        // while declaring risk `safe` must be refused before the port is ever
        // touched. With the R2-F04 intersection gate the exact-protected rule
        // area equals the target, so the item is refused as
        // `protected-descendant` (conservative upper bound); the older F12
        // risk-derivation path would also refuse — either way the port stays
        // silent.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\Users\\alice\\.ssh");
        let validator = validator_for(&probe, ProcessState::NotRunning);

        // Rules gate: `%USERPROFILE%\.ssh` is protected (like the built-in).
        let rules = exact_rules(vec![(
            r"C:\Users\alice\.ssh",
            RiskLevel::Protected,
            RuleSource::BuiltinProtected,
            "builtin-protected/ssh",
        )]);

        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\Users\\alice\\.ssh"), // declared Safe via capture
            ConfirmRequirement::None,
            None,
        )]);

        let port = Arc::new(FakePort::default());
        let engine = tamper_engine(port.clone(), validator, rules, &plan);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.contains("protected-descendant")
                    || reason.contains("risk-declaration-mismatch")
                    || reason.contains("risk-rederivation-mismatch")
        ));
        assert!(
            port.snapshot_calls().is_empty(),
            "tampered item must never reach the deletion port"
        );
        assert_eq!(run.session.totals.skipped, 1);
    }

    #[test]
    fn risk_matching_the_rule_gate_executes_normally() {
        // Legitimate flow: the plan's declared risk equals the rule gate's
        // re-derivation → the item proceeds (dry run previews).
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\session-data");
        let validator = validator_for(&probe, ProcessState::NotRunning);

        let rules = exact_rules(vec![(
            r"C:\proj\session-data",
            RiskLevel::Review,
            RuleSource::BuiltinDetection,
            "builtin-detection/session",
        )]);

        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap_with_risk(&validator, "C:\\proj\\session-data", RiskLevel::Review),
            ConfirmRequirement::Review,
            None,
        )]);

        let port = Arc::new(FakePort::default());
        let engine = tamper_engine(port.clone(), validator, rules, &plan);
        // Dry-run with review policy: allowed & previews as WouldExecute.
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    confirm_policy: ConfirmPolicy::Review,
                    ..EngineOptions::default()
                },
            )
            .expect("dry run");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::WouldExecute { .. }
        ));
        assert!(
            !port.snapshot_calls().is_empty() || !run.session.items.is_empty(),
            "legit item must not be skipped by the F12 gate"
        );
    }

    #[test]
    fn uncovered_provider_path_is_not_a_mismatch() {
        // A kondo-style artifact not covered by any rule must pass the F12 gate
        // (no rule match → no re-derivation).
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\target");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\target"),
            ConfirmRequirement::None,
            None,
        )]);

        let port = Arc::new(FakePort::default());
        let engine = plan_engine(port.clone(), validator, &plan);
        let run = engine.run(&plan, EngineOptions::default()).expect("run");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Success
        ));
        assert_eq!(port.snapshot_calls().len(), 1);
    }

    // ---- R01-R04/R08: authorisation-chain regression tests ------------------

    /// A hand-built scan item for authorisation tests.
    #[allow(clippy::too_many_arguments)]
    fn auth_item(
        id: u64,
        path: &str,
        source: SourceKind,
        risk: RiskLevel,
        action: CleanupAction,
        rule: Option<RuleId>,
    ) -> ScanItem {
        ScanItem {
            id: crate::ScanItemId::from_raw(id),
            path: path.into(),
            display_name: "auth".into(),
            product: Some("auth".into()),
            category: ResidueCategory::Temporary,
            risk,
            source,
            logical_size: 1,
            file_count: 1,
            last_modified: None,
            explanation: "auth test".into(),
            cleanup_action: action,
            evidence: rule
                .map(|r| Evidence::new("rule", "rule").with_rule(r))
                .into_iter()
                .collect(),
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    #[test]
    fn r01_replacement_between_scan_and_plan_is_denied_at_execution() {
        // The scan-time fingerprint is captured at identity=100 and carried on
        // the item/plan. The object is replaced (identity→200) *after* the plan
        // is built; validation against the carried snapshot must refuse and the
        // port must never be called.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let scan_snapshot = validator
            .capture(Path::new("C:\\review\\cache"), None, None, RiskLevel::Safe)
            .expect("scan-time capture");

        let original = ScanItem {
            scan_snapshot: Some(scan_snapshot.clone()),
            ..auth_item(
                1,
                "C:\\review\\cache",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            scan_snapshot,
            ConfirmRequirement::None,
            None,
        )]);

        // Replacement after planning: identity flips to 200.
        {
            let mut fs = probe.lock();
            fs.delete_and_recreate(Path::new("C:\\review\\cache"));
        }
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.starts_with("deny:identity-changed")
                    || reason.starts_with("deny:stale-snapshot")
        ));
        assert!(
            port.snapshot_calls().is_empty(),
            "replacement between scan and plan must never reach the port"
        );
    }

    #[test]
    fn r02_mode_tamper_recycle_to_external_is_denied() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // Scan item freezes a recycle action; the tampered plan declares an
        // external command.
        let original = auth_item(
            1,
            "C:\\review\\cache",
            SourceKind::Kondo,
            RiskLevel::Safe,
            CleanupAction::RecycleBin,
            None,
        );
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\review\\cache"),
            ConfirmRequirement::None,
            Some(ExternalCommandSpec::new(
                "review-placeholder.exe".into(),
                vec!["untrusted".into()],
                None,
                Some(1),
            )),
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("mode-mismatch")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r02_confirmation_downgrade_is_denied() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\sessions");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // Scan item risk Review → derived confirmation Review.
        let original = auth_item(
            1,
            "C:\\review\\sessions",
            SourceKind::Kondo,
            RiskLevel::Review,
            CleanupAction::RecycleBin,
            None,
        );
        // Tampered plan: confirmation downgraded to None while risk stays
        // Review (which a matching rule confirms).
        let rules = exact_rules(vec![(
            r"C:\review\sessions",
            RiskLevel::Review,
            RuleSource::BuiltinDetection,
            "builtin-detection/sessions",
        )]);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap_with_risk(&validator, "C:\\review\\sessions", RiskLevel::Review),
            ConfirmRequirement::None, // downgraded
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, rules, lookup);
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    ..EngineOptions::default()
                },
            )
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.starts_with("confirmation-downgraded")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r03_protected_descendant_blocks_parent_cleanup() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\target\\credentials");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // UserProtected rule on the child; the parent target is cleanable
        // (RegenerableLocal) but deleting it would delete the protected child.
        let rules = exact_rules(vec![(
            r"C:\review\target\credentials",
            RiskLevel::Protected,
            RuleSource::UserProtected,
            "user-protected/credentials",
        )]);
        let original = auth_item(
            1,
            "C:\\review\\target",
            SourceKind::Kondo,
            RiskLevel::RegenerableLocal,
            CleanupAction::RecycleBin,
            None,
        );
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap_with_risk(
                &validator,
                "C:\\review\\target",
                RiskLevel::RegenerableLocal,
            ),
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, rules, lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.starts_with("protected-descendant")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r04_stale_item_id_is_denied() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // Plan references id 7 which the current scan does not contain.
        let plan = build_plan(vec![plan_item(
            7,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\cache"),
            ConfirmRequirement::None,
            None,
        )]);
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), no_items());
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.starts_with("item-not-in-current-scan")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r08_missing_detection_rule_invalidates_the_plan() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = auth_item(
            1,
            "C:\\review\\cache",
            SourceKind::Rule,
            RiskLevel::Safe,
            CleanupAction::RecycleBin,
            Some(RuleId::from_raw(1)),
        );
        // The current rule gate no longer contains the discovery rule (only an
        // unrelated one) → discovery source invalidated.
        let rules = exact_rules(vec![(
            r"C:\unrelated",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            "unrelated",
        )]);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap_with_risk(&validator, "C:\\review\\cache", RiskLevel::Safe),
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, rules, lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.starts_with("discovery-source-invalidated")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    // ---- R05/R06: verified-deletion binding + tool-scope re-verification ----

    #[test]
    fn r05_engine_calls_the_verified_port_variant_with_the_live_identity() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\target");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\target"),
            ConfirmRequirement::None,
            None,
        )]);

        let port = Arc::new(FakePort::default());
        let engine = plan_engine(port.clone(), validator, &plan);
        let run = engine.run(&plan, EngineOptions::default()).expect("run");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Success
        ));
        let calls = port.snapshot_calls();
        // RecycleBin executes as a VERIFIED permanent deletion (the Windows
        // recycle API cannot be handle-bound; see execute_with). The R05
        // binding requirement still holds: the live identity must reach the
        // verified port variant — now delete-verified.
        assert!(
            calls.iter().any(|c| c.starts_with("delete-verified:")),
            "engine must bind the deletion to the validator's live identity (R05): {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("delete:") && !c.contains("verified")),
            "plain (unverified) deletion must not be used when an identity is available"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("recycle:") && !c.contains("verified")),
            "the recycle port must not be used by the engine routing"
        );
    }

    #[test]
    fn r05_identity_mismatch_at_the_port_fails_the_item() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\swapme");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::DirectDelete,
            snap(&validator, "C:\\proj\\swapme"),
            ConfirmRequirement::None,
            None,
        )]);

        // The object is replaced between validation and the port call: the
        // fake port reports IdentityMismatch → item Failed, never success.
        let port = Arc::new(FakePort::default());
        port.swap(Path::new("C:\\proj\\swapme"));
        let engine = plan_engine(port.clone(), validator, &plan);
        let run = engine.run(&plan, EngineOptions::default()).expect("run");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Failed { ref error }
                if error.starts_with("identity mismatch at port")
        ));
        assert_eq!(run.session.totals.failed, 1);
    }

    /// A fake ToolQueryPort answering the re-query with a canned root.
    struct CannedTool {
        root: std::sync::Mutex<Option<String>>,
    }

    impl CannedTool {
        fn new(root: &str) -> Self {
            Self {
                root: std::sync::Mutex::new(Some(root.to_string())),
            }
        }
    }

    impl crate::cleanup::port::ToolQueryPort for CannedTool {
        fn query_cache_root(&self, _spec: &crate::domain::action::ToolQuerySpec) -> Option<String> {
            self.root.lock().unwrap().clone()
        }
    }

    fn scoped_command(cache_root: &str) -> crate::ExternalCommandSpec {
        crate::ExternalCommandSpec::with_scope(
            "uv".into(),
            vec!["cache".into(), "clean".into(), cache_root.into()],
            None,
            Some(300),
            crate::domain::action::ScopeBinding {
                tool: "uv".into(),
                expected_cache_root: cache_root.into(),
                verify_query: crate::domain::action::ToolQuerySpec {
                    executable: "uv".into(),
                    args: vec!["cache".into(), "dir".into()],
                },
            },
        )
    }

    #[test]
    fn r06_tool_scope_drift_is_refused_before_execution() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\npm-cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // Frozen scope verified A; the tool now reports B → drift.
        let original = ScanItem {
            cleanup_action: CleanupAction::ExternalCommand {
                command: scoped_command(r"C:\review\cache-a"),
            },
            ..auth_item(
                1,
                "C:\\proj\\npm-cache",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\proj\\npm-cache"),
            ConfirmRequirement::None,
            Some(scoped_command(r"C:\review\cache-a")),
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);

        // Tool reports B ≠ frozen A.
        let tool = Arc::new(CannedTool::new(r"C:\review\cache-b"));
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    tool_query: Some(tool),
                    ..EngineOptions::default()
                },
            )
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Failed { ref error } if error.contains("tool-scope-drift")
        ));
        assert!(
            port.snapshot_calls().is_empty(),
            "a drifted tool scope must never reach the execution port"
        );
    }

    #[test]
    fn r06_matching_tool_scope_executes_the_command() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\npm-cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = ScanItem {
            cleanup_action: CleanupAction::ExternalCommand {
                command: scoped_command(r"C:\review\cache-a"),
            },
            ..auth_item(
                1,
                "C:\\proj\\npm-cache",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\proj\\npm-cache"),
            ConfirmRequirement::None,
            Some(scoped_command(r"C:\review\cache-a")),
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);

        // Tool reports A == frozen A → command executes.
        let tool = Arc::new(CannedTool::new(r"C:\review\cache-a"));
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    tool_query: Some(tool),
                    ..EngineOptions::default()
                },
            )
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Success
        ));
        assert!(port
            .snapshot_calls()
            .iter()
            .any(|c| c.starts_with("exec:uv")));
    }

    #[test]
    fn r06_missing_tool_query_port_for_a_scoped_command_is_refused() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\npm-cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = ScanItem {
            cleanup_action: CleanupAction::ExternalCommand {
                command: scoped_command(r"C:\review\cache-a"),
            },
            ..auth_item(
                1,
                "C:\\proj\\npm-cache",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\proj\\npm-cache"),
            ConfirmRequirement::None,
            Some(scoped_command(r"C:\review\cache-a")),
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        // No tool_query wired → the frozen scope cannot be verified → refuse.
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Failed { ref error } if error.contains("tool-scope-drift")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    // ---- Round-2 (review lane A) regressions: F01/F03/F04/F06/F09 ----------

    #[test]
    fn r2f01_retargeted_plan_snapshot_is_refused() {
        // R2-F01 evidence 1: original authorises A; the plan JSON retargets its
        // snapshot to protected B. The plan snapshot must never become the
        // execution object — refused as plan-snapshot-mismatch, port silent.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\cache-a");
        make_dir(&probe, "C:\\review\\protected-b");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = ScanItem {
            scan_snapshot: Some(snap(&validator, "C:\\review\\cache-a")),
            ..auth_item(
                1,
                "C:\\review\\cache-a",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        // Plan whose snapshot points at B (attacker edited the file).
        let b_snapshot = snap(&validator, "C:\\review\\protected-b");
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            b_snapshot,
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("plan-snapshot-mismatch")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r2f01_snapshot_identity_rewrite_is_refused() {
        // R2-F01 evidence 2: scan identity 100; attacker rewrites the plan
        // snapshot's identity to the current (replacement) object 200. The
        // plan snapshot no longer matches the scan-time identity → refused.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // Original scan snapshot records identity 100.
        let scan_snapshot = snap(&validator, "C:\\review\\cache");
        let original = ScanItem {
            scan_snapshot: Some(scan_snapshot.clone()),
            ..auth_item(
                1,
                "C:\\review\\cache",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        // Plan snapshot = the same path but captured at identity 200 (a
        // replaced object).
        {
            let mut fs = probe.lock();
            fs.delete_and_recreate(Path::new("C:\\review\\cache"));
        }
        let forged_snapshot = snap(&validator, "C:\\review\\cache");
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            forged_snapshot,
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("plan-snapshot-mismatch")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r2f01_risk_downgrade_without_rule_hit_is_refused() {
        // R2-F01 evidence 3: Review original, no rule hit; attacker edits plan
        // snapshot risk → Safe and confirmation → None. The authorisation must
        // bind to the ORIGINAL scan risk → risk-declaration-mismatch.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\review-only");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = ScanItem {
            scan_snapshot: Some(snap_with_risk(
                &validator,
                "C:\\review\\review-only",
                RiskLevel::Review,
            )),
            ..auth_item(
                1,
                "C:\\review\\review-only",
                SourceKind::Kondo,
                RiskLevel::Review,
                CleanupAction::RecycleBin,
                None,
            )
        };
        // Plan declares Safe + confirmation None (edited).
        let mut downgraded =
            snap_with_risk(&validator, "C:\\review\\review-only", RiskLevel::Review);
        downgraded.risk = RiskLevel::Safe;
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            downgraded,
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    confirm_policy: ConfirmPolicy::Review,
                    ..EngineOptions::default()
                },
            )
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.starts_with("risk-declaration-mismatch")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r2f03_generation_mismatch_refuses_the_whole_plan() {
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let mut plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\proj\\cache"),
            ConfirmRequirement::None,
            None,
        )]);
        plan.scan_generation = 1;
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), no_items());
        let err = engine
            .run(
                &plan,
                EngineOptions {
                    expected_scan_generation: Some(2),
                    ..EngineOptions::default()
                },
            )
            .expect_err("generation mismatch must refuse the run");
        assert!(matches!(err, EngineError::GenerationMismatch { .. }));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r2f04_glob_literal_root_equal_to_target_is_refused() {
        // F04: a user-protected glob `C:/review/target/**/credentials` has the
        // literal root `C:/review/target` equal to the deletion target. The
        // conservative protected-area intersection must refuse (it covers the
        // whole `target` tree).
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\target\\nested\\credentials");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        // Build a rule set whose protected area literal root equals the target.
        // We construct a glob compiled rule directly with that root.
        // Build a rule set whose protected area literal root equals the target.
        let rule = glob_rule_root(
            r"C:\review\target",
            r"C:\review\target\**\credentials",
            RiskLevel::Protected,
            "user-protected/credentials",
        );
        let rules = Arc::new(RuleSet {
            rules: vec![rule],
            issues: vec![],
        });
        let original = auth_item(
            1,
            "C:\\review\\target",
            SourceKind::Kondo,
            RiskLevel::Safe,
            CleanupAction::RecycleBin,
            None,
        );
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap(&validator, "C:\\review\\target"),
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, rules, lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason } if reason.contains("protected-descendant")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r2f06_removed_reclassification_rule_invalidates_authorization() {
        // F06: a Provider item reclassified by a User rule (risk → Safe) but
        // recorded only the classification slug. After the rule is removed,
        // plan/execution must refuse (the Safe classification's basis is gone).
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\review\\cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = ScanItem {
            risk: RiskLevel::Safe,
            classification_rule_id: Some("user/safe-cache".into()),
            scan_snapshot: Some(snap_with_risk(
                &validator,
                "C:\\review\\cache",
                RiskLevel::Safe,
            )),
            ..auth_item(
                1,
                "C:\\review\\cache",
                SourceKind::AgentProvider, // real provider source kept
                RiskLevel::Review,         // pre-classification risk
                CleanupAction::RecycleBin,
                None,
            )
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::RecycleBin,
            snap_with_risk(&validator, "C:\\review\\cache", RiskLevel::Safe),
            ConfirmRequirement::None,
            None,
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        // Rule gate does NOT contain the classification rule.
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        let run = engine
            .run(&plan, EngineOptions::default())
            .expect("session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Skipped { ref reason }
                if reason.starts_with("discovery-source-invalidated")
        ));
        assert!(port.snapshot_calls().is_empty());
    }

    #[test]
    fn r2f09_dry_run_reports_tool_scope_drift_instead_of_would_execute() {
        // F09: the tool-scope re-verification is preflight — dry-run must fail
        // identically to a real run when the frozen scope drifted.
        let probe = FakeProbe::new();
        make_dir(&probe, "C:\\proj\\npm-cache");
        let validator = validator_for(&probe, ProcessState::NotRunning);
        let original = ScanItem {
            cleanup_action: CleanupAction::ExternalCommand {
                command: scoped_command(r"C:\review\cache-a"),
            },
            ..auth_item(
                1,
                "C:\\proj\\npm-cache",
                SourceKind::Kondo,
                RiskLevel::Safe,
                CleanupAction::RecycleBin,
                None,
            )
        };
        let plan = build_plan(vec![plan_item(
            1,
            CleanupMode::ExternalCommand,
            snap(&validator, "C:\\proj\\npm-cache"),
            ConfirmRequirement::None,
            Some(scoped_command(r"C:\review\cache-a")),
        )]);
        let lookup: Arc<ItemLookup> = Arc::new(move |id| (id.raw() == 1).then(|| original.clone()));
        let port = Arc::new(FakePort::default());
        let engine = CleanupEngine::new(port.clone(), validator, inert_rules(), lookup);
        // Tool reports B (drift) deterministically.
        let tool = Arc::new(CannedTool::new(r"C:\review\cache-b"));
        let run = engine
            .run(
                &plan,
                EngineOptions {
                    dry_run: true,
                    tool_query: Some(tool),
                    ..EngineOptions::default()
                },
            )
            .expect("dry-run session");
        assert!(matches!(
            run.session.items[0].status,
            CleanupStatus::Failed { ref error } if error.contains("tool-scope-drift")
        ));
        assert!(port.snapshot_calls().is_empty());
    }
}
