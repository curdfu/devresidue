//! Unknown developer data provider integration tests (SPEC §25 / Phase 13):
//! heuristic emission, known-provider-root and protected-root exclusion,
//! user Ignore/Protect dispositions, and the F-2-1 rule gate reclassification.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use devresidue_core::rules::{load_rules, load_user_ignore_paths};
use devresidue_core::RiskLevel;
use devresidue_providers::scan_ctx::{apply_rule_classification, ScanContext};
use devresidue_providers::unknown;

use common::{make_dir_with_files, sample_env, FailingTool, TempDir};

/// Overrides the three profile roots to live under `dir`.
fn profile_env(dir: &Path) -> HashMap<String, String> {
    let mut env = sample_env();
    env.insert("USERPROFILE".into(), dir.to_string_lossy().into_owned());
    env.insert(
        "LOCALAPPDATA".into(),
        dir.join("Local").to_string_lossy().into_owned(),
    );
    env.insert(
        "APPDATA".into(),
        dir.join("Roaming").to_string_lossy().into_owned(),
    );
    env
}

fn ctx_of(env: HashMap<String, String>) -> ScanContext {
    common::ctx(env, Box::new(FailingTool), vec![], true)
}

#[test]
fn name_hits_layout_hits_emit_plain_dirs_do_not() {
    let dir = TempDir::new();
    // Name heuristic: .rustup is a developer tool directory.
    make_dir_with_files(
        &dir.path().join(".rustup"),
        &["toolchains/stable/bin/cargo.exe", "settings.toml"],
    );
    // Layout heuristic: no tool name, but ≥3 developer-layout children.
    let boxed = dir.path().join(".devbox");
    make_dir_with_files(
        &boxed,
        &["bin/x", "cache/y", "src/z", "config/toml", "a.txt"],
    );
    // Plain content directory: nothing tool-like → must NOT be emitted.
    make_dir_with_files(
        &dir.path().join(".documents-archive"),
        &["notes/a.txt", "b.md", "photos/c.jpg"],
    );

    let context = ctx_of(profile_env(dir.path()));
    let items = unknown::scan(&context);

    let names: Vec<String> = items.iter().map(|i| i.display_name.clone()).collect();
    assert!(
        names.contains(&".rustup".to_string()),
        "name hit missing: {names:?}"
    );
    assert!(
        names.contains(&".devbox".to_string()),
        "layout hit missing: {names:?}"
    );
    assert!(
        !names.contains(&".documents-archive".to_string()),
        "plain directory must not be reported: {names:?}"
    );
    // Every emitted item is Unknown risk / Unknown category / action None.
    for item in &items {
        assert_eq!(item.risk, RiskLevel::Unknown);
        assert_eq!(item.category, devresidue_core::ResidueCategory::Unknown);
        assert_eq!(item.cleanup_action, devresidue_core::CleanupAction::None);
        assert_eq!(item.source, devresidue_core::SourceKind::UnknownProvider);
        assert!(item.explanation.contains("unknown developer data"));
    }
}

#[test]
fn app_data_zones_are_scanned_and_provider_roots_excluded() {
    let dir = TempDir::new();
    // LOCALAPPDATA level: tool-named + layout-hinted children are candidates.
    make_dir_with_files(&dir.path().join("Local").join("python-tools"), &["x"]);
    make_dir_with_files(&dir.path().join("Local").join("Adobe"), &["a.dat"]);
    // npm-cache is a dev-cache managed root → never an Unknown item.
    make_dir_with_files(
        &dir.path().join("Local").join("npm-cache"),
        &["_cacache/content/x"],
    );
    // APPDATA: Cursor/Windsurf managed roots excluded; a tool-named dir is not.
    make_dir_with_files(&dir.path().join("Roaming").join("Cursor"), &["User/x"]);
    make_dir_with_files(
        &dir.path().join("Roaming").join("nvm"),
        &["v20/bin/node.exe"],
    );

    let context = ctx_of(profile_env(dir.path()));
    let items = unknown::scan(&context);
    let names: Vec<String> = items.iter().map(|i| i.display_name.clone()).collect();
    assert!(
        names.contains(&"python-tools".to_string()),
        "LOCALAPPDATA tool-name hit missing: {names:?}"
    );
    assert!(
        names.contains(&"nvm".to_string()),
        "APPDATA tool-name hit missing: {names:?}"
    );
    for excluded in ["npm-cache", "Adobe", "Cursor"] {
        assert!(
            !names.iter().any(|n| n == excluded),
            "{excluded} must not surface as Unknown: {names:?}"
        );
    }
}

