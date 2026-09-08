//! `create_cleanup_plan` / `execute_cleanup_plan` commands.
//!
//! Both are strictly ID-based (SPEC §20 / INV-013): the requests carry
//! `ScanItemId`s (plan build) or a `CleanupPlanId` (execution), never paths.
//! The planner/engine from core stay the single source of truth; this module
//! only maps their inputs/outputs onto the frontend DTOs and turns the
//! confirmation gate into a structured error the UI can react to.

use std::sync::Arc;

use devresidue_core::cleanup::engine::{CleanupEngine, EngineError, EngineOptions, ItemLookup};
use devresidue_core::cleanup::planner::PlannerOutput;
use devresidue_core::cleanup::{CleanupPlanner, PlanStore};
use devresidue_core::domain::ids::CleanupPlanId;
use devresidue_core::CleanupPlan;
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};
use tauri::{AppHandle, Emitter, State};

use crate::contract::{
    mode_label, session_dto, session_item_dto, CleanupItemPayload, CleanupPlanDto,
    CleanupSessionDto, CommandError, ConfirmPolicyArg, ErrorCode, PlannedItemDto, SkippedItemDto,
    EV_CLEANUP_ITEM,
};
use crate::state::AppState;
use crate::support;

/// `create_cleanup_plan` command: turns selected scan-item ids of the latest
/// scan into a persisted plan (per-item confirmation recorded).
///
/// R3-G03 / R4-H04: `scan_generation` is the generation the UI's selection
/// was made against (the frontend's scan store tracks it) and is
/// **required** — a missing pin must refuse the plan instead of silently
/// re-resolving ids against the newest snapshot's objects (a bare id list
/// cannot prove which scan it selected from).
#[tauri::command]
pub fn create_cleanup_plan(
    state: State<'_, AppState>,
    item_ids: Vec<u64>,
    policy: ConfirmPolicyArg,
    scan_generation: Option<u64>,
) -> Result<CleanupPlanDto, CommandError> {
    let model = state.model.lock().unwrap();
    let snapshot = model.latest().ok_or_else(|| {
        CommandError::new(
            ErrorCode::ScanNotFound,
            "cannot build a plan: no scan result yet",
        )
    })?;
    match scan_generation {
        Some(user_generation) if user_generation != snapshot.generation => {
            return Err(CommandError::new(
                ErrorCode::InvalidItem,
                format!(
                    "selection-generation-mismatch: the selection was made against scan \
                     generation {user_generation} but the latest scan is generation {}; \
                     re-scan and re-select",
                    snapshot.generation
                ),
            ));
        }
        // R4-H04: the pin is mandatory — a selection without it is refused.
        None => {
            return Err(CommandError::new(
                ErrorCode::InvalidItem,
                "selection-generation-missing: the plan request carries no scan \
                 generation; the UI must pin the generation its selection was made \
                 against",
            ));
        }
        Some(_) => {}
    }
    create_plan(model.data_dir(), snapshot, item_ids, policy)
}

