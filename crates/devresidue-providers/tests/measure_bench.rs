//! Phase 16 measurement benchmark (manual timing, no criterion).
//!
//! Builds synthetic residue trees on a temp dir and times `measure_tree`.
//! Run explicitly (ignored by default) — `#[ignore]` keeps CI fast:
//!
//! ```text
//! cargo test --release -p devresidue-providers --test measure_bench -- --ignored --nocapture
//! ```
//!
//! Tree shape via env:
//!
//! ```text
//! DR_BENCH=100k | 10k        (default 10k — quick self check)
//! DR_BENCH_TOP / DR_BENCH_SUB / DR_BENCH_FILES   override component counts
//! DR_BENCH_REUSE=1            keep the generated tree for repeated runs
//! ```
//!
//! The 100k shape (Phase 16 contract): 20 top dirs × 50 sub dirs × 100 files
//! (1 KiB each, sparse-created so generation is fast). The 10k shape mirrors
//! the fs-matrix fixture: 20 top dirs × 500 files.
//!
//! Baseline numbers must be captured BEFORE the parallel rewrite and re-run
//! after — the report table compares serial vs parallel on the same trees.

#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use devresidue_providers::measure::{measure_tree, measure_tree_parallel};

/// 1 KiB of zeros written as a sparse file (len set, no 1 KB heap churn).
const FILE_BYTES: u64 = 1024;

struct Tree {
    root: PathBuf,
}

impl Drop for Tree {
    fn drop(&mut self) {
        if std::env::var("DR_BENCH_REUSE").as_deref() != Ok("1") {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn tmp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "dr-measure-bench-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos() % 1_000_000
    ))
}

#[allow(clippy::unused_io_amount)]
fn synth_100k(base: &Path) {
    let top = env_count("DR_BENCH_TOP", 20);
    let sub = env_count("DR_BENCH_SUB", 50);
    let files = env_count("DR_BENCH_FILES", 100);
    let root = base.join("tree-100k");
    fs::create_dir_all(&root).unwrap();
    for t in 0..top {
        let top_dir = root.join(format!("top{t:03}"));
        fs::create_dir_all(&top_dir).unwrap();
        for s in 0..sub {
            let sub_dir = top_dir.join(format!("sub{s:03}"));
            fs::create_dir_all(&sub_dir).unwrap();
            for f in 0..files {
                let p = sub_dir.join(format!("f{f:04}.bin"));
                fs::OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_len(FILE_BYTES)
                    .unwrap();
            }
        }
    }
}

#[allow(clippy::unused_io_amount)]
fn synth_10k(base: &Path) {
    let top = env_count("DR_BENCH_TOP", 20);
    let files = env_count("DR_BENCH_FILES", 500);
    let root = base.join("tree-10k");
    fs::create_dir_all(&root).unwrap();
    for t in 0..top {
        let top_dir = root.join(format!("d{t:03}"));
        fs::create_dir_all(&top_dir).unwrap();
        for f in 0..files {
            let p = top_dir.join(format!("f{f:04}.bin"));
            fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&p)
                .unwrap()
                .set_len(FILE_BYTES)
                .unwrap();
        }
    }
}

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Times one serial `measure_tree` pass over `root`.
fn time_serial(root: &Path, expected_files: u64) {
    let start = Instant::now();
    let m = measure_tree(root, &|| true);
    let elapsed = start.elapsed();
    assert_eq!(m.file_count, expected_files, "bench fixture integrity");
    println!(
        "  serial   : {:>10.3} ms   ({:>2} files counted, {} bytes)",
        elapsed.as_secs_f64() * 1000.0,
        m.file_count,
        m.logical_size
    );
}

/// Times one parallel `measure_tree_parallel` pass over `root`.
fn time_parallel(root: &Path, expected_files: u64) {
    let start = Instant::now();
    let m = measure_tree_parallel(root, &|| true);
    let elapsed = start.elapsed();
    assert_eq!(m.file_count, expected_files, "bench fixture integrity");
    println!(
        "  parallel : {:>10.3} ms   ({:>2} files counted, {} bytes)",
        elapsed.as_secs_f64() * 1000.0,
        m.file_count,
        m.logical_size
    );
}

#[test]
#[ignore = "explicit benchmark; run with -- --ignored --nocapture"]
fn measure_benchmark_report() {
    let kind = std::env::var("DR_BENCH").unwrap_or_else(|_| "10k".into());
    let base = tmp_root();
    fs::create_dir_all(&base).unwrap();
    let (root, expected, reused) = match kind.as_str() {
        "100k" => {
            let root = base.join("tree-100k");
            let reused = root.exists();
            if !reused {
                println!("building 100k tree (20×50×100 sparse 1 KiB)...");
                let t = Instant::now();
                synth_100k(&base);
                println!("  build     : {:.1} s", t.elapsed().as_secs_f64());
            } else {
                println!("reusing existing 100k tree ({} s old check)", {
                    fs::metadata(&root).is_ok()
                });
            }
            (root, 100_000u64, reused)
        }
        _ => {
            let root = base.join("tree-10k");
            let reused = root.exists();
            if !reused {
                println!("building 10k tree (20×500 sparse 1 KiB)...");
                let t = Instant::now();
                synth_10k(&base);
                println!("  build     : {:.1} s", t.elapsed().as_secs_f64());
            } else {
                println!("reusing existing 10k tree");
            }
            (root, 10_000u64, reused)
        }
    };
    println!(
        "measure_tree on {} ({kind} files, {}):",
        root.display(),
        if reused { "reused" } else { "fresh" }
    );
    time_serial(&root, expected);
    time_parallel(&root, expected);
    let _tree = Tree { root };
}
