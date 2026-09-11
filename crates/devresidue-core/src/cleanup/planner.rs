//! The `CleanupPlanner` — turns a user selection of scan items into a
//! persisted, dry-run-able [`CleanupPlan`] (SPEC §16 / §19 / §23).
//!
//! Responsibilities:
//!
//! - resolves the selected [`ScanItemId`]s against a scan result;
//! - applies the SPEC §19 risk gate under the requested [`ConfirmPolicy`];
//! - records a [`TargetSnapshot`] per included item. When the live probe can
//!   capture the full fingerprint (real providers on a real machine) the
//!   snapshot is captured through the [`SafetyValidator`]; when the target
//!   does not exist any more the scan-time record is kept so the engine's
//!   revalidation reports `TargetMissing` instead of silently dropping the
//!   selection;
//! - never includes `Protected` / `Unknown` risk or `None` / `Defer` actions
//!   (INV-001 / INV-002, defence in depth);
//! - persists the plan through the [`PlanStore`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::domain::action::CleanupAction;
use crate::domain::ids::{CleanupPlanId, RuleId, ScanItemId};
use crate::domain::plan::{CleanupPlan, CleanupPlanItem, ConfirmRequirement};
use crate::domain::risk_level::RiskLevel;
use crate::domain::scan_item::ScanItem;
use crate::domain::snapshot::TargetSnapshot;
use crate::domain::source::SourceKind;
use crate::rules::RuleSet;
use crate::safety::identity::CaptureError;
use crate::safety::SafetyValidator;

use super::plan_store::{PlanStore, PlanStoreError};

/// How much user confirmation the planner requires (SPEC §19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfirmPolicy {
    /// Safe / RegenerableLocal items only.
    None,
    /// Additionally allow RegenerableDownload (user confirmed at
    /// `--confirm-redownload`).
    Redownload,
    /// Additionally allow Review (user confirmed at `--confirm-review`).
    Review,
    /// Confirm everything (`--yes`).
    All,
}

impl ConfirmPolicy {
    /// Whether this policy permits an item that requires `requirement`.
    #[must_use]
    pub fn permits(self, requirement: ConfirmRequirement) -> bool {
        match requirement {
            ConfirmRequirement::None => true,
            ConfirmRequirement::Redownload => matches!(
                self,
                ConfirmPolicy::Redownload | ConfirmPolicy::Review | ConfirmPolicy::All
            ),
            ConfirmRequirement::Review => {
                matches!(self, ConfirmPolicy::Review | ConfirmPolicy::All)
            }
        }
    }
}

/// Reason a selected item did not enter the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Risk `Protected` — INV-002.
    ProtectedRisk,
    /// Risk `Unknown` — INV-001.
    UnknownRisk,
    /// `CleanupAction::None` (nothing to do).
    ActionNone,
    /// `CleanupAction::Defer` with the provider's reason.
    ActionDeferred { reason: String },
    /// The item's risk class needs more confirmation than the current policy.
    ConfirmationRequired { required: ConfirmRequirement },
    /// The target could not be probed at plan time (permission/lock/other).
    Unverifiable { detail: String },
    /// A selection referenced an unknown id.
    UnknownSelection,
    /// A protected area (rule-protected path, R03) sits *inside* the target
    /// tree — deleting the target would delete the protected path too.
    ProtectedDescendant { area: PathBuf },
    /// The item was discovered by a rule whose id no longer matches the
    /// current rule gate (R08) — the discovery basis is invalidated.
    DiscoverySourceInvalidated { rule_id: Option<RuleId> },
    /// R2-F05: a real-scan item carries no scan-time authorisation snapshot
    /// (temporary probe failure, or a legacy record). Never re-authorise the
    /// current object — re-scan required.
    MissingScanSnapshot,
    /// R4-H05: the target hits the **current** protected-root registry
    /// (a workspace root configured after the scan, or any root equal to /
    /// above the target). The plan-time preview must match the execution-time
    /// deny set: such a target is skipped, not planned.
    CurrentProtectedRoot { root: PathBuf },
}

/// A skipped selection, reported to the caller (dry-run previews / summaries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipRecord {
    pub scan_item_id: ScanItemId,
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// Outcome of [`CleanupPlanner::build`].
#[derive(Debug, Clone)]
pub struct PlannerOutput {
    /// The generated plan (never executed — only stored / dry-run).
    pub plan: CleanupPlan,
    /// Items from the selection that were intentionally left out.
    pub skipped: Vec<SkipRecord>,
}

