//! `scan` / `cancel_scan` / `get_scan_results` commands plus the scan worker.
//!
//! Thread model: `scan` registers an active scan on the model (its id +
//! cancellation flag) and immediately returns an opaque handle — a `std::thread`
//! runs the synchronous provider pipeline and streams coarse events to the
//! frontend (`scan://progress`, `scan://item`, `scan://warning`,
//! `scan://done`). `cancel_scan` flips the flag the providers poll at every
//! existing boundary (per provider / per project / per entry); the worker then
//! persists the partial results with `cancelled = true` and finishes normally.
//!
//! The provider dispatch itself is pure Rust (reuses the exact synchronous
//! scans from `devresidue-providers`) and is unit-tested with injected
//! environments + fixture providers — no WebView involved.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use devresidue_core::ScanItem;
use devresidue_platform_windows::safety::WindowsPathProbe;
use devresidue_providers::agents;
use devresidue_providers::dev_cache;
use devresidue_providers::project;
use devresidue_providers::scan_ctx::{
    apply_rule_classification, EnvMap, ProgressEvent, ScanContext, ToolQuery,
};
use devresidue_providers::scan_store::{self, ScanMode, ScanSnapshot};
use devresidue_providers::unknown;
use tauri::{AppHandle, Emitter, State};

use crate::contract::{
    AppDataInfoDto, CommandError, ErrorCode, ScanHandleDto, ScanScope, ScanScopePreviewDto,
    WorkspaceRootValidationDto,
};
use crate::state::AppState;
use crate::support::ShellTool;

/// What kind of workspace roots the scan needs.
#[derive(Debug, Clone)]
enum RootsMode {
    /// Roots supplied explicitly (`Projects` scope).
    Explicit(Vec<PathBuf>),
    /// Resolve the default candidates (`Default` scope, same as the CLI).
    DefaultCandidates,
    /// Inject the *protection* roots (env `DEVRESIDUE_WORKSPACE_ROOTS` or the
    /// existing default candidates) without running kondo discovery (F07 —
    /// cache/agents/unknown scopes).
    ProtectionRoots,
}

/// One runnable provider family (a synchronous
/// `&ScanContext -> Vec<ScanItem>` function). The real families report their
/// provider-level progress through the context.
type FamilyFn = fn(&ScanContext) -> Vec<ScanItem>;

const DEV_CACHE_FAMILIES: [FamilyFn; 1] = [dev_cache::scan];
const PROJECT_FAMILIES: [FamilyFn; 1] = [project::scan];
const AGENT_FAMILIES: [FamilyFn; 1] = [agents::scan];
const UNKNOWN_FAMILIES: [FamilyFn; 1] = [unknown::scan];
const ALL_FAMILIES: [FamilyFn; 4] = [dev_cache::scan, project::scan, agents::scan, unknown::scan];
const NO_PROJECT_FAMILIES: [FamilyFn; 3] = [dev_cache::scan, agents::scan, unknown::scan];

/// UI-facing events streamed during a scan (provider-independent; the command
/// wrapper adds the `scan_id` and picks the Tauri event name).
#[derive(Debug, Clone)]
pub enum UiEvent {
    Progress {
        provider: String,
        stage: String,
    },
    Item(Box<ScanItem>),
    Warning(String),
    /// Published by the scan driver *after* the authoritative state was
    /// updated (R10); carries the persisted snapshot's generation so the
    /// frontend can reject stale finishes.
    Done {
        cancelled: bool,
        total: usize,
        generation: u64,
    },
    /// The scan failed before a snapshot could be finalised (R10) — never
    /// emitted as a Done.
    Failed {
        message: String,
    },
}

/// Injectable scan environment (tests inject a fake env + failing tool so no
/// real machine state is touched).
pub struct ScanEnv {
    pub env: Option<EnvMap>,
    pub tool: Option<Box<dyn ToolQuery>>,
    /// Test-only probe override (F05): when present, the scan context uses it
    /// instead of the real `WindowsPathProbe` so fingerprint-failure paths
    /// can be driven deterministically.
    pub probe: Option<Arc<dyn devresidue_core::safety::probe::PathProbe + Send + Sync>>,
}

impl ScanEnv {
    pub fn real() -> Self {
        Self {
            env: None,
            tool: None,
            probe: None,
        }
    }

    /// Test-only helper: inject a fake environment + tool.
    #[cfg(test)]
    pub fn injected(env: EnvMap, tool: Box<dyn ToolQuery>) -> Self {
        Self {
            env: Some(env),
            tool: Some(tool),
            probe: None,
        }
    }

    /// Test-only helper: also override the identity probe (F05 failure tests).
    #[cfg(test)]
    pub fn injected_with_probe(
        env: EnvMap,
        tool: Box<dyn ToolQuery>,
        probe: Arc<dyn devresidue_core::safety::probe::PathProbe + Send + Sync>,
    ) -> Self {
        Self {
            env: Some(env),
            tool: Some(tool),
            probe: Some(probe),
        }
    }
}

/// `scan` command: registers an active scan, spawns the worker thread and
/// returns the opaque handle immediately.
#[tauri::command]
pub fn scan(
    app: AppHandle,
    state: State<'_, AppState>,
    scope: ScanScope,
) -> Result<ScanHandleDto, CommandError> {
    let operation = state
        .begin_operation("scan")
        .map_err(|e| CommandError::new(ErrorCode::Busy, e))?;
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    // F-2-1 rule gate: assemble fail-closed *before* spawning so the caller
    // gets a structured error instead of a silently unguarded scan.
    let scan_rules = crate::support::load_scan_rules(&data_dir)
        .map_err(|e| CommandError::new(ErrorCode::Engine, e))?;

    let (scan_id, cancel) = {
        let mut model = state.model.lock().unwrap();
        model
            .begin_scan()
            .map_err(|e| CommandError::new(ErrorCode::PartialScan, e))?
    };

    let state = state.inner().clone();
    std::thread::spawn(move || {
        let _operation = operation;
        // R10: the worker reports its terminal outcome through the event
        // stream — the spawned closure only surfaces unexpected join/emit
        // problems on stderr. scan_job itself clears the active state on
        // every path (finish_scan / fail_scan) and emits the matching event.
        let mut bridge = |event: UiEvent| emit_to_frontend(&app, scan_id, event);
        if let Err(err) = scan_job(&state, scan_id, cancel, scope, scan_rules, &mut bridge) {
            eprintln!("scan {scan_id} failed: {err}");
        }
    });

    Ok(ScanHandleDto { scan_id })
}

