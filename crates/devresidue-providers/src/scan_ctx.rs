//! Scan context — everything a discovery provider may depend on, injected by
//! the caller (CLI wires real tools/env; tests inject fakes).
//!
//! Centralising the environment + tool access here keeps providers pure:
//! they declare *what* they need (a cache-dir query, a cancellation check, a
//! warn channel) and the caller decides *how* to provide it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use devresidue_core::rules::{resolve as resolve_rules, ResolvedRule, RuleSet};
use devresidue_core::safety::canonical;
use devresidue_core::safety::probe::PathProbe;
use devresidue_core::{Evidence, RiskLevel, ScanItem, ScanItemId, SourceKind};

/// Process-global scan-item id allocator (R04).
///
/// Ids are monotonic across scans **within one process**: re-running a scan
/// never re-uses an id from an earlier run, so a stale UI/CLI selection cannot
/// silently point at a different object of a later scan. Cross-process
/// uniqueness is layered on top by the persisted scan `generation` (see
/// `scan_store`) — the pair `(generation, item_id)` is unique process-wide.
static NEXT_ITEM_ID: AtomicU64 = AtomicU64::new(0);

/// Tool-query port: runs one short-lived read-only child process and returns
/// its trimmed stdout on success. No shell is ever involved (argv is passed
/// individually, SPEC §22 style); providers use this only to ask a package
/// manager where its cache lives.
pub trait ToolQuery {
    /// Runs `executable args...`, waits up to `timeout`, and returns the
    /// process stdout. Errors (spawn failure, non-zero exit, timeout) are
    /// reported as `Err` — callers fall back to their known defaults.
    fn run(&self, executable: &str, args: &[&str], timeout: Duration) -> Result<String, String>;
}

