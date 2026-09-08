//! Kondo streaming driver equivalence (Phase 16): `project::scan` (batch) and
//! `project::kondo::scan_with_sink` (per-project streaming) must produce the
//! identical item sequence — same paths, same order, same ids — so a UI shell
//! can switch to streaming without changing scan results.

mod common;

use std::path::Path;

use devresidue_providers::project;
use devresidue_providers::scan_ctx::ScanContext;

use common::{make_dir_with_files, sample_env, FailingTool, TempDir};

/// A three-project workspace farm (cargo / node / cmake), each with one
/// artifact directory.
fn build_farm(root: &Path) {
    make_dir_with_files(
        &root.join("cargo-proj"),
        &["Cargo.toml", "src/main.rs", "target/debug/x.exe"],
    );
    make_dir_with_files(
        &root.join("node-proj"),
        &["package.json", "src/index.js", "node_modules/pkg/index.js"],
    );
    make_dir_with_files(
        &root.join("cmake-proj"),
        &[
            "CMakeLists.txt",
            "src/main.cpp",
            "cmake-build-debug/out.exe",
        ],
    );
}

fn ctx_for(root: &Path) -> ScanContext {
    common::ctx(
        sample_env(),
        Box::new(FailingTool),
        vec![root.to_path_buf()],
        true,
    )
}

#[test]
fn streaming_delivers_every_project_batch_in_scan_order() {
    let dir = TempDir::new();
    build_farm(dir.path());

    // Batch reference.
    let items_batch = project::scan(&ctx_for(dir.path()));

    // Streaming aggregation.
    let mut items_stream: Vec<devresidue_core::ScanItem> = Vec::new();
    let mut batches = 0usize;
    let ctx = ctx_for(dir.path());
    project::kondo::scan_with_sink(&ctx, &mut |batch| {
        batches += 1;
        items_stream.extend(batch);
    });

    assert_eq!(
        items_stream.len(),
        items_batch.len(),
        "stream and batch must find the same items"
    );
    assert_eq!(batches, 3, "three projects → three delivery batches");
    for (a, b) in items_batch.iter().zip(items_stream.iter()) {
        assert_eq!(a.path, b.path, "same path in the same position");
        assert_eq!(a.logical_size, b.logical_size);
        assert_eq!(a.file_count, b.file_count);
        assert_eq!(a.explanation, b.explanation);
        // R04: ids are globally monotonic across scans — the batch and stream
        // passes mint distinct id ranges (never equal, never re-used).
        assert_ne!(a.id, b.id, "each scan run mints globally-unique ids");
    }
    // Both passes allocate strictly increasing ids internally.
    for items in [&items_batch, &items_stream] {
        for pair in items.windows(2) {
            assert!(pair[0].id < pair[1].id, "ids increase within one scan");
        }
    }
}

#[test]
fn streaming_with_a_cancelled_context_delivers_nothing() {
    let dir = TempDir::new();
    build_farm(dir.path());
    let ctx = common::ctx(
        sample_env(),
        Box::new(FailingTool),
        vec![dir.path().to_path_buf()],
        false, // cancelled from the start
    );
    let mut collected = Vec::new();
    project::kondo::scan_with_sink(&ctx, &mut |b| collected.extend(b));
    assert!(collected.is_empty());
}
