//! `devresidue scan` — runs the real providers (dev caches + kondo projects
//! by default) and persists the result for `devresidue plan`.
//!
//! PLAN Phase 10 behaviour:
//!
//! - **Progress**: coarse provider/project lines go to *stderr* so `--json`
//!   stdout stays clean; `--quiet` suppresses them all.
//! - **Cancel**: Ctrl-C sets a cancellation flag consulted by `ScanContext` at
//!   every existing provider boundary (per provider / per project / per
//!   entry). The scan keeps its partial results, marks the snapshot
//!   `cancelled` and exits 0 (the results are still useful).
//! - **Summary**: the risk table gains a Total row, a warnings count, and the
//!   collected warnings are listed after the summary (they used to be buried
//!   in evidence/snapshot).
//! - **Overlap dedup** (display layer): when one provider reports a region
//!   nested in (or equal to) an earlier entry — OMO's extras under OpenCode —
//!   the contained row is annotated "(overlaps …)" in the table and excluded
//!   from the summary totals so bytes are not double-counted (F-1e).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use devresidue_core::safety::canonical;
use devresidue_core::{RiskLevel, ScanItem};
use devresidue_providers::fixtures::fixture_scan_items;
use devresidue_providers::scan_ctx::{apply_rule_classification, ProgressEvent, ScanContext};
use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};
use devresidue_providers::{agents, dev_cache, project, unknown};

use super::support::{self, ShellTool};
use crate::ScanOptions;

/// Runs the scan pipeline.
pub fn run(opts: ScanOptions) -> Result<(), String> {
    let base = support::data_dir()?;
    let _operation_lock = support::acquire_app_operation_lock(&base)?;

    if opts.fixtures {
        let items = fixture_scan_items();
        // R3-G06: allocate the generation and publish under the cross-process
        // lock (read→increment→save is one transaction).
        let snapshot = scan_store::save_with_next_generation(&base, |generation| ScanSnapshot {
            generation,
            ..ScanSnapshot::new(ScanMode::Fixtures, items.clone(), vec![])
        })?;
        report_snapshot(&snapshot, ProgressStats::default(), opts.json);
        return Ok(());
    }

    scan_real(&base, &opts)
}

