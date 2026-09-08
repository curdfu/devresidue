//! Agent-provider integration tests: tempdir fixtures impersonating the six
//! Phase-9 agent layouts; asserts per-entry splitting, classification,
//! protected faces being absent, and the OpenCode session-db action.

mod common;

use std::collections::HashMap;
use std::path::Path;

use devresidue_core::{CleanupAction, RiskLevel, ScanItem};
use devresidue_providers::agents::layout::AgentLayout;
use devresidue_providers::agents::{self, layouts, AgentLayoutProvider};

use common::{ctx, make_dir_with_files, sample_env, FailingTool, TempDir};

/// Env map with the three data roots pointed at a tempdir's subdirs.
fn agent_env(base: &Path) -> HashMap<String, String> {
    let mut env = sample_env();
    env.insert(
        "USERPROFILE".into(),
        base.join("profile").to_string_lossy().into_owned(),
    );
    env.insert(
        "APPDATA".into(),
        base.join("appdata").to_string_lossy().into_owned(),
    );
    env.insert(
        "LOCALAPPDATA".into(),
        base.join("local").to_string_lossy().into_owned(),
    );
    env
}

fn run_layout(env: HashMap<String, String>, layout: &'static AgentLayout) -> Vec<ScanItem> {
    let context = ctx(env, Box::new(FailingTool), vec![], true);
    AgentLayoutProvider::new(layout).scan(&context)
}

/// Scans all agents over one shared context (same env + seen-set).
fn scan_all(env: HashMap<String, String>) -> Vec<ScanItem> {
    let context = ctx(env, Box::new(FailingTool), vec![], true);
    agents::scan(&context)
}

fn sep_agnostic(s: &str) -> String {
    s.replace('/', "\\")
}

fn norm_paths(items: &[ScanItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| sep_agnostic(&i.path.to_string_lossy()))
        .collect()
}

#[test]
fn codex_full_layout_splits_and_classifies() {
    let dir = TempDir::new();
    let base = dir.path();
    let p = base.join("profile").join(".codex");
    make_dir_with_files(
        &p,
        &[
            ".tmp/a.bin",
            "cache/computer-use/shot.png",
            "archived_sessions/old.jsonl",
            "sessions/2026/01/session.jsonl",
            "plugins/my-plugin/data",
            ".sandbox-bin/tool.exe",
            ".sandbox/state",
            "sqlite/codex-dev.db",
            // Protected faces that MUST NOT be produced:
            ".sandbox-secrets/secret",
            "mcp-oauth-locks/x.lock",
            "rules/AGENTS.md",
            "skills/skill.md",
            "thread-writer-locks/l",
            "node_repl/x",
            "dictation-history/h",
        ],
    );
    let env = agent_env(base);
    let items = run_layout(env.clone(), &layouts::CODEX);
    let names = norm_paths(&items);

    for expected in [
        ".tmp",
        "cache\\computer-use",
        "archived_sessions",
        "sessions",
        "plugins",
        ".sandbox-bin",
        ".sandbox",
        "sqlite\\codex-dev.db",
    ] {
        assert!(
            names.iter().any(|n| n.contains(expected)),
            "missing codex entry {expected}: {names:?}"
        );
    }

    for forbidden in [
        ".sandbox-secrets",
        "mcp-oauth-locks",
        "rules",
        "skills",
        "thread-writer-locks",
        "node_repl",
        "dictation-history",
    ] {
        assert!(
            !names.iter().any(|n| n.contains(forbidden)),
            "protected codex face leaked: {forbidden}: {names:?}"
        );
    }

    let risk_of = |needle: &str| -> RiskLevel {
        items
            .iter()
            .find(|i| sep_agnostic(&i.path.to_string_lossy()).contains(needle))
            .unwrap()
            .risk
    };
    assert_eq!(risk_of(".tmp"), RiskLevel::Safe);
    assert_eq!(risk_of("archived_sessions"), RiskLevel::Review);
    assert_eq!(risk_of("sessions"), RiskLevel::Review);
    assert_eq!(risk_of("plugins"), RiskLevel::Review);

    // F-1c: computer-use screen captures record whatever was visible on screen
    // during AI sessions (password managers, mail, documents, ...). They must
    // never be treated as a Safe, regenerable cache.
    let computer_use = items
        .iter()
        .find(|i| sep_agnostic(&i.path.to_string_lossy()).contains("computer-use"))
        .expect("codex computer-use entry");
    assert_eq!(computer_use.risk, RiskLevel::Review);
    assert_eq!(
        computer_use.category,
        devresidue_core::ResidueCategory::Session,
        "screen captures are session content, not an agent cache"
    );
    assert!(
        computer_use
            .explanation
            .to_lowercase()
            .contains("screen captures taken during ai computer-use sessions"),
        "explanation must state the screen-capture nature explicitly: {}",
        computer_use.explanation
    );

    // The single-file db entry is a path-level item (default action for
    // Codex is RecycleBin; the *OpenCode* db carries action None — tested in
    // the OpenCode case below).
    let db = items
        .iter()
        .find(|i| sep_agnostic(&i.path.to_string_lossy()).contains("codex-dev.db"))
        .unwrap();
    assert_eq!(db.cleanup_action, CleanupAction::RecycleBin);
    assert_eq!(db.risk, RiskLevel::Review);
    assert!(db.file_count == 1 && db.logical_size > 0);

    for item in &items {
        assert!(!item.explanation.trim().is_empty());
    }
}