/// Runs one scan to completion inside the worker thread and publishes the
/// terminal outcome in the R10 order:
///
/// ```text
///   scan (streams progress/item/warning) → persist snapshot
///   → update authoritative model state → emit scan://done (or scan://error)
/// ```
///
/// The done event therefore always refers to state the frontend can read
/// through `get_scan_results`. On failure the active handle is cleared
/// (`fail_scan`) and `UiEvent::Failed` is emitted instead of a done — a failed
/// scan never looks finished.
fn scan_job(
    state: &AppState,
    scan_id: u64,
    cancel: Arc<AtomicBool>,
    scope: ScanScope,
    scan_rules: crate::support::ScanRules,
    bridge: &mut dyn FnMut(UiEvent),
) -> Result<(), String> {
    let data_dir = { state.model.lock().unwrap().data_dir().to_path_buf() };

    // Bridge UiEvents (progress/item/warning) to the tauri event stream,
    // tagged with scan_id. Done/Failed are published by this function after
    // the model transition.
    let snapshot = {
        let mut emit = |event: UiEvent| match event {
            UiEvent::Done { .. } | UiEvent::Failed { .. } => {
                panic!("scan driver must not publish terminal events itself (R10)")
            }
            other => bridge(other),
        };
        run_scan(
            &data_dir,
            &scope,
            cancel,
            &mut emit,
            ScanEnv::real(),
            Some(scan_rules),
        )
    };
    publish_terminal(state, scan_id, snapshot, bridge)
}

/// Publishes the terminal outcome of a scan worker in the R10 order:
///
/// - success → `finish_scan` (authoritative `latest`) first, then
///   `UiEvent::Done` carrying the snapshot's generation;
/// - failure → `fail_scan` (clears the active handle, `latest` untouched),
///   then `UiEvent::Failed`. A failed scan is never reported as done.
///
/// Kept separate from the driver so tests can assert the ordering directly
/// against an [`AppState`].
fn publish_terminal(
    state: &AppState,
    scan_id: u64,
    snapshot: Result<ScanSnapshot, String>,
    bridge: &mut dyn FnMut(UiEvent),
) -> Result<(), String> {
    match snapshot {
        Ok(snapshot) => {
            let generation = snapshot.generation;
            let total = snapshot.items.len();
            let cancelled = snapshot.cancelled;
            // 1. Authoritative model state first…
            state.model.lock().unwrap().finish_scan(scan_id, snapshot);
            // 2. …then the terminal event (R10).
            bridge(UiEvent::Done {
                cancelled,
                total,
                generation,
            });
            Ok(())
        }
        Err(err) => {
            // Terminal-state cleanup on the error path (R10): clear the
            // active scan and surface scan://error — never a done.
            state.model.lock().unwrap().fail_scan(scan_id);
            bridge(UiEvent::Failed {
                message: err.clone(),
            });
            Err(err)
        }
    }
}

/// Emits one UI event as the corresponding tauri event (each payload carries
/// the scan id so the frontend can correlate concurrent views).
fn emit_to_frontend(app: &AppHandle, scan_id: u64, event: UiEvent) {
    use crate::contract::{
        ScanDonePayload, ScanErrorPayload, EV_SCAN_DONE, EV_SCAN_ERROR, EV_SCAN_ITEM,
        EV_SCAN_PROGRESS, EV_SCAN_WARNING,
    };
    match event {
        UiEvent::Progress { provider, stage } => {
            let _ = app.emit(
                EV_SCAN_PROGRESS,
                crate::contract::ProgressPayload {
                    scan_id,
                    provider,
                    stage,
                },
            );
        }
        UiEvent::Item(item) => {
            let _ = app.emit(
                EV_SCAN_ITEM,
                crate::contract::ScanItemPayload { scan_id, item },
            );
        }
        UiEvent::Warning(message) => {
            let _ = app.emit(
                EV_SCAN_WARNING,
                crate::contract::WarningPayload { scan_id, message },
            );
        }
        UiEvent::Done {
            cancelled,
            total,
            generation,
        } => {
            let _ = app.emit(
                EV_SCAN_DONE,
                ScanDonePayload {
                    scan_id,
                    cancelled,
                    total,
                    generation,
                },
            );
        }
        UiEvent::Failed { message } => {
            let _ = app.emit(EV_SCAN_ERROR, ScanErrorPayload { scan_id, message });
        }
    }
}

/// Resolves the provider families + roots for a scope (CLI flag semantics).
///
/// F07: cache / agents / unknown scopes do not run kondo discovery but still
/// inject the *protection* roots so the tool-path verifier (R07) sees them.
fn scope_plan(scope: &ScanScope) -> (&'static [FamilyFn], RootsMode) {
    match scope {
        ScanScope::Agents => (&AGENT_FAMILIES, RootsMode::ProtectionRoots),
        ScanScope::DevCache => (&DEV_CACHE_FAMILIES, RootsMode::ProtectionRoots),
        ScanScope::Projects { roots } => (
            &PROJECT_FAMILIES,
            RootsMode::Explicit(roots.iter().map(PathBuf::from).collect()),
        ),
        ScanScope::Unknown => (&UNKNOWN_FAMILIES, RootsMode::ProtectionRoots),
        ScanScope::Default { workspace_roots: None } => (&ALL_FAMILIES, RootsMode::DefaultCandidates),
        ScanScope::Default { workspace_roots: Some(roots) } if roots.is_empty() => {
            (&NO_PROJECT_FAMILIES, RootsMode::Explicit(Vec::new()))
        }
        ScanScope::Default { workspace_roots: Some(roots) } => (
            &ALL_FAMILIES,
            RootsMode::Explicit(roots.iter().map(PathBuf::from).collect()),
        ),
    }
}

