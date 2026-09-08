//! `CleanupSession` — one execution record of a `CleanupPlan`.

use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use super::{
    action::CleanupMode,
    category::ResidueCategory,
    ids::{CleanupPlanId, ProviderId, RuleId, ScanItemId},
    source::SourceKind,
};

/// Outcome of one planned item inside a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum CleanupStatus {
    /// The action completed.
    Success,
    /// The action was intentionally not performed.
    Skipped {
        /// Machine/human readable skip reason (e.g. process running,
        /// protected root, revalidation mismatch).
        reason: String,
    },
    /// The action failed.
    Failed {
        /// Error message recorded in the journal (never contains secret
        /// material, SPEC §24).
        error: String,
    },
    /// Dry-run simulation: validation allowed the item and this is the action
    /// that *would* run (SPEC §23 "Would Delete / Recycle / Execute").
    WouldExecute { mode: CleanupMode },
}

/// Per-item journal record (SPEC §24).
///
/// One `CleanupResult` is appended per planned item when a
/// [`CleanupSession`] runs. The journal explicitly **forbids** recording
/// tokens, credentials, secrets, private keys or file contents (SPEC §24) —
/// only paths, sizes, actions, ids and outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupResult {
    /// Origin scan item reference.
    pub scan_item_id: ScanItemId,
    /// Logical target path (from the plan, never user-supplied).
    pub path: PathBuf,
    /// Owning product, when known.
    pub product: Option<String>,
    /// Category at execution time.
    pub category: ResidueCategory,
    /// Detecting source.
    pub source: SourceKind,
    /// Rule that classified the item, when applicable.
    pub rule_id: Option<RuleId>,
    /// Provider that discovered the item, when applicable.
    pub provider_id: Option<ProviderId>,
    /// Which deletion mode the engine attempted.
    pub action: CleanupMode,
    /// Reclaim estimate in bytes.
    pub estimated_size: u64,
    /// Outcome (success / skipped / failed with detail).
    pub status: CleanupStatus,
}

/// Aggregated counters for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionTotals {
    /// Sum of `estimated_size` over all planned items.
    pub planned_bytes: u64,
    /// Bytes actually reclaimed on success.
    pub completed_bytes: u64,
    /// Number of items that finished successfully.
    pub succeeded: u32,
    /// Number of items skipped with a reason.
    pub skipped: u32,
    /// Number of items that failed.
    pub failed: u32,
}

/// One execution of a `CleanupPlan` (SPEC §19 / §24).
///
/// A session is created by the future `CleanupEngine` when it starts executing
/// a plan. In the current 1:1 model, one plan is executed by at most one
/// session, so `session_id` equals `plan_id`; `plan_id` is kept as a separate
/// field so a later model that re-executes plans can promote `session_id` to
/// its own id type without breaking journal consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupSession {
    /// Journal key of this execution (SPEC §24 `session_id`).
    pub session_id: CleanupPlanId,
    /// The plan this session executes.
    pub plan_id: CleanupPlanId,
    /// When execution started.
    #[serde(with = "crate::domain::serde_time::system_time")]
    pub started_at: SystemTime,
    /// When execution finished (`None` while running / after crash).
    #[serde(with = "crate::domain::serde_time::opt_system_time")]
    pub ended_at: Option<SystemTime>,
    /// Whether this session was a simulation (dry run, SPEC §23).
    pub dry_run: bool,
    /// Per-item results, in plan order.
    pub items: Vec<CleanupResult>,
    /// Aggregated counters.
    pub totals: SessionTotals,
    /// True when at least one journal append failed during this session.
    /// Cleanup itself is never rolled back by a journal failure; the flag
    /// tells operators the audit trail is incomplete.
    #[serde(default)]
    pub journal_degraded: bool,
}

impl CleanupSession {
    /// True when every planned item succeeded.
    #[must_use]
    pub fn is_fully_successful(&self) -> bool {
        self.totals.failed == 0 && self.totals.skipped == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{action::CleanupAction, ids::CleanupPlanId};
    use std::time::{Duration, SystemTime};

    fn ok_result(id: u64) -> CleanupResult {
        CleanupResult {
            scan_item_id: ScanItemId::from_raw(id),
            path: PathBuf::from(r"C:\Users\demo\residue\1"),
            product: Some("npm".into()),
            category: ResidueCategory::PackageCache,
            source: SourceKind::DeveloperCacheProvider,
            rule_id: None,
            provider_id: Some(ProviderId::from_raw(2)),
            action: CleanupMode::RecycleBin,
            estimated_size: 512,
            status: CleanupStatus::Success,
        }
    }

    #[test]
    fn session_records_journal_shape() {
        let plan_id = CleanupPlanId::from_raw(11);
        let mut items = vec![ok_result(1)];
        items.push(CleanupResult {
            path: PathBuf::from(r"C:\Users\demo\residue\2"),
            product: None,
            category: ResidueCategory::BuildArtifact,
            source: SourceKind::Kondo,
            action: CleanupMode::DirectDelete,
            estimated_size: 1024,
            status: CleanupStatus::Failed {
                error: "locked by process".into(),
            },
            ..ok_result(2)
        });

        // Quantise sub-second precision (serialisation is second-granular)
        // before comparing the round trip.
        let mut session = CleanupSession {
            session_id: plan_id,
            plan_id,
            started_at: SystemTime::now(),
            ended_at: Some(SystemTime::now()),
            dry_run: false,
            totals: SessionTotals {
                planned_bytes: 1536,
                completed_bytes: 512,
                succeeded: 1,
                skipped: 0,
                failed: 1,
            },
            items,
            journal_degraded: false,
        };
        for t in [&mut session.started_at, session.ended_at.as_mut().unwrap()] {
            let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            *t = std::time::UNIX_EPOCH + Duration::from_secs(secs);
        }
        assert!(!session.is_fully_successful());
        assert_eq!(session.items[1].estimated_size, 1024);

        let json = serde_json::to_string(&session).unwrap();
        let back: CleanupSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back, session);
    }

    #[test]
    fn never_serde_cleanup_action_in_journal_secrets() {
        // Ensures the action DTO stays stable/documented — no secret payloads
        // are part of CleanupResult by construction.
        let action = CleanupAction::Defer {
            reason: "user review".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"kind":"defer","reason":"user review"}"#);
    }
}