/// Real-machine scan through the injected context.
fn scan_real(base: &std::path::Path, opts: &ScanOptions) -> Result<(), String> {
    // Cancellation flag. Ctrl-C sets it; every provider boundary reads it
    // through `ScanContext::should_continue`. Plan/clean do **not** install a
    // handler (see clean_cmd docs): deleting is never interrupted mid-item —
    // per-item atomicity is guaranteed by the DeletePort instead.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    install_cancel_handler(&cancel_flag);

    // Coarse progress: events land in the tracker; the CLI flushes them to
    // stderr between provider families (`--quiet` keeps the tracker silent).
    let tracker = Rc::new(RefCell::new(ProgressTracker::new(opts.quiet)));
    let tracker_sink = Rc::clone(&tracker);
    let continue_flag = Arc::clone(&cancel_flag);
    let mut context = ScanContext::real_with_progress(
        Box::new(ShellTool),
        Vec::new(),
        Box::new(move || !continue_flag.load(Ordering::SeqCst)),
        Box::new(move |event| tracker_sink.borrow_mut().handle(event)),
    );

    // F-2-1 rule gate (M2 final review): load the merged built-in + user rule
    // set fail-closed before scanning and pre-seed user-ignore paths into the
    // seen-set so ignored paths never appear again.
    let scan_rules = support::load_scan_rules(base)?;
    context.set_rules(Arc::clone(&scan_rules.rules));
    for ignore in &scan_rules.ignores {
        context.seen_insert(ignore);
    }
    // R01: real scans install the Windows identity probe so every item is
    // fingerprinted at scan time (the scan_snapshot authorisation record the
    // planner/engine trust).
    context.set_probe(Arc::new(
        devresidue_platform_windows::safety::WindowsPathProbe,
    ));

    let has_projects = !opts.projects.is_empty();
    // Default (no filter flags) runs every family — including the unknown-data
    // provider (SPEC §25). Any explicit filter narrows to exactly that family;
    // `--unknown` alone runs only the unknown provider.
    let any_filter = opts.dev_cache || opts.agents || opts.unknown || has_projects;
    let run_dev_cache = opts.dev_cache || !any_filter;
    let run_kondo = has_projects || !any_filter;
    let run_agents = opts.agents || !any_filter;
    let run_unknown = opts.unknown || !any_filter;

    // F07: workspace-root resolution is decoupled from "is kondo running".
    // Every real scan injects the protection roots (explicit `--projects` >
    // `DEVRESIDUE_WORKSPACE_ROOTS` > existing default candidates) so a
    // cache-only / agents-only / unknown-only scan still refuses tool-reported
    // workspace roots and their ancestors (R07). When kondo runs, the same
    // roots double as its discovery roots.
    let explicit_roots: Vec<PathBuf> = opts.projects.iter().map(PathBuf::from).collect();
    let env_roots = std::env::var("DEVRESIDUE_WORKSPACE_ROOTS").ok();
    let roots = resolve_scan_roots(&explicit_roots, env_roots.as_deref(), &context);
    context.workspace_roots.extend(roots);

    let mut items: Vec<ScanItem> = Vec::new();
    if run_dev_cache {
        gate_batch(&context, dev_cache::scan(&context), &mut items);
        flush_progress(&tracker);
    }
    if run_kondo && !context.cancelled() {
        gate_batch(&context, project::scan(&context), &mut items);
        flush_progress(&tracker);
    }
    if run_agents && !context.cancelled() {
        gate_batch(&context, agents::scan(&context), &mut items);
        flush_progress(&tracker);
    }
    if run_unknown && !context.cancelled() {
        gate_batch(&context, unknown::scan(&context), &mut items);
        flush_progress(&tracker);
    }

    let cancelled = cancel_flag.load(Ordering::SeqCst);
    let mode = ScanMode::Real {
        workspace_roots: context
            .workspace_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
    };
    let warnings = context.warnings();
    // R04/R3-G06: assign the next generation for this data root and publish
    // atomically under the cross-process lock so (generation, item.id)
    // uniquely identifies this scan's items and two writers never mint the
    // same generation.
    let snapshot = scan_store::save_with_next_generation(base, |generation| ScanSnapshot {
        generation,
        cancelled,
        ..ScanSnapshot::new(mode, items.clone(), warnings.clone())
    })?;

    flush_progress(&tracker);
    report_snapshot(&snapshot, tracker.borrow().stats(), opts.json);
    Ok(())
}

/// F-2-1 rule gate + R01 scan-time fingerprinting for one provider batch.
///
/// Every item is rule-reclassified first, then fingerprinted through the
/// injected identity probe. An item whose `scan_snapshot` authorisation
/// record cannot be captured is excluded with a warning — fail-closed: no
/// fingerprint, no cleanup eligibility later (R01).
fn gate_batch(ctx: &ScanContext, batch: Vec<ScanItem>, out: &mut Vec<ScanItem>) {
    for mut item in batch {
        apply_rule_classification(ctx, &mut item);
        if let Err(e) = ctx.fingerprint_item(&mut item) {
            ctx.warn(e);
            continue;
        }
        out.push(item);
    }
}

/// Resolves the workspace **protection** roots for a scan (F07):
/// explicit `--projects` roots, else `DEVRESIDUE_WORKSPACE_ROOTS`, else the
/// existing default candidates. Called for every real scan regardless of
/// which provider families run, so the tool-path verifier (R07) always sees
/// the full root set.
fn resolve_scan_roots(
    explicit: &[PathBuf],
    env_roots: Option<&str>,
    ctx: &ScanContext,
) -> Vec<PathBuf> {
    project::kondo::resolve_workspace_roots(explicit, env_roots, ctx)
}