/// Runs the provider pipeline for a scope and persists the snapshot.
///
/// Returns the saved [`ScanSnapshot`] (partial results and the `cancelled`
/// flag included when the cancel flag was raised mid-run). `scan_rules` is the
/// F-2-1 rule gate assembled (fail-closed) before the scan started.
pub fn run_scan(
    data_dir: &Path,
    scope: &ScanScope,
    cancel: Arc<AtomicBool>,
    emit: &mut dyn FnMut(UiEvent),
    env: ScanEnv,
    scan_rules: Option<crate::support::ScanRules>,
) -> Result<ScanSnapshot, String> {
    let (families, roots) = scope_plan(scope);
    run_families(data_dir, families, roots, cancel, emit, env, scan_rules)
}

/// Core scan driver — kept free of any Tauri type so unit tests can drive it
/// with fake environments and fixture/fake providers.
fn run_families(
    data_dir: &Path,
    families: &[FamilyFn],
    roots: RootsMode,
    cancel: Arc<AtomicBool>,
    emit: &mut dyn FnMut(UiEvent),
    env: ScanEnv,
    scan_rules: Option<crate::support::ScanRules>,
) -> Result<ScanSnapshot, String> {
    let cancel_flag = Arc::clone(&cancel);
    let should_continue = Box::new(move || !cancel_flag.load(Ordering::SeqCst));

    // Provider progress arrives through the ScanContext (a 'static callback,
    // so it cannot borrow the caller's `emit`). Route it through a pending
    // queue drained at family boundaries — ordering is preserved because the
    // provider call is synchronous.
    let pending: Rc<RefCell<Vec<UiEvent>>> = Rc::new(RefCell::new(Vec::new()));
    let progress = {
        let pending = Rc::clone(&pending);
        move |event: ProgressEvent| {
            let ui = match event {
                ProgressEvent::ProviderStart(slug) => Some(UiEvent::Progress {
                    provider: slug.to_string(),
                    stage: "started".into(),
                }),
                ProgressEvent::ProviderDone(slug) => Some(UiEvent::Progress {
                    provider: slug.to_string(),
                    stage: "done".into(),
                }),
                ProgressEvent::ProjectFound { .. } => Some(UiEvent::Progress {
                    provider: "kondo".into(),
                    stage: "project".into(),
                }),
                // No per-file progress (SPEC §28); measured echoes are dropped.
                ProgressEvent::Measured { .. } => None,
            };
            if let Some(ui) = ui {
                pending.borrow_mut().push(ui);
            }
        }
    };

    let mut ctx = match (env.env, env.tool) {
        (None, _) => {
            let mut c = ScanContext::real_with_progress(
                Box::new(ShellTool),
                Vec::new(),
                should_continue,
                Box::new(progress),
            );
            // R01: real scans install the Windows identity probe so every item
            // is fingerprinted at scan time.
            c.set_probe(Arc::new(WindowsPathProbe));
            c
        }
        (Some(env), Some(tool)) => ScanContext::with_env_and_progress(
            env,
            tool,
            Vec::new(),
            should_continue,
            Box::new(progress),
        ),
        (Some(_), None) => {
            return Err("injected scan environment requires a tool".to_string());
        }
    };

    // F05: an injected probe override (tests) replaces the real one.
    if let Some(probe) = env.probe {
        ctx.set_probe(probe);
    }

    // F-2-1 rule gate: install rules + pre-seed user-ignore paths.
    if let Some(rules) = scan_rules {
        ctx.set_rules(Arc::clone(&rules.rules));
        for ignore in &rules.ignores {
            ctx.seen_insert(ignore);
        }
    }

    // Drain the queued progress events into the caller's sink.
    let drain_pending = |emit: &mut dyn FnMut(UiEvent)| {
        let mut queued = pending.borrow_mut();
        for event in queued.drain(..) {
            emit(event);
        }
    };

    match roots {
        RootsMode::Explicit(roots) => ctx.workspace_roots.extend(roots),
        RootsMode::DefaultCandidates | RootsMode::ProtectionRoots => {
            let env_roots = ctx.env("DEVRESIDUE_WORKSPACE_ROOTS");
            let mut roots =
                project::kondo::resolve_workspace_roots(&[], env_roots.as_deref(), &ctx);
            ctx.workspace_roots.append(&mut roots);
        }
    }

    let mut items: Vec<ScanItem> = Vec::new();
    for family in families {
        if ctx.cancelled() {
            break;
        }
        // Kondo is the one provider whose discovery can walk a huge workspace
        // project by project; deliver each project as soon as it is measured
        // (Phase 16) so the UI streams `scan://item` incrementally instead of
        // waiting for the whole family. Every other family keeps the batch
        // path. Both paths apply the F-2-1 rule gate per item before emission
        // and preserve id/order semantics.
        if std::ptr::fn_addr_eq(*family, project::scan as FamilyFn) {
            let mut collected = Vec::new();
            {
                let ctx_ref = &ctx;
                let pending = Rc::clone(&pending);
                project::kondo::scan_with_sink(ctx_ref, &mut |batch| {
                    // Flush progress produced so far (provider start, project
                    // found) before this project's items arrive.
                    let mut queued = pending.borrow_mut();
                    for event in queued.drain(..) {
                        emit(event);
                    }
                    for mut item in batch {
                        apply_rule_classification(ctx_ref, &mut item);
                        // R01: fingerprint at scan time when a probe is wired.
                        if let Err(e) = ctx_ref.fingerprint_item(&mut item) {
                            ctx_ref.warn(e);
                            continue;
                        }
                        emit(UiEvent::Item(Box::new(item.clone())));
                        collected.push(item);
                    }
                });
            }
            drain_pending(emit);
            items.extend(collected);
        } else {
            let mut found = family(&ctx);
            drain_pending(emit);
            // F-2-1: let the rule gate reclassify each item before streaming it.
            // F05: an item whose scan-time fingerprint fails is **removed from
            // the batch** — it is neither streamed nor persisted. A real item
            // without an authorisation snapshot must never appear in results
            // (the planner now also refuses such items, but the scan side must
            // not surface them in the first place). The user sees the reason
            // through a warning event.
            let mut kept: Vec<ScanItem> = Vec::with_capacity(found.len());
            for mut item in found.drain(..) {
                apply_rule_classification(&ctx, &mut item);
                // R01: scan-time fingerprint (with a probe wired).
                if let Err(e) = ctx.fingerprint_item(&mut item) {
                    ctx.warn(format!("item excluded: scan-time fingerprint failed: {e}"));
                    continue;
                }
                emit(UiEvent::Item(Box::new(item.clone())));
                kept.push(item);
            }
            items.extend(kept);
        }
    }

    let cancelled = ctx.cancelled();
    let warnings = ctx.warnings();
    drain_pending(emit);
    for warning in &warnings {
        emit(UiEvent::Warning(warning.clone()));
    }
    // No UiEvent::Done here (R10): the terminal event is published by the
    // scan driver *after* the authoritative model state was updated, so the
    // driver owns the finished/cancelled/failed ordering.

    let mode = ScanMode::Real {
        workspace_roots: ctx
            .workspace_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
    };
    // R04/R3-G06: allocate the next generation for this data root and publish
    // atomically under the cross-process lock — (generation, item.id)
    // uniquely identifies this scan's items and two concurrent writers
    // (CLI + GUI) never mint the same generation.
    let snapshot = scan_store::save_with_next_generation(data_dir, |generation| ScanSnapshot {
        generation,
        cancelled,
        ..ScanSnapshot::new(mode, items, warnings)
    })?;
    Ok(snapshot)
}

