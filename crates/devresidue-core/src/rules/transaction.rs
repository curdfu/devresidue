//! Atomic AI User Rule batch transaction (Task 2 / AiAdvisor Phase A).
//!
//! # All-or-nothing contract
//!
//! [`commit_ai_rule_batch`] commits a whole user-confirmed batch as **one**
//! transaction over the cumulative `user-dispositions.yaml` file. There is no
//! partial success: either every confirmation becomes a rule (and the reloaded
//! file re-verifies and the marker reaches `COMMITTED`), or the pre-transaction
//! state is restored and a single batch error is returned.
//!
//! The transaction runs entirely through the injected
//! [`UserRuleTransactionPort`](crate::ai::UserRuleTransactionPort) under its
//! exclusive guard:
//!
//! ```text
//! reject empty / duplicate-anchor batches
//!   → acquire the exclusive port guard
//!   → port.recover_if_needed → TransactionRecovery
//!       Clean                               → proceed
//!       RolledBackPendingCleanup(tx)        → validate restored live state;
//!                                             only on success
//!                                             port.cleanup_rolled_back(tx);
//!                                             any failure keeps the material and
//!                                             refuses the new transaction
//!       CommittedPendingCleanup(tx)         → re-read + re-parse + re-validate
//!                                             the committed live file; only on
//!                                             success port.cleanup_committed(tx);
//!                                             any failure keeps the material and
//!                                             refuses the new transaction
//!   → read + parse the original cumulative user file (preserve every rule)
//!   → build every draft (LocalRuleDraftBuilder)
//!   → detect normalized exact-anchor conflicts
//!   → merge: keep untouched rules, replace same-anchor ai-origin rules, append
//!     the new drafts
//!   → validate the full candidate file + run CandidateRuleVerifier
//!   → port.prepare (candidate + durable backup + ACTIVE/PRESENT|ABSENT marker)
//!   → port.replace (live only; marker/backup/candidate untouched)
//!   → re-read + re-parse + re-validate + verifier recheck of the live file
//!   → port.commit_verified (ACTIVE/* → COMMITTED — the sole commit point; an
//!     in-place marker rewrite with no no-marker window)
//!   → port.cleanup_committed (best effort; a cleanup failure never turns a
//!     committed transaction into an uncommitted report)
//!   → on ANY failure before commit_verified succeeds: port.rollback(tx), which
//!     restores and publishes ROLLED_BACK/*; then Core re-enters the same
//!     recover_rolled_back_pending validation gate used at startup, and only
//!     after that gate passes asks port.cleanup_rolled_back(tx); return one
//!     batch error
//! ```
//!
//! # Single persistent marker
//!
//! Transaction state is **never inferred** from whether the live YAML parses or
//! from the mere existence of a backup. `prepare` publishes a candidate, a
//! durable backup (only when the original live file existed) and finally a
//! single persistent marker whose contents are exactly `ACTIVE/PRESENT`,
//! `ACTIVE/ABSENT` or `COMMITTED`. `rollback` and `recover_if_needed` act
//! strictly on that marker: `ACTIVE/PRESENT` always restores the backup bytes
//! (regardless of how the replaced live file parses), `ACTIVE/ABSENT` always
//! removes the live file to restore the original absence, and `COMMITTED` is
//! never rolled back — a `COMMITTED` recovery is reported to the caller as
//! `CommittedPendingCleanup` for semantic re-validation before cleanup.
//!
//! # Authority boundary
//!
//! This module never constructs a `CleanupPlan`, never calls a delete port and
//! never imports any `cleanup/` module. It only writes classification rules
//! through the transaction port; deletion authority stays with the
//! `CleanupEngine`.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::ai::{
    PreparedRuleTransaction, RuleOrigin, TransactionRecovery, UserRuleTransactionPort,
};
use crate::rules::draft::{AiRuleConfirmation, CandidateRuleVerifier, LocalRuleDraftBuilder};
use crate::rules::loader::compile_rule_docs;
use crate::rules::matcher::expand_env;
use crate::rules::priority::{resolve, RuleSource};
use crate::rules::schema::{RuleDoc, RuleFile};
use crate::rules::user::dispositions_path;
use crate::safety::canonical;
use crate::RuleId;

/// Pure "rematch" verifier: compiles the whole candidate user file with the
/// existing validation/compilation semantics ([`compile_rule_docs`]) and then,
/// for every confirmation, re-resolves the original scanned path with
/// [`resolve`]. The commit is only accepted when each item resolves to its own
/// generated draft id with exactly the user's final risk.
///
/// This is the Core-side default used by tests and as the reference adapter;
/// richer layers (Task 7) may supply a verifier that merges built-in + staged
/// user rules first, as long as it honours the same
/// [`CandidateRuleVerifier`] contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct CandidateRematchVerifier;

impl CandidateRuleVerifier for CandidateRematchVerifier {
    fn verify(
        &self,
        candidate_user_file: &RuleFile,
        confirmations: &[AiRuleConfirmation],
    ) -> Result<(), String> {
        let lookup = |name: &str| std::env::var(name).ok();
        let set = compile_rule_docs(&candidate_user_file.rules, &lookup)
            .map_err(|e| format!("candidate rematch: compile failed: {e}"))?;

        for confirmation in confirmations {
            // Rebuild the expected draft to learn the id/risk we must observe.
            let draft = LocalRuleDraftBuilder.build(confirmation)
                .map_err(|e| format!("candidate rematch: rebuild draft: {e}"))?;
            let resolved = resolve(&confirmation.item.path, &set.rules).ok_or_else(|| {
                format!(
                    "candidate rematch: item path did not resolve to any candidate rule \
                     (expected generated `{}` with risk {:?})",
                    draft.id, confirmation.final_risk
                )
            })?;
            if resolved.id != draft.id {
                return Err(format!(
                    "candidate rematch: item resolved to `{}` (risk {:?}), expected the \
                     generated rule `{}` with risk {:?}",
                    resolved.id, resolved.risk, draft.id, confirmation.final_risk
                ));
            }
            if resolved.risk != confirmation.final_risk {
                return Err(format!(
                    "candidate rematch: item resolved to risk {:?}, expected {:?} from rule `{}`",
                    resolved.risk, confirmation.final_risk, resolved.id
                ));
            }
        }
        Ok(())
    }
}

/// Commits a whole AI-confirmed batch of user rules atomically. Returns the
/// numeric [`RuleId`]s of the written rules in confirmation order.
pub fn commit_ai_rule_batch(
    user_rules_dir: &Path,
    confirmations: &[AiRuleConfirmation],
    verifier: &dyn CandidateRuleVerifier,
    port: &dyn UserRuleTransactionPort,
) -> Result<Vec<RuleId>, String> {
    let lookup = |name: &str| std::env::var(name).ok();
    let rule_file = dispositions_path(user_rules_dir);

    if confirmations.is_empty() {
        return Err("refusing to commit an empty AI confirmation batch".to_string());
    }

    // All mutations run under the injected exclusive guard.
    let _guard = port.acquire_exclusive(&rule_file)?;
    match port.recover_if_needed(&rule_file)? {
        TransactionRecovery::Clean => {}
        TransactionRecovery::RolledBackPendingCleanup(tx) => {
            recover_rolled_back_pending(&rule_file, &tx, port, &lookup)?;
        }
        TransactionRecovery::CommittedPendingCleanup(tx) => {
            // A previous transaction reached COMMITTED but crashed before its
            // material was cleaned up. The committed rules are authoritative and
            // must NOT be rolled back. Re-read, re-parse and semantically
            // validate the live file; only on success may cleanup_committed
            // run. Any failure keeps the material and refuses the new
            // transaction (fail-closed, never silent).
            recover_committed_pending(&rule_file, &tx, port, &lookup)?;
        }
    }

    // `prepared` becomes `Some` only after `prepare` has succeeded (i.e. once a
    // rollback recovery source exists on disk). Failures before that point
    // have written nothing and need no rollback.
    let mut prepared: Option<PreparedRuleTransaction> = None;
    let outcome = exec_transaction(
        &rule_file,
        confirmations,
        verifier,
        port,
        &mut prepared,
        &lookup,
    );

    match outcome {
        Ok(ids) => Ok(ids),
        Err(err) => match prepared {
            // Rollback only ever happens before a successful commit_verified.
            Some(tx) => match port.rollback(&tx) {
                Ok(rolled_back_tx) => match recover_rolled_back_pending(
                    &rule_file,
                    &rolled_back_tx,
                    port,
                    &lookup,
                ) {
                    Ok(()) => Err(err),
                    Err(recovery_err) => Err(format!(
                        "{err}; same-process rollback recovery failed: {recovery_err}"
                    )),
                },
                Err(rollback_err) => Err(format!(
                    "{err}; rollback also failed to restore/publish the user rule file: \
                     {rollback_err} (manual repair required; recovery material preserved)"
                )),
            },
            None => Err(err),
        },
    }
}

/// Handles a `RolledBackPendingCleanup` recovery outcome. The port has already
/// restored the original state and verified the bytes/absence, but deliberately
/// kept the persistent ROLLED_BACK marker and recovery material. Core must
/// validate that state before allowing the cleanup; it never reopens or
/// reconstructs the candidate rule file.
fn recover_rolled_back_pending(
    rule_file: &Path,
    tx: &PreparedRuleTransaction,
    port: &dyn UserRuleTransactionPort,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let marker = std::fs::read_to_string(&tx.marker_path)
        .map_err(|e| format!("rolled-back recovery: cannot read marker: {e} "))?;
    match marker.trim() {
        "ROLLED_BACK/PRESENT" => {
            let bytes = port.read_current(rule_file)?.ok_or_else(|| {
                format!(
                    "rolled-back recovery: PRESENT live rule file {} is missing \
                     (evidence preserved; refusing a new transaction)",
                    rule_file.display()
                )
            })?;
            let parsed = parse_rule_file(Some(&bytes), rule_file).map_err(|e| {
                format!(
                    "rolled-back recovery: PRESENT live file failed to parse: {e} \
                     (evidence preserved; refusing a new transaction)"
                )
            })?;
            compile_rule_docs(&parsed.rules, lookup).map_err(|e| {
                format!(
                    "rolled-back recovery: PRESENT live file failed semantic validation: {e} \
                     (evidence preserved; refusing a new transaction)"
                )
            })?;
        }
        "ROLLED_BACK/ABSENT" => {
            if port.read_current(rule_file)?.is_some() {
                return Err(format!(
                    "rolled-back recovery: ABSENT live rule file {} reappeared \
                     (evidence preserved; refusing a new transaction)",
                    rule_file.display()
                ));
            }
        }
        other => {
            return Err(format!(
                "rolled-back recovery: unexpected marker `{other}` at {} \
                 (evidence preserved; refusing a new transaction)",
                tx.marker_path.display()
            ));
        }
    }

    port.cleanup_rolled_back(tx).map_err(|e| {
        format!(
            "rolled-back recovery: cleanup failed: {e} (restored state preserved; \
             refusing to start a new transaction until cleanup succeeds)"
        )
    })
}

/// Handles a `CommittedPendingCleanup` recovery outcome: re-reads, re-parses
/// and semantically validates the committed live user-rule file, then asks the
/// port to clean up the committed transaction's material.
///
/// A committed transaction is **never rolled back** here. Any validation or
/// cleanup failure keeps the marker/backup/candidate evidence and returns an
/// error, so a new transaction is refused until the committed state is resolved
/// (manual repair required — fail-closed).
fn recover_committed_pending(
    rule_file: &Path,
    tx: &PreparedRuleTransaction,
    port: &dyn UserRuleTransactionPort,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let bytes = port.read_current(rule_file)?.ok_or_else(|| {
        format!(
            "committed recovery: live rule file {} is missing despite a COMMITTED marker \
             (evidence preserved; manual repair required)",
            rule_file.display()
        )
    })?;
    let parsed = parse_rule_file(Some(&bytes), rule_file).map_err(|e| {
        format!(
            "{e} (COMMITTED live file must re-parse before cleanup; evidence preserved; \
             manual repair required)"
        )
    })?;
    for rule in &parsed.rules {
        if !matches!(rule.source, RuleSource::User | RuleSource::UserProtected) {
            return Err(format!(
                "committed recovery: committed live file holds a non-user rule source `{}` \
                 (evidence preserved; manual repair required)",
                rule.source
            ));
        }
    }
    compile_rule_docs(&parsed.rules, lookup).map_err(|e| {
        format!(
            "committed recovery: committed live file failed semantic validation: {e} \
             (evidence preserved; refusing to start a new transaction)"
        )
    })?;
    // Committed rules are valid: best-effort cleanup. A cleanup failure keeps
    // the material and refuses the new transaction (no rollback of committed
    // rules).
    port.cleanup_committed(tx).map_err(|e| {
        format!(
            "committed recovery: cleanup of the committed transaction failed: {e} \
             (committed rules are preserved; refusing to start a new transaction until \
             the residue is cleaned)"
        )
    })
}