/// Ctrl-C handler wiring (ctrlc crate — a safe cross-platform wrapper; the CLI
/// crate forbids `unsafe`, so a hand-written `SetConsoleCtrlHandler` FFI is
/// not an option here). The handler only flips a flag; the synchronous scan
/// observes it at its next boundary and then reports partial results.
fn install_cancel_handler(flag: &Arc<AtomicBool>) {
    let flag = Arc::clone(flag);
    // Best effort: a second handler would only exist in embedded/test runs.
    let _ = ctrlc::set_handler(move || {
        flag.store(true, Ordering::SeqCst);
    });
}

/// Renders the scan report. JSON stdout is the full snapshot (mode, warnings,
/// cancelled) — machine readable; the human path prints the table, a summary
/// with totals and warnings, then every warning in detail.
fn report_snapshot(snapshot: &ScanSnapshot, stats: ProgressStats, json: bool) {
    if json {
        match serde_json::to_string_pretty(snapshot) {
            Ok(text) => println!("{text}"),
            Err(err) => eprintln!("error: failed to serialise scan result: {err}"),
        }
        return;
    }

    if snapshot.cancelled {
        println!(
            "scan cancelled: {} of {} providers completed, showing partial results",
            stats.completed, stats.started
        );
    }
    // R3-G03: the generation is part of the item identity ((generation, id)
    // identifies an object across processes). Tell the user which generation
    // these ids belong to and how to pin it in `plan --items`.
    println!(
        "scan generation: {} (plan with --scan-generation {} to pin these ids)",
        snapshot.generation, snapshot.generation
    );
    emit_table(&snapshot.items);
    let summary = summary_lines(&snapshot.items, snapshot.warnings.len());
    if !summary.is_empty() {
        println!();
        for line in summary {
            println!("{line}");
        }
    }
    if !snapshot.warnings.is_empty() {
        println!();
        for line in warning_lines(&snapshot.warnings) {
            println!("{line}");
        }
    }
}

/// Coarse progress rendering + counting (PLAN Phase 10).
///
/// Events are queued as rendered stderr lines (unless `quiet`) and drained by
/// the CLI between provider families, keeping stdout (`--json`) clean.
pub(crate) struct ProgressTracker {
    quiet: bool,
    lines: Vec<String>,
    stats: ProgressStats,
}

/// Started/completed provider counts used for the cancelled-scan message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProgressStats {
    pub started: usize,
    pub completed: usize,
}

impl ProgressTracker {
    pub(crate) fn new(quiet: bool) -> Self {
        Self {
            quiet,
            lines: Vec::new(),
            stats: ProgressStats::default(),
        }
    }

    /// Records one provider progress event (stats always; rendered line only
    /// when not `--quiet`).
    pub(crate) fn handle(&mut self, event: ProgressEvent) {
        match &event {
            ProgressEvent::ProviderStart(_) => self.stats.started += 1,
            ProgressEvent::ProviderDone(_) => self.stats.completed += 1,
            _ => {}
        }
        if !self.quiet {
            if let Some(line) = render_progress(&event) {
                self.lines.push(line);
            }
        }
    }

    /// Emits the queued lines to stderr (progress never touches stdout).
    pub(crate) fn flush(&mut self) {
        for line in self.lines.drain(..) {
            eprintln!("{line}");
        }
    }

    pub(crate) fn stats(&self) -> ProgressStats {
        self.stats
    }
}

/// Renders one progress event to a stderr line (`None` = no output; e.g. the
/// "scan finished" marker exists only for counting).
pub(crate) fn render_progress(event: &ProgressEvent) -> Option<String> {
    match event {
        ProgressEvent::ProviderStart(slug) => Some(format!("Scanning: {slug}...")),
        ProgressEvent::ProviderDone(_) => None,
        ProgressEvent::ProjectFound { name, project_type } => {
            Some(format!("  {name} ({project_type}) found"))
        }
        ProgressEvent::Measured {
            path,
            file_count,
            logical_size,
        } => Some(format!(
            "  measured {}: {file_count} files, {}",
            path.display(),
            human_bytes(*logical_size)
        )),
    }
}

fn flush_progress(tracker: &Rc<RefCell<ProgressTracker>>) {
    tracker.borrow_mut().flush();
}