/// `cancel_scan` command: flips the active scan's flag. Returns false when no
/// scan with that id is running (it already finished — the UI ignores it).
#[tauri::command]
pub fn cancel_scan(state: State<'_, AppState>, scan_id: u64) -> bool {
    state.model.lock().unwrap().cancel_scan(scan_id)
}

/// `get_scan_results` command: the most recent finished scan snapshot.
#[tauri::command]
pub fn get_scan_results(state: State<'_, AppState>) -> Result<ScanSnapshot, CommandError> {
    let model = state.model.lock().unwrap();
    model.latest().cloned().ok_or_else(|| {
        CommandError::new(
            ErrorCode::ScanNotFound,
            "no scan result yet — run a scan first",
        )
    })
}

/// Opens a native workspace picker when one is available.
///
/// The shell intentionally has no dialog plugin dependency yet. Returning
/// `None` is a safe cancellation/no-op and keeps the manual path field as the
/// authoritative input until a native picker is added behind an explicit
/// dependency change.
#[tauri::command]
pub fn pick_workspace_directory() -> Result<Option<String>, CommandError> {
    Ok(None)
}

fn normalize_workspace_root(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut normalized = trimmed.replace('/', "\\");
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    Some(normalized)
}

fn is_absolute_workspace_root(value: &str) -> bool {
    value.starts_with("\\\\")
        || value.starts_with('/')
        || (value.as_bytes().get(1) == Some(&b':')
            && value
                .as_bytes()
                .get(2)
                .is_some_and(|byte| *byte == b'\\' || *byte == b'/'))
}

fn root_contains(parent: &str, child: &str) -> bool {
    let parent = parent.trim_end_matches(['\\', '/']);
    let parent_lower = parent.to_ascii_lowercase();
    let child_lower = child.to_ascii_lowercase();
    child_lower
        .strip_prefix(&parent_lower)
        .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
}

fn workspace_root_validation_error(
    normalized: Option<&str>,
    absolute: bool,
    duplicate: bool,
    contained_by: Option<&str>,
) -> Option<(&'static str, &'static str)> {
    if normalized.is_none() {
        Some(("blank", "目录不能为空"))
    } else if !absolute {
        Some(("not-absolute", "请输入绝对目录路径"))
    } else if duplicate {
        Some(("duplicate", "与前面的目录重复"))
    } else if contained_by.is_some() {
        Some(("nested", "被前面的工作区目录包含"))
    } else {
        None
    }
}

/// Validates workspace roots without scanning or changing any filesystem
/// state. Results are returned per input so the UI can show all corrections.
#[tauri::command]
pub fn validate_workspace_roots(
    roots: Vec<String>,
) -> Result<Vec<WorkspaceRootValidationDto>, CommandError> {
    let normalized: Vec<Option<String>> = roots
        .iter()
        .map(|input| normalize_workspace_root(input))
        .collect();

    Ok(roots
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let value = normalized[index].as_deref();
            let absolute = value.is_some_and(is_absolute_workspace_root);
            let duplicate = value.is_some_and(|candidate| {
                normalized[..index]
                    .iter()
                    .flatten()
                    .any(|previous| previous.eq_ignore_ascii_case(candidate))
            });
            let contained_by = value.and_then(|candidate| {
                normalized[..index]
                    .iter()
                    .flatten()
                    .find(|previous| root_contains(previous, candidate))
                    .cloned()
            });
            let mut error = workspace_root_validation_error(
                value,
                absolute,
                duplicate,
                contained_by.as_deref(),
            );
            if error.is_none() {
                if let Some(path) = value.map(Path::new) {
                    match std::fs::metadata(path) {
                        Ok(metadata) if !metadata.is_dir() => {
                            error = Some(("not-directory", "路径不是目录"));
                        }
                        Err(_) => {
                            error = Some(("not-found", "目录不存在或不可访问"));
                        }
                        Ok(_) => {}
                    }
                }
            }
            WorkspaceRootValidationDto {
                input: input.clone(),
                normalized: value.map(str::to_string),
                valid: error.is_none(),
                error_code: error.map(|(code, _)| code.to_string()),
                message: error.map(|(_, message)| message.to_string()),
                duplicate,
                contained_by,
            }
        })
        .collect())
}