/// Coarse progress events emitted by providers while a scan runs.
///
/// Progress is strictly cosmetic — it never affects scan results. The CLI
/// renders these to stderr (`--quiet` suppresses them); a future Tauri layer
/// could forward them as an event stream (SPEC §28 "UI 不阻塞"). Providers
/// emit at coarse boundaries only: one event per provider scan, per kondo
/// project, or after a very large directory measurement — never one event per
/// file (SPEC §28 forbids one-task-per-file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// A provider scan started (registry slug, e.g. `"npm"`).
    ProviderStart(&'static str),
    /// A provider scan finished without cancellation.
    ProviderDone(&'static str),
    /// kondo found one project with cleanable artifact directories.
    ProjectFound {
        /// Project directory name (human label).
        name: String,
        /// Project kind from kondo (e.g. `"Cargo"` / `"Node"`).
        project_type: String,
    },
    /// A large directory measurement finished — echoed as a size summary
    /// instead of per-file progress (only when the walk was big).
    Measured {
        path: PathBuf,
        file_count: u64,
        logical_size: u64,
    },
}

/// A tool query that is never available (unit tests / offline runs fall back
/// to known defaults).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTool;

impl ToolQuery for NoTool {
    fn run(&self, executable: &str, _args: &[&str], _timeout: Duration) -> Result<String, String> {
        Err(format!(
            "tool '{executable}' is not available in this context"
        ))
    }
}

/// A static view of the process environment captured once per scan (so tests
/// can inject a fixed map).
pub type EnvMap = HashMap<String, String>;

/// Captures the real process environment as an [`EnvMap`].
pub fn real_env() -> EnvMap {
    std::env::vars().collect()
}

/// Everything a provider scan may consult. Providers never read the process
/// environment directly — they go through this snapshot.
pub struct ScanContext {
    env: EnvMap,
    /// Workspace roots for project/artifact discovery.
    pub workspace_roots: Vec<PathBuf>,
    /// Tool cache-dir queries (fall back to defaults when unavailable).
    tool: Box<dyn ToolQuery>,
    /// Cancellation: checked between projects / during walks. When it returns
    /// `false` providers stop emitting new items. Held as an `Arc` so the
    /// parallel measurement walker can share it across worker threads
    /// (Phase 16); every injected closure must therefore be `Send + Sync`.
    should_continue: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Coarse progress channel (PLAN Phase 10). The caller decides how to
    /// render it; the no-op default keeps existing callers unchanged.
    progress: Box<dyn Fn(ProgressEvent)>,
    /// Warning channel surfaced at the end of a scan (INV-011 rejections etc).
    warnings: RefCell<Vec<String>>,
    /// Cross-provider seen-set keyed by canonical, case-folded path so the
    /// same directory is never reported twice.
    seen: RefCell<HashSet<String>>,
    /// Resolve rules (built-in + user dispositions) for F-2-1 protection.
    /// Loaded by the scan assembler once per scan; `None` = rule layer not
    /// consulted.
    rules: Option<Arc<RuleSet>>,
    /// Optional identity/attributes probe (R01). When injected by the scan
    /// assembler, every emitted item is fingerprinted at *scan* time and the
    /// [`ScanItem::scan_snapshot`] authorisation record is filled. `None`
    /// (fixtures/tests without a probe) leaves the field `None`.
    probe: Option<Arc<dyn PathProbe + Send + Sync>>,
}

impl ScanContext {
    /// Real-environment context over the given tool and workspace roots.
    pub fn real(
        tool: Box<dyn ToolQuery>,
        workspace_roots: Vec<PathBuf>,
        should_continue: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self::real_with_progress(tool, workspace_roots, should_continue, noop_progress())
    }

    /// Real-environment context with a coarse progress callback.
    pub fn real_with_progress(
        tool: Box<dyn ToolQuery>,
        workspace_roots: Vec<PathBuf>,
        should_continue: Box<dyn Fn() -> bool + Send + Sync>,
        progress: Box<dyn Fn(ProgressEvent)>,
    ) -> Self {
        Self::new(real_env(), tool, workspace_roots, should_continue, progress)
    }

    /// Test context: fully injected environment.
    pub fn with_env(
        env: EnvMap,
        tool: Box<dyn ToolQuery>,
        workspace_roots: Vec<PathBuf>,
        should_continue: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self::with_env_and_progress(env, tool, workspace_roots, should_continue, noop_progress())
    }

    /// Test context with a coarse progress callback.
    pub fn with_env_and_progress(
        env: EnvMap,
        tool: Box<dyn ToolQuery>,
        workspace_roots: Vec<PathBuf>,
        should_continue: Box<dyn Fn() -> bool + Send + Sync>,
        progress: Box<dyn Fn(ProgressEvent)>,
    ) -> Self {
        Self::new(env, tool, workspace_roots, should_continue, progress)
    }

    fn new(
        env: EnvMap,
        tool: Box<dyn ToolQuery>,
        workspace_roots: Vec<PathBuf>,
        should_continue: Box<dyn Fn() -> bool + Send + Sync>,
        progress: Box<dyn Fn(ProgressEvent)>,
    ) -> Self {
        Self {
            env,
            workspace_roots,
            tool,
            should_continue: Arc::from(should_continue),
            progress,
            warnings: RefCell::new(Vec::new()),
            seen: RefCell::new(HashSet::new()),
            rules: None,
            probe: None,
        }
    }

    /// Installs the identity probe (R01): when present, every item this scan
    /// emits is fingerprinted at scan time.
    pub fn set_probe(&mut self, probe: Arc<dyn PathProbe + Send + Sync>) {
        self.probe = Some(probe);
    }

    /// Whether an identity probe is installed.
    pub fn has_probe(&self) -> bool {
        self.probe.is_some()
    }

    /// The installed probe (scan-time fingerprinting).
    pub fn probe(&self) -> Option<&(dyn PathProbe + Sync)> {
        let probe: Option<&(dyn PathProbe + Send + Sync)> = self.probe.as_deref();
        probe.map(|p| p as &(dyn PathProbe + Sync))
    }

    /// Installs the resolve rules (F-2-1: built-in protected/detection rules +
    /// merged user dispositions). Set once by the scan assembler; fail-closed
    /// loading happens before a scan starts.
    pub fn set_rules(&mut self, rules: Arc<RuleSet>) {
        self.rules = Some(rules);
    }

    /// Whether the rule layer was installed.
    pub fn has_rules(&self) -> bool {
        self.rules.is_some()
    }

    /// The installed rule set (enum, read-only) — the rule-driven static
    /// cache provider enumerates exact-match builtin-detection rules from
    /// it to discover known cache locations. `None` when no rule layer is
    /// installed.
    #[must_use]
    pub fn rule_set(&self) -> Option<Arc<RuleSet>> {
        self.rules.clone()
    }

    /// Resolves the best matching rule for `path` (the F-2-1 mechanism gate).
    /// `None` when no rule layer is installed or nothing matched.
    #[must_use]
    pub fn resolved_rule(&self, path: &Path) -> Option<ResolvedRule> {
        let rules = self.rules.as_ref()?;
        resolve_rules(path, &rules.rules)
    }

    /// True when `path` resolves to a protected rule that is **not** the
    /// user's own Protect disposition (i.e. a built-in protected root such as
    /// `%USERPROFILE%\.ssh`, INV-007). Such paths are never residue and the
    /// unknown provider must not report them.
    #[must_use]
    pub fn is_builtin_protected(&self, path: &Path) -> bool {
        self.resolved_rule(path).is_some_and(|r| {
            r.risk == RiskLevel::Protected
                && r.source != devresidue_core::rules::RuleSource::UserProtected
        })
    }

    /// True when `path` resolves to a **user** protected rule — the user asked
    /// the item to show up as Protected, so it *should* be emitted (unlike
    /// built-in protected roots which are never residue).
    #[must_use]
    pub fn is_user_protected(&self, path: &Path) -> bool {
        self.resolved_rule(path).is_some_and(|r| {
            r.risk == RiskLevel::Protected
                && r.source == devresidue_core::rules::RuleSource::UserProtected
        })
    }

    /// Reports one coarse progress event to the caller's sink.
    pub fn progress(&self, event: ProgressEvent) {
        (self.progress)(event);
    }

    /// Allocates the next scan-item id.
    ///
    /// Ids come from a **process-global monotonic counter** (R04): they never
    /// repeat across scans within one process, so an id from an earlier scan
    /// can never resolve to a different object of a later scan.
    pub fn allocate_id(&self) -> ScanItemId {
        let next = NEXT_ITEM_ID.fetch_add(1, Ordering::Relaxed) + 1;
        ScanItemId::from_raw(next)
    }

    /// Env lookup.
    pub fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }

    /// Runs a cache-dir query through the injected tool.
    pub fn query_cache_dir(&self, executable: &str, args: &[&str]) -> Result<String, String> {
        self.tool.run(executable, args, Duration::from_secs(5))
    }

    /// Cancellation probe. Providers check this between large operations.
    pub fn cancelled(&self) -> bool {
        !(self.should_continue)()
    }

    /// A `Sync` cancellation handle suitable for the parallel measurement
    /// walker (Phase 16): workers share this closure across threads.
    pub fn cancel_fn(&self) -> &(dyn Fn() -> bool + Sync) {
        self.should_continue.as_ref()
    }

    /// Records a human-readable warning (INV-011 rejections, tool fallbacks).
    pub fn warn(&self, message: impl Into<String>) {
        self.warnings.borrow_mut().push(message.into());
    }

    /// The collected warnings, in order.
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.borrow().clone()
    }

    /// Deduplicates a target path across the whole scan. Returns `false` when
    /// the path was already emitted.
    pub fn seen_insert(&self, path: &Path) -> bool {
        let key = Self::canonical_key(path);
        self.seen.borrow_mut().insert(key)
    }

    /// Case-folded canonical key shared by the seen-set.
    pub fn canonical_key(path: &Path) -> String {
        canonical::normalize(path).to_string_lossy().to_lowercase()
    }

    /// Whether a path was already emitted.
    pub fn already_seen(&self, path: &Path) -> bool {
        self.seen.borrow().contains(&Self::canonical_key(path))
    }

    /// Fills `item.scan_snapshot` with a scan-time authorisation fingerprint
    /// (R01) when a probe is installed. Fail-closed on probe errors: an item
    /// whose identity could not be captured at scan time is returned as
    /// `Err(reason)` so the assembler can drop it rather than emit an
    /// un-authorisable record.
    pub fn fingerprint_item(&self, item: &mut ScanItem) -> Result<(), String> {
        let Some(probe) = self.probe() else {
            return Ok(()); // no probe (fixtures/demo): leave scan_snapshot None
        };
        let rule_id = item.evidence.iter().find_map(|e| e.rule_id);
        let snapshot = devresidue_core::safety::identity::IdentitySnapshot::capture(
            probe, &item.path, rule_id, None, item.risk,
        )
        .map_err(|e| {
            format!(
                "scan-time fingerprint failed for {}: {e}",
                item.path.display()
            )
        })?;
        item.scan_snapshot = Some(snapshot);
        Ok(())
    }
}

