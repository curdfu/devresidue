//! Shared wiring for the Phase 6 CLI commands (plan / clean / journal).
//!
//! The CLI is the first real consumer of the ports & adapters split: it wires
//! the Windows implementations of the safety probes and the deletion port into
//! the core planner/engine. No path ever enters a command line: everything
//! flows through registered `ScanItemId`s and persisted `CleanupPlanId`s
//! (INV-013).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use devresidue_core::cleanup::planner::ConfirmPolicy;
use devresidue_core::cleanup::PlanStore;
use devresidue_core::journal;
use devresidue_core::rules::{load_rules, load_user_ignore_paths, RuleSet};
use devresidue_core::safety::probe::{PathProbe, ProcessProbe};
use devresidue_core::safety::process::ProcessGuard;
use devresidue_core::safety::{ProtectedRootRegistry, SafetyValidator};
use devresidue_core::safety::FileLock;
use devresidue_core::ExternalCommandSpec;
use devresidue_platform_windows::safety::{WindowsPathProbe, WindowsProcessProbe};
use devresidue_providers::scan_ctx::ToolQuery;
use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};

/// Env override for the DevResidue data root (plans/ + journal/).
const DATA_DIR_ENV: &str = "DEVRESIDUE_DATA_DIR";

/// The DevResidue data directory (override or `<exe dir>\Data`, portable-first).
pub fn data_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var(DATA_DIR_ENV) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    journal::default_data_dir()
}

