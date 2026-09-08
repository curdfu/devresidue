//! Dev-cache provider integration tests: INV-011 path verification, cargo
//! sub-directory split, explanations, all against tempdir fixtures.

mod common;

use std::collections::HashMap;

use devresidue_core::RiskLevel;
use devresidue_providers::dev_cache;
use devresidue_providers::dev_cache::common::{query_verified, verify_tool_path, ToolPathVerdict};
use devresidue_providers::scan_ctx::ScanContext;

use common::{ctx, make_dir_with_files, sample_env, FailingTool, FakeTool, TempDir};

#[test]
fn tool_reporting_a_drive_root_is_rejected() {
    let env = sample_env();
    let context = ctx(
        env,
        Box::new(FakeTool::answering("npm", "C:\\")),
        vec![],
        true,
    );
    // The tool claims the cache is at C:\ — INV-011 must refuse it and the
    // provider must be skipped (warning surfaced, no item emitted).
    let items = dev_cache::npm::scan(&context);
    assert!(
        items.is_empty(),
        "npm must skip when the tool lies about C:\\"
    );
    assert!(
        context
            .warnings()
            .iter()
            .any(|w| w.contains("npm provider skipped")),
        "expected a skip warning, got {:?}",
        context.warnings()
    );
}

#[test]
fn tool_reporting_a_normal_location_is_accepted() {
    let dir = TempDir::new();
    // The reported path is a plausible, existing cache dir under the profile.
    let cache = dir.child("npm-cache");
    make_dir_with_files(&cache, &["_cacache/content-v2/x", "_logs/2026.log"]);

    let mut env = sample_env();
    env.insert(
        "LOCALAPPDATA".into(),
        cache.parent().unwrap().to_string_lossy().into_owned(),
    );
    let context = ctx(
        env,
        Box::new(FakeTool::answering("npm", cache.to_string_lossy().as_ref())),
        vec![],
        true,
    );
    let verdict = verify_tool_path(&cache, &context);
    assert_eq!(verdict, ToolPathVerdict::Accept { unusual: false });

    let items = dev_cache::npm::scan(&context);
    assert_eq!(
        items.len(),
        1,
        "npm cache should be emitted from tool report"
    );
    assert_eq!(items[0].risk, RiskLevel::RegenerableDownload);
    assert_eq!(items[0].source, devresidue_core::SourceKind::PackageManager);
    assert!(!items[0].explanation.is_empty());
}

#[test]
fn unusual_location_is_accepted_with_warning() {
    let context = ctx(
        sample_env(),
        Box::new(FakeTool::answering("npm", "D:\\odd\\cache")),
        vec![],
        true,
    );
    let verdict = verify_tool_path(std::path::Path::new("D:\\odd\\cache"), &context);
    assert_eq!(verdict, ToolPathVerdict::Accept { unusual: true });
}

#[test]
fn query_verified_maps_reject_into_err_and_failure_into_none() {
    let env = sample_env();
    // Reject case.
    let context = ctx(
        env.clone(),
        Box::new(FakeTool::answering("pip", "C:\\Users\\alice")), // == USERPROFILE
        vec![],
        true,
    );
    assert!(query_verified(&context, "pip", &["cache", "dir"], "pip").is_err());

    // Tool unavailable → default fallback (Ok(None)).
    let context = ctx(env, Box::new(FailingTool), vec![], true);
    assert_eq!(
        query_verified(&context, "pip", &["cache", "dir"], "pip").unwrap(),
        None
    );
}

#[test]
fn cargo_cache_split_never_emits_the_dot_cargo_root_or_bin() {
    let dir = TempDir::new();
    let home = dir.child("cargo-home");
    make_dir_with_files(
        &home,
        &[
            "registry/cache/idx/x.crate",
            "registry/src/idx/foo-1.0.0/src/lib.rs",
            "git/checkouts/repo/hash/work/Cargo.toml",
            "git/db/repo-abc",
            "bin/cargo-x.exe",
            "config.toml",
        ],
    );

    let mut env = sample_env();
    env.insert("CARGO_HOME".into(), home.to_string_lossy().into_owned());
    let context = ctx(env, Box::new(FailingTool), vec![], true);

    let items = dev_cache::cargo::scan(&context);

    // Exactly the three sub-items.
    assert_eq!(
        items.len(),
        3,
        "got {:?}",
        items
            .iter()
            .map(|i| i.path.display().to_string())
            .collect::<Vec<_>>()
    );

    let rel = |item: &devresidue_core::ScanItem| -> String {
        item.path
            .strip_prefix(&home)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };

    let cache = items
        .iter()
        .find(|i| rel(i).ends_with("registry\\cache"))
        .expect("registry cache item");
    assert_eq!(cache.risk, RiskLevel::RegenerableDownload);
    assert!(
        cache.explanation.contains("recycle-bin")
            || cache.explanation.to_lowercase().contains("recycl")
    );

    let src = items
        .iter()
        .find(|i| rel(i).ends_with("registry\\src"))
        .expect("registry src item");
    assert_eq!(src.risk, RiskLevel::RegenerableDownload);

    let git = items
        .iter()
        .find(|i| rel(i).ends_with("git\\checkouts"))
        .expect("git checkouts item");
    assert_eq!(git.risk, RiskLevel::Review);

    // The .cargo root, bin and config are never targeted.
    assert!(
        items
            .iter()
            .all(|i| !rel(i).ends_with("cargo-home") && !rel(i).contains("bin")),
        "root/bin must never be emitted"
    );

    for item in &items {
        assert!(!item.explanation.is_empty());
    }
}

