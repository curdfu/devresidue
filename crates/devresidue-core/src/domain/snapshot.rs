//! `TargetSnapshot` — the scan-time record re-validated before any cleanup.

use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use super::{
    ids::{ProviderId, RuleId},
    risk_level::RiskLevel,
    scan_item::ScanItem,
};
use crate::safety::canonical;
use crate::safety::probe::{AttrFlags, FileIdentity, ReparseKind};

/// Immutable facts captured about a target when a plan is created
/// (SPEC §16 "Snapshot", INV-005).
///
/// The TOCTOU model mandates:
///
/// ```text
/// Scan -> Snapshot -> User Selection -> CleanupPlan
///     -> Revalidate (compare against this snapshot) -> Cleanup
/// ```
///
/// Before deleting, the Phase 5 `SafetyValidator` re-reads the live target and
/// compares it with this snapshot (canonical path, attributes, reparse state,
/// last write time, NTFS file id, rule/provider/risk provenance). Any mismatch
/// fails closed. Platform-specific facts (reparse state, NTFS file id) are
/// produced by the platform adapter for the core [`PathProbe`] port and stored
/// here in their neutral forms — core has **no** Windows types.
///
/// The struct is intentionally neutral: core has **no** Windows types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSnapshot {
    /// Logical target path as produced by the scan/plan. Never user-supplied
    /// (INV-013); callers operate on ids, the plan carries the path.
    pub path: PathBuf,
    /// Canonical/normalized form of `path` computed by core's lexical
    /// canonicaliser at capture time.
    pub normalized_path: Option<PathBuf>,
    /// Attribute bits observed at snapshot time (read-only/hidden/system/
    /// directory/reparse). Missing on pre-Phase-5 snapshots.
    pub attributes: Option<AttrFlags>,
    /// Reparse kind observed at snapshot time; `None` for plain objects or
    /// pre-Phase-5 snapshots that did not probe. Revalidation compares this
    /// with the live state to catch link swaps.
    pub reparse_state: Option<ReparseKind>,
    /// Last write time observed at snapshot time. Re-checked before delete to
    /// detect concurrent modification.
    #[serde(with = "crate::domain::serde_time::opt_system_time")]
    pub last_write_time: Option<SystemTime>,
    /// Opaque OS file identity when the platform can provide one (e.g. NTFS
    /// file id). Used to detect rename/replace/symlink-swap between scan and
    /// cleanup (TOCTOU, SPEC §16 / §28). Carries its own last-write so a
    /// single probe produces both facts.
    pub file_identity: Option<FileIdentity>,
    /// Rule that classified this target, when applicable.
    pub rule_id: Option<RuleId>,
    /// Provider that discovered this target, when applicable.
    pub provider_id: Option<ProviderId>,
    /// Risk recorded at snapshot time; revalidation must observe the same
    /// risk for the plan to stay executable.
    pub risk: RiskLevel,
}

impl TargetSnapshot {
    /// Records a snapshot from scan-time knowledge. Platform-derived fields
    /// (`normalized_path`, `attributes`, `reparse_state`, `file_identity`)
    /// are `None` until the Phase 5 capture probe fills them.
    #[must_use]
    pub fn new(
        path: PathBuf,
        last_write_time: Option<SystemTime>,
        rule_id: Option<RuleId>,
        provider_id: Option<ProviderId>,
        risk: RiskLevel,
    ) -> Self {
        Self {
            path,
            normalized_path: None,
            attributes: None,
            reparse_state: None,
            last_write_time,
            file_identity: None,
            rule_id,
            provider_id,
            risk,
        }
    }

    /// Builds a scan-time snapshot from the trusted fields a provider recorded
    /// on a [`ScanItem`].
    ///
    /// This is the SPEC §16 "Scan → Snapshot" step: the platform-derived facts
    /// (`attributes`, `reparse_state`, `file_identity`) are left `None` — they
    /// were not probed at scan time. The SafetyValidator will therefore fail
    /// closed (NoRecordedIdentity) for a target that still exists, and report
    /// `TargetMissing` for one that does not. When a live probe is available
    /// (real providers), [`crate::SafetyValidator::capture`] should be used
    /// instead to record the full fingerprint.
    #[must_use]
    pub fn from_scan_item(item: &ScanItem) -> Self {
        let normalized = if canonical::is_absolute(&item.path) {
            Some(canonical::normalize(&item.path))
        } else {
            None
        };
        Self {
            path: item.path.clone(),
            normalized_path: normalized,
            attributes: None,
            reparse_state: None,
            last_write_time: item.last_modified,
            file_identity: None,
            rule_id: item.evidence.iter().find_map(|e| e.rule_id),
            provider_id: None,
            risk: item.risk,
        }
    }
}