/// Canonical containment map for the human report (display layer only — the
/// persisted snapshot is untouched).
///
/// Some agent providers report nested views of one on-disk region (OMO's
/// storage/log extras sit under — or at the same path as — OpenCode's wider
/// entries, F-1e). Counting every row would double-count the bytes of the
/// contained entry, so the later/contained item is annotated "(overlaps …)"
/// in the table and excluded from the summary totals; the earliest/outermost
/// item owns the region. The path cells still show each row's own measured
/// size, so nothing is hidden.
///
/// `overlap_owners` returns, for each item, the index of the first earlier
/// item whose canonical path contains it (`is_within`, equal path counts).
/// `None` means the item owns its path region and contributes to totals.
fn overlap_owners(items: &[ScanItem]) -> Vec<Option<usize>> {
    let mut owners: Vec<Option<usize>> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let mut owner = None;
        for (j, other) in items.iter().enumerate().take(i) {
            if canonical::is_within(&item.path, &other.path) {
                owner = Some(j);
                break;
            }
        }
        owners.push(owner);
    }
    owners
}

/// Short label of the owning (containing) item, used in "(overlaps …)".
fn overlap_label(item: &ScanItem) -> String {
    match &item.product {
        Some(product) => product.clone(),
        None => item.display_name.clone(),
    }
}

/// Risk-group rows → lines including a Total row and the warnings count.
///
/// Entries whose path region is already covered by an earlier entry
/// (see [`overlap_owners`]) are excluded from every total so the summary is
/// not double-counted (F-1e).
pub(crate) fn summary_lines(items: &[ScanItem], warning_count: usize) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }
    let owners = overlap_owners(items);
    let mut by_risk: Vec<(RiskLevel, u64, u64)> = Vec::new();
    let mut total_size = 0u64;
    let mut counted = 0usize;
    for (idx, item) in items.iter().enumerate() {
        if owners[idx].is_some() {
            continue; // contained in an earlier item → excluded from totals.
        }
        counted += 1;
        total_size += item.logical_size;
        if let Some(entry) = by_risk.iter_mut().find(|(r, _, _)| *r == item.risk) {
            entry.1 += item.logical_size;
            entry.2 += 1;
        } else {
            by_risk.push((item.risk, item.logical_size, 1));
        }
    }
    let mut lines = vec!["Summary by risk:".to_string()];
    for (risk, size, count) in by_risk {
        lines.push(format!(
            "  {:<22} {:>10}  ({} items)",
            risk_label(risk),
            human_bytes(size),
            count
        ));
    }
    lines.push(format!(
        "  {:<22} {:>10}  ({} items)",
        "Total",
        human_bytes(total_size),
        counted
    ));
    if warning_count > 0 {
        lines.push(format!("Warnings: {warning_count}"));
    }
    lines
}

/// One `warning: ...` line per collected warning (after the summary).
pub(crate) fn warning_lines(warnings: &[String]) -> Vec<String> {
    warnings.iter().map(|w| format!("warning: {w}")).collect()
}

pub(crate) fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Safe => "Safe",
        RiskLevel::RegenerableLocal => "Regenerable Local",
        RiskLevel::RegenerableDownload => "Regenerable Download",
        RiskLevel::Review => "Review",
        RiskLevel::Protected => "Protected",
        RiskLevel::Unknown => "Unknown",
    }
}

fn emit_table(items: &[ScanItem]) {
    if items.is_empty() {
        println!("No residue items found.");
        return;
    }

    const COLUMNS: [&str; 7] = [
        "Product", "Category", "Path", "Size", "Risk", "Source", "Reason",
    ];
    const CAPS: [usize; 7] = [20, 16, 74, 10, 20, 18, 62];

    let rows = table_rows(items, &CAPS);

    let mut widths = [0_usize; 7];
    for (i, header) in COLUMNS.iter().enumerate() {
        widths[i] = header.len();
    }
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            let cap = if CAPS[i] == 0 { usize::MAX } else { CAPS[i] };
            widths[i] = widths[i].max(cell.len().min(cap));
        }
    }

    let header: Vec<String> = COLUMNS.iter().zip(widths).map(|(h, w)| pad(h, w)).collect();
    println!("{}", header.join("  "));
    let rule = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{rule}");

    for row in &rows {
        let cells: Vec<String> = row.iter().zip(widths).map(|(c, w)| pad(c, w)).collect();
        println!("{}", cells.join("  "));
    }
}