/// Errors from the planner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlannerError {
    #[error("selection is empty")]
    EmptySelection,
    #[error("selection contains an unknown scan item id: {0}")]
    UnknownItem(ScanItemId),
    #[error("no planned item survived risk gating")]
    NothingToPlan,
    #[error("plan could not be persisted: {0}")]
    PlanStore(#[from] PlanStoreError),
}

/// Builds plans for an explicit selection under a confirmation policy.
pub struct CleanupPlanner {
    validator: SafetyValidator,
    policy: ConfirmPolicy,
    /// Optional rule gate (R03/R08). `None` keeps legacy behaviour for
    /// demo/fixture callers that carry no rules; real CLI/Tauri callers pass
    /// the merged rule set.
    rules: Option<Arc<RuleSet>>,
    /// R2-F03: the scan generation this planner builds plans against.
    scan_generation: u64,
    /// R2-F05: whether the planner is planning a *real* scan (vs fixtures).
    /// Real scans require every item to carry a scan-time snapshot.
    real_scan: bool,
}

impl CleanupPlanner {
    /// New planner over the given validator (used for live snapshot capture)
    /// and confirmation policy.
    pub fn new(validator: SafetyValidator, policy: ConfirmPolicy) -> Self {
        Self {
            validator,
            policy,
            rules: None,
            scan_generation: 0,
            real_scan: false,
        }
    }

    /// New planner with the rule gate (R03 protected-descendant pre-check and
    /// R08 rule-source revalidation).
    pub fn new_with_rules(
        validator: SafetyValidator,
        policy: ConfirmPolicy,
        rules: Arc<RuleSet>,
    ) -> Self {
        Self {
            validator,
            policy,
            rules: Some(rules),
            scan_generation: 0,
            real_scan: false,
        }
    }

    /// Binds the planner to a scan generation (R2-F03): plans it builds carry
    /// this generation and are refused by the engine against any newer scan.
    #[must_use]
    pub fn with_generation(mut self, generation: u64) -> Self {
        self.scan_generation = generation;
        self
    }

    /// Marks the planner as operating on a **real** scan (R2-F05): items
    /// without a scan-time snapshot are skipped rather than live-captured.
    #[must_use]
    pub fn with_real_scan(mut self, real: bool) -> Self {
        self.real_scan = real;
        self
    }

    /// Generates and persists the plan. `output.plan.id` is set to the
    /// assigned id on success.
    ///
    /// # Errors
    /// Propagates store I/O failures (a plan that cannot be persisted is not
    /// handed out as an in-memory fallback — fail closed).
    pub fn build_and_store(
        &self,
        items: &[ScanItem],
        selected: &[ScanItemId],
        store: &mut PlanStore,
    ) -> Result<(CleanupPlanId, PlannerOutput), PlannerError> {
        let mut output = self.build(items, selected)?;
        if output.plan.items.is_empty() {
            return Err(PlannerError::NothingToPlan);
        }
        let id = store.save(&mut output.plan)?;
        Ok((id, output))
    }