/// Builds + persists the plan over the given snapshot.
pub fn create_plan(
    data_dir: &std::path::Path,
    snapshot: &ScanSnapshot,
    item_ids: Vec<u64>,
    policy: ConfirmPolicyArg,
) -> Result<CleanupPlanDto, CommandError> {
    let ids: Vec<_> = item_ids
        .iter()
        .map(|raw| devresidue_core::ScanItemId::from_raw(*raw))
        .collect();

    // R3-G04: plan-time validator with the workspace protection roots of the
    // snapshot being planned (scan-persisted ∪ current env) — a workspace
    // root can never be planned as a cleanup target.
    let validator = support::build_validator_with_roots(
        support::workspace_protection_roots(Some(snapshot)),
        Vec::new(),
    )
    .map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    let mut store = open_plan_store(data_dir)?;
    // R03/R08 rule gate at plan time.
    let scan_rules =
        support::load_scan_rules(data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    let planner =
        CleanupPlanner::new_with_rules(validator, support::policy_from(policy), scan_rules.rules)
            // R2-F03: bind the plan to the scan generation it was built against.
            .with_generation(snapshot.generation)
            // R2-F05: real scans require scan-time snapshots on every item; the
            // fixture/demo channel keeps the legacy live-capture fallback.
            .with_real_scan(!matches!(snapshot.mode, ScanMode::Fixtures));

    let (plan_id, output) = planner
        .build_and_store(&snapshot.items, &ids, &mut store)
        .map_err(|e| {
            use devresidue_core::cleanup::PlannerError;
            match e {
                PlannerError::UnknownItem(id) => CommandError::new(
                    ErrorCode::InvalidItem,
                    format!(
                        "item id {id} does not belong to the latest scan — re-scan then plan \
                         again"
                    ),
                ),
                PlannerError::NothingToPlan => CommandError::new(
                    ErrorCode::InvalidItem,
                    "no selected item survived risk gating under the given confirmation level",
                ),
                other => CommandError::new(ErrorCode::Engine, other.to_string()),
            }
        })?;

    Ok(plan_dto(plan_id.raw(), &output))
}

/// `execute_cleanup_plan` command: runs a persisted plan through the engine.
///
/// Runs on the async runtime's blocking pool (engine + WindowsDeletePort calls
/// are synchronous and may take a while for large trees); per-item outcomes
/// stream as `cleanup://item` events before the session DTO is returned.
#[tauri::command]
pub async fn execute_cleanup_plan(
    app: AppHandle,
    state: State<'_, AppState>,
    plan_id: u64,
    policy: ConfirmPolicyArg,
    dry_run: bool,
) -> Result<CleanupSessionDto, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        execute_plan(&data_dir, Some(&app), plan_id, policy, dry_run)
    })
    .await
    .map_err(|e| CommandError::new(ErrorCode::Engine, format!("cleanup task join failed: {e}")))?
}

/// Executes a plan and maps the outcome (confirmation gate included).
///
/// `app` is optional so unit tests can run without a live AppHandle; when
/// present every per-item result is emitted as `cleanup://item`.
pub fn execute_plan(
    data_dir: &std::path::Path,
    app: Option<&AppHandle>,
    plan_id: u64,
    policy: ConfirmPolicyArg,
    dry_run: bool,
) -> Result<CleanupSessionDto, CommandError> {
    let plan = load_plan(data_dir, plan_id)?;
    // F12/R02/R04: the engine re-derives every planned item's risk / mode /
    // confirmation from the *current scan* (authoritative item lookup) plus the
    // merged rule set. Both loadings are fail-closed.
    let scan_rules =
        support::load_scan_rules(data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    let latest = scan_store_load_latest(data_dir)?;
    // R3-G04: execution-time validator with the workspace protection roots
    // (scan-persisted ∪ the current process's configuration) — a workspace
    // root configured after the scan is honoured before this execution can
    // delete through it (SPEC §11 / INV-011).
    let validator = support::build_validator_with_roots(
        support::workspace_protection_roots(Some(&latest)),
        Vec::new(),
    )
    .map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    // R2-F03: enforce the plan's generation against the current scan.
    let current_generation = latest.generation;
    let items_by_id = latest.items;
    let lookup: std::sync::Arc<ItemLookup> =
        std::sync::Arc::new(move |id: devresidue_core::ScanItemId| {
            items_by_id.iter().find(|i| i.id == id).cloned()
        });
    let engine = CleanupEngine::new(
        Arc::new(WindowsDeletePort),
        validator,
        scan_rules.rules,
        lookup,
    );

    let journal = if journal_enabled() {
        Some(
            devresidue_core::journal::Journal::open_at(data_dir)
                .map_err(|e| CommandError::new(ErrorCode::Engine, e.to_string()))?,
        )
    } else {
        None
    };
    let options = EngineOptions {
        dry_run,
        confirm_policy: support::policy_from(policy),
        journal,
        // R06: wire the real shell as the tool-scope re-query port.
        tool_query: Some(Arc::new(support::ShellTool)),
        // R2-F03: refuse plans from a different scan generation.
        expected_scan_generation: Some(current_generation),
    };

    let session = engine
        .run(&plan, options)
        .map_err(map_engine_error)?
        .session;

    // Stream per-item outcomes (plan order == session order).
    if let Some(app) = app {
        for result in &session.items {
            let _ = app.emit(
                EV_CLEANUP_ITEM,
                CleanupItemPayload {
                    plan_id,
                    item: session_item_dto(result),
                },
            );
        }
    }

    Ok(session_dto(&session))
}

/// Loads a persisted plan (fail → `PlanNotFound`).
fn load_plan(data_dir: &std::path::Path, plan_id: u64) -> Result<CleanupPlan, CommandError> {
    let store = open_plan_store(data_dir)?;
    store.load(CleanupPlanId::from_raw(plan_id)).map_err(|e| {
        CommandError::new(
            ErrorCode::PlanNotFound,
            format!("cannot load plan {plan_id}: {e}"),
        )
    })
}

/// Opens the persisted-plan store under the data directory.
fn open_plan_store(data_dir: &std::path::Path) -> Result<PlanStore, CommandError> {
    PlanStore::open_at(data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e.to_string()))
}