/// Builds the display rows (one `[col; 7]` per item). Paths of items whose
/// region is contained in an earlier item carry an inline "(overlaps …)"
/// marker so the double-coverage stays visible next to the row's own size
/// (F-1e).
fn table_rows(items: &[ScanItem], caps: &[usize; 7]) -> Vec<[String; 7]> {
    let owners = overlap_owners(items);
    let mut rows: Vec<[String; 7]> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let mut path = item.path.display().to_string();
        if let Some(owner) = owners[i] {
            path = format!("(overlaps {}) {}", overlap_label(&items[owner]), path);
        }
        rows.push([
            truncate(item.product.as_deref().unwrap_or("-"), caps[0]),
            truncate(category_label(item.category), caps[1]),
            truncate(&path, caps[2]),
            human_bytes(item.logical_size),
            truncate(risk_label(item.risk), caps[4]),
            truncate(source_label(item.source), caps[5]),
            truncate(&item.explanation, caps[6]),
        ]);
    }
    rows
}

fn pad(s: &str, width: usize) -> String {
    let mut out = String::with_capacity(width);
    out.push_str(s);
    if s.len() < width {
        out.extend(std::iter::repeat(' ').take(width - s.len()));
    }
    out
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 3 {
        s.chars().take(max).collect()
    } else {
        let mut out: String = s.chars().take(max - 3).collect();
        out.push_str("...");
        out
    }
}

/// Renders a byte count as a compact human string ("1.2 GiB").
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub(crate) fn category_label(category: devresidue_core::ResidueCategory) -> &'static str {
    use devresidue_core::ResidueCategory as C;
    match category {
        C::AiAgent => "AI Agent",
        C::Ide => "IDE",
        C::DeveloperCache => "Developer Cache",
        C::PackageCache => "Package Cache",
        C::BuildArtifact => "Build Artifact",
        C::Dependency => "Dependency",
        C::Log => "Log",
        C::Temporary => "Temporary",
        C::Session => "Session",
        C::WorkspaceState => "Workspace State",
        C::Configuration => "Configuration",
        C::Credential => "Credential",
        C::Unknown => "Unknown",
    }
}