#[test]
fn claude_only_sessions_is_still_emitted() {
    let dir = TempDir::new();
    let base = dir.path();
    let p = base.join("profile").join(".claude");
    make_dir_with_files(&p, &["sessions/s1.jsonl", "sessions/s2.jsonl"]);
    let env = agent_env(base);
    let items = run_layout(env, &layouts::CLAUDE);
    let session = items
        .iter()
        .find(|i| i.path.to_string_lossy().contains("sessions"))
        .expect("claude sessions must be reported even when it is the only dir");
    assert_eq!(session.risk, RiskLevel::Review);
    assert_eq!(session.logical_size, 2, "two session files × 1 byte each");
}

#[test]
fn opencode_three_way_split_and_omo_logs() {
    let dir = TempDir::new();
    let base = dir.path();
    let share = base
        .join("profile")
        .join(".local")
        .join("share")
        .join("opencode");
    make_dir_with_files(
        &share,
        &[
            "opencode.db",
            "storage/abc123/tui-state.json",
            "storage/oh-my-opencode-slim/hash1/tui-state.json",
            "storage/oh-my-opencode-slim/hash2/tui-state.json",
            "log/opencode.log",
            "log/oh-my-opencode-slim.log",
            "log/oh-my-opencode-slim.2026-01-01.log",
            "repos/abc/work/file",
            "tool-output/out.txt",
        ],
    );
    std::fs::create_dir_all(base.join("profile").join(".cache").join("opencode")).unwrap();
    std::fs::write(
        base.join("profile")
            .join(".cache")
            .join("opencode")
            .join("cache.bin"),
        vec![0u8; 128],
    )
    .unwrap();

    let env = agent_env(base);
    let items = scan_all(env);

    // OpenCode pieces.
    let db = items
        .iter()
        .find(|i| i.path.ends_with("opencode.db"))
        .expect("opencode session db");
    assert_eq!(db.cleanup_action, CleanupAction::None);
    assert_eq!(db.risk, RiskLevel::Review);

    let storage = items
        .iter()
        .find(|i| i.path.to_string_lossy().ends_with("storage"))
        .expect("opencode workspace storage");
    assert_eq!(storage.risk, RiskLevel::Review);

    let cache_root = items
        .iter()
        .find(|i| sep_agnostic(&i.path.to_string_lossy()).contains(".cache\\opencode"))
        .expect("opencode cache root");
    assert_eq!(cache_root.risk, RiskLevel::Safe);

    // OMO workspace state + aggregated logs.
    let omo_state = items
        .iter()
        .find(|i| {
            i.product.as_deref() == Some("OMO Slim")
                && sep_agnostic(&i.path.to_string_lossy()).contains("oh-my-opencode-slim")
        })
        .expect("OMO workspace state item");
    assert_eq!(omo_state.risk, RiskLevel::Review);

    let omo_logs = items
        .iter()
        .find(|i| {
            i.product.as_deref() == Some("OMO Slim")
                && i.category == devresidue_core::ResidueCategory::Log
        })
        .expect("OMO log aggregation item");
    assert_eq!(omo_logs.file_count, 2, "two omo logs aggregated");
    assert_eq!(omo_logs.risk, RiskLevel::Safe);
    // F-1e: the glob aggregate's path is the parent log directory — a
    // directory-level recycle would delete the whole log dir (including
    // OpenCode's own logs), so it must be non-cleanable with a pointer at the
    // parent entry.
    assert_eq!(omo_logs.cleanup_action, CleanupAction::None);
    assert!(
        omo_logs
            .explanation
            .contains("select the parent log-directory entry to clean"),
        "explanation must point the user at the parent log-directory item: {}",
        omo_logs.explanation
    );
}

#[test]
fn cursor_only_mcp_config_produces_nothing() {
    let dir = TempDir::new();
    let base = dir.path();
    let cursor = base.join("appdata").join("Cursor");
    make_dir_with_files(&cursor, &["User/mcp.json"]);
    let env = agent_env(base);
    let items = run_layout(env, &layouts::CURSOR);
    assert!(
        items.is_empty(),
        "config-only Cursor must not yield cleanable items"
    );
}

#[test]
fn missing_roots_are_empty_not_errors() {
    let dir = TempDir::new();
    let env = agent_env(dir.path());
    for layout in layouts::ALL {
        let items = run_layout(env.clone(), layout);
        assert!(items.is_empty(), "absent {} must be empty", layout.slug);
    }
}

#[test]
fn windsurf_absent_is_empty() {
    let dir = TempDir::new();
    let env = agent_env(dir.path());
    let items = run_layout(env, &layouts::WINDSURF);
    assert!(items.is_empty());
}