/// Acquires the fixed application-data operation lock shared with the Tauri
/// shell. The lock file is intentionally outside resettable stores and is
/// never deleted by maintenance commands.
pub fn acquire_app_operation_lock(base: &Path) -> Result<Box<dyn FileLock>, String> {
    std::fs::create_dir_all(base)
        .map_err(|error| format!("create {}: {error}", base.display()))?;
    let path = base.join("app-data-operation.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    devresidue_platform_windows::filesystem::try_lock_file_exclusive(file)
}

/// Opens the plans store at an explicit data root (commands that already hold
/// the resolved base path — clean/plan build and load through this).
pub fn open_store_at(base: &Path) -> Result<PlanStore, String> {
    PlanStore::open_at(base).map_err(|e| e.to_string())
}

/// Real tool-query adapter over the Windows structured shell runner. Every
/// cache-dir query is argv-structured with a 5 s timeout (never a shell
/// string).
#[derive(Debug, Clone, Copy, Default)]
pub struct ShellTool;

impl ToolQuery for ShellTool {
    fn run(&self, executable: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
        let spec = ExternalCommandSpec::new(
            executable.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
            None,
            Some(timeout.as_secs().max(1)),
        );
        let outcome = devresidue_platform_windows::shell::run(&spec).map_err(|e| e.to_string())?;
        if outcome.timed_out {
            return Err(format!("{executable} cache-dir query timed out"));
        }
        match outcome.exit_code {
            Some(0) => Ok(outcome.stdout),
            other => Err(format!(
                "{executable} cache-dir query exited {other:?}: {}",
                outcome.stderr.trim()
            )),
        }
    }
}

/// R06 adapter: `ShellTool` doubles as the core `ToolQueryPort` so the
/// engine can re-query the live cache root before a tool-native cleanup
/// (identical argv-structured shell runner as the scan-time query).
impl devresidue_core::cleanup::port::ToolQueryPort for ShellTool {
    fn query_cache_root(
        &self,
        spec: &devresidue_core::domain::action::ToolQuerySpec,
    ) -> Option<String> {
        let args: Vec<&str> = spec.args.iter().map(|s| s.as_str()).collect();
        ToolQuery::run(
            self,
            &spec.executable,
            &args,
            std::time::Duration::from_secs(5),
        )
        .ok()
    }
}

/// Builds a `SafetyValidator` over the real Windows probes and the real
/// environment's protected-root registry.
///
/// R3-G04: the registry includes the **workspace protection roots** — both
/// the roots persisted in the scan the plan/execution operates on (the
/// protection context the scan ran with) and the *current* process's
/// configured roots (`DEVRESIDUE_WORKSPACE_ROOTS`, fail-open only in the
/// sense that no configuration means no extra workspace protection). The
/// union only adds protection, so a workspace configured between scan and
/// clean is honoured before anything can delete through it (SPEC §11,
/// INV-011 / Root Protection).
/// `build_validator` with explicit workspace roots (the scan-persisted set
/// plus the executing process's current set).
pub fn build_validator_with_roots(
    scan_roots: Vec<PathBuf>,
    current_roots: Vec<PathBuf>,
) -> Result<SafetyValidator, String> {
    let path_probe: Arc<dyn PathProbe + Send + Sync> = Arc::new(WindowsPathProbe);
    let process_probe: Arc<dyn ProcessProbe + Send + Sync> = Arc::new(WindowsProcessProbe);
    let registry = ProtectedRootRegistry::build(&|key| std::env::var(key).ok(), vec![])
        .map_err(|e| e.to_string())?
        .with_workspace_roots(scan_roots, current_roots);
    let guard = ProcessGuard::new(
        devresidue_core::safety::process::DEFAULT_PROCESS_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        product_process_map(),
    );
    Ok(SafetyValidator::new(
        path_probe,
        process_probe,
        registry,
        guard,
    ))
}

/// R3-G04: resolves the workspace protection roots for the plan/execution
/// side — the union of the scan snapshot's persisted roots (when readable)
/// and the current environment's `DEVRESIDUE_WORKSPACE_ROOTS` (a
/// semicolon-separated list, same format the scan side reads).
pub fn workspace_protection_roots(base: &Path, snapshot: Option<&ScanSnapshot>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(snapshot) = snapshot {
        if let ScanMode::Real { workspace_roots } = &snapshot.mode {
            roots.extend(workspace_roots.iter().map(PathBuf::from));
        }
    }
    let _ = base; // the env var is process-global; the data root is not consulted
    if let Ok(text) = std::env::var("DEVRESIDUE_WORKSPACE_ROOTS") {
        for part in text.split(';') {
            let part = part.trim();
            if !part.is_empty() {
                roots.push(PathBuf::from(part));
            }
        }
    }
    roots
}

/// Per-product process watch-lists (SPEC §12) registered into the process
/// guard: product label → tool processes whose running state gates cleanup.
pub fn product_process_map() -> std::collections::HashMap<String, Vec<String>> {
    use std::collections::HashMap;
    let mut map = HashMap::new();
    map.insert(
        "npm".to_string(),
        vec!["npm".to_string(), "node".to_string()],
    );
    map.insert("bun".to_string(), vec!["bun".to_string()]);
    map.insert(
        "pip".to_string(),
        vec!["pip".to_string(), "python".to_string()],
    );
    map.insert(
        "uv".to_string(),
        vec!["uv".to_string(), "python".to_string()],
    );
    map.insert("cargo".to_string(), vec!["cargo".to_string()]);
    map.insert(
        "NuGet".to_string(),
        vec![
            "msbuild".to_string(),
            "dotnet".to_string(),
            "nuget".to_string(),
        ],
    );
    // AI agent products (Phase 9, SPEC §12). Product label ↔ running tool.
    map.insert(
        "Codex CLI".to_string(),
        vec!["codex".to_string(), "node".to_string()],
    );
    map.insert(
        "Claude Code".to_string(),
        vec!["claude".to_string(), "node".to_string()],
    );
    map.insert(
        "OpenCode".to_string(),
        vec!["opencode".to_string(), "node".to_string()],
    );
    map.insert(
        "OMO Slim".to_string(),
        vec!["opencode".to_string(), "node".to_string()],
    );
    map.insert("Cursor".to_string(), vec!["cursor".to_string()]);
    map.insert("Windsurf".to_string(), vec!["windsurf".to_string()]);
    map
}

/// Resolves the CLI confirmation flags into a [`ConfirmPolicy`]
/// (highest flag wins; `--yes` dominates).
pub fn policy_from(confirm_redownload: bool, confirm_review: bool, yes: bool) -> ConfirmPolicy {
    if yes {
        ConfirmPolicy::All
    } else if confirm_review {
        ConfirmPolicy::Review
    } else if confirm_redownload {
        ConfirmPolicy::Redownload
    } else {
        ConfirmPolicy::None
    }
}

/// Parses a comma-separated list of scan-item ids.
pub fn parse_ids(text: &str) -> Result<Vec<devresidue_core::ScanItemId>, String> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let raw = part
            .parse::<u64>()
            .map_err(|_| format!("invalid scan item id '{part}' (expected a number)"))?;
        out.push(devresidue_core::ScanItemId::from_raw(raw));
    }
    if out.is_empty() {
        return Err("no item ids given".to_string());
    }
    Ok(out)
}

