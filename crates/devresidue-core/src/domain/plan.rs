//! `CleanupPlan` and its per-item records.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::{
    action::{CleanupMode, ExternalCommandSpec},
    category::ResidueCategory,
    ids::{CleanupPlanId, ScanItemId},
    snapshot::TargetSnapshot,
    source::SourceKind,
};

/// How much user confirmation an item requires before it may be executed
/// (SPEC §19: RegenerableDownload → user confirmation, Review → explicit
/// confirmation). `None` items (Safe / RegenerableLocal) are executable by a
/// `clean` run with no confirmation flags.
///
/// Ordered so `max(...)` yields the strictest requirement in a plan:
/// `None < Redownload < Review`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum ConfirmRequirement {
    /// No confirmation needed.
    #[default]
    None,
    /// Deletion triggers re-downloads; the user must confirm at
    /// `redownload` level or above.
    Redownload,
    /// Item may hold user-valued data; the user must confirm at
    /// `review` level (`--confirm-review` / `--yes`).
    Review,
}

/// A validated, user-confirmed cleanup plan (SPEC §16 / §23).
///
/// A plan is the **only** handle through which deletion may ever be requested:
/// `devresidue clean --plan <id>` (PLAN Phase 6) — never a bare path
/// (INV-013). The plan records, per item, the execution mode chosen by the
/// planner, the estimated reclaim size and a scan-time [`TargetSnapshot`] that
/// the SafetyValidator must re-validate right before deletion.
///
/// Every plan supports dry-run (SPEC §23): a `dry_run: true` plan reports what
/// *would* happen (`Would Delete` / `Would Recycle` / `Would Execute` /
/// `Would Skip`) without touching the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupPlan {
    /// Opaque plan id; what the CLI/UI submit to execute this plan.
    pub id: CleanupPlanId,
    /// When the plan was created.
    #[serde(with = "crate::domain::serde_time::system_time")]
    pub created_at: SystemTime,
    /// Legacy field (Phase 15 F15): plans used to carry a `dry_run` flag that
    /// nothing consumed — the *run* decides dry-run via the engine options, not
    /// the plan. Kept as an optional compat slot so files written by older
    /// versions still parse; the value is absorbed and ignored (a dry run is
    /// always chosen at execution time). New files never serialise the key.
    #[serde(default, skip_serializing)]
    pub dry_run: Option<bool>,
    /// R2-F03: the scan **generation** this plan was built against
    /// (`ScanSnapshot.generation`). Execution refuses a plan whose generation
    /// differs from the current scan record, closing cross-process id reuse:
    /// a plan built over an older scan can never authorise objects of a newer
    /// one. 0 (legacy plans) never matches a real generation → refused.
    #[serde(default)]
    pub scan_generation: u64,
    /// The ordered list of items this plan covers.
    pub items: Vec<CleanupPlanItem>,
}

impl CleanupPlan {
    /// Sum of `estimated_size` over all planned items, in bytes.
    #[must_use]
    pub fn total_estimated_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.estimated_size).sum()
    }

    /// The strictest [`ConfirmRequirement`] among the plan's items.
    #[must_use]
    pub fn max_confirmation(&self) -> ConfirmRequirement {
        self.items
            .iter()
            .map(|i| i.confirmation)
            .max()
            .unwrap_or_default()
    }

    /// Convenience: a plan is created without the legacy dry-run flag.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            id: CleanupPlanId::from_raw(0),
            created_at: SystemTime::now(),
            dry_run: None,
            scan_generation: 0,
            items: Vec::new(),
        }
    }
}