fn env_workspace_roots() -> Vec<String> {
    let Ok(raw) = std::env::var("DEVRESIDUE_WORKSPACE_ROOTS") else {
        return Vec::new();
    };
    raw.split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn scope_preview_parts(scope: &ScanScope) -> (Vec<String>, Vec<String>, Vec<String>) {
    match scope {
        ScanScope::Agents => (
            vec!["Agent 数据".into()],
            Vec::new(),
            vec!["Agent 目录由后端按当前用户环境解析".into()],
        ),
        ScanScope::DevCache => (
            vec!["工具缓存".into()],
            Vec::new(),
            vec!["工具缓存目录由后端按当前用户环境解析".into()],
        ),
        ScanScope::Projects { roots } => (vec!["Kondo 项目".into()], roots.clone(), Vec::new()),
        ScanScope::Unknown => (
            vec!["未知开发数据".into()],
            Vec::new(),
            vec!["未知数据目录由后端按当前用户环境解析".into()],
        ),
        ScanScope::Default {
            workspace_roots: Some(roots),
        } if roots.is_empty() => (
            vec!["工具缓存".into(), "Agent 数据".into(), "未知开发数据".into()],
            Vec::new(),
            vec!["已明确跳过项目目录扫描".into()],
        ),
        ScanScope::Default {
            workspace_roots: Some(roots),
        } => (vec!["工具缓存".into(), "Kondo 项目".into(), "Agent 数据".into(), "未知开发数据".into()], roots.clone(), Vec::new()),
        ScanScope::Default {
            workspace_roots: None,
        } => (
            vec!["工具缓存".into(), "Kondo 项目".into(), "Agent 数据".into(), "未知开发数据".into()],
            env_workspace_roots(),
            vec!["工作区目录由后端自动解析".into()],
        ),
    }
}

/// Returns a provider/root preview only; it does not run providers or read
/// files from the workspace.
#[tauri::command]
pub fn get_scan_scope_preview(
    _state: State<'_, AppState>,
    scope: ScanScope,
) -> Result<ScanScopePreviewDto, CommandError> {
    let (providers, workspace_roots, mut warnings) = scope_preview_parts(&scope);
    Ok(ScanScopePreviewDto {
        scope,
        providers,
        known_locations: workspace_roots.clone(),
        workspace_roots,
        deferred_locations: vec!["外部工具报告的缓存目录".into()],
        warnings: {
            warnings.push("预览不会读取文件内容或创建清理计划".into());
            warnings
        },
    })
}

/// Returns the application-owned data directory used by this Tauri process.
#[tauri::command]
pub fn get_app_data_info(state: State<'_, AppState>) -> Result<AppDataInfoDto, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().display().to_string();
    Ok(AppDataInfoDto {
        data_dir,
        backend_mode: "tauri".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_providers::fixtures::fixture_scan_items;
    use devresidue_providers::scan_ctx::NoTool;
    use std::collections::HashMap;

    fn tmp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dr-tauri-{tag}-{}", std::process::id()))
    }

    fn sample_env(profile: &Path) -> EnvMap {
        let mut env = HashMap::new();
        env.insert("USERPROFILE".into(), profile.display().to_string());
        env.insert(
            "LOCALAPPDATA".into(),
            profile.join("AppData").join("Local").display().to_string(),
        );
        env.insert(
            "APPDATA".into(),
            profile
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

    /// Fake provider family: fixed fixture items + synthetic progress (the
    /// exact event shape real providers emit per provider).
    fn fake_family(ctx: &ScanContext) -> Vec<ScanItem> {
        ctx.progress(ProgressEvent::ProviderStart("fake"));
        let items = fixture_scan_items();
        ctx.progress(ProgressEvent::ProviderDone("fake"));
        items
    }

    /// A second fake family (proves cancellation skips later families).
    fn fake_family_b(ctx: &ScanContext) -> Vec<ScanItem> {
        ctx.progress(ProgressEvent::ProviderStart("fake-b"));
        let items = fixture_scan_items();
        ctx.progress(ProgressEvent::ProviderDone("fake-b"));
        items
    }

    fn cleanup(data: &Path, profile: &Path) {
        let _ = std::fs::remove_dir_all(data);
        let _ = std::fs::remove_dir_all(profile);
    }

    /// A probe whose every query fails (NotFound) — drives the F05
    /// fingerprint-failure exclusion deterministically.
    struct AlwaysFailsProbe;

    impl devresidue_core::safety::probe::PathProbe for AlwaysFailsProbe {
        fn attributes(
            &self,
            path: &std::path::Path,
        ) -> Result<
            devresidue_core::safety::probe::AttrFlags,
            devresidue_core::safety::probe::ProbeError,
        > {
            Err(devresidue_core::safety::probe::ProbeError::NotFound {
                path: path.to_path_buf(),
            })
        }
        fn reparse_info(
            &self,
            path: &std::path::Path,
        ) -> Result<
            devresidue_core::safety::probe::ReparseInfo,
            devresidue_core::safety::probe::ProbeError,
        > {
            Err(devresidue_core::safety::probe::ProbeError::NotFound {
                path: path.to_path_buf(),
            })
        }
        fn file_identity(
            &self,
            path: &std::path::Path,
        ) -> Result<
            devresidue_core::safety::probe::FileIdentity,
            devresidue_core::safety::probe::ProbeError,
        > {
            Err(devresidue_core::safety::probe::ProbeError::NotFound {
                path: path.to_path_buf(),
            })
        }
    }

    #[test]
    fn f05_fingerprint_failed_items_are_excluded_from_the_persisted_snapshot() {
        // F05: a real item whose scan-time fingerprint fails must be neither
        // streamed nor persisted. A fake family emits fixture items; the
        // injected probe fails on every query → every item is excluded, the
        // snapshot is empty and a warning explains the exclusion.
        let data = tmp_base("f05");
        let profile = tmp_base("f05-profile");
        std::fs::create_dir_all(&data).unwrap();

        let families: [FamilyFn; 1] = [fake_family];
        let mut events: Vec<UiEvent> = Vec::new();
        let probe: Arc<dyn devresidue_core::safety::probe::PathProbe + Send + Sync> =
            Arc::new(AlwaysFailsProbe);
        let snapshot = run_families(
            &data,
            &families,
            RootsMode::ProtectionRoots,
            Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
            ScanEnv::injected_with_probe(sample_env(&profile), Box::new(NoTool), probe),
            None,
        )
        .expect("scan runs");

        assert!(
            snapshot.items.is_empty(),
            "fingerprint-failed items must not be persisted: {:?}",
            snapshot.items
        );
        assert_eq!(
            count(&events, |e| matches!(e, UiEvent::Item(_))),
            0,
            "no item events for fingerprint-failed items"
        );
        assert!(
            events.iter().any(|e| matches!(e, UiEvent::Warning(w)
                if w.contains("fingerprint failed"))),
            "the exclusion must surface as a warning event: {events:?}"
        );

        // The persisted file agrees with the returned snapshot.
        let loaded = scan_store::load(&data).expect("persisted");
        assert!(loaded.items.is_empty());
        cleanup(&data, &profile);
    }

    fn count(events: &[UiEvent], what: impl Fn(&UiEvent) -> bool) -> usize {
        events.iter().filter(|e| what(e)).count()
    }

    #[test]
    fn scan_streams_events_and_persists_a_clean_snapshot() {
        let data = tmp_base("events");
        let profile = tmp_base("profile-events");
        std::fs::create_dir_all(&data).unwrap();

        let families: [FamilyFn; 2] = [fake_family, fake_family_b];
        let mut events: Vec<UiEvent> = Vec::new();
        let snapshot = run_families(
            &data,
            &families,
            RootsMode::ProtectionRoots,
            Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
            ScanEnv::injected(sample_env(&profile), Box::new(NoTool)),
            None,
        )
        .expect("scan runs");

        assert!(!snapshot.cancelled);
        assert_eq!(snapshot.items.len(), 10, "two fake families × 5 items");
        for (provider, stage) in [
            ("fake", "started"),
            ("fake", "done"),
            ("fake-b", "started"),
            ("fake-b", "done"),
        ] {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, UiEvent::Progress { provider: p, stage: s }
                    if p == provider && s == stage)),
                "missing {provider}/{stage}"
            );
        }
        assert_eq!(count(&events, |e| matches!(e, UiEvent::Item(_))), 10);

        let loaded = scan_store::load(&data).expect("persisted");
        assert_eq!(loaded.items.len(), 10);
        assert!(!loaded.cancelled);

        // R10: the terminal Done is published by the driver after the
        // authoritative model state — not by the scan itself.
        let state = AppState::at(data.clone());
        let (scan_id, _) = state.model.lock().unwrap().begin_scan().expect("begin");
        publish_terminal(&state, scan_id, Ok(snapshot), &mut |e| events.push(e))
            .expect("publish success");
        assert!(matches!(
            events.last(),
            Some(UiEvent::Done {
                cancelled: false,
                total: 10,
                generation: 1
            })
        ));
        assert!(
            state.model.lock().unwrap().latest().is_some(),
            "authoritative state must be set when done arrives"
        );
        cleanup(&data, &profile);
    }

    #[test]
    fn cancel_after_first_family_skips_later_families_and_flags_partial() {
        let data = tmp_base("cancel");
        let profile = tmp_base("profile-cancel");
        std::fs::create_dir_all(&data).unwrap();

        let families: [FamilyFn; 2] = [fake_family, fake_family_b];
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_sink = Arc::clone(&cancel);
        let mut events: Vec<UiEvent> = Vec::new();
        let mut emit = |e: UiEvent| {
            // Flip the flag once family A reports done — family B must never
            // start (the driver checks the flag between families).
            if matches!(&e, UiEvent::Progress { provider, stage }
                if provider == "fake" && stage == "done")
            {
                cancel_sink.store(true, Ordering::SeqCst);
            }
            events.push(e);
        };
        let snapshot = run_families(
            &data,
            &families,
            RootsMode::ProtectionRoots,
            cancel,
            &mut emit,
            ScanEnv::injected(sample_env(&profile), Box::new(NoTool)),
            None,
        )
        .expect("scan runs");

        assert!(snapshot.cancelled);
        assert_eq!(snapshot.items.len(), 5, "only family A kept");
        assert_eq!(count(&events, |e| matches!(e, UiEvent::Item(_))), 5);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, UiEvent::Progress { provider, .. }
                if provider == "fake-b")),
            "family B must not start"
        );

        // R10: driver publishes the cancelled Done after the model transition.
        let state = AppState::at(data.clone());
        let (scan_id, _) = state.model.lock().unwrap().begin_scan().expect("begin");
        publish_terminal(&state, scan_id, Ok(snapshot), &mut |e| events.push(e))
            .expect("publish success");
        assert!(matches!(
            events.last(),
            Some(UiEvent::Done {
                cancelled: true,
                total: 5,
                generation: 1
            })
        ));
        let published = state.model.lock().unwrap().latest().cloned().unwrap();
        assert!(published.cancelled);

        let loaded = scan_store::load(&data).expect("persisted partial");
        assert!(loaded.cancelled);
        assert_eq!(loaded.items.len(), 5);
        cleanup(&data, &profile);
    }

    #[test]
    fn mole_derived_rules_reclassify_discovered_cache_dirs() {
        // Mole-extracted rules (resources/rules/dev_cache/*) + the rule-driven
        // static-cache provider: scan the unknown/dev-cache families with the
        // REAL built-in rules and assert the new cache locations are
        // DISCOVERED (not just classified) with the rule's risk/product.
        let data = tmp_base("mole-rules");
        let profile = tmp_base("profile-mole");
        std::fs::create_dir_all(&data).unwrap();
        for rel in [
            ".hex/cache",
            ".opam/download-cache",
            ".pytest_cache",
            "go/pkg/mod/cache",
            "AppData/Local/pnpm/store",
        ] {
            std::fs::create_dir_all(profile.join(rel)).unwrap();
            std::fs::write(profile.join(rel).join("f"), b"x").unwrap();
        }

        // Load the REAL builtin rule files with the FIXTURE env so every
        // %ENV% anchor expands into the fixture profile (mirrors production
        // loading, which uses the process env).
        let rules_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/rules");
        let merged = devresidue_core::rules::load_rules(&rules_dir, &|k: &str| {
            sample_env(&profile).get(k).cloned()
        });
        assert!(merged.is_clean(), "rules load: {:?}", merged.issues);
        let rules = crate::support::ScanRules {
            rules: std::sync::Arc::new(merged),
            ignores: Vec::new(),
        };
        // Install rules into the context via run_families' scan_rules wiring.
        let families: [FamilyFn; 1] = [dev_cache::scan];
        let snapshot = run_families(
            &data,
            &families,
            RootsMode::ProtectionRoots,
            Arc::new(AtomicBool::new(false)),
            &mut |_e| {},
            ScanEnv::injected(sample_env(&profile), Box::new(NoTool)),
            Some(rules),
        )
        .expect("scan runs");

        let find = |needle: &str| {
            snapshot
                .items
                .iter()
                .find(|it| it.path.to_string_lossy().contains(needle))
                .unwrap_or_else(|| panic!("missing item for {needle}; items={:?}", snapshot.items))
        };
        use devresidue_core::RiskLevel as RL;

        let hex = find(".hex");
        assert_eq!(hex.risk, RL::RegenerableDownload);
        assert_eq!(hex.product.as_deref(), Some("Hex"));
        assert_eq!(
            hex.classification_rule_id.as_deref(),
            Some("builtin-detection/hex-cache")
        );

        let opam = find(".opam");
        assert_eq!(opam.risk, RL::RegenerableDownload);
        assert_eq!(
            opam.classification_rule_id.as_deref(),
            Some("builtin-detection/opam-download-cache")
        );

        let pytest = find(".pytest_cache");
        assert_eq!(pytest.risk, RL::RegenerableLocal);
        assert_eq!(
            pytest.classification_rule_id.as_deref(),
            Some("builtin-detection/pytest-cache-home")
        );

        let gomod = find(r"go\pkg\mod\cache");
        assert_eq!(gomod.risk, RL::RegenerableDownload);
        assert_eq!(
            gomod.classification_rule_id.as_deref(),
            Some("builtin-detection/go-module-cache")
        );

        let pnpm = find("pnpm");
        assert_eq!(pnpm.risk, RL::RegenerableDownload);
        assert_eq!(
            pnpm.classification_rule_id.as_deref(),
            Some("builtin-detection/pnpm-store")
        );

        cleanup(&data, &profile);
    }

    #[test]
    fn dev_cache_scope_streams_provider_events_and_fallback_warnings_offline() {
        // Injected empty profile + NoTool: providers fall back to known
        // defaults that do not exist → no items, but the per-provider event
        // sequence still streams and tool-query fallbacks surface as warnings.
        let data = tmp_base("real-dev-cache");
        let profile = tmp_base("profile-real");
        std::fs::create_dir_all(&data).unwrap();

        let mut events: Vec<UiEvent> = Vec::new();
        let snapshot = run_scan(
            &data,
            &ScanScope::DevCache,
            Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
            ScanEnv::injected(sample_env(&profile), Box::new(NoTool)),
            None,
        )
        .expect("dev-cache scan runs");

        assert!(!snapshot.cancelled);
        assert!(snapshot.items.is_empty());
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("known default")),
            "tool-query fallbacks must warn: {:?}",
            snapshot.warnings
        );
        for slug in ["npm", "bun", "pip", "uv", "cargo", "nuget"] {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, UiEvent::Progress { provider, stage }
                    if provider == slug && stage == "started")),
                "missing {slug} start"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, UiEvent::Progress { provider, stage }
                    if provider == slug && stage == "done")),
                "missing {slug} done"
            );
        }
        cleanup(&data, &profile);
    }

    #[test]
    fn f07_dev_cache_scope_injects_the_workspace_protection_roots() {
        // F07: a DevCache-only scan runs no kondo discovery but must still
        // receive the configured workspace roots so the tool-path verifier can
        // reject a tool that reports a workspace root (R07). The roots are
        // read from the injected env and travel into the persisted snapshot's
        // mode.
        let data = tmp_base("f07");
        let profile = tmp_base("f07-profile");
        std::fs::create_dir_all(&data).unwrap();

        let mut env = sample_env(&profile);
        env.insert(
            "DEVRESIDUE_WORKSPACE_ROOTS".into(),
            "D:\\review\\workspace;E:\\projects".into(),
        );
        let snapshot = run_scan(
            &data,
            &ScanScope::DevCache,
            Arc::new(AtomicBool::new(false)),
            &mut |_| {},
            ScanEnv::injected(env, Box::new(NoTool)),
            None,
        )
        .expect("dev-cache scan runs");

        let ScanMode::Real { workspace_roots } = &snapshot.mode else {
            panic!("dev-cache scan must produce a Real snapshot");
        };
        assert!(
            workspace_roots.iter().any(|r| {
                devresidue_core::safety::canonical::is_within(
                    Path::new(r"D:\review\workspace"),
                    Path::new(r),
                )
            }),
            "the first configured root must be injected: {workspace_roots:?}"
        );
        assert!(
            workspace_roots.iter().any(|r| r == "E:\\projects"),
            "the second configured root must be injected: {workspace_roots:?}"
        );
        cleanup(&data, &profile);
    }

    #[test]
    fn model_lifecycle_begin_cancel_finish() {
        let data = tmp_base("model");
        std::fs::create_dir_all(&data).unwrap();
        let state = AppState::at(data.clone());

        let (scan_id, cancel) = state.model.lock().unwrap().begin_scan().expect("begin");
        assert!(state.model.lock().unwrap().is_active(scan_id));
        assert!(!state.model.lock().unwrap().cancel_scan(scan_id + 99));
        assert!(state.model.lock().unwrap().cancel_scan(scan_id));
        assert!(cancel.load(Ordering::SeqCst));
        assert!(state.model.lock().unwrap().begin_scan().is_err());

        let snapshot = ScanSnapshot::new(
            ScanMode::Real {
                workspace_roots: vec![],
            },
            vec![],
            vec![],
        );
        state.model.lock().unwrap().finish_scan(scan_id, snapshot);
        assert!(!state.model.lock().unwrap().is_active(scan_id));
        assert!(state.model.lock().unwrap().begin_scan().is_ok());
        assert!(state.model.lock().unwrap().latest().is_some());
        cleanup(&data, &data);
    }

    #[test]
    fn done_event_reaches_the_frontend_only_after_authoritative_state() {
        // R10 core ordering: when the Done event is emitted, the model's
        // `latest` must already be the exact snapshot the event describes —
        // a frontend that reacts to done by calling get_scan_results can
        // never read a stale/older snapshot.
        let data = tmp_base("r10-order");
        let profile = tmp_base("r10-order-profile");
        std::fs::create_dir_all(&data).unwrap();

        let families: [FamilyFn; 1] = [fake_family];
        let snapshot = run_families(
            &data,
            &families,
            RootsMode::ProtectionRoots,
            Arc::new(AtomicBool::new(false)),
            &mut |_| {},
            ScanEnv::injected(sample_env(&profile), Box::new(NoTool)),
            None,
        )
        .expect("scan runs");
        let expected_gen = snapshot.generation;

        let state = AppState::at(data.clone());
        let (scan_id, _) = state.model.lock().unwrap().begin_scan().expect("begin");

        let mut saw_done = false;
        publish_terminal(&state, scan_id, Ok(snapshot), &mut |event| {
            if let UiEvent::Done {
                cancelled,
                total,
                generation,
            } = event
            {
                saw_done = true;
                // The authoritative state must already be visible at the
                // moment the terminal event is handed to the emitter.
                let latest = state
                    .model
                    .lock()
                    .unwrap()
                    .latest()
                    .expect("latest must be set before done")
                    .clone();
                assert_eq!(latest.generation, generation, "done generation = latest");
                assert_eq!(latest.cancelled, cancelled);
                assert_eq!(latest.items.len(), total);
            }
        })
        .expect("publish success");
        assert!(saw_done, "publish_terminal must emit the Done event");
        assert_eq!(expected_gen, 1, "first scan in an empty data root");
        assert!(!state.model.lock().unwrap().is_active(scan_id));
        cleanup(&data, &profile);
    }

    #[test]
    fn failed_scan_clears_active_emits_error_and_keeps_latest_untouched() {
        // R10 error path: no done event, active handle cleared, the previous
        // authoritative snapshot survives.
        let data = tmp_base("r10-error");
        std::fs::create_dir_all(&data).unwrap();
        let state = AppState::at(data.clone());

        // Seed a previous finished scan as `latest`.
        let (first_id, _) = state.model.lock().unwrap().begin_scan().expect("begin");
        let prior = ScanSnapshot::new(
            ScanMode::Real {
                workspace_roots: vec![],
            },
            vec![],
            vec![],
        );
        state
            .model
            .lock()
            .unwrap()
            .finish_scan(first_id, prior.clone());

        // A new scan fails mid-run.
        let (scan_id, _) = state.model.lock().unwrap().begin_scan().expect("begin");
        let mut events: Vec<UiEvent> = Vec::new();
        let outcome = publish_terminal(
            &state,
            scan_id,
            Err("simulated scan failure".to_string()),
            &mut |e| events.push(e),
        );
        assert!(outcome.is_err(), "the failure must propagate");

        assert!(
            !state.model.lock().unwrap().is_active(scan_id),
            "failed scan must clear the active handle"
        );
        assert!(
            state.model.lock().unwrap().begin_scan().is_ok(),
            "a new scan may start after the failure"
        );
        let latest = state.model.lock().unwrap().latest().cloned().unwrap();
        assert_eq!(latest, prior, "failed scan must not replace latest");
        assert_eq!(events.len(), 1, "exactly one terminal event");
        assert!(
            matches!(&events[0], UiEvent::Failed { message } if message == "simulated scan failure"),
            "expected a Failed event, got {:?}",
            events[0]
        );
        assert!(
            !events.iter().any(|e| matches!(e, UiEvent::Done { .. })),
            "a failed scan must never emit done"
        );
        cleanup(&data, &data);
    }

    /// Real three-project workspace farm on a temp dir (cargo/node/cmake).
    fn write_farm(root: &Path) {
        let mk = |files: &[&str]| {
            for f in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, b"x").unwrap();
            }
        };
        mk(&[
            "cargo/Cargo.toml",
            "cargo/src/main.rs",
            "cargo/target/debug/x.exe",
        ]);
        mk(&[
            "node/package.json",
            "node/src/index.js",
            "node/node_modules/p/index.js",
        ]);
        mk(&[
            "cmake/CMakeLists.txt",
            "cmake/src/a.cpp",
            "cmake/cmake-build-debug/o.exe",
        ]);
    }

    #[test]
    fn kondo_projects_scope_streams_one_item_batch_per_project_before_done() {
        // Phase 16: the Projects scope drives kondo through its per-project
        // streaming sink. Every `scan://item` event must arrive *before* the
        // provider-done progress (i.e. the UI sees items while the walk is
        // still running), and the persisted snapshot equals the streamed set.
        let data = tmp_base("kondo-stream");
        let profile = tmp_base("kondo-stream-profile");
        std::fs::create_dir_all(&data).unwrap();
        let farm = tmp_base("kondo-stream-farm");
        write_farm(&farm);

        let mut events: Vec<UiEvent> = Vec::new();
        let snapshot = run_scan(
            &data,
            &ScanScope::Projects {
                roots: vec![farm.to_string_lossy().into_owned()],
            },
            Arc::new(AtomicBool::new(false)),
            &mut |e| events.push(e),
            ScanEnv::injected(sample_env(&profile), Box::new(NoTool)),
            None,
        )
        .expect("projects scan runs");

        let item_events = count(&events, |e| matches!(e, UiEvent::Item(_)));
        assert_eq!(item_events, snapshot.items.len(), "stream = snapshot");
        assert_eq!(item_events, 3, "three farm projects → three items");

        // The first Item event must arrive after the provider started but with
        // item events interleaved before the final "done" — i.e. not batched
        // until family end. Concretely: every Item event precedes the kondo
        // "done" progress event.
        let done_idx = events
            .iter()
            .position(|e| {
                matches!(e, UiEvent::Progress { provider, stage }
                if provider == "kondo" && stage == "done")
            })
            .expect("kondo done progress must be emitted");
        assert!(
            events[..done_idx]
                .iter()
                .filter(|e| matches!(e, UiEvent::Item(_)))
                .count()
                == item_events,
            "all items stream before the provider-done marker"
        );
        // Snapshot paths match the events in order.
        let mut streamed_paths: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                UiEvent::Item(it) => Some(it.path.display().to_string()),
                _ => None,
            })
            .collect();
        streamed_paths.sort();
        let mut snap_paths: Vec<String> = snapshot
            .items
            .iter()
            .map(|i| i.path.display().to_string())
            .collect();
        snap_paths.sort();
        assert_eq!(streamed_paths, snap_paths);

        let _ = std::fs::remove_dir_all(&farm);
        cleanup(&data, &profile);
    }
}
