//! Shared helpers for developer-cache providers: tool-reported path
//! verification (INV-011), candidate → [`ScanItem`] emission with shared
//! measurement + dedupe.

use std::path::{Path, PathBuf};

use devresidue_core::domain::action::{ScopeBinding, ToolQuerySpec};
use devresidue_core::safety::canonical;
use devresidue_core::{
    CleanupAction, Evidence, ExternalCommandSpec, ResidueCategory, RiskLevel, ScanItem, SourceKind,
};

use crate::classify;
use crate::measure::measure_tree_parallel;
use crate::registry;
use crate::scan_ctx::{ProgressEvent, ScanContext};

/// Large-measure echo threshold: after measuring a tree with at least this
/// many files the shared emitter reports a one-line size summary (instead of
/// per-file progress, SPEC §28). Below it the measure is fast enough to stay
/// silent.
pub const MEASURE_ECHO_MIN_FILES: u64 = 1000;

/// One cache directory a provider proposes to emit. The shared emitter turns
/// it into a [`ScanItem`] after measuring and de-duplicating it.
pub struct CacheCandidate {
    /// Provider slug (registry table), e.g. `"npm"`.
    pub provider: &'static str,
    /// Absolute cache directory to scan/report.
    pub path: PathBuf,
    /// Short human name, e.g. `"npm cache"`.
    pub title: &'static str,
    /// Product label shown in listings, e.g. `"npm"`.
    pub product: &'static str,
    pub risk: RiskLevel,
    /// Human explanation (never empty — SPEC explainability).
    pub explanation: String,
    pub action: CleanupAction,
    /// `PackageManager` when the path came from a tool query, else
    /// `DeveloperCacheProvider`.
    pub source: SourceKind,
    /// Extra evidence notes (each becomes an Evidence row).
    pub notes: Vec<String>,
}

/// Verdict of an INV-011 tool-reported path check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPathVerdict {
    /// Path is a plausible cache location.
    Accept {
        /// True when the path sits outside the three user-data roots and was
        /// only accepted with a warning.
        unusual: bool,
    },
    /// Path is refused (drive root / user profile / system root / an
    /// ancestor of one). The provider must be skipped.
    Reject { reason: String },
}

/// Verifies a tool-reported cache path (SPEC §11, INV-011).
///
/// A candidate is rejected when it normalises to one of the protected roots
/// (any drive root, `%USERPROFILE%`, `%SYSTEMROOT%`, `%PROGRAMFILES%`,
/// `%PROGRAMFILES(X86)%`, `%PROGRAMDATA%`), to an *ancestor* of one (deleting
/// the target would delete the root), or to a **workspace root** or an
/// ancestor of one (R07 — SPEC §11 explicitly refuses tool-reported workspace
/// roots; a tool that reports the whole workspace is not a cache provider).
///
/// A path that is not under `%USERPROFILE%` / `%LOCALAPPDATA%` / `%APPDATA%`
/// is accepted with `unusual = true` (recorded as a warning), not refused.
pub fn verify_tool_path(path: &Path, ctx: &ScanContext) -> ToolPathVerdict {
    let norm = canonical::normalize(path);

    // Drive roots: any A:\ .. Z:\.
    for letter in 'A'..='Z' {
        let root = PathBuf::from(format!("{letter}:\\"));
        if canonical::normalized_eq_path(&norm, &root) || canonical::is_within(&root, &norm) {
            return ToolPathVerdict::Reject {
                reason: format!("tool reported the drive root '{}'", root.display()),
            };
        }
    }

    // Well-known protected roots from the environment.
    for key in [
        "USERPROFILE",
        "SYSTEMROOT",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMDATA",
    ] {
        let Some(value) = ctx.env(key) else { continue };
        let root = PathBuf::from(value);
        if canonical::normalized_eq_path(&norm, &root) || canonical::is_within(&root, &norm) {
            return ToolPathVerdict::Reject {
                reason: format!(
                    "tool reported a protected root ({key} = '{}')",
                    root.display()
                ),
            };
        }
    }

    // R07: workspace roots are never valid cache locations — a tool reporting
    // the workspace root (or an ancestor of it) is refused outright.
    for ws_root in &ctx.workspace_roots {
        if canonical::is_within(ws_root, &norm) {
            return ToolPathVerdict::Reject {
                reason: format!(
                    "tool reported the workspace root '{}' (or an ancestor of it)",
                    ws_root.display()
                ),
            };
        }
    }

    // Unusual-location warning when not inside the three user-data roots.
    let unusual = !["USERPROFILE", "LOCALAPPDATA", "APPDATA"]
        .iter()
        .any(|key| {
            ctx.env(key)
                .map(|root| {
                    let root = PathBuf::from(&root);
                    canonical::is_within(&norm, root.clone())
                        || canonical::normalized_eq_path(&norm, root)
                })
                .unwrap_or(false)
        });

    ToolPathVerdict::Accept { unusual }
}

