//! Wiring duplication of the CLI's `support.rs` — the Tauri shell plugs the
//! same real Windows adapters into the core planner/engine that the CLI uses.
//!
//! The content mirrors `crates/devresidue-cli/src/support.rs` (the shell may
//! not modify the crates, so the few lines of assembly live here instead).
//! Everything stays ID-based: no path ever enters a command line.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use devresidue_core::cleanup::planner::ConfirmPolicy;
use devresidue_core::journal;
use devresidue_core::rules::{load_rules, load_user_ignore_paths, RuleSet};
use devresidue_core::safety::probe::{PathProbe, ProcessProbe};
use devresidue_core::safety::process::ProcessGuard;
use devresidue_core::safety::{ProtectedRootRegistry, SafetyValidator};
use devresidue_core::ExternalCommandSpec;
use devresidue_platform_windows::safety::{WindowsPathProbe, WindowsProcessProbe};
use devresidue_providers::scan_ctx::ToolQuery;
use devresidue_providers::scan_store::ScanSnapshot;

/// Env override for the DevResidue data root (plans/ + journal/) — same knob
/// as the CLI, used by tests.
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

/// Real tool-query adapter over the Windows structured shell runner (same as
/// the CLI: argv-structured, 5 s timeout, never a shell string).
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

/// R06 adapter: `ShellTool` doubles as the core `ToolQueryPort` for the
/// engine's tool-scope re-verification (same argv-structured shell runner).
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
/// R3-G04: the registry includes the **workspace protection roots** — the
/// scan-persisted roots of the snapshot a plan/execution operates on plus
/// the current process's configured roots. The union only adds protection
/// (SPEC §11 / INV-011: a workspace root can never be a cleanup target,
/// including one configured between the scan and the clean).
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

/// R3-G04: the workspace protection roots for plan/execution — the union of
/// the scan snapshot's persisted roots and the current environment's
/// `DEVRESIDUE_WORKSPACE_ROOTS` (semicolon-separated, same format the scan
/// side reads).
pub fn workspace_protection_roots(snapshot: Option<&ScanSnapshot>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(snapshot) = snapshot {
        if let devresidue_providers::scan_store::ScanMode::Real { workspace_roots } = &snapshot.mode
        {
            roots.extend(workspace_roots.iter().map(PathBuf::from));
        }
    }
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

/// Per-product process watch-lists (SPEC §12). Copied verbatim from the CLI's
/// `support::product_process_map` so both shells gate on identical products.
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

/// Resolves the confirmation DTO into a core [`ConfirmPolicy`]
/// (highest granted level wins).
pub fn policy_from(confirm: crate::contract::ConfirmPolicyArg) -> ConfirmPolicy {
    use crate::contract::ConfirmPolicyArg as A;
    match confirm {
        A::Default => ConfirmPolicy::None,
        A::Redownload => ConfirmPolicy::Redownload,
        A::Review => ConfirmPolicy::Review,
        A::All => ConfirmPolicy::All,
    }
}

/// Env override for the rules directory (mirrors the CLI knob; the desktop
/// shell looks up `resources/rules` relative to the working directory first).
const RULES_DIR_ENV: &str = "DEVRESIDUE_RULES_DIR";

/// Locates the rules directory. Resolution order (portable-first):
///
/// 1. `DEVRESIDUE_RULES_DIR` env override (diagnostics);
/// 2. `resources/rules` **next to the running executable** (portable layout);
/// 3. `rules` next to the executable (flat portable layout);
/// 4. `resources/rules` found by walking up from the working directory (dev
///    mode).
pub fn locate_rules_dir() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(RULES_DIR_ENV) {
        let dir = PathBuf::from(custom);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            if let Some(found) = locate_rules_dir_at(exe_dir) {
                return Some(found);
            }
        }
    }
    let start = std::env::current_dir().ok()?;
    start.ancestors().find_map(|ancestor| {
        let candidate = ancestor.join("resources").join("rules");
        candidate.is_dir().then_some(candidate)
    })
}

/// Probes the exe-relative candidate locations under `base`:
/// `base/resources/rules` first (portable layout), then `base/rules`.
pub fn locate_rules_dir_at(base: &Path) -> Option<PathBuf> {
    let nested = base.join("resources").join("rules");
    if nested.is_dir() {
        return Some(nested);
    }
    let flat = base.join("rules");
    if flat.is_dir() {
        return Some(flat);
    }
    None
}

/// The F-2-1 scan rule assembly (built-in registry + user rules container).
pub struct ScanRules {
    pub rules: Arc<RuleSet>,
    pub ignores: Vec<PathBuf>,
}

/// Loads the merged rule set fail-closed before a scan starts (a scan without
/// a healthy rule gate is refused — protection depends on it).
pub fn load_scan_rules(data_dir: &Path) -> Result<ScanRules, String> {
    let rules_dir = locate_rules_dir().ok_or_else(|| {
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

    let user_dir = devresidue_core::rules::user_rules_dir(data_dir);
    std::fs::create_dir_all(&user_dir)
        .map_err(|e| format!("create {}: {e}", user_dir.display()))?;

    let mut merged = builtin;
    let user_set = load_rules(&data_dir.join("rules"), &env_lookup);
    merged.merge(user_set);
    if !merged.is_clean() {
        return Err(format!(
            "user rules failed validation ({}): {} error(s) — refusing to scan",
            data_dir.join("rules").display(),
            merged.error_count()
        ));
    }
    let ignores = load_user_ignore_paths(&user_dir, &env_lookup);
    Ok(ScanRules {
        rules: Arc::new(merged),
        ignores,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("dr-tauri-rules-loc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn portable_rules_locations_prefer_nested_then_flat() {
        let base = tmp("nested");
        std::fs::create_dir_all(base.join("resources").join("rules")).unwrap();
        std::fs::create_dir_all(base.join("rules")).unwrap();
        assert_eq!(
            locate_rules_dir_at(&base),
            Some(base.join("resources").join("rules"))
        );
        let _ = std::fs::remove_dir_all(&base);

        let flat = tmp("flat");
        std::fs::create_dir_all(flat.join("rules")).unwrap();
        assert_eq!(locate_rules_dir_at(&flat), Some(flat.join("rules")));
        let _ = std::fs::remove_dir_all(&flat);

        let none = tmp("none");
        assert_eq!(locate_rules_dir_at(&none), None);
        let _ = std::fs::remove_dir_all(&none);
    }
}
