//! AI agent data providers -- first batch (SPEC §9): Codex / Claude Code /
//! OpenCode / OMO Slim / Cursor / Windsurf.
//!
//! One parameterised implementation ([`AgentLayoutProvider`]) drives six
//! agents from the declarative tables in [`layout`]/[`layouts`]. The agent
//! root is **never** emitted as one SAFE item -- only the fine-grained
//! sub-entries are. Protected faces (auth/config/secrets) are not in the
//! tables at all; `resources/rules/protected/*.yaml` covers them.
//!
//! Discovery only (INV-009): providers never delete; cleanup_action defaults
//! to `RecycleBin`, except OpenCode's `opencode.db` and every glob-aggregated
//! entry, whose action is `None` with an explicit reason (a glob aggregate's
//! path is its *parent* directory, so a dir-level action would over-reach —
//! F-1e).

pub mod layout;
pub mod layouts;

use std::path::{Path, PathBuf};

use devresidue_core::{CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, SourceKind};

use crate::measure::{measure_tree_parallel, path_is_reparse_like};
use crate::registry;
use crate::scan_ctx::{ProgressEvent, ScanContext};

use layout::{AgentLayout, Entry, Kind};
use layouts::ALL;

/// A scan over every agent layout, reporting one start/done pair per agent
/// through the context (coarse progress, PLAN Phase 10). Cancellation takes
/// effect between agents and between layout entries.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let mut items = Vec::new();
    for agent in ALL {
        if ctx.cancelled() {
            break;
        }
        let provider = AgentLayoutProvider::new(agent);
        ctx.progress(ProgressEvent::ProviderStart(agent.slug));
        items.extend(provider.scan(ctx));
        if ctx.cancelled() {
            break;
        }
        ctx.progress(ProgressEvent::ProviderDone(agent.slug));
    }
    items
}
/// Parameterised provider over one layout table.
pub struct AgentLayoutProvider {
    layout: &'static AgentLayout,
}

impl AgentLayoutProvider {
    /// New provider for a layout.
    pub const fn new(layout: &'static AgentLayout) -> Self {
        Self { layout }
    }