/// The ordered, guarded body of the transaction (after the guard is held and
/// any interrupted transaction recovered). Records the prepared transaction in
/// `prepared` so the caller can roll back exactly when a recovery source
/// exists. Every failure after `prepare` triggers a rollback by the caller.
#[allow(clippy::too_many_arguments)]
fn exec_transaction(
    rule_file: &Path,
    confirmations: &[AiRuleConfirmation],
    verifier: &dyn CandidateRuleVerifier,
    port: &dyn UserRuleTransactionPort,
    prepared: &mut Option<PreparedRuleTransaction>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<RuleId>, String> {
    // Parse the complete cumulative user file (ignore/protect/user-analysis/
    // manual + previous ai rules). A missing/empty file is an empty rule file.
    let original = port.read_current(rule_file)?;
    let original_file = parse_rule_file(original.as_deref(), rule_file)?;

    // 1. Build every draft (Unknown and dangerous anchors fail here).
    let drafts: Vec<RuleDoc> = confirmations
        .iter()
        .map(|confirmation| {
            LocalRuleDraftBuilder.build_with_lookup(confirmation, lookup).map_err(|e| {
                format!(
                    "refusing AI rule batch: draft for item {} failed: {e}",
                    confirmation.item.id
                )
            })
        })
        .collect::<Result<_, String>>()?;

    // 2. Duplicate anchors inside the batch are rejected.
    for i in 0..drafts.len() {
        for j in (i + 1)..drafts.len() {
            if same_anchor(
                drafts[i].match_spec.exact.as_deref(),
                drafts[j].match_spec.exact.as_deref(),
                lookup,
            ) {
                return Err(format!(
                    "refusing AI rule batch: confirmations {} and {} anchor the same \
                     normalized path `{}`",
                    confirmations[i].item.id,
                    confirmations[j].item.id,
                    drafts[i].match_spec.exact.as_deref().unwrap_or_default()
                ));
            }
        }
    }

    // 3. Detect conflicts against the existing rules (never literal equality).
    //    Only an existing same-anchor rule with provenance.origin = ai-advisor
    //    may be replaced idempotently; everything else is a hard conflict.
    let mut replace_indexes: BTreeSet<usize> = BTreeSet::new();
    for (index, existing) in original_file.rules.iter().enumerate() {
        let Some(existing_anchor) = existing.match_spec.exact.as_deref() else {
            continue; // only exact anchors are comparable anchors
        };
        for draft in &drafts {
            let draft_anchor = draft.match_spec.exact.as_deref();
            if !same_anchor(Some(existing_anchor), draft_anchor, lookup) {
                continue;
            }
            let is_ai = existing
                .provenance
                .as_ref()
                .is_some_and(|p| p.origin == RuleOrigin::AiAdvisor);
            if is_ai {
                replace_indexes.insert(index);
            } else {
                return Err(format!(
                    "AI rule conflict: existing rule `{}` hand-authored on the same anchor \
                     cannot be overwritten by an AI rule",
                    existing.id
                ));
            }
        }
    }

    // 4. Merge: keep every untouched rule, drop replaced ai rules, append the
    //    new drafts. The cumulative file therefore preserves all other rules.
    let mut candidate_rules: Vec<RuleDoc> = original_file
        .rules
        .iter()
        .enumerate()
        .filter(|(i, _)| !replace_indexes.contains(i))
        .map(|(_, rule)| rule.clone())
        .collect();
    candidate_rules.extend(drafts.iter().cloned());

    // 5. The candidate file only ever holds user-owned rules with unique ids.
    for rule in &candidate_rules {
        if !matches!(rule.source, RuleSource::User | RuleSource::UserProtected) {
            return Err(format!(
                "refusing AI rule batch: candidate holds a non-user rule source `{}`",
                rule.source
            ));
        }
    }
    let mut seen_ids = HashMap::new();
    for rule in &candidate_rules {
        if seen_ids.insert(rule.id.as_str(), ()).is_some() {
            return Err(format!(
                "refusing AI rule batch: duplicate rule id `{}` in the candidate file",
                rule.id
            ));
        }
    }

    // 6. Validate + compile the full candidate file, then let the verifier
    //    re-match every confirmation against it — before any disk write.
    compile_rule_docs(&candidate_rules, lookup)
        .map_err(|e| format!("refusing AI rule batch: candidate file rejected: {e}"))?;
    let candidate_file = RuleFile {
        rules: candidate_rules.clone(),
    };
    verifier
        .verify(&candidate_file, confirmations)
        .map_err(|e| format!("refusing AI rule batch: candidate rematch failed: {e}"))?;

    // 7. prepare: stage the candidate, a durable backup of the pre-replace
    //    live file (when it exists) and the ACTIVE/PRESENT|ABSENT marker. The
    //    live file is not replaced here. After this point a rollback source
    //    exists on disk.
    let text = serde_yaml_ng::to_string(&candidate_file)
        .map_err(|e| format!("serialise candidate user rule file: {e}"))?;
    let tx = port.prepare(rule_file, text.as_bytes())?;
    *prepared = Some(tx);

    // 8. Replace the live file only. The marker/backup/candidate are kept so a
    //    crash or a verification failure can still roll back by marker state.
    port.replace(prepared.as_ref().expect("prepared just set"))?;

    // 9. Reload once more after replacement, re-validate and re-verify. A
    //    mismatch here means the replace did not land as staged → rollback.
    let reloaded_bytes = port
        .read_current(rule_file)?
        .ok_or_else(|| "reload after replace: live user rule file is missing".to_string())?;
    let reloaded = parse_rule_file(Some(&reloaded_bytes), rule_file)?;
    for rule in &reloaded.rules {
        if !matches!(rule.source, RuleSource::User | RuleSource::UserProtected) {
            return Err(format!(
                "reload after replace: candidate holds a non-user rule source `{}`",
                rule.source
            ));
        }
    }
    compile_rule_docs(&reloaded.rules, lookup)
        .map_err(|e| format!("reload after replace: candidate file rejected: {e}"))?;
    verifier
        .verify(&reloaded, confirmations)
        .map_err(|e| format!("reload after replace: rematch failed: {e}"))?;

    // 10. Resolve the numeric rule ids of the freshly written drafts in
    //     confirmation order (from the verified reloaded file).
    let set = compile_rule_docs(&reloaded.rules, lookup)?;
    let id_to_numeric: HashMap<&str, RuleId> = set
        .rules
        .iter()
        .map(|rule| (rule.id.as_str(), rule.numeric_id))
        .collect();
    let ids = drafts
        .iter()
        .map(|draft| {
            id_to_numeric
                .get(draft.id.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "reload after replace: generated rule `{}` missing from the compiled set",
                        draft.id
                    )
                })
        })
        .collect::<Result<Vec<RuleId>, String>>()?;

    // 11. Sole commit point: atomically transition the marker to COMMITTED via
    //     an in-place rewrite (no no-marker window). An Err means the commit
    //     point was not reached → roll back.
    let tx = prepared.as_ref().expect("prepared just set");
    port.commit_verified(tx)?;

    // 12. Best-effort cleanup of the committed material. A cleanup failure
    //     must NEVER turn the already-committed transaction into an uncommitted
    //     report — the batch is committed here regardless. Leftover material is
    //     resolved by the next transaction's committed recovery.
    let _ = port.cleanup_committed(tx);
    Ok(ids)
}

/// Parses raw user-rule file bytes into a [`RuleFile`], treating a missing or
/// empty file as an empty rule file. A corrupt file is a hard error (never
/// silently overwrite user rules).
fn parse_rule_file(bytes: Option<&[u8]>, path: &Path) -> Result<RuleFile, String> {
    let Some(bytes) = bytes else {
        return Ok(RuleFile { rules: vec![] });
    };
    let text = String::from_utf8_lossy(bytes);
    if text.trim().is_empty() {
        return Ok(RuleFile { rules: vec![] });
    }
    serde_yaml_ng::from_str(&text)
        .map_err(|e| format!("parse {} (refusing to overwrite user rules): {e}", path.display()))
}