/// A progress sink that drops everything (default for existing callers).
fn noop_progress() -> Box<dyn Fn(ProgressEvent)> {
    Box::new(|_| {})
}

/// F-2-1 mechanism gate (M2 final review, applied by scan assemblers): every
/// emitted item is checked against the resolve rules right before it is
/// persisted. A rule hit overrides the item's classification and records the
/// rule on the evidence:
///
/// - a `Protected` hit forces `risk = Protected` (and `cleanup_action = None`)
///   so protected paths can never be planned, regardless of which provider
///   discovered them (INV-002, defence in depth);
/// - any other rule hit (user detection rules from the analyzer / future
///   community rules) classifies the path with the rule's risk/category;
/// - `user-ignore/` declarations never compile into the set, so ignored paths
///   are already pre-seeded into the seen-set and never reach this point.
///
/// `source` is preserved (evidence keeps the origin); the rule id is attached
/// as evidence for explainability. R2-F06: **every** rule hit (Protected or
/// not) additionally records the rule slug in
/// [`ScanItem::classification_rule_id`] so the authorisation chain can
/// re-verify that exact rule at plan/execution time — a reclassified Provider
/// item whose rule was later removed must not keep its rewritten Safe risk.
pub fn apply_rule_classification(ctx: &ScanContext, item: &mut ScanItem) {
    let Some(rule) = ctx.resolved_rule(&item.path) else {
        return;
    };
    item.category = rule.category;
    item.risk = rule.risk;
    item.classification_rule_id = Some(rule.id.clone());
    if let Some(product) = &rule.product {
        item.product = Some(product.clone());
    }
    item.explanation = format!(
        "{} — rule {}: {}",
        item.explanation, rule.id, rule.description
    );
    item.evidence.push(Evidence::new(
        "rule-match",
        format!("matched {} (rule id {})", rule.id, rule.rule_id.raw()),
    ));
    if rule.risk == RiskLevel::Protected {
        item.cleanup_action = devresidue_core::CleanupAction::None;
        item.source = SourceKind::Rule;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_env() -> EnvMap {
        let mut env = HashMap::new();
        env.insert("USERPROFILE".into(), r"C:\Users\alice".into());
        env.insert(
            "LOCALAPPDATA".into(),
            r"C:\Users\alice\AppData\Local".into(),
        );
        env
    }

    fn ctx(env: EnvMap) -> ScanContext {
        ScanContext::with_env_and_progress(
            env,
            Box::new(NoTool),
            vec![],
            Box::new(|| true),
            Box::new(|_| {}),
        )
    }

    #[test]
    fn r04_ids_are_globally_monotonic_across_scan_contexts() {
        // Two distinct scan runs must never hand out the same item id (R04) —
        // a stale selection cannot point at a different object of a later scan.
        let a = ctx(base_env());
        let first_a = a.allocate_id();
        let b = ctx(base_env());
        let first_b = b.allocate_id();
        assert_ne!(first_a, first_b, "cross-scan ids must differ");

        // Within one scan ids keep increasing.
        let mut prev = first_a;
        for _ in 0..10 {
            let next = a.allocate_id();
            assert!(next > prev);
            prev = next;
        }
        // And the second scan continues *after* the first (no reset).
        assert!(first_b > first_a);
    }
}
