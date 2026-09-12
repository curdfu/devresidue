//! `devresidue clean` — execute a persisted plan, or build-and-run `--safe`
//! over the most recent **real** scan (R09). Dry-run (SPEC §23) shares the
//! full validation pipeline and never calls the deletion port.
//!
//! R09: the old `--safe` shortcut planned straight over the built-in demo
//! fixtures (hard-coded synthetic paths). It now loads the persisted
//! `last-scan.json` like `plan` does, refuses fixture-mode snapshots, applies
//! the same cancelled-scan gate and selects only `Safe` / `RegenerableLocal`
//! items (identical semantics to `plan --safe`).

use std::path::Path;
use std::sync::Arc;

use devresidue_core::cleanup::engine::{CleanupEngine, EngineOptions, ItemLookup};
use devresidue_core::cleanup::planner::{ConfirmPolicy, PlannerOutput};
use devresidue_core::cleanup::CleanupPlanner;
use devresidue_core::domain::plan::ConfirmRequirement;
use devresidue_core::journal;
use devresidue_core::{RiskLevel, ScanItemId};
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};

use crate::plan_cmd;
use crate::scan::human_bytes;
use crate::support;
use crate::CleanOptions;

/// Runs the clean subcommand.
pub fn run(opts: CleanOptions) -> Result<(), String> {
    let base = support::data_dir()?;
    let _operation_lock = support::acquire_app_operation_lock(&base)?;
    let policy = support::policy_from(opts.confirm_redownload, opts.confirm_review, opts.yes);

    match (opts.plan, opts.safe) {
        (Some(_), true) => Err("choose either --plan <id> or --safe, not both".to_string()),
        (Some(id), false) => {
            let plan = load_plan(&base, id)?;
            run_plan(&base, &plan, opts.dry_run, policy)
        }
        (None, true) => safe_shortcut(&base, opts.dry_run, policy, opts.allow_partial),
        (None, false) => Err("clean requires --plan <id> or --safe".to_string()),
    }
}

/// `--safe` shortcut (R09): load the latest real scan, select only
/// `Safe` / `RegenerableLocal` items, plan with the current confirm policy
/// and run. Refuses fixture-mode snapshots and cancelled scans (unless
/// `--allow-partial`) — demo data and partial results never enter the formal
/// cleanup chain implicitly.
fn safe_shortcut(
    base: &Path,
    dry_run: bool,
    policy: ConfirmPolicy,
    allow_partial: bool,
) -> Result<(), String> {
    let (id, output) = build_safe_plan(base, policy, allow_partial)?;
    println!(
        "clean --safe: built plan {} ({} item(s)) from the most recent real scan",
        id.raw(),
        output.plan.items.len()
    );
    let plan = load_plan(base, id.raw())?;
    run_plan(base, &plan, dry_run, policy)
}

/// Error text when `clean --safe` runs with no persisted scan at all.
pub(crate) const NO_SCAN_ERROR: &str = "no scan results available; run 'devresidue scan' first";

/// Loads and gates the snapshot a `clean --safe` may operate on:
///
/// 1. a missing snapshot is a predictable error (never an implicit empty
///    plan);
/// 2. a fixture-mode snapshot (`scan --fixtures`) is refused — demo data must
///    not be cleaned through the formal path (R09);
/// 3. a cancelled (partial) snapshot is refused unless `--allow-partial`
///    (same gate as `plan`, F-6-1).
fn real_snapshot_for_safe_clean(base: &Path, allow_partial: bool) -> Result<ScanSnapshot, String> {
    let snapshot = scan_store::load(base).map_err(|e| format!("{NO_SCAN_ERROR} ({e})"))?;
    if matches!(snapshot.mode, ScanMode::Fixtures) {
        return Err(
            "refusing clean --safe: the most recent scan is demo fixture data \
             (`scan --fixtures`), not a real machine scan; run `devresidue scan` first"
                .to_string(),
        );
    }
    plan_cmd::reject_partial_scan(snapshot.cancelled, allow_partial)?;
    Ok(snapshot)
}