/// Normalised-exact-anchor equality: expand env references on both sides and
/// compare through `canonical::normalized_eq_path`. `None` anchors never
/// compare equal. Never a literal string comparison.
fn same_anchor(
    a: Option<&str>,
    b: Option<&str>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    match (expand_env(a, lookup), expand_env(b, lookup)) {
        (Ok(x), Ok(y)) => canonical::normalized_eq_path(x, y),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{
        AiProfileId, AiSuggestionId, AiZone, RuleOrigin, RuleProvenance,
        UserRuleTransactionGuard,
    };
    use crate::domain::source::SourceKind;
    use crate::rules::draft::LocalRuleDraftBuilder;
    use crate::rules::schema::MatchSpec;
    use crate::{ResidueCategory, RiskLevel, ScanItemId};
    use std::collections::VecDeque;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, SystemTime};

    const PROFILE_CANON: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";

    // ---- marker tokens (single persistent transaction marker) ---------------

    const MARKER_ACTIVE_PRESENT: &str = "ACTIVE/PRESENT";
    const MARKER_ACTIVE_ABSENT: &str = "ACTIVE/ABSENT";
    const MARKER_ROLLED_BACK_PRESENT: &str = "ROLLED_BACK/PRESENT";
    const MARKER_ROLLED_BACK_ABSENT: &str = "ROLLED_BACK/ABSENT";
    const MARKER_COMMITTED: &str = "COMMITTED";

    /// Parsed state of the persistent transaction marker.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum MarkerState {
        ActivePresent,
        ActiveAbsent,
        RolledBackPresent,
        RolledBackAbsent,
        Committed,
    }

    /// Fault-injection steps used to simulate crashes / IO failures inside the
    /// fake port (each is a one-shot; the first matching step is consumed).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Step {
        /// Writing the `COMMITTED` marker fails (marker left as `ACTIVE`) → the
        /// commit point is not reached and Core must roll back.
        CommitMarker,
        /// A crash mid-way through the in-place `COMMITTED` marker rewrite
        /// leaves a truncated/invalid marker (never an absent marker) → the
        /// port/Core must fail closed and preserve evidence, never treating the
        /// staged candidate as committed.
        CommitMarkerCorrupt,
        /// `cleanup_committed` fails while removing the backup → the committed
        /// rules survive and cleanup residue is reported (never an uncommitted
        /// report).
        CommitCleanupBackup,
        /// `recover_if_needed` with no marker fails to remove a stray
        /// candidate/backup → refuse the new transaction.
        RecoverStrayCleanup,
        /// `prepare` on an `ABSENT` first creation fails to remove a stale
        /// backup → refuse prepare.
        PrepareAbsentStaleBackup,
        /// `rollback` fails after writing the restore temp but before
        /// publishing it over live → recovery material must survive.
        RollbackRestoreBeforeRename,
        /// ROLLED_BACK cleanup fails before removing the candidate.
        RolledBackCleanupCandidate,
        /// ROLLED_BACK cleanup fails before removing the marker.
        RolledBackCleanupMarker,
        /// ROLLED_BACK/PRESENT cleanup fails before removing the backup.
        RolledBackCleanupBackup,
        /// Publishing the ROLLED_BACK marker fails after restoration. The
        /// existing ACTIVE marker and recovery material must remain intact.
        RolledBackMarker,
        /// After a same-process PRESENT rollback publishes its durable marker,
        /// replace the restored live bytes with a semantically invalid rule
        /// file. Core must validate before cleanup rather than trusting the
        /// port's byte restoration alone.
        RollbackPostPublishInvalidLive,
        /// After a same-process ABSENT rollback publishes its durable marker,
        /// recreate the live file. Core must reject the reappeared live state
        /// before cleanup.
        RollbackPostPublishReappearedLive,
    }

    // ---- fixtures -----------------------------------------------------------

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dr-ai-tx-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn provenance(suggestion: &str, final_risk: RiskLevel) -> RuleProvenance {
        RuleProvenance {
            origin: RuleOrigin::AiAdvisor,
            profile_id: AiProfileId::parse(PROFILE_CANON).unwrap(),
            scan_generation: 7,
            suggestion_id: AiSuggestionId::parse(suggestion).unwrap(),
            user_final_risk: final_risk,
            created_at_epoch_secs: 1_760_000_000,
        }
    }

    fn item(path: &str, id: u64) -> crate::ScanItem {
        crate::ScanItem {
            id: ScanItemId::from_raw(id),
            path: PathBuf::from(path),
            display_name: "reviewed residue".to_string(),
            product: None,
            category: ResidueCategory::DeveloperCache,
            risk: RiskLevel::Unknown,
            source: SourceKind::UnknownProvider,
            logical_size: 1024,
            file_count: 1,
            last_modified: Some(SystemTime::now() - Duration::from_secs(3600)),
            explanation: "trusted local discovery evidence".to_string(),
            cleanup_action: crate::CleanupAction::None,
            evidence: Vec::new(),
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    fn confirmation(path: &str, item_id: u64, suggestion: &str, risk: RiskLevel) -> AiRuleConfirmation {
        AiRuleConfirmation {
            item: item(path, item_id),
            final_risk: risk,
            final_category: ResidueCategory::DeveloperCache,
            provenance: provenance(suggestion, risk),
        }
    }

    fn exact_rule(id: &str, anchor: &str, source: RuleSource, risk: RiskLevel, ai: bool) -> RuleDoc {
        RuleDoc {
            id: id.to_string(),
            description: "existing test rule".to_string(),
            product: None,
            category: ResidueCategory::DeveloperCache,
            risk,
            source,
            match_spec: MatchSpec {
                exact: Some(anchor.to_string()),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: Vec::new(),
            exclude: Vec::new(),
            provenance: if ai {
                Some(provenance(
                    "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                    risk,
                ))
            } else {
                None
            },
        }
    }

    /// Writes a user-dispositions.yaml holding `rules`.
    fn seed_file(user_dir: &Path, rules: &[RuleDoc]) {
        let file = dispositions_path(user_dir);
        let text = serde_yaml_ng::to_string(&RuleFile {
            rules: rules.to_vec(),
        })
        .unwrap();
        std::fs::write(&file, text).unwrap();
    }

    /// Serialises a plain (hand-authored-looking) user rule file that parses
    /// cleanly — a "valid candidate" live file for crash-state tests.
    fn valid_live_yaml(user_dir: &Path) -> Vec<u8> {
        seed_file(
            user_dir,
            &[exact_rule(
                "user-detection/seed",
                r"C:\Users\me\.tool\seed",
                RuleSource::User,
                RiskLevel::Review,
                false,
            )],
        );
        std::fs::read(dispositions_path(user_dir)).unwrap()
    }

    fn invalid_semantic_live_yaml() -> Vec<u8> {
        let invalid_doc = RuleDoc {
            id: "user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f".to_string(),
            description: "invalid same-process rollback state".to_string(),
            product: None,
            category: ResidueCategory::DeveloperCache,
            risk: RiskLevel::Safe,
            source: RuleSource::User,
            match_spec: MatchSpec {
                exact: Some(r"C:\Users\me\.tool\cache".to_string()),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: Vec::new(),
            exclude: Vec::new(),
            provenance: Some(provenance(
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Protected,
            )),
        };
        serde_yaml_ng::to_string(&RuleFile {
            rules: vec![invalid_doc],
        })
        .unwrap()
        .into_bytes()
    }

    fn read_bytes(user_dir: &Path) -> Option<Vec<u8>> {
        let file = dispositions_path(user_dir);
        std::fs::read(&file).ok()
    }

    fn rule_count(user_dir: &Path) -> usize {
        read_bytes(user_dir)
            .map(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                if text.trim().is_empty() {
                    0
                } else {
                    serde_yaml_ng::from_str::<RuleFile>(&text).unwrap().rules.len()
                }
            })
            .unwrap_or(0)
    }

    // ---- low-level crash-state helpers ---------------------------------------

    /// Atomically publishes `bytes` to `target` through a sibling temp file
    /// (`<target>.<label>.tmp`) + flush + rename. Used by the fake port so
    /// every state transition is a full-file write.
    fn publish_file(target: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
        let mut tmp = target.as_os_str().to_owned();
        tmp.push(format!(".{label}.tmp"));
        let tmp = PathBuf::from(tmp);
        {
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| format!("create {}: {e}", tmp.display()))?;
            f.write_all(bytes)
                .map_err(|e| format!("write {}: {e}", tmp.display()))?;
            f.sync_all().map_err(|e| format!("flush {}: {e}", tmp.display()))?;
        }
        // Windows `rename` is not guaranteed to overwrite an existing target,
        // so drop the target first (the temp is fully written+flushed first).
        let _ = std::fs::remove_file(target);
        std::fs::rename(&tmp, target).map_err(|e| format!("publish {}: {e}", target.display()))?;
        Ok(())
    }

    fn publish_copy(target: &Path, src: &Path, label: &str) -> Result<(), String> {
        let bytes = std::fs::read(src)
            .map_err(|e| format!("read {}: {e}", src.display()))?;
        publish_file(target, &bytes, label)
    }

    /// Rewrites the transaction marker **in place** (ReplaceFileW-style): opens
    /// the marker path, writes the whole fixed token and flushes. The marker
    /// file is never removed, so a state transition has **no no-marker window**.
    /// An interruption after truncation leaves a shorter/invalid token
    /// (fail-closed on the next read), never an absent marker.
    fn publish_marker(marker_path: &Path, token: &str, label: &str) -> Result<(), String> {
        let mut f = std::fs::File::create(marker_path)
            .map_err(|e| format!("open marker {label} {}: {e}", marker_path.display()))?;
        f.write_all(token.as_bytes())
            .map_err(|e| format!("write marker {label} {}: {e}", marker_path.display()))?;
        f.sync_all()
            .map_err(|e| format!("flush marker {label} {}: {e}", marker_path.display()))?;
        Ok(())
    }

    /// Removes one recovery-material file, treating a missing file as success
    /// (cleanup is idempotent). Any other error is surfaced so the caller keeps
    /// the remaining material and can report/retry.
    fn remove_cleanup(path: &Path, what: &str) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{what} remove {}: {e}", path.display())),
        }
    }

    fn read_marker(marker_path: &Path) -> Result<Option<MarkerState>, String> {
        match std::fs::read_to_string(marker_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read marker {}: {e}", marker_path.display())),
            Ok(text) => match text.trim() {
                MARKER_ACTIVE_PRESENT => Ok(Some(MarkerState::ActivePresent)),
                MARKER_ACTIVE_ABSENT => Ok(Some(MarkerState::ActiveAbsent)),
                MARKER_ROLLED_BACK_PRESENT => Ok(Some(MarkerState::RolledBackPresent)),
                MARKER_ROLLED_BACK_ABSENT => Ok(Some(MarkerState::RolledBackAbsent)),
                MARKER_COMMITTED => Ok(Some(MarkerState::Committed)),
                other => Err(format!(
                    "invalid transaction marker `{other}` at {} (refusing to guess)",
                    marker_path.display()
                )),
            },
        }
    }

    fn write_marker(marker_path: &Path, state: MarkerState) {
        let token = match state {
            MarkerState::ActivePresent => MARKER_ACTIVE_PRESENT,
            MarkerState::ActiveAbsent => MARKER_ACTIVE_ABSENT,
            MarkerState::RolledBackPresent => MARKER_ROLLED_BACK_PRESENT,
            MarkerState::RolledBackAbsent => MARKER_ROLLED_BACK_ABSENT,
            MarkerState::Committed => MARKER_COMMITTED,
        };
        std::fs::write(marker_path, token).unwrap();
    }

    // ---- FakeTransactionPort -------------------------------------------------

    /// In-memory + real-filesystem fake of the Task 4 Windows port, driving the
    /// single persistent marker state machine. Owns a temp-dir-relative rule
    /// file with the canonical sibling state files:
    ///
    /// ```text
    /// live       user-dispositions.yaml
    /// marker     user-dispositions.yaml.txn
    /// backup     user-dispositions.yaml.txn.original
    /// candidate  user-dispositions.yaml.txn.candidate
    /// ```
    ///
    /// The exclusive lock is a process-local compare-exchange, so a second
    /// concurrent transaction is refused (there is no real OS file lock in Core
    /// tests). Fault injection simulates crashes / IO failures at each state
    /// transition so the recovery table is exercised without real process
    /// death.
    struct FakeTransactionPort {
        lock_held: Arc<AtomicBool>,
        acquire_count: Arc<AtomicUsize>,
        rollback_count: Arc<AtomicUsize>,
        recover_count: Arc<AtomicUsize>,
        faults: Mutex<VecDeque<Step>>,
    }

    impl FakeTransactionPort {
        fn new() -> Self {
            Self {
                lock_held: Arc::new(AtomicBool::new(false)),
                acquire_count: Arc::new(AtomicUsize::new(0)),
                rollback_count: Arc::new(AtomicUsize::new(0)),
                recover_count: Arc::new(AtomicUsize::new(0)),
                faults: Mutex::new(VecDeque::new()),
            }
        }

        fn with_faults(faults: &[Step]) -> Self {
            let port = Self::new();
            port.faults.lock().unwrap().extend(faults.iter().copied());
            port
        }

        fn rollback_count(&self) -> usize {
            self.rollback_count.load(Ordering::SeqCst)
        }

        fn marker_path(rule_file: &Path) -> PathBuf {
            let mut s = rule_file.as_os_str().to_owned();
            s.push(".txn");
            PathBuf::from(s)
        }

        fn backup_path(rule_file: &Path) -> PathBuf {
            let mut s = rule_file.as_os_str().to_owned();
            s.push(".txn.original");
            PathBuf::from(s)
        }

        fn candidate_path(rule_file: &Path) -> PathBuf {
            let mut s = rule_file.as_os_str().to_owned();
            s.push(".txn.candidate");
            PathBuf::from(s)
        }

        /// One-shot fault gate: consumes the first queued fault that matches
        /// `step` and returns an error, simulating a crash/IO failure there.
        fn trip(&self, step: Step) -> Result<(), String> {
            let mut queue = self.faults.lock().unwrap();
            if queue.front() == Some(&step) {
                queue.pop_front();
                return Err(format!("injected fault at {step:?}"));
            }
            Ok(())
        }

        /// One-shot fault probe: consumes the first queued fault that matches
        /// `step` without failing, so the caller can apply the fault's on-disk
        /// side effect (e.g. truncating the marker) before returning an error.
        fn hit(&self, step: Step) -> bool {
            let mut queue = self.faults.lock().unwrap();
            if queue.front() == Some(&step) {
                queue.pop_front();
                true
            } else {
                false
            }
        }

        /// Publishes one of the durable ROLLED_BACK marker states without
        /// performing any cleanup.  The marker remains the classification for
        /// the recovery material even when this publication is fault-injected.
        fn publish_rolled_back_marker(
            &self,
            marker_path: &Path,
            state: MarkerState,
            label: &str,
        ) -> Result<(), String> {
            self.trip(Step::RolledBackMarker)?;
            let token = match state {
                MarkerState::RolledBackPresent => MARKER_ROLLED_BACK_PRESENT,
                MarkerState::RolledBackAbsent => MARKER_ROLLED_BACK_ABSENT,
                other => {
                    return Err(format!(
                        "cannot publish non-ROLLED_BACK marker state {other:?}"
                    ))
                }
            };
            publish_marker(marker_path, token, label)
        }
    }

    struct FakeGuard {
        flag: Arc<AtomicBool>,
    }

    impl Drop for FakeGuard {
        fn drop(&mut self) {
            self.flag.store(false, Ordering::SeqCst);
        }
    }

    impl UserRuleTransactionGuard for FakeGuard {}

    impl UserRuleTransactionPort for FakeTransactionPort {
        fn acquire_exclusive(
            &self,
            _rule_file: &Path,
        ) -> Result<Box<dyn UserRuleTransactionGuard>, String> {
            self.acquire_count.fetch_add(1, Ordering::SeqCst);
            if self
                .lock_held
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err(
                    "user rule transaction lock is already held by another transaction".to_string(),
                );
            }
            Ok(Box::new(FakeGuard {
                flag: self.lock_held.clone(),
            }))
        }

        fn recover_if_needed(
            &self,
            rule_file: &Path,
        ) -> Result<TransactionRecovery, String> {
            self.recover_count.fetch_add(1, Ordering::SeqCst);
            let marker = Self::marker_path(rule_file);
            let backup = Self::backup_path(rule_file);
            let candidate = Self::candidate_path(rule_file);

            let Some(state) = read_marker(&marker)? else {
                // No marker: the live file is authoritative. Only stray
                // candidate/backup files are removed (live is never changed);
                // a failed stray cleanup refuses the new transaction.
                if self.trip(Step::RecoverStrayCleanup).is_err() {
                    return Err(
                        "recover: stray candidate/backup cleanup failed (no marker) — \
                         refusing to start a new transaction (evidence preserved)"
                            .to_string(),
                    );
                }
                remove_cleanup(&candidate, "recover stray candidate")?;
                remove_cleanup(&backup, "recover stray backup")?;
                return Ok(TransactionRecovery::Clean);
            };

            match state {
                MarkerState::ActivePresent => {
                    // ACTIVE/PRESENT must always restore the backup, regardless
                    // of whether the replaced live YAML parses.
                    if !backup.exists() {
                        return Err(
                            "recover: ACTIVE/PRESENT marker without a backup — manual repair \
                             required (evidence preserved)"
                                .to_string(),
                        );
                    }
                    let bytes = std::fs::read(&backup)
                        .map_err(|e| format!("recover read backup {}: {e}", backup.display()))?;
                    publish_file(rule_file, &bytes, "recover-present")?;
                    // Keep marker/backup until the restored bytes are verified.
                    let live = std::fs::read(rule_file)
                        .map_err(|e| format!("recover verify live {}: {e}", rule_file.display()))?;
                    if live != bytes {
                        return Err(format!(
                            "recover: restored live file does not match the backup at {} \
                             (evidence preserved)",
                            rule_file.display()
                        ));
                    }
                    let tx = PreparedRuleTransaction {
                        live_path: rule_file.to_path_buf(),
                        marker_path: marker,
                        backup_path: backup,
                        candidate_path: candidate,
                    };
                    // The original bytes are restored, but all recovery
                    // material remains classified until Core validates the
                    // live state and cleanup succeeds.
                    self.publish_rolled_back_marker(
                        &tx.marker_path,
                        MarkerState::RolledBackPresent,
                        "recover-rolled-back-present",
                    )?;
                    Ok(TransactionRecovery::RolledBackPendingCleanup(tx))
                }
                MarkerState::ActiveAbsent => {
                    // ACTIVE/ABSENT must always remove the live file, restoring
                    // the original absence (first-creation transaction).
                    if backup.exists() {
                        return Err(
                            "recover: ACTIVE/ABSENT marker with a backup — state contradiction \
                             (evidence preserved)"
                                .to_string(),
                        );
                    }
                    match std::fs::remove_file(rule_file) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            return Err(format!(
                                "recover: cannot remove live {}: {e} (evidence preserved)",
                                rule_file.display()
                            ))
                        }
                    }
                    if rule_file.exists() {
                        return Err(format!(
                            "recover: live file still present after ACTIVE/ABSENT removal at {} \
                             (evidence preserved)",
                            rule_file.display()
                        ));
                    }
                    let tx = PreparedRuleTransaction {
                        live_path: rule_file.to_path_buf(),
                        marker_path: marker,
                        backup_path: backup,
                        candidate_path: candidate,
                    };
                    self.publish_rolled_back_marker(
                        &tx.marker_path,
                        MarkerState::RolledBackAbsent,
                        "recover-rolled-back-absent",
                    )?;
                    Ok(TransactionRecovery::RolledBackPendingCleanup(tx))
                }
                MarkerState::RolledBackPresent => Ok(TransactionRecovery::RolledBackPendingCleanup(
                    PreparedRuleTransaction {
                        live_path: rule_file.to_path_buf(),
                        marker_path: marker,
                        backup_path: backup,
                        candidate_path: candidate,
                    },
                )),
                MarkerState::RolledBackAbsent => {
                    if backup.exists() {
                        return Err(
                            "recover: ROLLED_BACK/ABSENT marker with a backup — state contradiction \
                             (evidence preserved)"
                                .to_string(),
                        );
                    }
                    Ok(TransactionRecovery::RolledBackPendingCleanup(
                        PreparedRuleTransaction {
                            live_path: rule_file.to_path_buf(),
                            marker_path: marker,
                            backup_path: backup,
                            candidate_path: candidate,
                        },
                    ))
                }
                MarkerState::Committed => {
                    // COMMITTED is never rolled back and never cleaned up here:
                    // the port keeps the candidate/backup/marker and reports the
                    // pending cleanup, so the Core caller can first re-read,
                    // re-parse and semantically validate the committed live file.
                    // A missing live file is a hard refusal (evidence preserved).
                    if !rule_file.exists() {
                        return Err(format!(
                            "recover: COMMITTED marker but the live file is missing at {} — \
                             manual repair required (evidence preserved)",
                            rule_file.display()
                        ));
                    }
                    Ok(TransactionRecovery::CommittedPendingCleanup(
                        PreparedRuleTransaction {
                            live_path: rule_file.to_path_buf(),
                            marker_path: marker,
                            backup_path: backup,
                            candidate_path: candidate,
                        },
                    ))
                }
            }
        }

        fn read_current(&self, rule_file: &Path) -> Result<Option<Vec<u8>>, String> {
            match std::fs::read(rule_file) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("read {}: {e}", rule_file.display())),
            }
        }

        fn prepare(
            &self,
            rule_file: &Path,
            bytes: &[u8],
        ) -> Result<PreparedRuleTransaction, String> {
            let tx = PreparedRuleTransaction {
                live_path: rule_file.to_path_buf(),
                marker_path: Self::marker_path(rule_file),
                backup_path: Self::backup_path(rule_file),
                candidate_path: Self::candidate_path(rule_file),
            };
            let live_existed = rule_file.exists();

            // 1. Fully write + flush + atomically publish the candidate.
            publish_file(&tx.candidate_path, bytes, "candidate")?;

            // 2. When the original live file exists, keep a durable backup.
            //    Otherwise (first creation / ABSENT) any stale backup must be
            //    removed successfully BEFORE the ACTIVE/ABSENT marker may be
            //    published; a failed removal refuses prepare.
            if live_existed {
                publish_copy(&tx.backup_path, rule_file, "backup")?;
            } else {
                if self.trip(Step::PrepareAbsentStaleBackup).is_err() {
                    return Err(
                        "prepare: ACTIVE/ABSENT could not remove a stale backup — refusing \
                         to prepare (evidence preserved)"
                            .to_string(),
                    );
                }
                remove_cleanup(&tx.backup_path, "prepare stale backup")?;
            }

            // 3. Finally publish the single persistent ACTIVE marker via an
            //    in-place rewrite. The live file is NOT replaced here.
            let token = if live_existed {
                MARKER_ACTIVE_PRESENT
            } else {
                MARKER_ACTIVE_ABSENT
            };
            publish_marker(&tx.marker_path, token, "active")?;
            Ok(tx)
        }

        fn replace(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
            let bytes = std::fs::read(&tx.candidate_path)
                .map_err(|e| format!("replace read {}: {e}", tx.candidate_path.display()))?;
            // Replace the live file only; marker/backup/candidate are kept so a
            // crash or verification failure can still roll back by marker.
            publish_file(&tx.live_path, &bytes, "replace-live")
        }

        fn rollback(&self, tx: &PreparedRuleTransaction) -> Result<PreparedRuleTransaction, String> {
            self.rollback_count.fetch_add(1, Ordering::SeqCst);
            let Some(state) = read_marker(&tx.marker_path)? else {
                return Err(format!(
                    "rollback: no transaction marker at {} (cannot determine ACTIVE state)",
                    tx.marker_path.display()
                ));
            };
            match state {
                MarkerState::ActivePresent => {
                    // Always restore the backup, regardless of how the replaced
                    // live YAML parses.
                    if !tx.backup_path.exists() {
                        return Err(format!(
                            "rollback: ACTIVE/PRESENT without a backup at {} \
                             (evidence preserved)",
                            tx.backup_path.display()
                        ));
                    }
                    let bytes = std::fs::read(&tx.backup_path).map_err(|e| {
                        format!("rollback read backup {}: {e}", tx.backup_path.display())
                    })?;
                    // Write the restore temp + flush, then publish over live.
                    let mut rt = tx.live_path.as_os_str().to_owned();
                    rt.push(".txn.restore");
                    let rt = PathBuf::from(rt);
                    {
                        let mut f = std::fs::File::create(&rt)
                            .map_err(|e| format!("rollback create {}: {e}", rt.display()))?;
                        f.write_all(&bytes)
                            .map_err(|e| format!("rollback write {}: {e}", rt.display()))?;
                        f.sync_all()
                            .map_err(|e| format!("rollback flush {}: {e}", rt.display()))?;
                    }
                    // Simulate a crash between writing the restore temp and
                    // publishing it → backup/marker must survive.
                    self.trip(Step::RollbackRestoreBeforeRename)?;
                    let _ = std::fs::remove_file(&tx.live_path);
                    std::fs::rename(&rt, &tx.live_path).map_err(|e| {
                        format!("rollback restore {}: {e}", tx.live_path.display())
                    })?;
                    // Verify the restored bytes before cleaning the material.
                    let live = std::fs::read(&tx.live_path).map_err(|e| {
                        format!("rollback verify live {}: {e}", tx.live_path.display())
                    })?;
                    if live != bytes {
                        return Err(format!(
                            "rollback: restored live does not match backup at {} \
                             (evidence preserved)",
                            tx.live_path.display()
                        ));
                    }
                    // The original bytes are restored and verified. Publish a
                    // durable classified state before deleting any recovery
                    // material; cleanup is a separate idempotent operation.
                    self.publish_rolled_back_marker(
                        &tx.marker_path,
                        MarkerState::RolledBackPresent,
                        "rollback-rolled-back-present",
                    )?;
                    if self.hit(Step::RollbackPostPublishInvalidLive) {
                        std::fs::write(&tx.live_path, invalid_semantic_live_yaml())
                            .map_err(|e| {
                                format!(
                                    "rollback test mutation {}: {e}",
                                    tx.live_path.display()
                                )
                            })?;
                    }
                    Ok(tx.clone())
                }
                MarkerState::ActiveAbsent => {
                    // First-creation rollback: remove the live file, restoring
                    // the original absence. A backup here is a contradiction.
                    if tx.backup_path.exists() {
                        return Err(format!(
                            "rollback: ACTIVE/ABSENT with a backup at {} — state contradiction \
                             (evidence preserved)",
                            tx.backup_path.display()
                        ));
                    }
                    match std::fs::remove_file(&tx.live_path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            return Err(format!(
                                "rollback: cannot remove live {}: {e} (evidence preserved)",
                                tx.live_path.display()
                            ))
                        }
                    }
                    if tx.live_path.exists() {
                        return Err(format!(
                            "rollback: live still present after ACTIVE/ABSENT removal at {} \
                             (evidence preserved)",
                            tx.live_path.display()
                        ));
                    }
                    self.publish_rolled_back_marker(
                        &tx.marker_path,
                        MarkerState::RolledBackAbsent,
                        "rollback-rolled-back-absent",
                    )?;
                    if self.hit(Step::RollbackPostPublishReappearedLive) {
                        std::fs::write(&tx.live_path, b"rules: []").map_err(|e| {
                            format!(
                                "rollback test reappearance {}: {e}",
                                tx.live_path.display()
                            )
                        })?;
                    }
                    Ok(tx.clone())
                }
                MarkerState::RolledBackPresent | MarkerState::RolledBackAbsent => Err(format!(
                    "rollback: refusing to roll back an already ROLLED_BACK transaction at {}",
                    tx.marker_path.display()
                )),
                MarkerState::Committed => Err(format!(
                    "rollback: refusing to roll back a COMMITTED transaction at {}",
                    tx.marker_path.display()
                )),
            }
        }

        fn cleanup_rolled_back(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
            let Some(state) = read_marker(&tx.marker_path)? else {
                return Err(format!(
                    "rolled-back cleanup: marker is missing at {} (evidence preserved)",
                    tx.marker_path.display()
                ));
            };
            match state {
                MarkerState::RolledBackPresent => {
                    // Keep the marker classified until the backup is gone.
                    // In particular, a backup deletion failure must not leave
                    // an unclassified sibling that a later no-marker recovery
                    // would treat as stray.
                    self.trip(Step::RolledBackCleanupCandidate)?;
                    remove_cleanup(&tx.candidate_path, "rolled-back candidate")?;
                    self.trip(Step::RolledBackCleanupBackup)?;
                    remove_cleanup(&tx.backup_path, "rolled-back backup")?;
                    self.trip(Step::RolledBackCleanupMarker)?;
                    remove_cleanup(&tx.marker_path, "rolled-back marker")?;
                    Ok(())
                }
                MarkerState::RolledBackAbsent => {
                    if tx.backup_path.exists() {
                        return Err(format!(
                            "rolled-back cleanup: ROLLED_BACK/ABSENT has a backup at {} \
                             (state contradiction; evidence preserved)",
                            tx.backup_path.display()
                        ));
                    }
                    self.trip(Step::RolledBackCleanupCandidate)?;
                    remove_cleanup(&tx.candidate_path, "rolled-back candidate")?;
                    self.trip(Step::RolledBackCleanupMarker)?;
                    remove_cleanup(&tx.marker_path, "rolled-back marker")?;
                    Ok(())
                }
                other => Err(format!(
                    "rolled-back cleanup: unexpected marker state {other:?} at {} \
                     (evidence preserved)",
                    tx.marker_path.display()
                )),
            }
        }

        fn commit_verified(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
            // Sole commit point: in-place rewrite of the marker to COMMITTED.
            // An error here means the commit point was not reached (Core must
            // roll back by the still-ACTIVE marker).
            if self.trip(Step::CommitMarker).is_err() {
                return Err(
                    "injected fault at CommitMarker: commit point not reached".to_string(),
                );
            }
            if self.hit(Step::CommitMarkerCorrupt) {
                // Simulate a crash mid-rewrite: the marker file still exists
                // but holds a truncated (invalid) token — never an absent
                // marker. The next read fails closed.
                std::fs::write(&tx.marker_path, "COMMIT")
                    .map_err(|e| format!("corrupt marker write: {e}"))?;
                return Err(
                    "injected fault at CommitMarkerCorrupt: marker left truncated \
                     (invalid, not absent)"
                        .to_string(),
                );
            }
            publish_marker(&tx.marker_path, MARKER_COMMITTED, "commit")
        }

        fn cleanup_committed(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
            // Best-effort removal of a committed transaction's material.
            // Order: candidate → backup → marker, so the COMMITTED marker is the
            // last to go — an interruption keeps the marker and the next
            // recovery re-reports pending cleanup. A cleanup failure never
            // implies the rules are uncommitted.
            remove_cleanup(&tx.candidate_path, "committed candidate")?;
            if self.trip(Step::CommitCleanupBackup).is_err() {
                return Err(
                    "injected fault at CommitCleanupBackup: backup retained \
                     (committed rules preserved)"
                        .to_string(),
                );
            }
            remove_cleanup(&tx.backup_path, "committed backup")?;
            remove_cleanup(&tx.marker_path, "committed marker")?;
            Ok(())
        }
    }

    // ---- verifiers used by tests --------------------------------------------

    /// Always rejects: exercises candidate pre-write verification rollback
    /// (before `prepare`, so nothing is written).
    struct RejectSecondDraft;

    impl CandidateRuleVerifier for RejectSecondDraft {
        fn verify(
            &self,
            _candidate: &RuleFile,
            _confirmations: &[AiRuleConfirmation],
        ) -> Result<(), String> {
            Err("verifier rejects this candidate batch".to_string())
        }
    }

    /// Stateful verifier that accepts the first (pre-write) call but rejects
    /// the second (post-replace reload) call, forcing a real rollback.
    struct RejectOnReload {
        calls: Mutex<usize>,
    }

    impl RejectOnReload {
        fn new() -> Self {
            Self {
                calls: Mutex::new(0),
            }
        }
    }

    impl CandidateRuleVerifier for RejectOnReload {
        fn verify(
            &self,
            _candidate: &RuleFile,
            _confirmations: &[AiRuleConfirmation],
        ) -> Result<(), String> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls >= 2 {
                Err("reload rematch failed (simulated replace corruption)".to_string())
            } else {
                Ok(())
            }
        }
    }

    // ---- success / conflict / rollback behaviour ----------------------------

    #[test]
    fn exact_successful_commit_writes_one_rule_per_confirmation() {
        let dir = tmp_dir("ok");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let port = FakeTransactionPort::new();
        let confs = [
            confirmation(
                r"C:\Users\me\.tool\cache-a",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            ),
            confirmation(
                r"C:\Users\me\.tool\cache-b",
                2,
                "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
                RiskLevel::Review,
            ),
        ];
        let ids = commit_ai_rule_batch(&user, &confs, &CandidateRematchVerifier, &port).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids[0].raw() >= 1 && ids[1].raw() >= 1);
        let text = read_bytes(&user).unwrap();
        assert!(String::from_utf8_lossy(&text).contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"));
        assert!(String::from_utf8_lossy(&text).contains("user-ai/7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d"));
        assert_eq!(rule_count(&user), 2);
        // No transaction state remains after a successful commit.
        let live = dispositions_path(&user);
        assert!(!FakeTransactionPort::marker_path(&live).exists());
        assert!(!FakeTransactionPort::backup_path(&live).exists());
        assert!(!FakeTransactionPort::candidate_path(&live).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manual_rule_same_anchor_is_an_ai_rule_conflict() {
        let dir = tmp_dir("conf-manual");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        seed_file(
            &user,
            &[exact_rule(
                "user-analysis/1",
                r"C:\Users\me\.tool\cache",
                RuleSource::User,
                RiskLevel::Safe,
                false,
            )],
        );
        let port = FakeTransactionPort::new();
        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::RegenerableDownload,
        );
        let before = read_bytes(&user);
        let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port)
            .unwrap_err();
        assert!(err.contains("AI rule conflict"), "{err}");
        assert_eq!(read_bytes(&user), before, "file must be unchanged on conflict");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heuristic_ignore_and_protected_same_anchor_are_conflicts() {
        for (tag, rule) in [
            (
                "heuristic",
                exact_rule(
                    "user-analysis/1",
                    r"C:\Users\me\.tool\cache",
                    RuleSource::User,
                    RiskLevel::Safe,
                    false,
                ),
            ),
            (
                "ignore",
                exact_rule(
                    "user-ignore/1",
                    r"C:\Users\me\.tool\cache",
                    RuleSource::User,
                    RiskLevel::Protected,
                    false,
                ),
            ),
            (
                "protected",
                exact_rule(
                    "user-protected/1",
                    r"C:\Users\me\.tool\cache",
                    RuleSource::UserProtected,
                    RiskLevel::Protected,
                    false,
                ),
            ),
            (
                "manual",
                exact_rule(
                    "user-detection/manual",
                    r"C:\Users\me\.tool\cache",
                    RuleSource::User,
                    RiskLevel::Review,
                    false,
                ),
            ),
        ] {
            let dir = tmp_dir(&format!("conf-{tag}"));
            let user = dir.join("rules").join("user");
            std::fs::create_dir_all(&user).unwrap();
            seed_file(&user, &[rule]);
            let port = FakeTransactionPort::new();
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let before = read_bytes(&user);
            let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port)
                .unwrap_err();
            assert!(err.contains("AI rule conflict"), "[{tag}] {err}");
            assert_eq!(read_bytes(&user), before, "[{tag}] file unchanged expected");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn ai_provenance_same_anchor_is_idempotently_replaced() {
        let dir = tmp_dir("conf-ai");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let anchor = r"C:\Users\me\.tool\cache";
        // A previous AI rule (same anchor, ai provenance, old risk).
        seed_file(
            &user,
            &[exact_rule(
                "user-ai/00000000-0000-4000-8000-000000000001",
                anchor,
                RuleSource::User,
                RiskLevel::Review,
                true,
            )],
        );
        let port = FakeTransactionPort::new();
        let conf = confirmation(
            anchor,
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let ids = commit_ai_rule_batch(
            &user,
            std::slice::from_ref(&conf),
            &CandidateRematchVerifier,
            &port,
        )
        .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(rule_count(&user), 1, "exactly one rule remains at the anchor");
        let text = String::from_utf8_lossy(&read_bytes(&user).unwrap()).into_owned();
        assert!(text.contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"));
        assert!(!text.contains("user-ai/00000000-0000-4000-8000-000000000001"));

        // Idempotent re-submit of the very same confirmation keeps one rule.
        let ids2 = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap();
        assert_eq!(ids2.len(), 1);
        assert_eq!(rule_count(&user), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn protected_mapping_commit_resolves_to_user_protected() {
        let dir = tmp_dir("protected-map");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let port = FakeTransactionPort::new();
        let anchor = r"C:\Users\me\.tool\config-store";
        let conf = confirmation(
            anchor,
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Protected,
        );
        let ids = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap();
        assert_eq!(ids.len(), 1);
        let text = String::from_utf8_lossy(&read_bytes(&user).unwrap()).into_owned();
        assert!(text.contains("source: user-protected"), "{text}");
        assert!(text.contains("risk: protected"), "{text}");
        // The rematch verifier resolves the protected rule with risk protected.
        let set = compile_rule_docs(
            &serde_yaml_ng::from_str::<RuleFile>(&text).unwrap().rules,
            &|n| std::env::var(n).ok(),
        )
        .unwrap();
        let hit = resolve(std::path::Path::new(anchor), &set.rules).unwrap();
        assert_eq!(hit.risk, RiskLevel::Protected);
        assert_eq!(hit.source, RuleSource::UserProtected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preexisting_unrelated_rules_are_preserved_across_the_batch() {
        let dir = tmp_dir("preserve");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        seed_file(
            &user,
            &[
                exact_rule(
                    "user-ignore/1",
                    r"C:\Users\me\.hidden",
                    RuleSource::User,
                    RiskLevel::Protected,
                    false,
                ),
                exact_rule(
                    "user-protected/1",
                    r"C:\Users\me\.creds",
                    RuleSource::UserProtected,
                    RiskLevel::Protected,
                    false,
                ),
                exact_rule(
                    "user-analysis/1",
                    r"C:\Users\me\.tool\other",
                    RuleSource::User,
                    RiskLevel::Review,
                    false,
                ),
            ],
        );
        let port = FakeTransactionPort::new();
        let conf = confirmation(
            r"C:\Users\me\.tool\new-cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap();
        assert_eq!(rule_count(&user), 4, "all pre-existing + 1 new rule survive");
        let text = String::from_utf8_lossy(&read_bytes(&user).unwrap()).into_owned();
        for id in [
            "user-ignore/1",
            "user-protected/1",
            "user-analysis/1",
            "user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
        ] {
            assert!(text.contains(id), "missing {id}:\n{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_batch_and_duplicate_anchors_are_rejected() {
        let dir = tmp_dir("empty");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let port = FakeTransactionPort::new();
        let err = commit_ai_rule_batch(&user, &[], &CandidateRematchVerifier, &port).unwrap_err();
        assert!(err.contains("empty"), "{err}");

        let anchor = r"C:\Users\me\.tool\cache";
        let confs = [
            confirmation(
                anchor,
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            ),
            confirmation(
                anchor, // same normalized path, different suggestion id
                2,
                "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
                RiskLevel::Review,
            ),
        ];
        let err = commit_ai_rule_batch(&user, &confs, &CandidateRematchVerifier, &port).unwrap_err();
        assert!(err.contains("same normalized path"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn candidate_verification_failure_rolls_back_before_any_write() {
        let dir = tmp_dir("verify-fail");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        // One pre-existing manual rule that must survive untouched.
        seed_file(
            &user,
            &[exact_rule(
                "user-protected/1",
                r"C:\Users\me\.creds",
                RuleSource::UserProtected,
                RiskLevel::Protected,
                false,
            )],
        );
        let before = read_bytes(&user);
        let port = FakeTransactionPort::new();
        let confs = [
            confirmation(
                r"C:\Users\me\.tool\cache-a",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            ),
            confirmation(
                r"C:\Users\me\.tool\cache-b",
                2,
                "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
                RiskLevel::Safe,
            ),
        ];
        let err = commit_ai_rule_batch(&user, &confs, &RejectSecondDraft, &port).unwrap_err();
        assert!(err.contains("verifier rejects"), "{err}");
        assert_eq!(read_bytes(&user), before, "nothing may be written");
        // No transaction state was created (failure precedes prepare).
        let live = dispositions_path(&user);
        assert!(!FakeTransactionPort::marker_path(&live).exists());
        assert!(!FakeTransactionPort::backup_path(&live).exists());
        assert!(!FakeTransactionPort::candidate_path(&live).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_replace_reload_failure_restores_the_original_file() {
        let dir = tmp_dir("reload-fail");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        seed_file(
            &user,
            &[exact_rule(
                "user-protected/1",
                r"C:\Users\me\.creds",
                RuleSource::UserProtected,
                RiskLevel::Protected,
                false,
            )],
        );
        let before = read_bytes(&user);
        let port = FakeTransactionPort::new();
        let verifier = RejectOnReload::new();
        let confs = [
            confirmation(
                r"C:\Users\me\.tool\cache-a",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            ),
            confirmation(
                r"C:\Users\me\.tool\cache-b",
                2,
                "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
                RiskLevel::Safe,
            ),
        ];
        let err = commit_ai_rule_batch(&user, &confs, &verifier, &port).unwrap_err();
        assert!(err.contains("reload"), "{err}");
        assert_eq!(
            read_bytes(&user),
            before,
            "original file must be restored after a failed post-replace rematch"
        );
        assert!(port.rollback_count() >= 1, "port.rollback must have been called");
        // Rollback cleaned up the ACTIVE transaction state.
        let live = dispositions_path(&user);
        assert!(!FakeTransactionPort::marker_path(&live).exists());
        assert!(!FakeTransactionPort::backup_path(&live).exists());
        assert!(!FakeTransactionPort::candidate_path(&live).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A same-process rollback must pass through the same Core validation gate
    /// as startup recovery. If the restored PRESENT live file is no longer
    /// semantically valid before cleanup, all ROLLED_BACK evidence remains.
    #[test]
    fn same_process_rollback_present_invalid_live_blocks_cleanup() {
        let dir = tmp_dir("same-process-rollback-invalid-present");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        std::fs::write(&rule_file, &original).unwrap();

        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let port = FakeTransactionPort::with_faults(&[Step::RollbackPostPublishInvalidLive]);
        let err = commit_ai_rule_batch(&user, &[conf], &RejectOnReload::new(), &port)
            .unwrap_err();

        assert!(err.contains("rolled-back recovery"), "{err}");
        assert_eq!(
            read_marker(&FakeTransactionPort::marker_path(&rule_file)).unwrap(),
            Some(MarkerState::RolledBackPresent),
            "same-process rollback must retain the classified marker"
        );
        assert!(FakeTransactionPort::candidate_path(&rule_file).exists());
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());
        assert_ne!(
            std::fs::read(&rule_file).unwrap(),
            original,
            "the injected invalid live state must remain visible to the gate"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A same-process ABSENT rollback must also pass through the Core gate. A
    /// live file reappearing after the port verified absence is contradictory,
    /// so cleanup must not remove the marker/candidate evidence.
    #[test]
    fn same_process_rollback_absent_reappeared_live_blocks_cleanup() {
        let dir = tmp_dir("same-process-rollback-invalid-absent");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        assert!(!rule_file.exists());

        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let port = FakeTransactionPort::with_faults(&[Step::RollbackPostPublishReappearedLive]);
        let err = commit_ai_rule_batch(&user, &[conf], &RejectOnReload::new(), &port)
            .unwrap_err();

        assert!(err.contains("rolled-back recovery"), "{err}");
        assert_eq!(
            read_marker(&FakeTransactionPort::marker_path(&rule_file)).unwrap(),
            Some(MarkerState::RolledBackAbsent),
            "same-process rollback must retain the classified marker"
        );
        assert!(FakeTransactionPort::candidate_path(&rule_file).exists());
        assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
        assert!(rule_file.exists(), "the injected reappearance must remain visible");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_transaction_holding_the_lock_is_refused() {
        let dir = tmp_dir("concurrent");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let port = Arc::new(FakeTransactionPort::new());
        let rule_file = dispositions_path(&user);
        let barrier = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            let port_inner = port.clone();
            let rule_file_inner = rule_file.clone();
            let barrier_inner = barrier.clone();
            scope.spawn(move || {
                // Holder: takes the exclusive guard first and signals the main
                // thread, then keeps holding it until told to release.
                let guard: Box<dyn UserRuleTransactionGuard> =
                    port_inner.acquire_exclusive(&rule_file_inner).unwrap();
                barrier_inner.wait();
                // Give the main thread ample time to attempt its commit.
                std::thread::sleep(Duration::from_millis(50));
                drop(guard);
            });

            barrier.wait();
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &*port)
                .unwrap_err();
            assert!(err.contains("already held"), "{err}");
            assert_eq!(read_bytes(&user), None, "no partial write may occur");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rematch_verifier_rejects_when_a_higher_priority_rule_wins() {
        // A UserProtected *glob* rule outranks a User exact rule on the same
        // path (source priority), so the pure rematch verifier must reject the
        // candidate — the AI rule would not actually govern the item.
        let anchor = r"C:\Users\me\.tool\cache";
        let candidate = RuleFile {
            rules: vec![
                exact_rule(
                    "user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                    anchor,
                    RuleSource::User,
                    RiskLevel::Safe,
                    true,
                ),
                // Higher-priority protected glob covering the same path.
                RuleDoc {
                    id: "user-protected/9".to_string(),
                    description: "protected glob".to_string(),
                    product: None,
                    category: ResidueCategory::Unknown,
                    risk: RiskLevel::Protected,
                    source: RuleSource::UserProtected,
                    match_spec: MatchSpec {
                        exact: None,
                        glob: Some(r"C:\Users\me\.tool\**".to_string()),
                        parent_marker: None,
                        exists: false,
                    },
                    include: Vec::new(),
                    exclude: Vec::new(),
                    provenance: None,
                },
            ],
        };
        let conf = confirmation(
            anchor,
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let err = CandidateRematchVerifier
            .verify(&candidate, &[conf])
            .unwrap_err();
        assert!(err.contains("expected the generated rule"), "{err}");
    }

    #[test]
    fn draft_builder_round_trips_through_commit_without_lookup() {
        // LocalRuleDraftBuilder::build (real env) must produce the same doc as
        // the fake-env path for env-free absolute anchors.
        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let real = LocalRuleDraftBuilder.build(&conf).unwrap();
        let fake = LocalRuleDraftBuilder
            .build_with_lookup(&conf, &|name| std::env::var(name).ok())
            .unwrap();
        assert_eq!(real, fake);
    }

    // AiZone is referenced by the ai module tests; keep a small compile guard
    // so dropping the import above does not silently rot the fixture surface.
    #[allow(dead_code)]
    fn _zone_token() -> AiZone {
        AiZone::Other
    }

    // ---- fix round 2: single persistent marker state machine ----------------

    /// ACTIVE/PRESENT recovery must restore the backup even when the replaced
    /// live file parses cleanly as a candidate — parseability is never used to
    /// guess that the transaction committed.
    #[test]
    fn active_present_restores_backup_even_when_live_candidate_parses() {
        let dir = tmp_dir("fr2-active-present");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);

        // Original pre-transaction content.
        let original = valid_live_yaml(&user);
        // The crash state: the replace landed (live = a *valid, parseable*
        // candidate) and the marker is ACTIVE/PRESENT with a backup.
        let candidate_live = {
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let mut rules = serde_yaml_ng::from_str::<RuleFile>(&String::from_utf8_lossy(&original))
                .unwrap()
                .rules;
            rules.push(LocalRuleDraftBuilder.build(&conf).unwrap());
            serde_yaml_ng::to_string(&RuleFile { rules }).unwrap()
        };
        std::fs::write(&rule_file, &candidate_live).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActivePresent,
        );

        // A fresh process runs crash recovery.  ACTIVE recovery restores the
        // original bytes, publishes ROLLED_BACK, and returns pending cleanup;
        // it must not perform stateless cleanup before Core validation.
        let port = FakeTransactionPort::new();
        let recovery = port.recover_if_needed(&rule_file).unwrap();
        let TransactionRecovery::RolledBackPendingCleanup(pending) = recovery else {
            panic!("expected RolledBackPendingCleanup, got {recovery:?}");
        };
        assert_eq!(
            std::fs::read(&rule_file).unwrap(),
            original,
            "ACTIVE/PRESENT must roll back to the backup even though the \
             candidate live YAML parses"
        );
        assert_eq!(
            read_marker(&pending.marker_path).unwrap(),
            Some(MarkerState::RolledBackPresent)
        );
        assert!(pending.backup_path.exists());
        port.cleanup_rolled_back(&pending).unwrap();
        assert!(!pending.marker_path.exists());
        assert!(!pending.backup_path.exists());
        assert!(!pending.candidate_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ACTIVE/ABSENT recovery (first-creation crash) must remove the live file
    /// — even a fully parseable candidate — restoring the original absence.
    #[test]
    fn active_absent_first_creation_crash_removes_live() {
        let dir = tmp_dir("fr2-active-absent");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);

        // First-creation crash state: no original live file ever existed, the
        // candidate replace landed, marker = ACTIVE/ABSENT, no backup.
        let candidate_live = {
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            serde_yaml_ng::to_string(&RuleFile {
                rules: vec![LocalRuleDraftBuilder.build(&conf).unwrap()],
            })
            .unwrap()
        };
        std::fs::write(&rule_file, candidate_live.as_bytes()).unwrap();
        assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActiveAbsent,
        );

        let port = FakeTransactionPort::new();
        let recovery = port.recover_if_needed(&rule_file).unwrap();
        let TransactionRecovery::RolledBackPendingCleanup(pending) = recovery else {
            panic!("expected RolledBackPendingCleanup, got {recovery:?}");
        };
        assert!(
            !rule_file.exists(),
            "ACTIVE/ABSENT must remove the live file (original absence restored)"
        );
        assert_eq!(
            read_marker(&pending.marker_path).unwrap(),
            Some(MarkerState::RolledBackAbsent)
        );
        port.cleanup_rolled_back(&pending).unwrap();
        assert!(!pending.marker_path.exists());
        assert!(!pending.candidate_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rollback interrupted between writing the restore temp and publishing
    /// it over live must keep the recovery material (backup + marker + ACTIVE
    /// state) so a later recovery can finish the job.
    #[test]
    fn interrupted_rollback_keeps_recovery_material() {
        let dir = tmp_dir("fr2-interrupted-rollback");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        let candidate_live = {
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let mut rules = serde_yaml_ng::from_str::<RuleFile>(&String::from_utf8_lossy(&original))
                .unwrap()
                .rules;
            rules.push(LocalRuleDraftBuilder.build(&conf).unwrap());
            serde_yaml_ng::to_string(&RuleFile { rules }).unwrap()
        };
        std::fs::write(&rule_file, candidate_live.as_bytes()).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActivePresent,
        );
        let tx = PreparedRuleTransaction {
            live_path: rule_file.clone(),
            marker_path: FakeTransactionPort::marker_path(&rule_file),
            backup_path: FakeTransactionPort::backup_path(&rule_file),
            candidate_path: FakeTransactionPort::candidate_path(&rule_file),
        };

        // Rollback fails while publishing the restore (before live is
        // replaced). All recovery material must survive.
        let faulted = FakeTransactionPort::with_faults(&[Step::RollbackRestoreBeforeRename]);
        assert!(faulted.rollback(&tx).is_err(), "rollback must fail at the fault");
        assert!(
            FakeTransactionPort::backup_path(&rule_file).exists(),
            "durable backup must survive an interrupted rollback"
        );
        assert_eq!(
            read_marker(&tx.marker_path).unwrap(),
            Some(MarkerState::ActivePresent),
            "ACTIVE marker must survive an interrupted rollback"
        );

        // A fresh port finishes the rollback from the surviving material.
        let fresh = FakeTransactionPort::new();
        let pending = fresh.rollback(&tx).unwrap();
        assert_eq!(
            std::fs::read(&rule_file).unwrap(),
            original,
            "a later rollback must restore the original from the surviving backup"
        );
        assert_eq!(
            read_marker(&pending.marker_path).unwrap(),
            Some(MarkerState::RolledBackPresent)
        );
        fresh.cleanup_rolled_back(&pending).unwrap();
        assert!(!pending.marker_path.exists());
        assert!(!pending.backup_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rolled-back cleanup failure at ANY removal step must keep enough
    /// marker/backup evidence to retry — the live file is already restored, but
    /// the material is never destroyed out of order (candidate → backup →
    /// marker; the marker remains until the backup is gone).
    #[test]
    fn rollback_cleanup_failure_at_any_step_keeps_marker_and_backup() {
        // candidate-removal failure: everything survives.
        // marker-removal failure: candidate is gone; marker + backup survive.
        // backup-removal failure: candidate is gone; marker + backup survive.
        for (step, candidate_exists, marker_exists, backup_exists) in [
            (Step::RolledBackCleanupCandidate, true, true, true),
            (Step::RolledBackCleanupMarker, false, true, false),
            (Step::RolledBackCleanupBackup, false, true, true),
        ] {
            let dir = tmp_dir(&format!("fr2-rollback-cleanup-{step:?}"));
            let user = dir.join("rules").join("user");
            std::fs::create_dir_all(&user).unwrap();
            let rule_file = dispositions_path(&user);
            let original = valid_live_yaml(&user);
            let candidate_live = {
                let conf = confirmation(
                    r"C:\Users\me\.tool\cache",
                    1,
                    "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                    RiskLevel::Safe,
                );
                let mut rules =
                    serde_yaml_ng::from_str::<RuleFile>(&String::from_utf8_lossy(&original))
                        .unwrap()
                        .rules;
                rules.push(LocalRuleDraftBuilder.build(&conf).unwrap());
                serde_yaml_ng::to_string(&RuleFile { rules }).unwrap()
            };
            std::fs::write(&rule_file, candidate_live.as_bytes()).unwrap();
            std::fs::write(FakeTransactionPort::candidate_path(&rule_file), candidate_live.as_bytes())
                .unwrap();
            std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
            write_marker(
                &FakeTransactionPort::marker_path(&rule_file),
                MarkerState::ActivePresent,
            );
            let tx = PreparedRuleTransaction {
                live_path: rule_file.clone(),
                marker_path: FakeTransactionPort::marker_path(&rule_file),
                backup_path: FakeTransactionPort::backup_path(&rule_file),
                candidate_path: FakeTransactionPort::candidate_path(&rule_file),
            };

            let faulted = FakeTransactionPort::with_faults(&[step]);
            let pending = faulted.rollback(&tx).unwrap();
            let err = faulted.cleanup_rolled_back(&pending).unwrap_err();
            assert!(err.contains("injected fault"), "[{step:?}] {err}");
            // Live was restored, and the remaining evidence is exactly what a
            // later rollback/recover still needs.
            assert_eq!(std::fs::read(&rule_file).unwrap(), original, "[{step:?}] live restored");
            assert_eq!(
                FakeTransactionPort::candidate_path(&rule_file).exists(),
                candidate_exists,
                "[{step:?}] candidate presence"
            );
            assert_eq!(
                FakeTransactionPort::marker_path(&rule_file).exists(),
                marker_exists,
                "[{step:?}] marker presence"
            );
            assert_eq!(
                FakeTransactionPort::backup_path(&rule_file).exists(),
                backup_exists,
                "[{step:?}] backup presence"
            );

            // A fresh port finishes the classified cleanup from the surviving
            // evidence; it must not attempt a second rollback.
            let fresh = FakeTransactionPort::new();
            fresh.cleanup_rolled_back(&tx).unwrap();
            assert_eq!(std::fs::read(&rule_file).unwrap(), original);
            assert!(!FakeTransactionPort::marker_path(&rule_file).exists());
            assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// `commit_verified` must reach the COMMITTED commit point before cleanup;
    /// a cleanup failure after that point still reports committed success.
    #[test]
    fn committed_cleanup_failure_still_reports_success() {
        let dir = tmp_dir("fr2-committed-cleanup");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        std::fs::write(&rule_file, &original).unwrap();

        // A commit whose best-effort backup cleanup fails must still succeed.
        let port = FakeTransactionPort::with_faults(&[Step::CommitCleanupBackup]);
        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let ids = commit_ai_rule_batch(&user, std::slice::from_ref(&conf), &CandidateRematchVerifier, &port)
            .unwrap();
        assert_eq!(ids.len(), 1);
        let text = String::from_utf8_lossy(&read_bytes(&user).unwrap()).into_owned();
        assert!(
            text.contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"),
            "commit must be reported successful even though backup cleanup failed"
        );
        // The commit point was reached, but the backup-cleanup fault fired
        // inside cleanup_committed: the candidate is gone, while the durable
        // backup and the COMMITTED marker remain (they are the pending-cleanup
        // evidence).
        assert!(!FakeTransactionPort::candidate_path(&rule_file).exists());
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());
        assert_eq!(
            read_marker(&FakeTransactionPort::marker_path(&rule_file)).unwrap(),
            Some(MarkerState::Committed),
            "COMMITTED marker must survive a cleanup failure"
        );
        // A fresh recover reports the committed transaction as pending cleanup
        // WITHOUT deleting the live file or the material.
        let fresh = FakeTransactionPort::new();
        let recovery = fresh.recover_if_needed(&rule_file).unwrap();
        let TransactionRecovery::CommittedPendingCleanup(pending) = recovery else {
            panic!("expected CommittedPendingCleanup, got {recovery:?}");
        };
        assert!(
            String::from_utf8_lossy(&read_bytes(&user).unwrap())
                .contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"),
            "COMMITTED recovery must preserve the new live file"
        );
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());
        assert!(FakeTransactionPort::marker_path(&rule_file).exists());
        // The Core caller re-validates the committed live file, then cleans up.
        fresh.cleanup_committed(&pending).unwrap();
        assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
        assert!(!FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(
            String::from_utf8_lossy(&read_bytes(&user).unwrap())
                .contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"),
            "cleaning the committed residue must never remove the committed rules"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// If the commit point itself fails (marker cannot become COMMITTED), the
    /// Core transaction rolls back by the ACTIVE marker.
    #[test]
    fn commit_marker_failure_rolls_back_by_marker_state() {
        let dir = tmp_dir("fr2-commit-marker-fail");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        seed_file(
            &user,
            &[exact_rule(
                "user-protected/1",
                r"C:\Users\me\.creds",
                RuleSource::UserProtected,
                RiskLevel::Protected,
                false,
            )],
        );
        let before = read_bytes(&user);
        let port = FakeTransactionPort::with_faults(&[Step::CommitMarker]);
        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap_err();
        assert!(err.contains("injected fault"), "{err}");
        assert_eq!(
            read_bytes(&user),
            before,
            "a failed commit point must roll back to the original"
        );
        assert!(port.rollback_count() >= 1);
        let live = dispositions_path(&user);
        assert!(!FakeTransactionPort::marker_path(&live).exists());
        assert!(!FakeTransactionPort::backup_path(&live).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Invalid or contradictory markers must fail closed and preserve all
    /// evidence, never guessing a recovery.
    #[test]
    fn recover_rejects_invalid_and_contradictory_markers() {
        // (a) ACTIVE/PRESENT without a backup → refuse, preserve evidence.
        let dir = tmp_dir("fr2-bad-a");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        std::fs::write(&rule_file, &original).unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActivePresent,
        );
        let err = FakeTransactionPort::new()
            .recover_if_needed(&rule_file)
            .unwrap_err();
        assert!(err.contains("manual repair"), "(a) {err}");
        assert_eq!(std::fs::read(&rule_file).unwrap(), original, "(a) live preserved");
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "(a) marker preserved");
        let _ = std::fs::remove_dir_all(&dir);

        // (b) ACTIVE/ABSENT with a backup → contradiction → refuse.
        let dir = tmp_dir("fr2-bad-b");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        std::fs::write(&rule_file, valid_live_yaml(&user)).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"stale-backup").unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActiveAbsent,
        );
        let err = FakeTransactionPort::new()
            .recover_if_needed(&rule_file)
            .unwrap_err();
        assert!(err.contains("contradiction"), "(b) {err}");
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "(b) marker preserved");
        assert!(FakeTransactionPort::backup_path(&rule_file).exists(), "(b) backup preserved");
        let _ = std::fs::remove_dir_all(&dir);

        // (c) Invalid marker text → refuse, preserve evidence.
        let dir = tmp_dir("fr2-bad-c");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        std::fs::write(&rule_file, valid_live_yaml(&user)).unwrap();
        std::fs::write(FakeTransactionPort::marker_path(&rule_file), b"HALF/COMMITTED").unwrap();
        let err = FakeTransactionPort::new()
            .recover_if_needed(&rule_file)
            .unwrap_err();
        assert!(err.contains("invalid transaction marker"), "(c) {err}");
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "(c) marker preserved");
        let _ = std::fs::remove_dir_all(&dir);

        // (d) COMMITTED with a missing live file → refuse, preserve evidence.
        let dir = tmp_dir("fr2-bad-d");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::Committed,
        );
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"backup").unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
        let err = FakeTransactionPort::new()
            .recover_if_needed(&rule_file)
            .unwrap_err();
        assert!(err.contains("manual repair"), "(d) {err}");
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "(d) marker preserved");
        assert!(FakeTransactionPort::backup_path(&rule_file).exists(), "(d) backup preserved");
        let _ = std::fs::remove_dir_all(&dir);

        // (e) COMMITTED with an unparseable live file → recover reports the
        // transaction as pending cleanup WITHOUT deleting material or live; the
        // semantic/parse refusal happens in the Core caller (see the fix-round-3
        // committed-recovery tests below), never by guessing here.
        let dir = tmp_dir("fr2-bad-e");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        std::fs::write(&rule_file, b"rules: [{ broken").unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"backup").unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::Committed,
        );
        let recovery = FakeTransactionPort::new()
            .recover_if_needed(&rule_file)
            .unwrap();
        assert!(
            matches!(
                &recovery,
                TransactionRecovery::CommittedPendingCleanup(_)
            ),
            "(e) expected CommittedPendingCleanup, got {recovery:?}"
        );
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "(e) marker preserved");
        assert!(FakeTransactionPort::backup_path(&rule_file).exists(), "(e) backup preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No marker → the live file is authoritative; only stray candidate/backup
    /// files are removed, live is never touched.
    #[test]
    fn no_marker_leaves_live_authoritative_and_removes_stray_state() {
        let dir = tmp_dir("fr2-no-marker");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        std::fs::write(&rule_file, &original).unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"stray").unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"stray").unwrap();

        let port = FakeTransactionPort::new();
        port.recover_if_needed(&rule_file).unwrap();
        assert_eq!(
            std::fs::read(&rule_file).unwrap(),
            original,
            "no marker: live must be unchanged"
        );
        assert!(!FakeTransactionPort::candidate_path(&rule_file).exists());
        assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crash after `replace` (marker still ACTIVE) is recovered before the
    /// next commit, which then appends on top of the restored original.
    #[test]
    fn active_state_is_recovered_before_the_next_commit() {
        let dir = tmp_dir("fr2-recover-then-commit");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        // Crash after replace: live = parseable candidate, ACTIVE/PRESENT.
        let candidate_live = {
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let mut rules = serde_yaml_ng::from_str::<RuleFile>(&String::from_utf8_lossy(&original))
                .unwrap()
                .rules;
            rules.push(LocalRuleDraftBuilder.build(&conf).unwrap());
            serde_yaml_ng::to_string(&RuleFile { rules }).unwrap()
        };
        std::fs::write(&rule_file, candidate_live.as_bytes()).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActivePresent,
        );

        // A new commit first recovers (restoring the original), then appends.
        let port = FakeTransactionPort::new();
        let conf = confirmation(
            r"C:\Users\me\.tool\other-cache",
            2,
                "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
            RiskLevel::Review,
        );
        commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(&rule_file).unwrap()).into_owned();
        assert!(text.contains("user-detection/seed"), "original rule preserved: {text}");
        assert!(
            text.contains("user-ai/7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d"),
            "new rule committed on top of recovered original: {text}"
        );
        assert!(!FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- fix round 3: committed-pending recovery + marker publish safety ----

    /// A first-creation (ABSENT) prepare that cannot remove a stale backup must
    /// refuse before publishing the ACTIVE marker, leaving the live file absent.
    #[test]
    fn prepare_absent_refuses_when_stale_backup_removal_fails() {
        let dir = tmp_dir("fr3-prepare-absent");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        assert!(!rule_file.exists(), "first creation expected");

        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let text = serde_yaml_ng::to_string(&RuleFile {
            rules: vec![LocalRuleDraftBuilder.build(&conf).unwrap()],
        })
        .unwrap();
        // A stale backup must not be present for an ABSENT first creation.
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"stale").unwrap();

        let port = FakeTransactionPort::with_faults(&[Step::PrepareAbsentStaleBackup]);
        let err = port.prepare(&rule_file, text.as_bytes()).unwrap_err();
        assert!(err.contains("stale backup"), "{err}");
        // The ACTIVE/ABSENT marker was never published and live stays absent.
        assert!(
            !FakeTransactionPort::marker_path(&rule_file).exists(),
            "no marker may be published when the stale backup could not be removed"
        );
        assert!(!rule_file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crash mid-way through the in-place ACTIVE→COMMITTED marker rewrite
    /// leaves a truncated/invalid marker — NEVER an absent marker — so the
    /// staged candidate is never mistaken for committed and recovery fails
    /// closed with all evidence preserved.
    #[test]
    fn commit_marker_publish_crash_leaves_invalid_marker_never_no_marker() {
        let dir = tmp_dir("fr3-marker-corrupt");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        seed_file(
            &user,
            &[exact_rule(
                "user-protected/1",
                r"C:\Users\me\.creds",
                RuleSource::UserProtected,
                RiskLevel::Protected,
                false,
            )],
        );
        let rule_file = dispositions_path(&user);
        let port = FakeTransactionPort::with_faults(&[Step::CommitMarkerCorrupt]);
        let conf = confirmation(
            r"C:\Users\me\.tool\cache",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap_err();

        // The marker file still exists (never a no-marker window), now holding
        // a truncated/invalid token.
        assert!(
            FakeTransactionPort::marker_path(&rule_file).exists(),
            "a crash mid-publish must never leave an absent marker"
        );
        assert!(
            err.contains("invalid transaction marker") || err.contains("rollback"),
            "commit must fail closed via the invalid marker, got: {err}"
        );
        // Recovery refuses (invalid marker) and preserves all evidence — the
        // staged candidate was never treated as committed.
        let fresh = FakeTransactionPort::new();
        assert!(
            fresh.recover_if_needed(&rule_file).is_err(),
            "invalid marker recovery must refuse"
        );
        assert!(FakeTransactionPort::backup_path(&rule_file).exists(), "backup preserved");
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "marker preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A COMMITTED live file that parses but fails semantic validation must be
    /// refused by the Core caller: the recovery material is kept and a new
    /// transaction is refused (never cleaned, never rolled back, never silently
    /// accepted).
    #[test]
    fn committed_recovery_rejects_semantically_invalid_live_and_keeps_material() {
        let dir = tmp_dir("fr3-committed-invalid");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);

        // A committed live file whose YAML parses but whose rule fails the
        // validator (provenance.user_final_risk disagrees with rule risk).
        let invalid_doc = RuleDoc {
            id: "user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f".to_string(),
            description: "invalid committed rule".to_string(),
            product: None,
            category: ResidueCategory::DeveloperCache,
            risk: RiskLevel::Safe,
            source: RuleSource::User,
            match_spec: MatchSpec {
                exact: Some(r"C:\Users\me\.tool\cache".to_string()),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: Vec::new(),
            exclude: Vec::new(),
            provenance: Some(provenance(
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Protected, // contradicts rule risk Safe
            )),
        };
        let live = serde_yaml_ng::to_string(&RuleFile {
            rules: vec![invalid_doc],
        })
        .unwrap();
        std::fs::write(&rule_file, live.as_bytes()).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"backup").unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::Committed,
        );

        // The next commit first runs committed recovery; the semantic
        // validation fails → the whole commit is refused.
        let port = FakeTransactionPort::new();
        let conf = confirmation(
            r"C:\Users\me\.tool\other-cache",
            1,
            "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
            RiskLevel::Safe,
        );
        let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port).unwrap_err();
        assert!(
            err.contains("committed recovery") && err.contains("user_final_risk"),
            "semantic validation failure must refuse the commit, got: {err}"
        );
        // All evidence is preserved; nothing was cleaned or rolled back.
        assert_eq!(
            std::fs::read(&rule_file).unwrap(),
            live.as_bytes(),
            "live file must be preserved untouched"
        );
        assert!(FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());
        assert!(FakeTransactionPort::candidate_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the committed-residue cleanup keeps failing, the already-committed
    /// rules survive and a new transaction is refused.
    #[test]
    fn committed_cleanup_failure_preserves_rules_and_refuses_new_tx() {
        let dir = tmp_dir("fr3-committed-refuse");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        seed_file(
            &user,
            &[exact_rule(
                "user-protected/1",
                r"C:\Users\me\.creds",
                RuleSource::UserProtected,
                RiskLevel::Protected,
                false,
            )],
        );

        // First commit: the commit point is reached but the best-effort
        // cleanup_committed keeps failing → still reported committed.
        let port_a = FakeTransactionPort::with_faults(&[Step::CommitCleanupBackup]);
        let conf_a = confirmation(
            r"C:\Users\me\.tool\cache-a",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        commit_ai_rule_batch(&user, &[conf_a], &CandidateRematchVerifier, &port_a).unwrap();
        assert!(
            String::from_utf8_lossy(&read_bytes(&user).unwrap())
                .contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"),
            "first commit reported success"
        );
        assert!(FakeTransactionPort::marker_path(&rule_file).exists(), "COMMITTED marker left");
        assert!(FakeTransactionPort::backup_path(&rule_file).exists(), "backup left");

        // Second commit: committed recovery's cleanup fails again → the new
        // transaction is refused, but the previously committed rules survive.
        let port_b = FakeTransactionPort::with_faults(&[Step::CommitCleanupBackup]);
        let conf_b = confirmation(
            r"C:\Users\me\.tool\cache-b",
            2,
            "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
            RiskLevel::Review,
        );
        let err = commit_ai_rule_batch(&user, &[conf_b], &CandidateRematchVerifier, &port_b)
            .unwrap_err();
        assert!(err.contains("committed recovery"), "{err}");
        let text = String::from_utf8_lossy(&read_bytes(&user).unwrap()).into_owned();
        assert!(
            text.contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"),
            "previously committed rule must survive: {text}"
        );
        assert!(
            !text.contains("user-ai/7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d"),
            "refused new transaction must not write: {text}"
        );
        assert!(FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Once the committed residue is cleaned up, a new transaction is allowed
    /// and appends on top of the previously committed rules.
    #[test]
    fn committed_recovery_cleans_then_allows_a_new_commit() {
        let dir = tmp_dir("fr3-committed-then-new");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        seed_file(
            &user,
            &[exact_rule(
                "user-protected/1",
                r"C:\Users\me\.creds",
                RuleSource::UserProtected,
                RiskLevel::Protected,
                false,
            )],
        );

        // First commit leaves COMMITTED residue behind (cleanup fault).
        let port_a = FakeTransactionPort::with_faults(&[Step::CommitCleanupBackup]);
        let conf_a = confirmation(
            r"C:\Users\me\.tool\cache-a",
            1,
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
            RiskLevel::Safe,
        );
        commit_ai_rule_batch(&user, &[conf_a], &CandidateRematchVerifier, &port_a).unwrap();
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());

        // A clean second port recovers (validates + cleans), then commits.
        let port_b = FakeTransactionPort::new();
        let conf_b = confirmation(
            r"C:\Users\me\.tool\cache-b",
            2,
            "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
            RiskLevel::Review,
        );
        commit_ai_rule_batch(&user, &[conf_b], &CandidateRematchVerifier, &port_b).unwrap();
        let text = String::from_utf8_lossy(&read_bytes(&user).unwrap()).into_owned();
        assert!(
            text.contains("user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"),
            "previously committed rule survives: {text}"
        );
        assert!(
            text.contains("user-ai/7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d"),
            "new transaction committed after cleanup: {text}"
        );
        assert!(!FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(!FakeTransactionPort::backup_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no marker, a stray candidate/backup whose cleanup fails refuses the
    /// new transaction (the live file is authoritative but residue blocks).
    #[test]
    fn recover_stray_cleanup_failure_refuses_new_transaction() {
        let dir = tmp_dir("fr3-stray-fail");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        std::fs::write(&rule_file, &original).unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"stray").unwrap();

        let port = FakeTransactionPort::with_faults(&[Step::RecoverStrayCleanup]);
        let err = port.recover_if_needed(&rule_file).unwrap_err();
        assert!(err.contains("stray"), "{err}");
        // Live is untouched and the stray evidence is preserved.
        assert_eq!(std::fs::read(&rule_file).unwrap(), original);
        assert!(FakeTransactionPort::candidate_path(&rule_file).exists());

        // Once the failure clears, the same state recovers cleanly (Clean).
        let fresh = FakeTransactionPort::new();
        let recovery = fresh.recover_if_needed(&rule_file).unwrap();
        assert_eq!(recovery, TransactionRecovery::Clean);
        assert!(!FakeTransactionPort::candidate_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- fix round 4: durable ROLLED_BACK cleanup state ----------------------

    /// Once ACTIVE/PRESENT has restored the original bytes, rollback must
    /// publish a durable ROLLED_BACK/PRESENT marker before any cleanup.  If the
    /// later backup removal fails, the marker remains classified and the backup
    /// is not mistaken for an unowned stray file.
    #[test]
    fn rollback_backup_cleanup_failure_keeps_rolled_back_marker() {
        let dir = tmp_dir("fr4-rollback-backup");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        let candidate_live = {
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let mut rules = serde_yaml_ng::from_str::<RuleFile>(&String::from_utf8_lossy(&original))
                .unwrap()
                .rules;
            rules.push(LocalRuleDraftBuilder.build(&conf).unwrap());
            serde_yaml_ng::to_string(&RuleFile { rules }).unwrap()
        };
        std::fs::write(&rule_file, &candidate_live).unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), &candidate_live).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActivePresent,
        );
        let tx = PreparedRuleTransaction {
            live_path: rule_file.clone(),
            marker_path: FakeTransactionPort::marker_path(&rule_file),
            backup_path: FakeTransactionPort::backup_path(&rule_file),
            candidate_path: FakeTransactionPort::candidate_path(&rule_file),
        };

        let faulted = FakeTransactionPort::with_faults(&[Step::RolledBackCleanupBackup]);
        let rolled_back = faulted.rollback(&tx).unwrap();
        assert_eq!(std::fs::read(&rule_file).unwrap(), original);
        assert_eq!(
            read_marker(&rolled_back.marker_path).unwrap(),
            Some(MarkerState::RolledBackPresent)
        );
        assert!(
            faulted.cleanup_rolled_back(&rolled_back).is_err(),
            "backup cleanup must be observable as a pending-cleanup error"
        );
        assert_eq!(
            read_marker(&rolled_back.marker_path).unwrap(),
            Some(MarkerState::RolledBackPresent),
            "backup failure must not erase the rollback classification"
        );
        assert!(rolled_back.backup_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A subsequent recovery must return the durable ROLLED_BACK state rather
    /// than treating its backup as a no-marker stray.  It must not delete or
    /// otherwise reinterpret the recovery material before Core validates it.
    #[test]
    fn next_recovery_returns_rolled_back_pending_cleanup_not_stray_backup() {
        let dir = tmp_dir("fr4-rolled-back-recover");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        std::fs::write(&rule_file, &original).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::RolledBackPresent,
        );

        let port = FakeTransactionPort::new();
        let recovery = port.recover_if_needed(&rule_file).unwrap();
        let TransactionRecovery::RolledBackPendingCleanup(pending) = recovery else {
            panic!("expected RolledBackPendingCleanup, got {recovery:?}");
        };
        assert_eq!(pending.live_path, rule_file);
        assert!(pending.backup_path.exists());
        assert!(pending.marker_path.exists());
        assert!(pending.candidate_path.exists());
        assert_eq!(
            read_marker(&pending.marker_path).unwrap(),
            Some(MarkerState::RolledBackPresent)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Core must not clean ROLLED_BACK/PRESENT evidence merely because the live
    /// bytes are parseable: semantic validation is required first.
    #[test]
    fn rolled_back_present_semantically_invalid_live_refuses_and_keeps_evidence() {
        let dir = tmp_dir("fr4-rolled-back-invalid-present");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let invalid_doc = RuleDoc {
            id: "user-ai/9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f".to_string(),
            description: "invalid rolled-back rule".to_string(),
            product: None,
            category: ResidueCategory::DeveloperCache,
            risk: RiskLevel::Safe,
            source: RuleSource::User,
            match_spec: MatchSpec {
                exact: Some(r"C:\Users\me\.tool\cache".to_string()),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: Vec::new(),
            exclude: Vec::new(),
            provenance: Some(provenance(
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Protected,
            )),
        };
        let invalid_live = serde_yaml_ng::to_string(&RuleFile {
            rules: vec![invalid_doc],
        })
        .unwrap();
        std::fs::write(&rule_file, invalid_live.as_bytes()).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), b"original").unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::RolledBackPresent,
        );

        let conf = confirmation(
            r"C:\Users\me\.tool\new-cache",
            1,
            "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
            RiskLevel::Review,
        );
        let port = FakeTransactionPort::new();
        let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port)
            .unwrap_err();
        assert!(err.contains("rolled-back recovery"), "{err}");
        assert_eq!(std::fs::read(&rule_file).unwrap(), invalid_live.as_bytes());
        assert!(FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(FakeTransactionPort::backup_path(&rule_file).exists());
        assert!(FakeTransactionPort::candidate_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Core must reject a live file that reappears under ROLLED_BACK/ABSENT;
    /// absence is an explicit recovery invariant, not a best-effort cleanup
    /// hint.
    #[test]
    fn rolled_back_absent_live_reappears_refuses_and_keeps_evidence() {
        let dir = tmp_dir("fr4-rolled-back-invalid-absent");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let live = valid_live_yaml(&user);
        std::fs::write(&rule_file, &live).unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::RolledBackAbsent,
        );

        let conf = confirmation(
            r"C:\Users\me\.tool\new-cache",
            1,
            "7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d",
            RiskLevel::Review,
        );
        let port = FakeTransactionPort::new();
        let err = commit_ai_rule_batch(&user, &[conf], &CandidateRematchVerifier, &port)
            .unwrap_err();
        assert!(err.contains("rolled-back recovery"), "{err}");
        assert_eq!(std::fs::read(&rule_file).unwrap(), live);
        assert!(FakeTransactionPort::marker_path(&rule_file).exists());
        assert!(FakeTransactionPort::candidate_path(&rule_file).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// If a process crashes after ACTIVE/PRESENT restoration and before the
    /// ROLLED_BACK cleanup, a fresh recovery still sees a classified pending
    /// transaction and can finish it without treating the backup as stray.
    #[test]
    fn active_present_recovery_crash_before_rolled_back_cleanup_recovers() {
        let dir = tmp_dir("fr4-active-to-rolled-back");
        let user = dir.join("rules").join("user");
        std::fs::create_dir_all(&user).unwrap();
        let rule_file = dispositions_path(&user);
        let original = valid_live_yaml(&user);
        let candidate_live = {
            let conf = confirmation(
                r"C:\Users\me\.tool\cache",
                1,
                "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f",
                RiskLevel::Safe,
            );
            let mut rules = serde_yaml_ng::from_str::<RuleFile>(&String::from_utf8_lossy(&original))
                .unwrap()
                .rules;
            rules.push(LocalRuleDraftBuilder.build(&conf).unwrap());
            serde_yaml_ng::to_string(&RuleFile { rules }).unwrap()
        };
        std::fs::write(&rule_file, &candidate_live).unwrap();
        std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
        std::fs::write(FakeTransactionPort::candidate_path(&rule_file), &candidate_live).unwrap();
        write_marker(
            &FakeTransactionPort::marker_path(&rule_file),
            MarkerState::ActivePresent,
        );
        let tx = PreparedRuleTransaction {
            live_path: rule_file.clone(),
            marker_path: FakeTransactionPort::marker_path(&rule_file),
            backup_path: FakeTransactionPort::backup_path(&rule_file),
            candidate_path: FakeTransactionPort::candidate_path(&rule_file),
        };

        let first = FakeTransactionPort::new();
        let pending = first.rollback(&tx).unwrap();
        assert_eq!(
            read_marker(&pending.marker_path).unwrap(),
            Some(MarkerState::RolledBackPresent)
        );
        assert_eq!(std::fs::read(&rule_file).unwrap(), original);

        let second = FakeTransactionPort::new();
        let recovery = second.recover_if_needed(&rule_file).unwrap();
        assert!(matches!(
            recovery,
            TransactionRecovery::RolledBackPendingCleanup(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every ROLLED_BACK cleanup step is fail-closed: a failed candidate,
    /// backup, or marker removal leaves the ROLLED_BACK marker in place.
    #[test]
    fn every_rolled_back_cleanup_failure_keeps_marker() {
        for (step, state) in [
            (
                Step::RolledBackCleanupCandidate,
                MarkerState::RolledBackPresent,
            ),
            (
                Step::RolledBackCleanupBackup,
                MarkerState::RolledBackPresent,
            ),
            (
                Step::RolledBackCleanupMarker,
                MarkerState::RolledBackPresent,
            ),
        ] {
            let dir = tmp_dir(&format!("fr4-cleanup-{step:?}"));
            let user = dir.join("rules").join("user");
            std::fs::create_dir_all(&user).unwrap();
            let rule_file = dispositions_path(&user);
            let original = valid_live_yaml(&user);
            std::fs::write(&rule_file, &original).unwrap();
            std::fs::write(FakeTransactionPort::backup_path(&rule_file), &original).unwrap();
            std::fs::write(FakeTransactionPort::candidate_path(&rule_file), b"candidate").unwrap();
            write_marker(&FakeTransactionPort::marker_path(&rule_file), state);
            let tx = PreparedRuleTransaction {
                live_path: rule_file.clone(),
                marker_path: FakeTransactionPort::marker_path(&rule_file),
                backup_path: FakeTransactionPort::backup_path(&rule_file),
                candidate_path: FakeTransactionPort::candidate_path(&rule_file),
            };
            let faulted = FakeTransactionPort::with_faults(&[step]);
            assert!(faulted.cleanup_rolled_back(&tx).is_err(), "{step:?}");
            assert!(tx.marker_path.exists(), "marker must survive {step:?}");
            assert!(
                matches!(
                    read_marker(&tx.marker_path).unwrap(),
                    Some(MarkerState::RolledBackPresent)
                ),
                "marker classification must survive {step:?}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
