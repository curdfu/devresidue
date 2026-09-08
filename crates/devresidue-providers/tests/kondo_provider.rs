//! Kondo provider integration tests over a synthetic project tree (tempdir).
//!
//! Verifies project discovery, risk mapping, dedupe and cancellation against
//! the real `kondo-lib` — no dependency on the developer's actual disks.

mod common;

use devresidue_core::RiskLevel;
use devresidue_providers::measure::measure_tree;
use devresidue_providers::project;

use common::{ctx, make_dir_with_files, sample_env, FailingTool, TempDir};

/// Builds seven kinds of project under `root`, each with an artifact dir.
fn build_project_farm(root: &std::path::Path) {
    // Cargo
    make_dir_with_files(
        &root.join("cargo-proj"),
        &[
            "Cargo.toml",
            "src/main.rs",
            "target/debug/x.exe",
            "target/release/y.exe",
        ],
    );
    // Node
    make_dir_with_files(
        &root.join("node-proj"),
        &["package.json", "node_modules/pkg/index.js", "src/index.js"],
    );
    // Python (marker: .py file)
    make_dir_with_files(
        &root.join("py-proj"),
        &[
            "main.py",
            ".venv/Scripts/python.exe",
            ".venv/Lib/site-packages/pkg.py",
        ],
    );
    // .NET
    make_dir_with_files(
        &root.join("dotnet-proj"),
        &[
            "App.csproj",
            "Program.cs",
            "bin/Debug/net8/app.dll",
            "obj/Debug/net8/x",
        ],
    );
    // CMake
    make_dir_with_files(
        &root.join("cmake-proj"),
        &[
            "CMakeLists.txt",
            "src/main.cpp",
            "cmake-build-debug/out.exe",
        ],
    );
    // Gradle
    make_dir_with_files(
        &root.join("gradle-proj"),
        &[
            "build.gradle",
            "src/main/java/A.java",
            "build/libs/a.jar",
            ".gradle/6.8/cache/x",
        ],
    );
    // Maven
    make_dir_with_files(
        &root.join("mvn-proj"),
        &["pom.xml", "src/main/java/B.java", "target/classes/b.class"],
    );
}

#[test]
fn kondo_discovers_artifacts_with_correct_risks_and_counts() {
    let dir = TempDir::new();
    build_project_farm(dir.path());

    let context = ctx(
        sample_env(),
        Box::new(FailingTool),
        vec![dir.path().to_path_buf()],
        true,
    );
    let items = project::scan(&context);

    // Cargo target + Node node_modules + Python .venv + .NET bin/obj +
    // CMake build + Gradle build/.gradle + Maven target — all present.
    let by_name: Vec<String> = items
        .iter()
        .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    assert!(
        by_name.iter().any(|n| n == "target"),
        "cargo target: {by_name:?}"
    );
    assert!(by_name.iter().any(|n| n == "node_modules"), "node_modules");
    assert!(by_name.iter().any(|n| n == ".venv"), ".venv");
    assert!(
        by_name.iter().any(|n| n == "bin" || n == "obj"),
        ".NET bin/obj"
    );
    assert!(by_name.iter().any(|n| n == "cmake-build-debug"), "cmake");
    assert!(by_name.iter().any(|n| n == "build"), "gradle build");
    assert!(by_name.iter().any(|n| n == ".gradle"), "gradle .gradle");
    assert!(by_name.iter().any(|n| n == "target"), "maven target");

    for item in &items {
        assert!(
            item.file_count > 0,
            "{} must count files",
            item.path.display()
        );
        assert!(item.logical_size > 0);
        assert!(!item.explanation.is_empty(), "explanation required");
    }

    // Risk mapping spot checks.
    let find = |name: &str| {
        items
            .iter()
            .find(|i| i.path.file_name().unwrap() == name)
            .unwrap()
    };
    assert_eq!(find("node_modules").risk, RiskLevel::RegenerableDownload);
    assert_eq!(find(".venv").risk, RiskLevel::RegenerableDownload);
    assert_eq!(find("target").risk, RiskLevel::RegenerableLocal);
    assert_eq!(find("cmake-build-debug").risk, RiskLevel::RegenerableLocal);
}

#[test]
fn nested_project_is_discovered_once_by_kondo_itself() {
    let dir = TempDir::new();
    // Outer project with an inner project beneath it.
    make_dir_with_files(
        &dir.child("outer"),
        &[
            "Cargo.toml",
            "target/debug/x",
            "inner/Cargo.toml",
            "inner/target/debug/y",
        ],
    );
    let context = ctx(
        sample_env(),
        Box::new(FailingTool),
        vec![dir.path().to_path_buf()],
        true,
    );
    let items = project::scan(&context);

    // kondo's skip_current_dir prevents descending into the discovered outer
    // project, so the nested inner target must never surface.
    assert!(
        !items
            .iter()
            .any(|i| i.path.to_string_lossy().contains("inner")),
        "nested inner project must not be reported: {:?}",
        items
            .iter()
            .map(|i| i.path.display().to_string())
            .collect::<Vec<_>>()
    );
    assert!(
        items
            .iter()
            .any(|i| i.path.to_string_lossy().ends_with("target")),
        "outer target should be reported"
    );
}

#[test]
fn duplicate_paths_are_emitted_once_via_the_seen_set() {
    let dir = TempDir::new();
    make_dir_with_files(&dir.child("p"), &["Cargo.toml", "target/debug/x"]);
    // Two roots that overlap: the second scan pass re-meets the same artifact.
    let context = ctx(
        sample_env(),
        Box::new(FailingTool),
        vec![dir.path().to_path_buf(), dir.path().to_path_buf()],
        true,
    );
    let items = project::scan(&context);
    let target_count = items
        .iter()
        .filter(|i| i.path.to_string_lossy().ends_with("target"))
        .count();
    assert_eq!(target_count, 1, "same path must be emitted once");
}

#[test]
fn cancelled_scan_returns_without_panicking() {
    let dir = TempDir::new();
    build_project_farm(dir.path());
    let context = ctx(
        sample_env(),
        Box::new(FailingTool),
        vec![dir.path().to_path_buf()],
        false, // cancelled from the start
    );
    let items = project::scan(&context);
    assert!(items.is_empty());
}

#[test]
fn reparse_root_is_not_measured() {
    let dir = TempDir::new();
    let holder = dir.child("holder");
    let payload = dir.child("payload-outside");
    std::fs::create_dir_all(&holder).unwrap();
    make_dir_with_files(&payload, &["big.bin"]);
    // Junction fixture (mklink). If this environment cannot create junctions,
    // fail loudly — the safety property is core to measurement.
    let link = holder.join("link-to-payload");
    common::create_junction(&link, &payload).expect("mklink /J must work for this test");

    let m = measure_tree(&link, &|| true);
    assert_eq!(
        m.logical_size, 0,
        "a junction root must never be measured through"
    );
    assert!(m.error_count >= 1);

    // Walking the holder directory that contains the junction must not
    // descend into the linked payload (which lives outside the holder).
    let measured = measure_tree(&holder, &|| true);
    assert_eq!(
        measured.logical_size, 0,
        "junction contents leaked into parent measure: {}",
        measured.logical_size
    );
    assert_eq!(measured.file_count, 0);
}