/// Loads the latest scan snapshot (the authoritative item source for engine
/// re-derivation).
fn scan_store_load_latest(data_dir: &std::path::Path) -> Result<ScanSnapshot, CommandError> {
    scan_store::load(data_dir).map_err(|e| CommandError::new(ErrorCode::ScanNotFound, e))
}

/// Journaling is on by default; disable with `DEVRESIDUE_NO_JOURNAL=1` (same
/// as the CLI).
fn journal_enabled() -> bool {
    std::env::var("DEVRESIDUE_NO_JOURNAL").as_deref() != Ok("1")
}

/// Maps the engine's failure modes onto the structured command error.
fn map_engine_error(err: EngineError) -> CommandError {
    match err {
        EngineError::ConfirmationRequired { required, .. } => {
            CommandError::confirmation_required(required)
        }
        EngineError::NoRules => CommandError::new(ErrorCode::Engine, err.to_string()),
        EngineError::GenerationMismatch { .. } => {
            CommandError::new(ErrorCode::InvalidItem, err.to_string())
        }
    }
}

/// Turns a planner output into the frontend plan DTO (vocabulary mirrors the
/// CLI plan summary: recycle/delete/execute, none/redownload/review).
fn plan_dto(plan_id: u64, output: &PlannerOutput) -> CleanupPlanDto {
    CleanupPlanDto {
        plan_id,
        // Dry-run is always decided at execution time (the plan's legacy
        // dry_run field was removed in F15).
        dry_run: false,
        total_estimated_bytes: output.plan.total_estimated_bytes(),
        items: output
            .plan
            .items
            .iter()
            .map(|item| PlannedItemDto {
                scan_item_id: item.scan_item_id.raw(),
                action: mode_label(item.mode).to_string(),
                confirmation: crate::contract::confirm_label(item.confirmation).to_string(),
                estimated_size: item.estimated_size,
                path: item.snapshot.path.display().to_string(),
            })
            .collect(),
        skipped: output
            .skipped
            .iter()
            .map(|skip| SkippedItemDto {
                scan_item_id: skip.scan_item_id.raw(),
                path: skip.path.display().to_string(),
                reason: crate::contract::skip_reason_label(&skip.reason),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ConfirmPolicyArg as P;
    use devresidue_core::{CleanupAction, Evidence, ResidueCategory, RiskLevel, SourceKind};
    use devresidue_providers::fixtures::fixture_scan_items;
    use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};
    use std::time::{Duration, SystemTime};

    fn tmp_base(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dr-tauri-plan-{tag}-{}", std::process::id()))
    }

    fn snapshot_of(items: Vec<devresidue_core::ScanItem>) -> ScanSnapshot {
        // Fixtures mode: the planner keeps the legacy no-snapshot fallback for
        // these offline demo items (R2-F05 real-scan gating is exercised by
        // the core planner tests and the real CLI path).
        let mut snap = ScanSnapshot::new(ScanMode::Fixtures, items, vec![]);
        snap.scanned_at = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        snap
    }

    /// A real item that does NOT need to exist on disk (paths point at the
    /// sample profile) — the planner's snapshot capture falls back to the
    /// scan-time record, so this works fully offline.
    fn fixture_item(id: u64) -> devresidue_core::ScanItem {
        devresidue_core::ScanItem {
            id: devresidue_core::ScanItemId::from_raw(id),
            path: format!(r"C:\Users\demo\residue\fixture-{id}").into(),
            display_name: format!("fixture {id}"),
            product: Some("test".into()),
            category: ResidueCategory::BuildArtifact,
            risk: RiskLevel::RegenerableLocal,
            source: SourceKind::Kondo,
            logical_size: 1024,
            file_count: 1,
            last_modified: Some(SystemTime::now() - Duration::from_secs(3600)),
            explanation: "test item".into(),
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![Evidence::new("provider:test", "e")],
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    fn ids(items: &[devresidue_core::ScanItem]) -> Vec<u64> {
        items.iter().map(|i| i.id.raw()).collect()
    }

    #[test]
    fn create_plan_maps_planned_and_skipped_with_cli_vocabulary() {
        let data = tmp_base("create");
        std::fs::create_dir_all(&data).unwrap();

        // Real fixture scan: items 1..=5 (safe, redownload, local, protected,
        // review/deferred). Build with a redownload policy → 1/2/3 planned,
        // 4 protected-skip, 5 deferred-skip.
        let items = fixture_scan_items();
        let snapshot = snapshot_of(items);
        let all: Vec<u64> = (1..=5).collect();
        let dto = create_plan(&data, &snapshot, all, P::Redownload).expect("plan builds");

        assert_eq!(dto.plan_id, 1, "first persisted plan id");
        assert_eq!(dto.items.len(), 3);
        let by_id: std::collections::HashMap<u64, &PlannedItemDto> =
            dto.items.iter().map(|i| (i.scan_item_id, i)).collect();
        assert_eq!(by_id[&1].confirmation, "none");
        assert_eq!(by_id[&2].confirmation, "redownload");
        assert_eq!(by_id[&2].action, "execute", "npm cache is tool-native");
        assert_eq!(by_id[&3].confirmation, "none");
        assert_eq!(by_id[&3].action, "recycle");

        assert_eq!(dto.skipped.len(), 2);
        assert!(dto
            .skipped
            .iter()
            .any(|s| s.scan_item_id == 4 && s.reason.contains("protected")));
        assert!(dto
            .skipped
            .iter()
            .any(|s| s.scan_item_id == 5 && s.reason.contains("deferred")));
        assert!(dto.total_estimated_bytes > 0);

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn create_plan_invalid_item_and_missing_scan_are_structured_errors() {
        let data = tmp_base("create-err");
        std::fs::create_dir_all(&data).unwrap();

        let items = vec![fixture_item(1)];
        let snapshot = snapshot_of(items);
        let err = create_plan(&data, &snapshot, vec![999], P::Default).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidItem);
        assert!(err.message.contains("999"));

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn r3g03_selection_from_an_older_generation_is_refused() {
        // The user's UI selection was made against scan A (generation 7);
        // the authoritative snapshot is scan B (generation 8). The plan
        // build must refuse the ids instead of re-resolving them against
        // B's objects (cross-generation id aliasing, R3-G03).
        let data = tmp_base("g03");
        std::fs::create_dir_all(&data).unwrap();

        let mut snapshot = snapshot_of(vec![fixture_item(1)]);
        snapshot.generation = 8;

        let err =
            create_cleanup_plan_with_generation(&data, &snapshot, vec![1], P::Default, Some(7))
                .expect_err("a stale-generation selection must be refused");
        assert_eq!(err.code, ErrorCode::InvalidItem);
        assert!(
            err.message.contains("selection-generation-mismatch"),
            "got: {}",
            err.message
        );

        // A matching pin passes through to the normal planner flow.
        let dto =
            create_cleanup_plan_with_generation(&data, &snapshot, vec![1], P::Redownload, Some(8))
                .expect("matching generation plans normally");
        assert!(!dto.items.is_empty());

        // R4-H04: NO pin is now refused too — a bare id list cannot prove
        // which scan it selected from.
        let err =
            create_cleanup_plan_with_generation(&data, &snapshot, vec![1], P::Redownload, None)
                .expect_err("unpinned selection must be refused");
        assert_eq!(err.code, ErrorCode::InvalidItem);
        assert!(
            err.message.contains("selection-generation-missing"),
            "got: {}",
            err.message
        );

        let _ = std::fs::remove_dir_all(&data);
    }

    /// `create_cleanup_plan`'s generation gate, driven without a Tauri
    /// `State` (mirrors the command's logic over an explicit data dir).
    fn create_cleanup_plan_with_generation(
        data_dir: &std::path::Path,
        snapshot: &ScanSnapshot,
        item_ids: Vec<u64>,
        policy: P,
        scan_generation: Option<u64>,
    ) -> Result<CleanupPlanDto, CommandError> {
        match scan_generation {
            Some(user_generation) if user_generation != snapshot.generation => {
                return Err(CommandError::new(
                    ErrorCode::InvalidItem,
                    format!(
                        "selection-generation-mismatch: the selection was made against scan \
                         generation {user_generation} but the latest scan is generation {}; \
                         re-scan and re-select",
                        snapshot.generation
                    ),
                ));
            }
            None => {
                return Err(CommandError::new(
                    ErrorCode::InvalidItem,
                    "selection-generation-missing: the plan request carries no scan \
                     generation; the UI must pin the generation its selection was made \
                     against",
                ));
            }
            Some(_) => {}
        }
        create_plan(data_dir, snapshot, item_ids, policy)
    }

    #[test]
    fn execute_gate_maps_confirmation_required_before_any_deletion() {
        let data = tmp_base("gate");
        std::fs::create_dir_all(&data).unwrap();

        // Fixture scan; plan over item #2 (npm cache, needs redownload).
        let items = fixture_scan_items();
        let snapshot = snapshot_of(items);
        // The authoritative "latest scan" file must exist for the engine's
        // item re-derivation (R02/R04).
        scan_store::save(&data, &snapshot).expect("persist latest scan");
        let dto = create_plan(&data, &snapshot, vec![2], P::Redownload).expect("plan");
        let plan_id = dto.plan_id;

        // Executing with a lower level than the plan requires → structured
        // ConfirmationRequired error (never a panic, never a partial delete).
        let err = execute_plan(&data, None, plan_id, P::Default, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfirmationRequired);
        assert_eq!(err.required.as_deref(), Some("redownload"));

        // Dry-run may preview regardless of confirmation (SPEC §23) and never
        // deletes; the fixture target does not exist → validation denies with
        // target-missing, which surfaces as skipped items, not an error.
        let session = execute_plan(&data, None, plan_id, P::Default, true).expect("dry run ok");
        assert!(session.dry_run);
        assert_eq!(session.items.len(), 1);
        assert_eq!(session.items[0].status, "skipped");
        assert!(
            session.items[0]
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("target"),
            "missing fixture target must be denied by validation"
        );

        // Unknown plan id → PlanNotFound.
        let err = execute_plan(&data, None, plan_id + 500, P::Default, true).unwrap_err();
        assert_eq!(err.code, ErrorCode::PlanNotFound);

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn fixture_scan_persists_and_plan_reads_snapshot_like_cli() {
        // Guard: the full fixture scan → snapshot round trip (scan_store) used
        // by both plan paths stays consistent.
        let data = tmp_base("roundtrip");
        std::fs::create_dir_all(&data).unwrap();
        let items = fixture_scan_items();
        let snapshot = snapshot_of(items.clone());
        scan_store::save(&data, &snapshot).unwrap();
        let loaded = scan_store::load(&data).unwrap();
        assert_eq!(ids(&loaded.items), ids(&items));
        let _ = std::fs::remove_dir_all(&data);
    }
}