    /// Builds the in-memory plan (no persistence).
    ///
    /// # Errors
    /// When the selection is empty or references unknown ids.
    pub fn build(
        &self,
        items: &[ScanItem],
        selected: &[ScanItemId],
    ) -> Result<PlannerOutput, PlannerError> {
        if selected.is_empty() {
            return Err(PlannerError::EmptySelection);
        }
        let by_id: HashMap<ScanItemId, &ScanItem> = items.iter().map(|i| (i.id, i)).collect();
        for id in selected {
            if !by_id.contains_key(id) {
                return Err(PlannerError::UnknownItem(*id));
            }
        }

        let mut plan_items: Vec<CleanupPlanItem> = Vec::new();
        let mut skipped: Vec<SkipRecord> = Vec::new();

        for id in selected {
            let item = by_id[id];
            // R03: refuse targets whose tree contains a protected area even
            // when the top-level path itself is cleanable (planner-side gate;
            // the engine re-checks at execution).
            if let Some(area) = self.protected_descendant(&item.path) {
                skipped.push(SkipRecord {
                    scan_item_id: item.id,
                    path: item.path.clone(),
                    reason: SkipReason::ProtectedDescendant { area },
                });
                continue;
            }
            // R08: a rule-discovered item must still match its discovery rule
            // under the current rule gate; an invalidated basis is skipped.
            if self.rule_source_invalidated(item) {
                skipped.push(SkipRecord {
                    scan_item_id: item.id,
                    path: item.path.clone(),
                    reason: SkipReason::DiscoverySourceInvalidated {
                        rule_id: item.evidence.iter().find_map(|e| e.rule_id),
                    },
                });
                continue;
            }
            // R4-H05: consult the CURRENT protected-root registry (the one
            // the caller built with the scan-persisted ∪ current workspace
            // roots). Real-scan items carry a scan-time snapshot that
            // snapshot_for() returns as-is — that is their authorisation
            // record and stays untouched — but the *protection context* is
            // evaluated now, so a target that became a workspace root after
            // the scan is skipped at plan time exactly as the engine will
            // deny it at execution time (preview/execution consistency).
            if let Some(hit) = self.validator.registry().protection_hit(&item.path) {
                skipped.push(SkipRecord {
                    scan_item_id: item.id,
                    path: item.path.clone(),
                    reason: SkipReason::CurrentProtectedRoot { root: hit.root },
                });
                continue;
            }
            match self.classify(item) {
                Classified::Plan {
                    mode,
                    confirmation,
                    external_command,
                } => {
                    if !self.policy.permits(confirmation) {
                        skipped.push(SkipRecord {
                            scan_item_id: item.id,
                            path: item.path.clone(),
                            reason: SkipReason::ConfirmationRequired {
                                required: confirmation,
                            },
                        });
                        continue;
                    }
                    match self.snapshot_for(item) {
                        Ok(snapshot) => plan_items.push(CleanupPlanItem::new(
                            item.id,
                            mode,
                            item.logical_size,
                            snapshot,
                            item.product.clone(),
                            item.category,
                            item.source,
                            confirmation,
                            external_command,
                        )),
                        Err(reason) => skipped.push(SkipRecord {
                            scan_item_id: item.id,
                            path: item.path.clone(),
                            reason,
                        }),
                    }
                }
                Classified::Skip(reason) => skipped.push(SkipRecord {
                    scan_item_id: item.id,
                    path: item.path.clone(),
                    reason,
                }),
            }
        }

        Ok(PlannerOutput {
            plan: CleanupPlan {
                id: CleanupPlanId::from_raw(0), // assigned on persistence
                created_at: SystemTime::now(),
                dry_run: None, // legacy field, never used (F15)
                // R2-F03: the plan is bound to the scan generation it was built
                // over; execution refuses any generation drift.
                scan_generation: self.scan_generation,
                items: plan_items,
            },
            skipped,
        })
    }

    /// Risk-gate + action resolution for one scan item.
    fn classify(&self, item: &ScanItem) -> Classified {
        use RiskLevel::*;
        match item.risk {
            Protected => return Classified::Skip(SkipReason::ProtectedRisk),
            Unknown => return Classified::Skip(SkipReason::UnknownRisk),
            _ => {}
        }
        let confirmation = match item.risk {
            Safe | RegenerableLocal => ConfirmRequirement::None,
            RegenerableDownload => ConfirmRequirement::Redownload,
            Review => ConfirmRequirement::Review,
            Protected | Unknown => unreachable!("gated above"),
        };
        match &item.cleanup_action {
            CleanupAction::None => Classified::Skip(SkipReason::ActionNone),
            CleanupAction::Defer { reason } => Classified::Skip(SkipReason::ActionDeferred {
                reason: reason.clone(),
            }),
            CleanupAction::RecycleBin => Classified::Plan {
                mode: crate::CleanupMode::RecycleBin,
                confirmation,
                external_command: None,
            },
            CleanupAction::DirectDelete => Classified::Plan {
                mode: crate::CleanupMode::DirectDelete,
                confirmation,
                external_command: None,
            },
            CleanupAction::ExternalCommand { command } => Classified::Plan {
                mode: crate::CleanupMode::ExternalCommand,
                confirmation,
                external_command: Some(command.clone()),
            },
        }
    }

    /// Whether a protected area (R03) strictly sits inside `target`'s tree.
    fn protected_descendant(&self, target: &Path) -> Option<PathBuf> {
        let areas = self.rules.as_ref()?.protected_areas();
        let norm_target = crate::safety::canonical::normalize(target);
        areas.into_iter().find(|area| {
            let norm_area = crate::safety::canonical::normalize(area);
            crate::safety::canonical::is_within(&norm_area, &norm_target)
                && !crate::safety::canonical::normalized_eq_path(&norm_area, &norm_target)
        })
    }

