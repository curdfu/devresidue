//! Kondo provider — project/build-artifact discovery through `kondo-lib`
//! (SPEC §13, INV-008: discovery only, its delete API is never called).

use std::path::{Path, PathBuf};

use devresidue_core::{CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, SourceKind};

use crate::classify::{dir_risk, is_local_rebuild_prefix};
use crate::measure::{measure_tree_parallel, path_is_reparse_like};
use crate::registry;
use crate::scan_ctx::{ProgressEvent, ScanContext};

pub const PROVIDER: &str = "kondo";

/// Default workspace-root candidates (see `resolve_workspace_roots`).
///
/// Keep automatic discovery limited to the conventional per-user source
/// directory. Machine-specific roots such as `C:\\Code` / `D:\\Code` must be
/// added explicitly in Settings so the user controls the scan boundary.
pub const DEFAULT_WORKSPACE_ROOT_CANDIDATES: [&str; 2] = [
    "%USERPROFILE%\\source",
    "%USERPROFILE%\\Projects",
];

/// Resolves the workspace roots: explicit list, then
/// `DEVRESIDUE_WORKSPACE_ROOTS` (semicolon-separated), then the default
/// candidates that actually exist.
pub fn resolve_workspace_roots(
    explicit: &[PathBuf],
    env_roots: Option<&str>,
    ctx: &ScanContext,
) -> Vec<PathBuf> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }
    if let Some(joined) = env_roots {
        if !joined.is_empty() {
            let parsed: Vec<PathBuf> = joined
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }
    let mut roots = Vec::new();
    for candidate in DEFAULT_WORKSPACE_ROOT_CANDIDATES {
        let path = expand_env_candidate(candidate, ctx);
        if path.is_dir() {
            roots.push(path);
        }
    }
    roots
}

/// Expands `%USERPROFILE%\source`-style candidate strings via the env map.
fn expand_env_candidate(candidate: &str, ctx: &ScanContext) -> PathBuf {
    if let Some(rest) = candidate.strip_prefix('%') {
        if let Some((key, tail)) = rest.split_once('%') {
            if let Some(value) = ctx.env(key) {
                return PathBuf::from(value).join(tail.trim_start_matches('\\'));
            }
        }
    }
    PathBuf::from(candidate)
}

/// Runs kondo discovery over every workspace root and maps each existing
/// artifact directory onto a [`ScanItem`].
///
/// Reports coarse progress: one start/done pair for the provider itself plus
/// one [`ProgressEvent::ProjectFound`] per project that yields artifacts.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let mut items = Vec::new();
    scan_with_sink(ctx, &mut |batch| items.extend(batch));
    items
}

/// Project-granular streaming driver (Phase 16).
///
/// Runs the same discovery as [`scan`], but delivers every project's items to
/// `sink` the moment that project has been measured — instead of returning
/// only when the *whole* workspace root finished. A UI consumer (the Tauri
/// shell) can therefore emit `scan://item` per project while a large workspace
/// is still walking, and still apply the F-2-1 rule gate / dedupe at the same
/// point the batch path does (per item, before emission).
///
/// Ordering contract: `sink` is invoked strictly in scan order and each batch
/// preserves the item order of [`scan`], so a caller aggregating batches
/// reproduces the exact [`ScanItem`] sequence (and id allocation order) of the
/// batch API.
pub fn scan_with_sink(ctx: &ScanContext, sink: &mut dyn FnMut(Vec<ScanItem>)) {
    if ctx.cancelled() {
        return;
    }
    ctx.progress(ProgressEvent::ProviderStart(PROVIDER));
    scan_impl(ctx, sink);
    if !ctx.cancelled() {
        ctx.progress(ProgressEvent::ProviderDone(PROVIDER));
    }
}

/// The actual walk (see [`scan`] / [`scan_with_sink`] for the wrappers).
fn scan_impl(ctx: &ScanContext, sink: &mut dyn FnMut(Vec<ScanItem>)) {
    let options = kondo_lib::ScanOptions {
        follow_symlinks: false,
        same_file_system: true,
    };

    for root in &ctx.workspace_roots {
        if !root.is_dir() || ctx.cancelled() {
            continue;
        }
        for result in kondo_lib::scan(root, &options) {
            if ctx.cancelled() {
                return;
            }
            match result {
                Ok(project) => {
                    let found = project_items(ctx, &project);
                    if !found.is_empty() {
                        ctx.progress(ProgressEvent::ProjectFound {
                            name: project_label(&project.path),
                            project_type: project.type_name().to_string(),
                        });
                    }
                    // Deliver this project's items immediately (streaming).
                    sink(found);
                }
                Err(err) => {
                    let detail = match &err {
                        kondo_lib::Red::IOError(e) => e.to_string(),
                        kondo_lib::Red::WalkdirError(e) => e.to_string(),
                    };
                    ctx.warn(format!("kondo scan error: {detail}"));
                }
            }
        }
    }
}

/// Human label for a project path (its directory name when present).
fn project_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// One project's existing artifact directories → ScanItems.
fn project_items(ctx: &ScanContext, project: &kondo_lib::Project) -> Vec<ScanItem> {
    let mut out = Vec::new();
    for name in project.artifact_dirs() {
        if ctx.cancelled() {
            break;
        }
        let dir = project.path.join(name);
        if !dir.exists() || path_is_reparse_like(&dir) {
            continue;
        }
        // Nested projects are skipped by kondo itself (skip_current_dir); the
        // shared seen-set is a second guarantee across providers.
        if !ctx.seen_insert(&dir) {
            continue;
        }
        let risk = if is_local_rebuild_prefix(name) || dir_risk(name) == RiskLevel::RegenerableLocal
        {
            RiskLevel::RegenerableLocal
        } else {
            RiskLevel::RegenerableDownload
        };
        let category = if risk == RiskLevel::RegenerableDownload {
            ResidueCategory::Dependency
        } else {
            ResidueCategory::BuildArtifact
        };

        let measure = measure_tree_parallel(&dir, ctx.cancel_fn());
        let reclaim_text = if risk == RiskLevel::RegenerableDownload {
            "deleting means re-downloading dependencies"
        } else {
            "deleting costs a local rebuild only"
        };
        let type_name = project.type_name();
        let explanation = format!(
            "kondo-discovered {type_name} project '{}' artifact directory '{}' \
             ({reclaim_text}).",
            project.path.display(),
            name
        );

        out.push(ScanItem {
            id: ctx.allocate_id(),
            path: dir,
            display_name: format!("{type_name} {name} artifacts"),
            product: Some(type_name.to_string()),
            category,
            risk,
            source: SourceKind::Kondo,
            logical_size: measure.logical_size,
            file_count: measure.file_count,
            last_modified: measure.last_modified,
            explanation,
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![
                Evidence::new(registry::evidence_tag(PROVIDER), "kondo project discovery"),
                Evidence::new(
                    "kondo-project-type",
                    format!("{type_name} ({:?})", project.project_type),
                ),
                Evidence::new("measure", format!("{} files counted", measure.file_count)),
            ],
            scan_snapshot: None, // filled by the scan assembler when a probe is wired
            classification_rule_id: None,
        });
    }
    out
}