#[test]
fn nuget_and_bun_emit_with_explanations_when_they_exist() {
    let dir = TempDir::new();
    // NuGet global packages live at %USERPROFILE%\.nuget\packages.
    make_dir_with_files(
        &dir.child(".nuget").join("packages"),
        &["newtonsoft.json/13.0/lib/net8/a.dll"],
    );
    let mut env = sample_env();
    env.insert(
        "USERPROFILE".into(),
        dir.path().to_string_lossy().into_owned(),
    );

    let context = ctx(env.clone(), Box::new(FailingTool), vec![], true);
    let items = dev_cache::nuget::scan(&context);
    assert_eq!(items.len(), 1, "nuget packages item missing");
    assert!(items[0].path.ends_with(".nuget\\packages"));
    assert!(items[0].explanation.contains("NuGet"));

    // bun under USERPROFILE\\.bun\\install\\cache.
    make_dir_with_files(
        &dir.child(".bun").join("install").join("cache"),
        &["pkg/index.js"],
    );
    let items = dev_cache::bun::scan(&context);
    assert_eq!(items.len(), 1, "bun cache item missing");
    assert!(!items[0].explanation.is_empty());
}

#[test]
fn all_first_batch_providers_emit_nonempty_explanations() {
    // Guards SPEC explainability even when nothing exists (no panic, empty ok)
    // — run each provider against a bare env and assert explanations only on
    // whatever they emit.
    let dir = TempDir::new();
    let mut env = sample_env();
    env.insert(
        "USERPROFILE".into(),
        dir.path().to_string_lossy().into_owned(),
    );
    env.insert(
        "LOCALAPPDATA".into(),
        dir.path().to_string_lossy().into_owned(),
    );
    env.insert(
        "CARGO_HOME".into(),
        dir.child("ch").to_string_lossy().into_owned(),
    );

    let context = ctx(env, Box::new(FailingTool), vec![], true);
    let items = dev_cache::scan(&context);
    for item in &items {
        assert!(
            !item.explanation.trim().is_empty(),
            "provider explanation must never be empty for {}",
            item.path.display()
        );
    }
}

/// An env hash map usable by tests that need to override several vars.
#[allow(dead_code)]
fn env_with(
    mut env: HashMap<String, String>,
    overrides: &[(&str, &str)],
) -> HashMap<String, String> {
    for (k, v) in overrides {
        env.insert((*k).to_string(), (*v).to_string());
    }
    env
}

/// Confirms ScanContext is usable from integration tests.
#[allow(dead_code)]
fn _ctx_type_check(_ctx: ScanContext) {}

#[test]
fn r06_tool_native_actions_freeze_the_verified_cache_root_as_scope() {
    use devresidue_core::CleanupAction;
    let dir = TempDir::new();
    let cache = dir.child("uv-cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("f"), b"x").unwrap();

    let mut env = sample_env();
    env.insert(
        "LOCALAPPDATA".into(),
        dir.path().to_string_lossy().into_owned(),
    );
    let context = ctx(
        env,
        Box::new(FakeTool::answering(
            "uv",
            cache.display().to_string().as_str(),
        )),
        vec![],
        true,
    );
    let items = devresidue_providers::dev_cache::uv::scan(&context);
    assert_eq!(items.len(), 1, "tool-reported uv cache must be found");

    let CleanupAction::ExternalCommand { command } = &items[0].cleanup_action else {
        panic!("uv cleanup must be tool-native");
    };
    // F08: uv's cache-directory option is `--cache-dir`; positional arguments
    // to `uv cache clean` are *package names*, so the verified root must sit
    // after `--cache-dir`, never as a bare positional argument. Asserting only
    // "the path appears somewhere in the args" would green-light the broken
    // package-position syntax (R2 report).
    let expected_args = vec![
        "cache".to_string(),
        "clean".to_string(),
        "--cache-dir".to_string(),
        cache.display().to_string(),
    ];
    assert_eq!(
        command.args, expected_args,
        "uv cache clean must pass the verified root via --cache-dir (F08)"
    );
    let scope = command
        .scope
        .as_ref()
        .expect("tool-native action must freeze its scope (R06)");
    assert_eq!(scope.tool, "uv");
    assert_eq!(scope.verify_query.executable, "uv");
    assert_eq!(scope.verify_query.args, vec!["cache", "dir"]);
    assert_eq!(
        scope.expected_cache_root,
        devresidue_core::safety::canonical::normalize(&cache),
        "frozen scope must equal the canonical verified root"
    );
}