/// Builds (and persists) the plan behind `clean --safe`. Selecting only
/// `Safe` / `RegenerableLocal` items mirrors `plan --safe`'s "what enters the
/// plan" semantics; anything riskier stays out of the selection entirely.
fn build_safe_plan(
    base: &Path,
    policy: ConfirmPolicy,
    allow_partial: bool,
) -> Result<(devresidue_core::CleanupPlanId, PlannerOutput), String> {
    let snapshot = real_snapshot_for_safe_clean(base, allow_partial)?;
    // R3-G04: the scan-persisted workspace roots (protection context).
    let workspace_roots = support::workspace_protection_roots(base, Some(&snapshot));
    let items = snapshot.items;

    let selection: Vec<_> = items
        .iter()
        .filter(|i| matches!(i.risk, RiskLevel::Safe | RiskLevel::RegenerableLocal))
        .map(|i| i.id)
        .collect();
    if selection.is_empty() {
        return Err(
            "no Safe / RegenerableLocal items in the most recent scan — nothing to clean; \
             re-run `devresidue scan` if you expected results"
                .to_string(),
        );
    }

    // R3-G04: plan-time validator with the workspace protection roots
    // (scan-persisted ∪ current env).
    let validator = support::build_validator_with_roots(workspace_roots, Vec::new())?;
    // R03/R08 rule gate at plan time (protected-descendant pre-check and
    // rule-source revalidation) — same merged set the scan used.
    let scan_rules = support::load_scan_rules(base)?;
    let mut store = support::open_store_at(base)?;
    let planner = CleanupPlanner::new_with_rules(validator, policy, scan_rules.rules);
    let (id, output) = planner
        .build_and_store(&items, &selection, &mut store)
        .map_err(|e| e.to_string())?;
    Ok((id, output))
}

fn load_plan(base: &Path, id: u64) -> Result<devresidue_core::CleanupPlan, String> {
    let store = support::open_store_at(base)?;
    store
        .load(devresidue_core::CleanupPlanId::from_raw(id))
        .map_err(|e| format!("cannot load plan {id}: {e}"))
}