/// Why a tool query produced no verified path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoToolPath {
    /// The tool itself could not be run (missing / spawn failure / timeout):
    /// a tool-native cleanup command can never execute or re-verify. The
    /// provider may still emit the cache as a plain deletable item.
    ToolUnavailable,
    /// The tool ran but returned no usable path (empty output / no absolute
    /// path line). The tool EXISTS; a scoped command remains meaningful.
    NoUsableAnswer,
}

/// Runs the tool cache-dir query and applies INV-011.
///
/// Returns:
///
/// - `Ok(Some(path))` — an accepted tool-reported path (with any
///   unusual-location warning already emitted);
/// - `Ok(None)` — no tool result; the caller falls back to its known default
///   path and should consult [`query_no_path_reason`] to decide whether a
///   tool-scoped command is still meaningful;
/// - `Err(reason)` — the tool reported a refused path; the provider must skip
///   itself entirely (no default fallback — the tool's answer is suspect).
pub fn query_verified(
    ctx: &ScanContext,
    executable: &str,
    args: &[&str],
    label: &str,
) -> Result<Option<PathBuf>, String> {
    let output = match ctx.query_cache_dir(executable, args) {
        Ok(out) => out,
        Err(_) => {
            // Tool missing / failed / timed out → the caller falls back to its
            // known default. Recorded as a warning (PLAN Phase 10: fallbacks
            // must be user-visible, not just evidence).
            ctx.warn(format!(
                "{label} cache location from known default (tool query failed)"
            ));
            LAST_NO_PATH.with(|c| c.set(NoToolPath::ToolUnavailable));
            return Ok(None);
        }
    };
    let Some(line) = first_path_line(&output) else {
        ctx.warn(format!(
            "{label} cache location from known default (tool query returned no usable path)"
        ));
        LAST_NO_PATH.with(|c| c.set(NoToolPath::NoUsableAnswer));
        return Ok(None);
    };
    let candidate = PathBuf::from(line);
    match verify_tool_path(&candidate, ctx) {
        ToolPathVerdict::Accept { unusual } => {
            if unusual {
                ctx.warn(format!(
                    "{label}: tool-reported cache path '{}' is outside the standard \
                     user-data roots; accepting with extra caution",
                    candidate.display()
                ));
            }
            Ok(Some(candidate))
        }
        ToolPathVerdict::Reject { reason } => Err(reason),
    }
}

// Last "no path" reason from `query_verified` (thread-local: providers run
// on the scan thread). Consulted by the caller after an `Ok(None)` to decide
// whether a tool-scoped ExternalCommand is still meaningful — a MISSING tool
// can neither execute `uv cache clean` nor answer the execution-time scope
// re-query, so the fallback item must be a plain deletable entry instead.
thread_local! {
    static LAST_NO_PATH: std::cell::Cell<NoToolPath> =
        const { std::cell::Cell::new(NoToolPath::NoUsableAnswer) };
}

/// Why the most recent [`query_verified`] on this thread returned `Ok(None)`.
pub fn query_no_path_reason() -> NoToolPath {
    LAST_NO_PATH.with(|c| c.get())
}

/// First line of `output` that looks like an absolute Windows path.
fn first_path_line(output: &str) -> Option<String> {
    output.lines().map(str::trim).find_map(|line| {
        if line.is_empty() {
            return None;
        }
        // Paths may be wrapped by label text ("Cache Dir: C:\...\cache").
        let candidate = if line.contains('\\') {
            line.split_whitespace()
                .find(|tok| tok.contains('\\') || tok.starts_with('/'))
                .unwrap_or(line)
        } else {
            line
        };
        let candidate = candidate.trim_end_matches(['\r', '"', '\'']);
        (candidate.starts_with(|c: char| c.is_ascii_alphabetic())
            && candidate.as_bytes().get(1) == Some(&b':'))
        .then(|| candidate.to_string())
    })
}