    /// R08: whether a rule-discovered item's discovery rule no longer matches
    /// under the current rule gate. Only applies to `SourceKind::Rule` items
    /// carrying a recorded rule id.
    fn rule_source_invalidated(&self, item: &ScanItem) -> bool {
        let rules = match &self.rules {
            Some(rules) => rules,
            None => return false,
        };
        if item.source != SourceKind::Rule {
            return false;
        }
        let Some(rule_id) = item.evidence.iter().find_map(|e| e.rule_id) else {
            return false;
        };
        // The discovery rule must still be the winning rule for this path.
        match crate::rules::resolve(&item.path, &rules.rules) {
            Some(hit) => hit.rule_id != rule_id,
            None => true,
        }
    }

    /// Captures the snapshot for a planned item (R01 / R2-F05).
    ///
    /// A real provider item carries its **scan-time** authorisation snapshot
    /// ([`ScanItem::scan_snapshot`]); it is used as-is, so a replacement of
    /// the on-disk object between scan and plan is *not* silently re-authorised
    /// — the engine's validation against the scan-time identity then refuses
    /// it.
    ///
    /// R2-F05: when the scan is a **Real** one (`real_scan`), an item without
    /// a scan-time snapshot is **skipped** (`MissingScanSnapshot`) instead of
    /// falling back to a live capture — a temporary probe failure that left an
    /// unauthorised item in the scan, or a legacy record lacking the field,
    /// must never re-authorise the current on-disk object. Only the fixture /
    /// demo channel (`real_scan == false`) keeps the legacy live-capture
    /// fallback.
    fn snapshot_for(&self, item: &ScanItem) -> Result<TargetSnapshot, SkipReason> {
        if let Some(snap) = &item.scan_snapshot {
            return Ok(snap.clone());
        }
        if self.real_scan {
            return Err(SkipReason::MissingScanSnapshot);
        }
        match self.validator.capture(
            Path::new(&item.path),
            item.evidence.iter().find_map(|e| e.rule_id),
            None,
            item.risk,
        ) {
            Ok(snap) => Ok(snap),
            Err(CaptureError::NotFound { .. }) => Ok(TargetSnapshot::from_scan_item(item)),
            Err(other) => Err(SkipReason::Unverifiable {
                detail: other.to_string(),
            }),
        }
    }
}