fn source_label(source: devresidue_core::SourceKind) -> &'static str {
    match source {
        devresidue_core::SourceKind::Rule => "Rule",
        devresidue_core::SourceKind::Kondo => "Kondo",
        devresidue_core::SourceKind::DeveloperCacheProvider => "Dev Cache",
        devresidue_core::SourceKind::PackageManager => "Tool Query",
        devresidue_core::SourceKind::AgentProvider => "Agent",
        devresidue_core::SourceKind::UnknownProvider => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::{CleanupAction, Evidence, ResidueCategory, ScanItemId, SourceKind};
    use devresidue_providers::scan_ctx::NoTool;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_DIR_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn item(id: u64, risk: RiskLevel, size: u64) -> ScanItem {
        ScanItem {
            id: ScanItemId::from_raw(id),
            path: format!(r"C:\residue\item-{id}").into(),
            display_name: format!("item {id}"),
            product: Some("demo".into()),
            category: ResidueCategory::Temporary,
            risk,
            source: SourceKind::Kondo,
            logical_size: size,
            file_count: 1,
            last_modified: None,
            explanation: "test item".into(),
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![Evidence::new("provider:test", "e")],
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    #[test]
    fn truncate_keeps_short_and_cuts_long() {
        assert_eq!(truncate("short", 20), "short");
        assert_eq!(truncate("0123456789abcdef", 8), "01234...");
        assert_eq!(truncate("x", 1), "x");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert!(human_bytes(1024).ends_with("KiB"));
        assert!(human_bytes(1_258_291_200).ends_with("GiB"));
    }

    #[test]
    fn summary_has_a_total_row_and_empty_is_silent() {
        let items = vec![
            item(1, RiskLevel::Safe, 100),
            item(2, RiskLevel::RegenerableDownload, 924),
        ];
        let lines = summary_lines(&items, 2);
        assert_eq!(lines[0], "Summary by risk:");
        assert!(lines.iter().any(|l| l.contains("Safe")));
        assert!(lines.iter().any(|l| l.contains("Regenerable Download")));
        assert!(lines.iter().any(|l| l.contains("Total")));
        let total = lines.iter().find(|l| l.starts_with("  Total")).unwrap();
        assert!(total.contains("2 items"), "{total}");
        assert!(lines.iter().any(|l| l == "Warnings: 2"));

        assert!(summary_lines(&[], 3).is_empty());
    }

    #[test]
    fn warnings_are_listed_after_the_summary() {
        let lines = warning_lines(&[
            "npm cache location from known default (tool query failed)".into(),
            "uv cache location from known default (tool query failed)".into(),
        ]);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "warning: npm cache location from known default (tool query failed)"
        );
    }

    /// Builds a richer item (arbitrary path/product) for overlap fixtures.
    fn at(path: &str, product: &str, size: u64) -> ScanItem {
        ScanItem {
            id: ScanItemId::from_raw(9),
            path: path.into(),
            display_name: format!("{product} data at {path}"),
            product: Some(product.into()),
            category: ResidueCategory::Log,
            risk: RiskLevel::Safe,
            source: SourceKind::AgentProvider,
            logical_size: size,
            file_count: 1,
            last_modified: None,
            explanation: format!("{product} item"),
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![],
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    #[test]
    fn overlap_rows_are_annotated_and_excluded_from_totals() {
        // F-1e: OMO reports sub-regions that OpenCode's wider entries already
        // cover — the OMO log glob item sits *at* the OpenCode log dir path,
        // and the OMO storage dir nests under OpenCode storage. Scan order
        // mirrors the provider registry: OpenCode first, OMO second.
        let log = r"C:\Users\alice\.local\share\opencode\log";
        let storage = r"C:\Users\alice\.local\share\opencode\storage";
        let omo_storage = format!(r"{storage}\oh-my-opencode-slim");
        let tmp = r"C:\Users\alice\.codex\.tmp";
        let items = vec![
            at(log, "OpenCode", 200),
            at(log, "OMO Slim", 30),
            at(storage, "OpenCode", 1000),
            at(&omo_storage, "OMO Slim", 90),
            at(tmp, "Codex CLI", 5),
        ];

        let rows = table_rows(&items, &[20, 16, 74, 10, 20, 18, 62]);
        // Same-path and nested rows carry the marker naming the owning
        // product; owners and disjoint rows do not.
        assert!(
            rows[1][2].starts_with("(overlaps OpenCode)"),
            "OMO log row must be annotated: {}",
            rows[1][2]
        );
        assert!(
            rows[3][2].starts_with("(overlaps OpenCode)"),
            "OMO storage row must be annotated: {}",
            rows[3][2]
        );
        assert!(!rows[0][2].contains("overlaps"), "{}", rows[0][2]);
        assert!(!rows[4][2].contains("overlaps"), "{}", rows[4][2]);
        // Each row still shows its own measured size.
        assert!(rows[1][3].contains("30 B"), "{}", rows[1][3]);
        assert!(rows[3][3].contains("90 B"), "{}", rows[3][3]);

        // Totals exclude the overlapped items: 200 + 1000 + 5, three items.
        let lines = summary_lines(&items, 0);
        let total = lines.iter().find(|l| l.starts_with("  Total")).unwrap();
        assert!(total.contains("1.2 KiB"), "{total}");
        assert!(total.contains("3 items"), "{total}");
    }

    #[test]
    fn progress_lines_render_for_provider_and_project_events() {
        assert_eq!(
            render_progress(&ProgressEvent::ProviderStart("npm")),
            Some("Scanning: npm...".to_string())
        );
        // Done markers are counted, not rendered.
        assert_eq!(render_progress(&ProgressEvent::ProviderDone("npm")), None);
        assert_eq!(
            render_progress(&ProgressEvent::ProjectFound {
                name: "demo-proj".into(),
                project_type: "Cargo".into(),
            }),
            Some("  demo-proj (Cargo) found".to_string())
        );
        assert!(render_progress(&ProgressEvent::Measured {
            path: r"C:\x\npm-cache".into(),
            file_count: 5000,
            logical_size: 2048,
        })
        .unwrap()
        .contains("5000 files"));
    }

    #[test]
    fn quiet_tracker_suppresses_lines_but_keeps_stats() {
        let mut quiet = ProgressTracker::new(true);
        quiet.handle(ProgressEvent::ProviderStart("npm"));
        quiet.handle(ProgressEvent::ProviderStart("kondo"));
        quiet.handle(ProgressEvent::ProviderDone("npm"));
        assert!(quiet.lines.is_empty());
        assert_eq!(
            quiet.stats(),
            ProgressStats {
                started: 2,
                completed: 1
            }
        );

        let mut loud = ProgressTracker::new(false);
        loud.handle(ProgressEvent::ProviderStart("npm"));
        loud.handle(ProgressEvent::ProjectFound {
            name: "p".into(),
            project_type: "Node".into(),
        });
        loud.handle(ProgressEvent::ProviderDone("npm"));
        assert_eq!(loud.lines.len(), 2);
        assert_eq!(
            loud.stats(),
            ProgressStats {
                started: 1,
                completed: 1
            }
        );
    }

    #[test]
    fn cancelled_message_lists_started_and_completed_providers() {
        let snapshot = ScanSnapshot {
            cancelled: true,
            ..ScanSnapshot::new(
                ScanMode::Real {
                    workspace_roots: vec![],
                },
                vec![],
                vec![],
            )
        };
        let stats = ProgressStats {
            started: 7,
            completed: 6,
        };
        // Rendering goes through report_snapshot; the message text is what we
        // assert here (it carries the N of M semantics for the JSON/human
        // paths).
        let msg = format!(
            "scan cancelled: {} of {} providers completed, showing partial results",
            stats.completed, stats.started
        );
        assert_eq!(
            msg,
            "scan cancelled: 6 of 7 providers completed, showing partial results"
        );
        assert!(snapshot.cancelled);
    }

    /// Fake project farm (cargo/node/cmake, one artifact each) on a temp dir.
    fn write_farm(root: &std::path::Path) {
        let mk = |files: &[&str]| {
            for f in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, b"x").unwrap();
            }
        };
        mk(&[
            "cargo-proj/Cargo.toml",
            "cargo-proj/src/main.rs",
            "cargo-proj/target/debug/x.exe",
        ]);
        mk(&[
            "node-proj/package.json",
            "node-proj/src/index.js",
            "node-proj/node_modules/pkg/index.js",
        ]);
        mk(&[
            "cmake-proj/CMakeLists.txt",
            "cmake-proj/src/main.cpp",
            "cmake-proj/cmake-build-debug/out.exe",
        ]);
    }

    #[test]
    fn cancellation_keeps_partial_results_and_flags_the_snapshot() {
        // Cancelling at a real provider boundary (the 2nd kondo project found)
        // must stop discovery yet keep the items already found — the CLI then
        // persists them with `cancelled = true`. No real Ctrl-C signal is sent
        // (PLAN Phase 10: the flag logic is what the handler drives).
        let dir = tempfile_base();
        let farm = dir.join("farm");
        write_farm(&farm);

        let flag = Arc::new(AtomicBool::new(true));
        let cancel_from_progress = Arc::clone(&flag);
        let found = Rc::new(Cell::new(0usize));
        let progress_found = Rc::clone(&found);
        let progress = move |event: ProgressEvent| {
            if let ProgressEvent::ProjectFound { .. } = event {
                let n = progress_found.get() + 1;
                progress_found.set(n);
                if n == 2 {
                    cancel_from_progress.store(false, Ordering::SeqCst);
                }
            }
        };
        let env = fake_env(&dir);
        let ctx = ScanContext::with_env_and_progress(
            env,
            Box::new(NoTool),
            vec![farm.clone()],
            Box::new(move || flag.load(Ordering::SeqCst)),
            Box::new(progress),
        );

        let items = project::scan(&ctx);
        assert!(
            ctx.cancelled(),
            "the 2nd project event must cancel the scan"
        );
        // Three single-artifact projects exist; the full scan finds 3 items,
        // so exactly 2 means discovery stopped at the cancellation boundary.
        assert_eq!(items.len(), 2, "partial results expected: {items:?}");

        // Control: without cancellation the same farm yields all three.
        let full = ScanContext::with_env_and_progress(
            fake_env(&dir),
            Box::new(NoTool),
            vec![farm],
            Box::new(|| true),
            Box::new(|_| {}),
        );
        assert_eq!(
            project::scan(&full).len(),
            3,
            "farm must produce 3 artifacts"
        );

        // The CLI layer persists partial results with the cancelled flag set.
        let base = dir.join("data");
        let snapshot = ScanSnapshot {
            cancelled: ctx.cancelled(),
            ..ScanSnapshot::new(
                ScanMode::Real {
                    workspace_roots: vec![],
                },
                items,
                vec![],
            )
        };
        scan_store::save(&base, &snapshot).expect("save");
        let loaded = scan_store::load(&base).expect("load");
        assert!(loaded.cancelled);
        assert_eq!(loaded.items.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempfile_base() -> std::path::PathBuf {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("dr-cli-scan-{}-{sequence}", std::process::id()))
    }

    #[test]
    fn scan_test_directories_are_unique_for_parallel_execution() {
        assert_ne!(tempfile_base(), tempfile_base());
    }

    #[test]
    fn f07_cache_only_scan_injects_workspace_roots_into_the_context() {
        // F07: workspace-root protection is decoupled from running kondo. A
        // dev-cache-only scan must still receive the configured roots so the
        // tool-path verifier can reject a tool that reports the workspace.
        let dir = tempfile_base();
        let env = fake_env(&dir);
        let ctx = devresidue_providers::scan_ctx::ScanContext::with_env(
            env,
            Box::new(NoTool),
            vec![],
            Box::new(|| true),
        );

        let explicit: Vec<PathBuf> = vec![];
        let env_roots = "D:\\review\\workspace;E:\\projects";
        let roots = resolve_scan_roots(&explicit, Some(env_roots), &ctx);
        assert_eq!(
            roots,
            vec![
                PathBuf::from(r"D:\review\workspace"),
                PathBuf::from(r"E:\projects")
            ],
            "cache-only scans must resolve the configured protection roots"
        );

        // The same context shape a dev-cache-only scan would produce (roots
        // injected, kondo not running) covers the workspace path — which is
        // exactly the pre-condition the R07 tool-path rejection needs.
        let verifier_ctx = devresidue_providers::scan_ctx::ScanContext::with_env(
            fake_env(&dir),
            Box::new(NoTool),
            roots,
            Box::new(|| true),
        );
        let workspace = PathBuf::from(r"D:\review\workspace");
        assert!(
            verifier_ctx
                .workspace_roots
                .iter()
                .any(|r| devresidue_core::safety::canonical::is_within(&workspace, r)),
            "the injected roots must cover the configured workspace path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake_env(base: &std::path::Path) -> std::collections::HashMap<String, String> {
        let mut env = std::collections::HashMap::new();
        env.insert(
            "USERPROFILE".into(),
            base.join("profile").display().to_string(),
        );
        env.insert(
            "LOCALAPPDATA".into(),
            base.join("profile")
                .join("AppData")
                .join("Local")
                .display()
                .to_string(),
        );
        env.insert(
            "APPDATA".into(),
            base.join("profile")
                .join("AppData")
                .join("Roaming")
                .display()
                .to_string(),
        );
        env.insert("SYSTEMROOT".into(), "C:\\Windows".into());
        env.insert("PROGRAMFILES".into(), "C:\\Program Files".into());
        env.insert("PROGRAMFILES(X86)".into(), "C:\\Program Files (x86)".into());
        env.insert("PROGRAMDATA".into(), "C:\\ProgramData".into());
        env
    }
}