/// Measures and emits one cache candidate as a [`ScanItem`].
///
/// Returns `None` when the path does not exist, was already emitted by an
/// earlier provider, or the scan was cancelled.
pub fn emit_candidate(ctx: &ScanContext, cand: CacheCandidate) -> Option<ScanItem> {
    if ctx.cancelled() || !cand.path.exists() {
        return None;
    }
    if !ctx.seen_insert(&cand.path) {
        return None;
    }
    let CacheCandidate {
        provider,
        path,
        title,
        product,
        risk,
        explanation,
        action,
        source,
        notes,
    } = cand;

    let measure = measure_tree_parallel(&path, ctx.cancel_fn());
    // Echo a size summary for very large trees (SPEC §28 spirit: no per-file
    // progress; a single post-measure summary line is the cheap alternative).
    if measure.file_count >= MEASURE_ECHO_MIN_FILES {
        ctx.progress(ProgressEvent::Measured {
            path: path.clone(),
            file_count: measure.file_count,
            logical_size: measure.logical_size,
        });
    }
    let last_modified = measure.last_modified;

    let mut evidence = vec![
        Evidence::new(registry::evidence_tag(provider), title),
        Evidence::new(
            "cache-measure",
            format!(
                "measured {} files, {} errors",
                measure.file_count, measure.error_count
            ),
        ),
    ];
    for note in notes {
        evidence.push(Evidence::new("cache-layout", note));
    }

    Some(ScanItem {
        id: ctx.allocate_id(),
        path,
        display_name: title.to_string(),
        product: Some(product.to_string()),
        category: category_of(risk),
        risk,
        source,
        logical_size: measure.logical_size,
        file_count: measure.file_count,
        last_modified,
        explanation,
        cleanup_action: action,
        evidence,
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
    })
}

/// Category derived from risk.
fn category_of(risk: RiskLevel) -> ResidueCategory {
    match risk {
        RiskLevel::RegenerableDownload | RiskLevel::Review => ResidueCategory::Dependency,
        RiskLevel::RegenerableLocal => ResidueCategory::BuildArtifact,
        _ => ResidueCategory::Unknown,
    }
}

/// Builds a tool-native cleanup command **frozen to the verified cache root**
/// (R06). The returned spec carries a `scope` binding whose `verify_query`
/// re-asks the tool where its cache lives at execution time; the engine
/// re-runs it and refuses on drift.
///
/// `verify_executable`/`verify_args` are the query command that reproduces the
/// cache root (e.g. `uv cache dir`); `working_dir` pins the child process to
/// the verified root when the tool supports it.
#[allow(clippy::too_many_arguments)]
pub fn tool_cache_command(
    executable: &str,
    clean_args: Vec<String>,
    verify_executable: &str,
    verify_args: Vec<String>,
    tool: &str,
    expected_cache_root: &Path,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
) -> CleanupAction {
    CleanupAction::ExternalCommand {
        command: ExternalCommandSpec::with_scope(
            executable.to_string(),
            clean_args,
            working_dir,
            Some(timeout_secs),
            ScopeBinding {
                tool: tool.to_string(),
                // Canonicalised at freeze time so the execution-time compare is
                // case/separator stable.
                expected_cache_root: canonical::normalize(expected_cache_root),
                verify_query: ToolQuerySpec {
                    executable: verify_executable.to_string(),
                    args: verify_args,
                },
            },
        ),
    }
}

/// Fallback action for a known-default cache location after the tool query
/// produced no verified path.
///
/// - tool MISSING (spawn/timeout failure): a tool-scoped command can never
///   execute and the execution-time scope re-query would hard-refuse with
///   `tool-scope-drift` (the uv/npm incident class) — return a plain
///   `RecycleBin` action so the leftover cache stays cleanable through the
///   engine's verified delete;
/// - tool answered but unusably: the tool EXISTS, so the scoped command (and
///   its execution-time re-verification) remains meaningful — return it.
#[allow(clippy::too_many_arguments)]
pub fn fallback_cache_action(
    tool: &str,
    clean_args: Vec<String>,
    verify_args: Vec<String>,
    default: &Path,
    working_dir: Option<PathBuf>,
    timeout_secs: u64,
) -> CleanupAction {
    if query_no_path_reason() == NoToolPath::ToolUnavailable {
        return CleanupAction::RecycleBin;
    }
    tool_cache_command(
        tool,
        clean_args,
        tool,
        verify_args,
        tool,
        default,
        working_dir,
        timeout_secs,
    )
}