    /// The layout table (tests inspect this).
    #[must_use]
    pub const fn layout(&self) -> &'static AgentLayout {
        self.layout
    }

    /// Environment view derived from the injected ScanContext.
    fn env_from<'a>(&'a self, ctx: &'a ScanContext) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| ctx.env(key)
    }

    /// Runs the discovery for this one agent. An absent root is fine (empty).
    pub fn scan(&self, ctx: &ScanContext) -> Vec<ScanItem> {
        let env = self.env_from(ctx);
        let mut items = Vec::new();

        // OMO is special-cased: it adds extras over the shared OpenCode tree.
        if self.layout.slug == "omo" {
            return self.scan_omo(ctx);
        }

        let Some(root) = self.layout.root_path(&env) else {
            return items;
        };
        if !root.is_dir() {
            return items;
        }

        for entry in self.layout.entries {
            if ctx.cancelled() {
                break;
            }
            let target = root.join(entry.rel);
            if let Some(item) = self.emit_entry(ctx, &target, entry, &root) {
                items.push(item);
            }
        }

        // Alternate root (OpenCode's `~\.cache\opencode` cache half).
        if let Some(alt) = self.layout.alt_root_path(&env) {
            if alt.is_dir() {
                items.extend(self.emit_alt_root(ctx, &alt));
            }
        }

        items
    }

    /// Special handling for the OpenCode layout (its storage/log split plus
    /// OMO's own entries inside it).
    fn scan_omo(&self, ctx: &ScanContext) -> Vec<ScanItem> {
        // The OMO layout's root/alt point at two *containers*; discovery is
        // delegated onto OpenCode's layout entries filtered to OMO's paths:
        // storage/oh-my-opencode-slim + log/oh-my-opencode-slim*.log.
        let env = self.env_from(ctx);
        let mut items = Vec::new();

        // 1. OMO workspace state under OpenCode storage.
        let share_root = match env("USERPROFILE") {
            Some(profile) => PathBuf::from(profile)
                .join(".local")
                .join("share")
                .join("opencode"),
            None => return items,
        };
        let storage_omo = share_root.join("storage").join("oh-my-opencode-slim");
        if storage_omo.is_dir() {
            if let Some(item) = self.emit_raw(
                ctx,
                &storage_omo,
                "OMO Slim workspace state",
                "OMO Slim workspace state (aggregated over project hash dirs)",
                ResidueCategory::WorkspaceState,
                RiskLevel::Review,
                "storage/oh-my-opencode-slim under the OpenCode share root",
            ) {
                items.push(item);
            }
        }

        // 2. OMO logs glob under OpenCode log dir.
        let log_dir = share_root.join("log");
        if log_dir.is_dir() {
            let entry = layout::glob_files(
                "",
                "oh-my-opencode-slim*.log",
                "OMO Slim aggregated logs",
                ResidueCategory::Log,
                RiskLevel::Safe,
                "OMO Slim logs",
            );
            if let Some(item) = self.emit_entry(ctx, &log_dir, &entry, &share_root) {
                items.push(item);
            }
        }
        items
    }

    fn emit_alt_root(&self, ctx: &ScanContext, alt: &Path) -> Vec<ScanItem> {
        let mut out = Vec::new();
        if self.layout.slug == "opencode" {
            // ~\.cache\opencode is one cleanable cache unit.
            if let Some(item) = self.emit_raw(
                ctx,
                alt,
                "OpenCode cache",
                "AI-agent cache data",
                ResidueCategory::AiAgent,
                RiskLevel::Safe,
                "cache root of the OpenCode layout",
            ) {
                out.push(item);
            }
        }
        out
    }

    /// Measures and emits one layout entry (dir / single file / glob cluster).
    fn emit_entry(
        &self,
        ctx: &ScanContext,
        target: &Path,
        entry: &Entry,
        root: &Path,
    ) -> Option<ScanItem> {
        match &entry.kind {
            Kind::Dir => {
                if !target.is_dir() || path_is_reparse_like(target) {
                    return None;
                }
                self.emit_raw(
                    ctx,
                    target,
                    entry.title,
                    entry.purpose,
                    entry.category,
                    entry.risk,
                    &format!(
                        "sub-layout {} of {}",
                        target.strip_prefix(root).ok()?.display(),
                        self.layout.product
                    ),
                )
            }
            Kind::File => {
                if !target.is_file() || path_is_reparse_like(target) {
                    return None;
                }
                let item = self.emit_raw(
                    ctx,
                    target,
                    entry.title,
                    entry.purpose,
                    entry.category,
                    entry.risk,
                    &format!(
                        "single-file data store {} of {}",
                        target.file_name()?.to_string_lossy(),
                        self.layout.product
                    ),
                )?;
                // Special case: OpenCode's session DB must not be recycled by
                // a raw directory action -- tool-native management is required.
                if self.layout.slug == "opencode" && entry.title == "OpenCode session database" {
                    return Some(ScanItem {
                        cleanup_action: CleanupAction::None,
                        explanation: format!(
                            "{} -- {}. The session database is managed by OpenCode itself; \
                             no direct deletion (tool-native cleanup preferred).",
                            item.explanation, entry.purpose
                        ),
                        ..item
                    });
                }
                Some(item)
            }
            Kind::GlobFiles { name_glob } => self.emit_glob_cluster(ctx, target, name_glob, entry),
        }
    }

    /// Aggregates files under `parent` matching `name_glob` into one item.
    fn emit_glob_cluster(
        &self,
        ctx: &ScanContext,
        parent: &Path,
        name_glob: &str,
        entry: &Entry,
    ) -> Option<ScanItem> {
        let matches: Vec<PathBuf> = std::fs::read_dir(parent)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                if !p.is_file() {
                    return false;
                }
                match p.file_name() {
                    Some(name) => layout::name_matches(name_glob, &name.to_string_lossy()),
                    None => false,
                }
            })
            .collect();
        if matches.is_empty() {
            return None;
        }
        // Measure each file individually; aggregate sizes/counts.
        let mut size = 0u64;
        let mut count = 0u64;
        let mut newest = None;
        for f in &matches {
            if let Ok(meta) = std::fs::metadata(f) {
                size += meta.len();
                count += 1;
                if let Ok(m) = meta.modified() {
                    newest = Some(newest.map_or(m, |old: std::time::SystemTime| old.max(m)));
                }
            }
        }
        // Dedupe on a composite key (parent + glob) so a glob aggregation can
        // coexist with a whole-directory item for the same parent.
        let composite = format!("{}//glob:{name_glob}", parent.display());
        if !ctx.seen_insert(Path::new(&composite)) {
            return None;
        }
        Some(ScanItem {
            id: ctx.allocate_id(),
            path: parent.to_path_buf(),
            display_name: entry.title.to_string(),
            product: Some(self.layout.product.to_string()),
            category: entry.category,
            risk: entry.risk,
            source: SourceKind::AgentProvider,
            logical_size: size,
            file_count: count,
            last_modified: newest,
            explanation: format!(
                "{} -- {} ({} matching '{}'). Aggregated glob entry: the item's path is \
                 the parent directory, so deleting it would remove the whole directory \
                 (including non-matching logs); select the parent log-directory entry to \
                 clean the matched files. {}",
                self.layout.product,
                entry.purpose,
                count,
                name_glob,
                impact_text(entry.risk)
            ),
            // F-1e: never let a glob aggregate be recycled at directory level —
            // the engine would delete the whole parent (e.g. the OpenCode log
            // directory with its main log) instead of just the matched files.
            cleanup_action: CleanupAction::None,
            scan_snapshot: None, // filled by the scan assembler when a probe is wired
            classification_rule_id: None,
            evidence: vec![
                Evidence::new(registry::evidence_tag(self.layout.slug), entry.title),
                Evidence::new(
                    "agent-layout",
                    format!("glob {name_glob} under {}", parent.display()),
                ),
            ],
        })
    }

    /// Builds a measured item for one path (no existence checks -- caller does
    /// them). Dedupes through the shared seen-set.
    #[allow(clippy::too_many_arguments)]
    fn emit_raw(
        &self,
        ctx: &ScanContext,
        path: &Path,
        title: &str,
        purpose: &str,
        category: ResidueCategory,
        risk: RiskLevel,
        note: &str,
    ) -> Option<ScanItem> {
        if !ctx.seen_insert(path) {
            return None;
        }
        let measure = if path.is_file() {
            match std::fs::metadata(path) {
                Ok(meta) => crate::measure::Measure {
                    logical_size: meta.len(),
                    file_count: 1,
                    last_modified: meta.modified().ok(),
                    error_count: 0,
                },
                Err(_) => return None,
            }
        } else {
            measure_tree_parallel(path, ctx.cancel_fn())
        };
        if ctx.cancelled() {
            return None;
        }

        Some(ScanItem {
            id: ctx.allocate_id(),
            path: path.to_path_buf(),
            display_name: title.to_string(),
            product: Some(self.layout.product.to_string()),
            category,
            risk,
            source: SourceKind::AgentProvider,
            logical_size: measure.logical_size,
            file_count: measure.file_count,
            last_modified: measure.last_modified,
            explanation: format!(
                "{} -- {}. {} ({})",
                self.layout.product,
                purpose,
                impact_text(risk),
                note
            ),
            cleanup_action: CleanupAction::RecycleBin,
            scan_snapshot: None, // filled by the scan assembler when a probe is wired
            classification_rule_id: None,
            evidence: vec![
                Evidence::new(registry::evidence_tag(self.layout.slug), title),
                Evidence::new("agent-layout", note),
            ],
        })
    }
}

/// Human impact text for a risk class.
fn impact_text(risk: RiskLevel) -> String {
    match risk {
        RiskLevel::Safe => "Deleting loses no lasting data (cache/logs/temp).".to_string(),
        RiskLevel::Review => {
            "May hold user-valued history/state -- requires explicit review before cleanup."
                .to_string()
        }
        RiskLevel::RegenerableDownload => "Deleting triggers re-downloads.".to_string(),
        RiskLevel::RegenerableLocal => "Deleting costs a local rebuild.".to_string(),
        _ => "Not eligible for automatic cleanup.".to_string(),
    }
}

/// Re-export RootSource for convenience.
pub use layout::RootSource as AgentRootSource;