#[test]
fn hidden_agent_roots_are_not_reported_as_unknown() {
    let dir = TempDir::new();
    // A fake (empty-ish) .codex tree — the agent provider owns that region.
    make_dir_with_files(&dir.path().join(".codex"), &["sessions/a.json"]);
    make_dir_with_files(&dir.path().join(".claude"), &["projects/p/x"]);
    // .local is the OpenCode share-root container.
    make_dir_with_files(
        &dir.path().join(".local").join("share"),
        &["opencode/storage/x"],
    );
    let context = ctx_of(profile_env(dir.path()));
    let items = unknown::scan(&context);
    assert!(
        items.is_empty(),
        "agent root regions must never surface as Unknown: {:?}",
        items
            .iter()
            .map(|i| i.path.display().to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn builtin_protected_roots_never_emit_and_user_protect_reclassifies() {
    let dir = TempDir::new();
    // .ssh matches the built-in protected rule (%USERPROFILE%/.ssh exact).
    make_dir_with_files(&dir.path().join(".ssh"), &["id_ed25519", "config"]);
    // .my-tool-cache would normally be a name/layout candidate.
    make_dir_with_files(
        &dir.path().join(".my-tool-cache"),
        &["bin/x", "cache/y", "src/z"],
    );

    // Rules: a resources-style directory with the built-in ssh protection,
    // plus a user container that Protects .my-tool-cache.
    let env = {
        let base = dir.path().to_string_lossy().into_owned();
        move |k: &str| {
            let map: HashMap<&str, String> = [
                ("USERPROFILE", base.clone()),
                ("LOCALAPPDATA", format!("{base}\\Local")),
                ("APPDATA", format!("{base}\\Roaming")),
            ]
            .into_iter()
            .collect();
            map.get(&k.to_ascii_uppercase().as_str()).cloned()
        }
    };
    let protected_dir = dir.child("builtin");
    std::fs::create_dir_all(&protected_dir).unwrap();
    std::fs::write(
        protected_dir.join("ssh.yaml"),
        r#"
rules:
  - id: builtin-protected/ssh
    description: SSH keys & config
    category: credential
    risk: protected
    source: builtin-protected
    match:
      exact: "%USERPROFILE%/.ssh"
"#,
    )
    .unwrap();
    let builtin = load_rules(&protected_dir, &env);
    assert!(builtin.is_clean(), "{:?}", builtin.issues);

    let user_container = dir.child("user-rules");
    let user_dir = user_container.join("user");
    std::fs::create_dir_all(&user_dir).unwrap();
    let target = dir.path().join(".my-tool-cache");
    devresidue_core::rules::upsert_disposition(
        &user_dir,
        &target,
        None,
        devresidue_core::ResidueCategory::DeveloperCache,
        devresidue_core::rules::DispositionKind::Protect,
    )
    .unwrap();
    let mut user_set = load_rules(&user_container, &env);
    assert!(user_set.is_clean(), "{:?}", user_set.issues);
    user_set.merge(builtin);

    let mut context = ctx_of(profile_env(dir.path()));
    context.set_rules(Arc::new(user_set));
    let mut items = unknown::scan(&context);
    // .ssh suppressed (built-in protected). .my-tool-cache candidate present.
    assert!(
        items.iter().all(|i| !i.display_name.ends_with(".ssh")),
        ".ssh must never be emitted"
    );
    assert_eq!(items.len(), 1, "only the user-protected candidate remains");

    // F-2-1 gate (applied by the scan assembler) reclassifies it Protected.
    for item in &mut items {
        apply_rule_classification(&context, item);
    }
    assert_eq!(items[0].risk, RiskLevel::Protected);
    assert_eq!(
        items[0].cleanup_action,
        devresidue_core::CleanupAction::None
    );
    assert!(
        items[0].evidence.iter().any(|e| e.source == "rule-match"),
        "rule evidence expected"
    );
}

#[test]
fn user_ignore_pre_seed_suppresses_the_path() {
    let dir = TempDir::new();
    let cache = dir.path().join(".my-noisy-cache");
    make_dir_with_files(&cache, &["bin/x", "cache/y", "src/z", "config"]);

    // Write an Ignore disposition for the candidate.
    let user_container = dir.child("user-rules");
    let user_dir = user_container.join("user");
    std::fs::create_dir_all(&user_dir).unwrap();
    devresidue_core::rules::upsert_disposition(
        &user_dir,
        &cache,
        None,
        devresidue_core::ResidueCategory::Unknown,
        devresidue_core::rules::DispositionKind::Ignore,
    )
    .unwrap();

    // The scan assembler pre-seeds ignore paths into the seen-set.
    let ignores = load_user_ignore_paths(&user_dir, &|k: &str| {
        let base = dir.path().to_string_lossy().into_owned();
        match k.to_ascii_uppercase().as_str() {
            "USERPROFILE" => Some(base.clone()),
            "LOCALAPPDATA" => Some(format!("{base}\\Local")),
            "APPDATA" => Some(format!("{base}\\Roaming")),
            _ => None,
        }
    });
    assert_eq!(ignores, vec![cache.clone()]);

    let context = ctx_of(profile_env(dir.path()));
    for path in &ignores {
        context.seen_insert(path);
    }
    let items = unknown::scan(&context);
    assert!(
        items.is_empty(),
        "ignored path must not be reported: {:?}",
        items
            .iter()
            .map(|i| i.path.display().to_string())
            .collect::<Vec<_>>()
    );
    let _ = PathBuf::new();
}