enum Classified {
    Plan {
        mode: crate::CleanupMode,
        confirmation: ConfirmRequirement,
        external_command: Option<crate::ExternalCommandSpec>,
    },
    Skip(SkipReason),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::action::CleanupAction;
    use crate::domain::category::ResidueCategory;
    use crate::domain::source::SourceKind;
    use crate::rules::priority::RuleSource;
    use crate::safety::fake::FakeProbe;
    use crate::safety::probe::{PathProbe, ProcessProbe, ProcessState};
    use crate::safety::process::ProcessGuard;
    use crate::safety::ProtectedRootRegistry;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    fn fake_env() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("USERPROFILE".into(), r"C:\Users\alice".into());
        m.insert("SYSTEMROOT".into(), r"C:\Windows".into());
        m.insert("PROGRAMFILES".into(), r"C:\Program Files".into());
        m.insert("PROGRAMFILES(X86)".into(), r"C:\Program Files (x86)".into());
        m.insert("PROGRAMDATA".into(), r"C:\ProgramData".into());
        m
    }

    struct FixedProcess(ProcessState);

    impl ProcessProbe for FixedProcess {
        fn process_state(&self, _names: &[&str]) -> ProcessState {
            self.0
        }
    }

    fn validator(probe: &Arc<FakeProbe>) -> SafetyValidator {
        let env = fake_env();
        let registry =
            ProtectedRootRegistry::build(&|k| env.get(k).cloned(), vec![]).expect("fake env");
        let path_probe: Arc<dyn PathProbe + Send + Sync> = probe.clone();
        let process_probe: Arc<dyn ProcessProbe + Send + Sync> =
            Arc::new(FixedProcess(ProcessState::NotRunning));
        SafetyValidator::new(
            path_probe,
            process_probe,
            registry,
            ProcessGuard::default_list(),
        )
    }

    fn planner(probe: &Arc<FakeProbe>, policy: ConfirmPolicy) -> CleanupPlanner {
        CleanupPlanner::new(validator(probe), policy)
    }

    fn item(id: u64, risk: RiskLevel, action: CleanupAction) -> ScanItem {
        ScanItem {
            id: crate::ScanItemId::from_raw(id),
            path: format!(r"C:\residue\item-{id}").into(),
            display_name: format!("item {id}"),
            product: Some("demo".into()),
            category: ResidueCategory::Temporary,
            risk,
            source: SourceKind::Kondo,
            logical_size: 1000 * id,
            file_count: 1,
            last_modified: Some(SystemTime::now() - Duration::from_secs(60)),
            explanation: "test item".into(),
            cleanup_action: action,
            evidence: vec![],
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    fn planned_ids(output: &PlannerOutput) -> Vec<u64> {
        output
            .plan
            .items
            .iter()
            .map(|i| i.scan_item_id.raw())
            .collect()
    }

    fn skip_kinds(output: &PlannerOutput) -> HashMap<u64, SkipReason> {
        output
            .skipped
            .iter()
            .map(|s| (s.scan_item_id.raw(), s.reason.clone()))
            .collect()
    }

    #[test]
    fn risk_gate_matrix_for_each_confirm_policy() {
        let probe = FakeProbe::new();
        let all = vec![
            item(1, RiskLevel::Safe, CleanupAction::RecycleBin),
            item(2, RiskLevel::RegenerableLocal, CleanupAction::DirectDelete),
            item(3, RiskLevel::RegenerableDownload, CleanupAction::RecycleBin),
            item(4, RiskLevel::Review, CleanupAction::RecycleBin),
        ];
        let selection: Vec<_> = all.iter().map(|i| i.id).collect();

        // Policy None: 1,2 planned; 3,4 need confirmation.
        let out = planner(&probe, ConfirmPolicy::None)
            .build(&all, &selection)
            .unwrap();
        assert_eq!(planned_ids(&out), vec![1, 2]);
        assert_eq!(
            skip_kinds(&out)[&3],
            SkipReason::ConfirmationRequired {
                required: ConfirmRequirement::Redownload
            }
        );
        assert_eq!(
            skip_kinds(&out)[&4],
            SkipReason::ConfirmationRequired {
                required: ConfirmRequirement::Review
            }
        );

        // Redownload: 3 joins.
        let out = planner(&probe, ConfirmPolicy::Redownload)
            .build(&all, &selection)
            .unwrap();
        assert_eq!(planned_ids(&out), vec![1, 2, 3]);

        // Review and All: 4 joins as well.
        for policy in [ConfirmPolicy::Review, ConfirmPolicy::All] {
            let out = planner(&probe, policy).build(&all, &selection).unwrap();
            assert_eq!(planned_ids(&out), vec![1, 2, 3, 4]);
        }
    }

    #[test]
    fn protected_and_unknown_never_enter_any_plan() {
        let probe = FakeProbe::new();
        let all = vec![
            item(1, RiskLevel::Protected, CleanupAction::RecycleBin),
            item(2, RiskLevel::Unknown, CleanupAction::RecycleBin),
            item(3, RiskLevel::Safe, CleanupAction::RecycleBin),
        ];
        let selection: Vec<_> = all.iter().map(|i| i.id).collect();

        for policy in [
            ConfirmPolicy::None,
            ConfirmPolicy::Redownload,
            ConfirmPolicy::Review,
            ConfirmPolicy::All,
        ] {
            let out = planner(&probe, policy).build(&all, &selection).unwrap();
            assert_eq!(planned_ids(&out), vec![3], "policy {policy:?}");
            assert_eq!(skip_kinds(&out)[&1], SkipReason::ProtectedRisk);
            assert_eq!(skip_kinds(&out)[&2], SkipReason::UnknownRisk);
        }
    }

    #[test]
    fn safe_item_enters_only_a_cleanup_plan_not_an_execution_result() {
        let probe = FakeProbe::new();
        let safe = item(41, RiskLevel::Safe, CleanupAction::RecycleBin);

        // CleanupPlanner only produces a handle-bound description. It owns no
        // DeletePort and cannot execute this selection; CleanupEngine remains
        // the sole execution authority after a later confirmation/revalidate.
        let output = planner(&probe, ConfirmPolicy::None)
            .build(&[safe], &[crate::ScanItemId::from_raw(41)])
            .unwrap();

        assert_eq!(planned_ids(&output), vec![41]);
        assert!(output.skipped.is_empty());
        let planned = &output.plan.items[0];
        assert_eq!(planned.scan_item_id.raw(), 41);
        assert_eq!(planned.mode, crate::CleanupMode::RecycleBin);
        assert_eq!(planned.confirmation, ConfirmRequirement::None);
    }

    #[test]
    fn none_and_defer_actions_are_skipped_with_reason() {
        let probe = FakeProbe::new();
        let all = vec![
            item(1, RiskLevel::Safe, CleanupAction::None),
            item(
                2,
                RiskLevel::Safe,
                CleanupAction::Defer {
                    reason: "review risk class".into(),
                },
            ),
        ];
        let selection: Vec<_> = all.iter().map(|i| i.id).collect();
        let out = planner(&probe, ConfirmPolicy::All)
            .build(&all, &selection)
            .unwrap();
        assert!(out.plan.items.is_empty());
        assert_eq!(skip_kinds(&out)[&1], SkipReason::ActionNone);
        assert_eq!(
            skip_kinds(&out)[&2],
            SkipReason::ActionDeferred {
                reason: "review risk class".into()
            }
        );
    }

    #[test]
    fn external_command_spec_travels_into_plan() {
        let probe = FakeProbe::new();
        let spec = crate::ExternalCommandSpec::new(
            "npm".into(),
            vec!["cache".into(), "clean".into()],
            None,
            Some(30),
        );
        let item = item(
            1,
            RiskLevel::RegenerableDownload,
            CleanupAction::ExternalCommand {
                command: spec.clone(),
            },
        );
        let out = planner(&probe, ConfirmPolicy::Redownload)
            .build(&[item], &[crate::ScanItemId::from_raw(1)])
            .unwrap();
        assert_eq!(planned_ids(&out), vec![1]);
        let planned = &out.plan.items[0];
        assert_eq!(planned.mode, crate::CleanupMode::ExternalCommand);
        assert_eq!(planned.external_command.as_ref(), Some(&spec));
        assert_eq!(planned.confirmation, ConfirmRequirement::Redownload);
    }

    #[test]
    fn selection_validation_errors() {
        let probe = FakeProbe::new();
        let items = vec![item(1, RiskLevel::Safe, CleanupAction::RecycleBin)];

        // Empty selection.
        assert!(matches!(
            planner(&probe, ConfirmPolicy::None).build(&items, &[]),
            Err(PlannerError::EmptySelection)
        ));
        // Unknown id.
        assert!(matches!(
            planner(&probe, ConfirmPolicy::None).build(&items, &[crate::ScanItemId::from_raw(99)]),
            Err(PlannerError::UnknownItem(_))
        ));
    }

    #[test]
    fn missing_target_falls_back_to_scan_snapshot_not_skip() {
        let probe = FakeProbe::new();
        // Fake fs is empty → the capture reports NotFound → the item is still
        // planned with the scan-time record (engine will deny at clean time).
        let items = vec![item(1, RiskLevel::Safe, CleanupAction::RecycleBin)];
        let out = planner(&probe, ConfirmPolicy::None)
            .build(&items, &[crate::ScanItemId::from_raw(1)])
            .unwrap();
        assert_eq!(planned_ids(&out), vec![1]);
        assert!(out.plan.items[0].snapshot.file_identity.is_none());
    }

    /// A rule set carrying one exact rule.
    fn rules_with(exact: &str, risk: RiskLevel, source: RuleSource, id: &str) -> RuleSet {
        let compiled = crate::rules::loader::CompiledRule {
            id: id.into(),
            numeric_id: crate::RuleId::from_raw(1),
            source,
            risk,
            category: ResidueCategory::Credential,
            product: None,
            description: "test".into(),
            pattern: crate::rules::loader::CompiledPattern::Exact {
                target: exact.to_string(),
            },
            parent_marker: None,
            requires_existing: false,
            include_raw: vec![],
            exclude_raw: vec![],
            include_matchers: vec![],
            exclude_matchers: vec![],
            order: 0,
        };
        RuleSet {
            rules: vec![compiled],
            issues: vec![],
        }
    }

    #[test]
    fn r01_plan_carries_the_scan_time_snapshot_not_a_fresh_capture() {
        let probe = FakeProbe::new();
        // Fake fs has the target at scan time; capture the scan-time identity.
        {
            let mut fs = probe.lock();
            fs.create_dir(Path::new("C:\\review\\cache"));
            fs.create_file(Path::new("C:\\review\\cache\\f"), None);
        }
        let v = validator(&probe);
        let scan_snapshot = v
            .capture(Path::new("C:\\review\\cache"), None, None, RiskLevel::Safe)
            .expect("scan-time capture");
        let scan_identity = scan_snapshot.file_identity.clone();

        // The object is replaced after the scan.
        {
            let mut fs = probe.lock();
            fs.delete_and_recreate(Path::new("C:\\review\\cache"));
        }

        let mut scanned = item(1, RiskLevel::Safe, CleanupAction::RecycleBin);
        scanned.path = Path::new("C:\\review\\cache").into();
        scanned.scan_snapshot = Some(scan_snapshot);

        let out = planner(&probe, ConfirmPolicy::None)
            .build(&[scanned], &[crate::ScanItemId::from_raw(1)])
            .unwrap();
        // The plan must carry the ORIGINAL scan identity, never the replaced
        // object's (R01).
        assert_eq!(out.plan.items[0].snapshot.file_identity, scan_identity);
    }

    // ---- R4-H05: plan-time preview must match the current protection set ---

    #[test]
    fn h05_workspace_root_configured_after_the_scan_is_skipped_at_plan_time() {
        // The scan recorded B as a cleanable cache (with its scan-time
        // snapshot — the authorisation record, which must stay untouched).
        // The CURRENT validator's registry now includes B as a workspace
        // root (configured after the scan). The planner must SKIP B —
        // exactly what the engine would deny at execution — instead of
        // planning it as `recycle / confirm=none`.
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_dir(Path::new("D:\\ws\\after-scan"));
            fs.create_file(Path::new("D:\\ws\\after-scan\\f"), None);
        }
        let v = validator(&probe);
        let scan_snapshot = v
            .capture(Path::new("D:\\ws\\after-scan"), None, None, RiskLevel::Safe)
            .expect("scan-time capture");

        // The "new" validator: same probes, registry extended with the
        // after-scan workspace root (mirrors build_validator_with_roots).
        let env = fake_env();
        let registry = ProtectedRootRegistry::build(&|k| env.get(k).cloned(), vec![])
            .expect("fake env")
            .with_workspace_roots([r"D:\ws\after-scan"], std::iter::empty::<&Path>());
        let path_probe: Arc<dyn PathProbe + Send + Sync> = probe.clone();
        let process_probe: Arc<dyn ProcessProbe + Send + Sync> =
            Arc::new(FixedProcess(ProcessState::NotRunning));
        let current_validator = SafetyValidator::new(
            path_probe,
            process_probe,
            registry,
            ProcessGuard::default_list(),
        );

        let mut scanned = item(1, RiskLevel::Safe, CleanupAction::RecycleBin);
        scanned.path = Path::new("D:\\ws\\after-scan").into();
        scanned.scan_snapshot = Some(scan_snapshot);

        let out = CleanupPlanner::new(current_validator, ConfirmPolicy::None)
            .build(&[scanned], &[crate::ScanItemId::from_raw(1)])
            .unwrap();

        // Skipped with the current-protected-root reason…
        assert_eq!(
            skip_kinds(&out).get(&1),
            Some(&SkipReason::CurrentProtectedRoot {
                root: PathBuf::from(r"D:\ws\after-scan")
            }),
            "a post-scan workspace root is skipped at plan time: {:?}",
            out.skipped
        );
        // …not planned at all.
        assert!(out.plan.items.is_empty(), "nothing may be planned");
    }

    #[test]
    fn h05_cleanable_target_outside_the_new_roots_still_plans() {
        // Control: a sibling target NOT covered by the new workspace roots
        // still plans normally (the check does not over-reject).
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_dir(Path::new("C:\\review\\normal-cache"));
            fs.create_file(Path::new("C:\\review\\normal-cache\\f"), None);
        }
        let v = validator(&probe);
        let scan_snapshot = v
            .capture(
                Path::new("C:\\review\\normal-cache"),
                None,
                None,
                RiskLevel::Safe,
            )
            .expect("scan-time capture");

        let env = fake_env();
        let registry = ProtectedRootRegistry::build(&|k| env.get(k).cloned(), vec![])
            .expect("fake env")
            .with_workspace_roots([r"D:\ws\after-scan"], std::iter::empty::<&Path>());
        let path_probe: Arc<dyn PathProbe + Send + Sync> = probe.clone();
        let process_probe: Arc<dyn ProcessProbe + Send + Sync> =
            Arc::new(FixedProcess(ProcessState::NotRunning));
        let current_validator = SafetyValidator::new(
            path_probe,
            process_probe,
            registry,
            ProcessGuard::default_list(),
        );

        let mut scanned = item(1, RiskLevel::Safe, CleanupAction::RecycleBin);
        scanned.path = Path::new("C:\\review\\normal-cache").into();
        scanned.scan_snapshot = Some(scan_snapshot);

        let out = CleanupPlanner::new(current_validator, ConfirmPolicy::None)
            .build(&[scanned], &[crate::ScanItemId::from_raw(1)])
            .unwrap();
        assert_eq!(planned_ids(&out), vec![1], "unaffected target still plans");
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn r03_planner_skips_targets_containing_a_protected_descendant() {
        let probe = FakeProbe::new();
        let rule_set = rules_with(
            r"C:\residue\item-1\credentials",
            RiskLevel::Protected,
            RuleSource::UserProtected,
            "user-protected/credentials",
        );
        let mut items = vec![item(
            1,
            RiskLevel::RegenerableLocal,
            CleanupAction::RecycleBin,
        )];
        items[0].path = Path::new("C:\\residue\\item-1").into();
        let sel = vec![crate::ScanItemId::from_raw(1)];
        let planner = CleanupPlanner::new_with_rules(
            validator(&probe),
            ConfirmPolicy::None,
            Arc::new(rule_set),
        );
        let out = planner.build(&items, &sel).unwrap();
        assert!(out.plan.items.is_empty());
        assert!(matches!(
            skip_kinds(&out)[&1],
            SkipReason::ProtectedDescendant { .. }
        ));
    }

    #[test]
    fn r08_planner_skips_rule_items_whose_rule_no_longer_matches() {
        let probe = FakeProbe::new();
        // Discovery rule existed at scan time and no longer matches (the
        // current gate has only an unrelated rule).
        let rule_set = rules_with(
            r"C:\unrelated",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            "unrelated",
        );
        let mut scanned = item(1, RiskLevel::Safe, CleanupAction::RecycleBin);
        scanned.source = SourceKind::Rule;
        scanned.evidence.push(
            crate::domain::evidence::Evidence::new("path-layout", "detection")
                .with_rule(crate::RuleId::from_raw(1)),
        );
        let sel = vec![crate::ScanItemId::from_raw(1)];
        let planner = CleanupPlanner::new_with_rules(
            validator(&probe),
            ConfirmPolicy::None,
            Arc::new(rule_set),
        );
        let out = planner.build(&[scanned], &sel).unwrap();
        assert!(out.plan.items.is_empty());
        assert!(matches!(
            skip_kinds(&out)[&1],
            SkipReason::DiscoverySourceInvalidated { .. }
        ));
    }

    #[test]
    fn r2f05_real_scan_item_without_snapshot_is_skipped() {
        // F05: a Real-scan item lacking a scan-time snapshot (legacy record /
        // probe failure) must be skipped — never live-captured and
        // re-authorised against the current object.
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_dir(Path::new("C:\\review\\legacy"));
            fs.create_file(Path::new("C:\\review\\legacy\\f"), None);
        }
        let mut scanned = item(1, RiskLevel::Safe, CleanupAction::RecycleBin);
        scanned.path = Path::new("C:\\review\\legacy").into();
        assert!(scanned.scan_snapshot.is_none());

        let planner =
            CleanupPlanner::new(validator(&probe), ConfirmPolicy::None).with_real_scan(true);
        let out = planner
            .build(&[scanned], &[crate::ScanItemId::from_raw(1)])
            .unwrap();
        assert!(
            out.plan.items.is_empty(),
            "real-scan item without a snapshot must not be planned"
        );
        assert!(matches!(
            skip_kinds(&out)[&1],
            SkipReason::MissingScanSnapshot
        ));

        // Control: the same item on the fixture/demo channel keeps the legacy
        // live-capture fallback.
        let demo = CleanupPlanner::new(validator(&probe), ConfirmPolicy::None);
        let scanned = item(1, RiskLevel::Safe, CleanupAction::RecycleBin);
        let out = demo
            .build(&[scanned], &[crate::ScanItemId::from_raw(1)])
            .unwrap();
        assert_eq!(out.plan.items.len(), 1);
    }
}