/// Shared execution path: build engine + journal, run, render.
///
/// No Ctrl-C handler is installed here on purpose: the engine is a
/// synchronous loop whose per-item atomicity is guaranteed by the DeletePort,
/// so a deletion in progress is never interrupted half-way. Ctrl-C keeps the
/// process's default terminating behaviour during plan/clean (only `scan`
/// installs a cancellation handler — see scan.rs).
fn run_plan(
    base: &Path,
    plan: &devresidue_core::CleanupPlan,
    dry_run: bool,
    policy: ConfirmPolicy,
) -> Result<(), String> {
    // F12 rule gate: the engine re-derives every item's risk through the same
    // merged rule set the scan uses. Loading is fail-closed.
    let scan_rules = support::load_scan_rules(base)?;
    // R02/R04 authoritative source: the engine re-derives each planned item's
    // mode / command / confirmation from the *current scan's* original items
    // (never from the editable plan record alone). A missing scan therefore
    // blocks execution of any plan.
    let snapshot = scan_store::load(base).map_err(|e| {
        format!(
            "cannot run clean: no scan results available to re-derive item \
             authorisation from ({e}); run `devresidue scan` first"
        )
    })?;
    // F03: the whole-plan generation gate — this execution is only valid for a
    // plan built from the *current* scan generation.
    let current_generation = snapshot.generation;
    // R3-G04: the execution-time validator carries the workspace protection
    // roots (scan-persisted ∪ the current process's configuration) so a
    // workspace root configured after the scan is honoured before this
    // execution can delete through it (SPEC §11 / INV-011).
    let validator = support::build_validator_with_roots(
        support::workspace_protection_roots(base, Some(&snapshot)),
        Vec::new(),
    )?;
    let lookup_items = snapshot.items;
    let lookup: Arc<ItemLookup> =
        Arc::new(move |id: ScanItemId| lookup_items.iter().find(|i| i.id == id).cloned());
    let engine = CleanupEngine::new(
        Arc::new(WindowsDeletePort),
        validator,
        scan_rules.rules,
        lookup,
    );

    let journal_on = journal_enabled();
    let records_before = if journal_on {
        Some(journal_record_count(base)?)
    } else {
        None
    };
    let journal = if journal_on {
        Some(journal::Journal::open_at(base).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let options = EngineOptions {
        dry_run,
        confirm_policy: policy,
        journal,
        // R06 tool-scope re-verification: the engine re-queries the package
        // manager for its live cache root before executing a tool-native
        // cleanup (same argv-structured adapter the scan used).
        tool_query: Some(Arc::new(support::ShellTool)),
        // F03: execution is authorised only for the current scan generation.
        expected_scan_generation: Some(current_generation),
    };
    let run = engine.run(plan, options).map_err(|e| e.to_string())?;

    render_session(plan, &run.session, dry_run);
    // Audit visibility (PLAN Phase 10): always say where the records went.
    if let Some(before) = records_before {
        let after = journal_record_count(base)?;
        println!(
            "journal: {} entries written to {}",
            after.saturating_sub(before),
            base.join("journal").display()
        );
    }
    if run.session.journal_degraded {
        eprintln!(
            "warning: journal degraded — some audit records could not be \
             written (cleanup itself was not rolled back)"
        );
    }
    Ok(())
}

/// Counts every journal record on disk (all shards).
fn journal_record_count(base: &Path) -> Result<usize, String> {
    journal::read_last(base, usize::MAX)
        .map(|records| records.len())
        .map_err(|e| e.to_string())
}

/// Journaling is on by default; disable with `DEVRESIDUE_NO_JOURNAL=1`.
fn journal_enabled() -> bool {
    std::env::var("DEVRESIDUE_NO_JOURNAL").as_deref() != Ok("1")
}

/// Renders the session per SPEC §23 vocabulary (dry) or real outcomes.
fn render_session(
    plan: &devresidue_core::CleanupPlan,
    session: &devresidue_core::CleanupSession,
    dry_run: bool,
) {
    println!(
        "\nSession {} (dry_run={dry_run}):",
        session.session_id.raw()
    );
    for (idx, item) in plan.items.iter().enumerate() {
        let result = session
            .items
            .get(idx)
            .expect("session has one result per plan item");
        let (verb, detail) = match &result.status {
            devresidue_core::CleanupStatus::Success => ("OK".to_string(), String::new()),
            devresidue_core::CleanupStatus::WouldExecute { mode } => {
                (would_verb(*mode), String::new())
            }
            devresidue_core::CleanupStatus::Skipped { reason } => {
                let verb = if dry_run { "Would Skip" } else { "SKIP" };
                (verb.to_string(), format!("reason: {reason}"))
            }
            devresidue_core::CleanupStatus::Failed { error } => {
                ("FAIL".to_string(), format!("error: {error}"))
            }
        };
        println!(
            "  {verb:<13} {:>10}  {:<16}  {}  {}",
            human_bytes(item.estimated_size),
            confirm_short(item.confirmation),
            result.path.display(),
            detail
        );
    }

    let totals = &session.totals;
    println!(
        "\nTotals: planned {} | succeeded {} | skipped {} | failed {} | reclaimed {} | journal_degraded {}",
        human_bytes(totals.planned_bytes),
        totals.succeeded,
        totals.skipped,
        totals.failed,
        human_bytes(totals.completed_bytes),
        session.journal_degraded,
    );
}

fn would_verb(mode: devresidue_core::CleanupMode) -> String {
    match mode {
        devresidue_core::CleanupMode::RecycleBin => "Would Recycle".to_string(),
        devresidue_core::CleanupMode::DirectDelete => "Would Delete".to_string(),
        devresidue_core::CleanupMode::ExternalCommand => "Would Execute".to_string(),
    }
}

fn confirm_short(req: ConfirmRequirement) -> &'static str {
    match req {
        ConfirmRequirement::None => "-",
        ConfirmRequirement::Redownload => "confirm:redownload",
        ConfirmRequirement::Review => "confirm:review",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::cleanup::planner::ConfirmPolicy;
    use devresidue_core::{
        CleanupAction, Evidence, ResidueCategory, ScanItem, ScanItemId, SourceKind,
    };
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Unique temp dir per test (removed on drop).
    struct Base(std::path::PathBuf);
    impl Base {
        fn new() -> Self {
            let unique = format!(
                "dr-clean-r09-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let mut p = std::env::temp_dir();
            p.push(unique);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn child(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Base {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A scan item whose path really exists under `base` (planner captures
    /// live snapshots through the real probes) or is a ghost for download /
    /// protected decoys.
    fn item(id: u64, risk: RiskLevel, path: &std::path::Path) -> ScanItem {
        ScanItem {
            id: ScanItemId::from_raw(id),
            path: path.to_path_buf(),
            display_name: format!("item {id}"),
            product: Some("r09-test".into()),
            category: ResidueCategory::DeveloperCache,
            risk,
            source: SourceKind::DeveloperCacheProvider,
            logical_size: 42,
            file_count: 1,
            last_modified: None,
            explanation: "r09 test item".into(),
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![Evidence::new("provider:test", "e")],
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    /// Writes a real-mode snapshot with the given items into `base`.
    fn save_real_snapshot(base: &Path, items: Vec<ScanItem>, cancelled: bool) {
        let mut snap = ScanSnapshot::new(
            ScanMode::Real {
                workspace_roots: vec![],
            },
            items,
            vec![],
        );
        snap.cancelled = cancelled;
        scan_store::save(base, &snap).expect("save snapshot");
    }

    #[test]
    fn missing_scan_reports_a_predictable_error() {
        let base = Base::new();
        let err = build_safe_plan(base.path(), ConfirmPolicy::None, false)
            .expect_err("no snapshot must fail");
        assert!(
            err.contains(NO_SCAN_ERROR),
            "error must carry the no-scan message: {err}"
        );
    }

    #[test]
    fn fixture_mode_snapshot_is_refused() {
        let base = Base::new();
        let snap = ScanSnapshot::new(ScanMode::Fixtures, vec![], vec![]);
        scan_store::save(base.path(), &snap).expect("save fixture snapshot");

        let err = build_safe_plan(base.path(), ConfirmPolicy::None, false)
            .expect_err("fixture data must be refused");
        assert!(
            err.contains("fixture"),
            "error must name the fixture mode: {err}"
        );
    }

    #[test]
    fn cancelled_snapshot_refused_unless_allow_partial() {
        let base = Base::new();
        let safe_dir = base.child("safe-cache");
        std::fs::create_dir(&safe_dir).unwrap();
        save_real_snapshot(base.path(), vec![item(1, RiskLevel::Safe, &safe_dir)], true);

        let err = build_safe_plan(base.path(), ConfirmPolicy::None, false)
            .expect_err("cancelled scan must be refused by default");
        assert_eq!(err, plan_cmd::PARTIAL_SCAN_ERROR, "reuses the plan gate");

        // --allow-partial opens the hatch and planning proceeds.
        let (_, output) = build_safe_plan(base.path(), ConfirmPolicy::None, true)
            .expect("--allow-partial proceeds");
        assert_eq!(output.plan.items.len(), 1, "safe item planned");
    }

    #[test]
    fn real_snapshot_selects_only_safe_and_regenerable_local_items() {
        let base = Base::new();
        let safe_dir = base.child("safe-cache");
        let local_dir = base.child("local-target");
        std::fs::create_dir(&safe_dir).unwrap();
        std::fs::create_dir(&local_dir).unwrap();
        let ghost = base.child("ghost-redownload");
        let ghost_protected = base.child("ghost-protected");
        let items = vec![
            item(1, RiskLevel::Safe, &safe_dir),
            item(2, RiskLevel::RegenerableLocal, &local_dir),
            item(3, RiskLevel::RegenerableDownload, &ghost),
            item(4, RiskLevel::Protected, &ghost_protected),
        ];
        save_real_snapshot(base.path(), items.clone(), false);

        let (_, output) =
            build_safe_plan(base.path(), ConfirmPolicy::None, false).expect("build safe plan");

        // Only the Safe + RegenerableLocal items entered the plan; the
        // download / protected decoys were never selected.
        let planned: Vec<u64> = output
            .plan
            .items
            .iter()
            .map(|i| i.scan_item_id.raw())
            .collect();
        assert_eq!(
            planned,
            vec![1, 2],
            "only safe/local items planned: {planned:?}"
        );
        assert!(output.skipped.is_empty(), "no skipped records expected");
    }

    #[test]
    fn real_snapshot_with_no_safe_items_is_a_clear_error() {
        let base = Base::new();
        let ghost = base.child("ghost-download");
        save_real_snapshot(
            base.path(),
            vec![item(1, RiskLevel::RegenerableDownload, &ghost)],
            false,
        );
        let err = build_safe_plan(base.path(), ConfirmPolicy::None, false)
            .expect_err("no cleanable items must be an error");
        assert!(err.contains("nothing to clean"), "clear message: {err}");
    }
}