/// Loads the most recent scan snapshot (the plan/unknown/analyze commands all
/// operate on it).
pub fn latest_snapshot() -> Result<ScanSnapshot, String> {
    let base = data_dir()?;
    scan_store::load(&base).map_err(|e| {
        format!(
            "cannot read the most recent scan ({e}); run `devresidue scan` first \
             (ids refer to the latest scan only)"
        )
    })
}

/// Finds one item by id in the most recent scan (INV-013: commands operate on
/// ids, never on arbitrary paths).
pub fn find_latest_item(id: u64) -> Result<devresidue_core::ScanItem, String> {
    let snapshot = latest_snapshot()?;
    snapshot
        .items
        .into_iter()
        .find(|item| item.id.raw() == id)
        .ok_or_else(|| {
            format!(
                "item id {id} does not belong to the most recent scan — a newer scan \
                 replaced it; re-run `devresidue scan` then retry"
            )
        })
}

/// The F-2-1 scan rule assembly (M2 final review): the product rule registry
/// plus the user rules container.
///
/// Fail-closed: a rules directory that cannot be located, or any blocking rule
/// issue, aborts the scan — protection depends on the rules being present and
/// healthy.
pub struct ScanRules {
    pub rules: Arc<RuleSet>,
    /// Ignore-declaration anchor paths to pre-seed into the scan's seen-set.
    pub ignores: Vec<PathBuf>,
}

/// Loads the merged rule set (built-in registry first, then the user rules
/// container `<data_dir>/rules`) and the user ignore paths.
pub fn load_scan_rules(data_dir: &Path) -> Result<ScanRules, String> {
    let rules_dir = crate::rules_cmd::locate_rules_dir().ok_or_else(|| {
        "cannot locate `resources/rules` for the scan rule gate (F-2-1); set \
         DEVRESIDUE_RULES_DIR to point at it"
            .to_string()
    })?;
    let env_lookup = |name: &str| std::env::var(name).ok();
    let builtin = load_rules(&rules_dir, &env_lookup);
    if !builtin.is_clean() {
        return Err(format!(
            "built-in rules failed validation ({}): {} error(s) — refusing to scan \
             without a healthy rule gate",
            rules_dir.display(),
            builtin.error_count()
        ));
    }

    // Ensure the user rules container exists so its load is deterministic.
    let user_dir = devresidue_core::rules::user_rules_dir(data_dir);
    std::fs::create_dir_all(&user_dir)
        .map_err(|e| format!("create {}: {e}", user_dir.display()))?;

    let mut merged = builtin;
    let user_container = data_dir.join("rules");
    let user_set = load_rules(&user_container, &env_lookup);
    merged.merge(user_set);
    if !merged.is_clean() {
        return Err(format!(
            "user rules failed validation ({}): {} error(s) — refusing to scan",
            user_container.display(),
            merged.error_count()
        ));
    }

    let ignores = load_user_ignore_paths(&user_dir, &env_lookup);
    Ok(ScanRules {
        rules: Arc::new(merged),
        ignores,
    })
}