/// One planned action inside a [`CleanupPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupPlanItem {
    /// Reference to the originating scan item (INV-013: never a path).
    pub scan_item_id: ScanItemId,
    /// Deletion mode the engine would use for this item.
    pub mode: CleanupMode,
    /// Estimated reclaimable bytes.
    pub estimated_size: u64,
    /// Scan-time snapshot used for pre-delete revalidation (SPEC §16).
    pub snapshot: TargetSnapshot,
    /// Owning product of the item (process-guard input, journal field).
    #[serde(default)]
    pub product: Option<String>,
    /// Category recorded at scan time (journal field).
    pub category: ResidueCategory,
    /// Detecting source (journal field).
    pub source: SourceKind,
    /// Confirmation the item needs before execution (SPEC §19).
    #[serde(default)]
    pub confirmation: ConfirmRequirement,
    /// Structured external command, present iff `mode == ExternalCommand`
    /// (SPEC §22 — never a shell string).
    #[serde(default)]
    pub external_command: Option<ExternalCommandSpec>,
}

impl CleanupPlanItem {
    /// Assembles one planned item.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        scan_item_id: ScanItemId,
        mode: CleanupMode,
        estimated_size: u64,
        snapshot: TargetSnapshot,
        product: Option<String>,
        category: ResidueCategory,
        source: SourceKind,
        confirmation: ConfirmRequirement,
        external_command: Option<ExternalCommandSpec>,
    ) -> Self {
        Self {
            scan_item_id,
            mode,
            estimated_size,
            snapshot,
            product,
            category,
            source,
            confirmation,
            external_command,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ids::ScanItemId, risk_level::RiskLevel, snapshot::TargetSnapshot};
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn plan_item(id: u64, size: u64) -> CleanupPlanItem {
        CleanupPlanItem::new(
            ScanItemId::from_raw(id),
            CleanupMode::RecycleBin,
            size,
            TargetSnapshot::new(
                PathBuf::from(format!(r"C:\Users\demo\residue\{id}")),
                Some(SystemTime::now() - Duration::from_secs(3600)),
                None,
                None,
                RiskLevel::Safe,
            ),
            None,
            ResidueCategory::BuildArtifact,
            SourceKind::Kondo,
            ConfirmRequirement::None,
            None,
        )
    }

    #[test]
    fn legacy_dry_run_field_is_absorbed_and_totals_sum() {
        let plan = CleanupPlan {
            id: CleanupPlanId::from_raw(1),
            created_at: SystemTime::now(),
            dry_run: Some(true), // legacy value: ignored by execution (F15)
            scan_generation: 0,
            items: vec![plan_item(1, 100), plan_item(2, 250), plan_item(3, 650)],
        };
        assert_eq!(plan.total_estimated_bytes(), 1000);
        assert_eq!(plan.items.len(), 3);
    }

    #[test]
    fn new_plans_omit_dry_run_but_old_files_still_parse() {
        let plan = CleanupPlan {
            id: CleanupPlanId::from_raw(7),
            created_at: SystemTime::now(),
            dry_run: None,
            scan_generation: 0,
            items: vec![plan_item(1, 2048)],
        };
        // New serialisation never emits the legacy key.
        let json = serde_json::to_string(&plan).unwrap();
        assert!(
            !json.contains("dry_run"),
            "legacy dry_run must not be serialised: {json}"
        );

        // An old plan file carrying `"dry_run": true` still deserialises and
        // the value is absorbed into the compat slot.
        let old = r#"{"id":9,"created_at":0,"dry_run":true,"items":[]}"#;
        let back: CleanupPlan = serde_json::from_str(old).expect("old plan must load");
        assert_eq!(back.dry_run, Some(true));
        assert!(back.items.is_empty());
    }

    #[test]
    fn plan_serde_round_trips() {
        // Compare against the second-quantised plan: serialisation floors
        // sub-second precision by design.
        let mut plan = CleanupPlan {
            id: CleanupPlanId::from_raw(7),
            created_at: SystemTime::now(),
            dry_run: None,
            scan_generation: 0,
            items: vec![plan_item(1, 2048)],
        };
        let quantise = |t: &mut std::time::SystemTime| {
            let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            *t = std::time::UNIX_EPOCH + Duration::from_secs(secs);
        };
        quantise(&mut plan.created_at);
        if let Some(last_write_time) = &mut plan.items[0].snapshot.last_write_time {
            quantise(last_write_time);
        }
        let json = serde_json::to_string(&plan).unwrap();
        let back: CleanupPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, plan);
    }
}