/// Convenience: emits one candidate into a possibly-empty item vec.
pub fn emit(ctx: &ScanContext, cand: CacheCandidate) -> Vec<ScanItem> {
    emit_candidate(ctx, cand).into_iter().collect()
}

/// Convenience: Local/Dependency classification reused by both kondo and dev
/// cache splitting.
pub use classify::{dir_risk, is_local_rebuild_prefix};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_ctx::{EnvMap, NoTool};
    use std::collections::HashMap;

    fn env() -> EnvMap {
        let mut env = HashMap::new();
        env.insert("USERPROFILE".into(), r"C:\Users\alice".into());
        env.insert(
            "LOCALAPPDATA".into(),
            r"C:\Users\alice\AppData\Local".into(),
        );
        env.insert("APPDATA".into(), r"C:\Users\alice\AppData\Roaming".into());
        env.insert("SYSTEMROOT".into(), r"C:\Windows".into());
        env.insert("PROGRAMFILES".into(), r"C:\Program Files".into());
        env.insert("PROGRAMFILES(X86)".into(), r"C:\Program Files (x86)".into());
        env.insert("PROGRAMDATA".into(), r"C:\ProgramData".into());
        env
    }

    fn ws_ctx(workspace_roots: Vec<PathBuf>) -> ScanContext {
        ScanContext::with_env_and_progress(
            env(),
            Box::new(NoTool),
            workspace_roots,
            Box::new(|| true),
            Box::new(|_| {}),
        )
    }

    #[test]
    fn tool_missing_fallback_is_a_plain_deletable_action() {
        // NoTool: query_verified returns Ok(None) with the ToolUnavailable
        // reason -> the known-default fallback must NOT carry a scoped command
        // (its verify_query would hard-refuse at execution - the uv/npm
        // incident); it is a plain RecycleBin action.
        let ctx = ws_ctx(vec![]);
        let result = query_verified(
            &ctx,
            "definitely-missing-tool-xyz",
            &["cache", "dir"],
            "xyz",
        );
        assert!(
            matches!(result, Ok(None)),
            "no-tool query falls back: {result:?}"
        );
        assert_eq!(query_no_path_reason(), NoToolPath::ToolUnavailable);

        let action = fallback_cache_action(
            "definitely-missing-tool-xyz",
            vec!["cache".into(), "clean".into()],
            vec!["cache".into(), "dir".into()],
            Path::new("C:/Users/alice/AppData/Local/xyz-cache"),
            None,
            300,
        );
        assert!(
            matches!(action, CleanupAction::RecycleBin),
            "tool missing -> plain deletable item, got {action:?}"
        );
    }

    #[test]
    fn r07_workspace_root_and_its_ancestors_are_rejected() {
        let ws = PathBuf::from(r"D:\review\workspace");
        let ctx = ws_ctx(vec![ws.clone()]);

        // Equal to the workspace root → reject.
        assert!(matches!(
            verify_tool_path(&ws, &ctx),
            ToolPathVerdict::Reject { .. }
        ));
        // Ancestor of the workspace root → reject.
        assert!(matches!(
            verify_tool_path(Path::new(r"D:\review"), &ctx),
            ToolPathVerdict::Reject { .. }
        ));
        assert!(matches!(
            verify_tool_path(Path::new(r"D:\"), &ctx),
            ToolPathVerdict::Reject { .. }
        ));
        // A safe cache dir *inside* a sibling area is accepted (not a ws root).
        assert!(matches!(
            verify_tool_path(Path::new(r"D:\review\workspace\.cache\tool"), &ctx),
            ToolPathVerdict::Accept { .. }
        ));
    }

    #[test]
    fn r07_multiple_workspace_roots_and_case_folding() {
        let ctx = ws_ctx(vec![
            PathBuf::from(r"D:\review\workspace"),
            PathBuf::from(r"E:\projects\apps"),
        ]);
        // Second root, case-insensitive.
        assert!(matches!(
            verify_tool_path(Path::new(r"e:\PROJECTS\apps"), &ctx),
            ToolPathVerdict::Reject { .. }
        ));
        // Unrelated custom dir under the profile stays accepted.
        let custom = Path::new(r"C:\Users\alice\AppData\Local\my-tool-cache");
        assert!(matches!(
            verify_tool_path(custom, &ctx),
            ToolPathVerdict::Accept { unusual: false }
        ));
    }
}
