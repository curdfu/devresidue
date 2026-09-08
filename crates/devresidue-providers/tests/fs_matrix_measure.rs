//! Phase 15-B / SPEC §33 Filesystem matrix — **measurement** half.
//!
//! The providers' `measure_tree` is the only scan-time walker that sizes
//! residue trees. The platform tests carry the probe/delete/recycle halves of
//! the same scenarios; this file exercises the walker against the same real
//! on-disk attack surfaces: locked / read-only / permission-denied / long
//! path / junction descent (the mount-point machinery is identical reparse
//! handling, no privilege needed) / symlink-style loops / hardlinks / large
//! trees / cancellation.
//!
//! Per scenario contract the walker must *never panic*: unreadable entries
//! bump `error_count` and the walk continues.

mod common;

use std::cell::Cell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use devresidue_providers::measure::{measure_tree, path_is_reparse_like, Measure};

use common::{create_junction, TempDir};

/// Completes a measurement within a bounded budget and returns it.
fn bounded_measure(root: &std::path::Path, budget: Duration) -> Measure {
    let start = Instant::now();
    let m = measure_tree(root, &|| true);
    assert!(
        start.elapsed() <= budget,
        "measure_tree exceeded the {budget:?} budget on '{}'",
        root.display()
    );
    m
}

// ---------------------------------------------------------------------------
// Scenario 1 — locked
// ---------------------------------------------------------------------------

#[test]
fn measure_of_a_tree_containing_a_locked_file_never_panics() {
    let dir = TempDir::new();
    let tree = dir.child("locked-tree");
    std::fs::create_dir(&tree).unwrap();
    let held = tree.join("held.bin");
    std::fs::write(&held, vec![0xAB; 64]).unwrap();

    // Exclusive (share_mode(0)) open of the file.
    let _lock = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&held)
            .expect("exclusive open")
    };

    // The walker reads directory entries + attributes — it never *opens* the
    // file for I/O, so a locked file is countable, not a walk failure.
    let m = bounded_measure(&tree, Duration::from_secs(30));
    assert!(
        m.error_count == 0 && m.file_count == 1 && m.logical_size == 64,
        "locked file must be measured via metadata without error (got {m:?})"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2 — read-only
// ---------------------------------------------------------------------------

#[test]
fn measure_counts_readonly_files_and_directories() {
    let dir = TempDir::new();
    let tree = dir.child("ro-tree");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    let ro_file = tree.join("ro.txt");
    std::fs::write(&ro_file, b"read-only").unwrap();
    set_readonly_attr(&ro_file);
    std::fs::write(tree.join("sub").join("plain.txt"), b"x").unwrap();

    let m = bounded_measure(&tree, Duration::from_secs(30));
    assert_eq!(
        m.file_count, 2,
        "read-only entries are still countable: {m:?}"
    );
    assert_eq!(m.logical_size, 10, "9 + 1 bytes: {m:?}");
    assert_eq!(m.error_count, 0, "no read errors expected: {m:?}");
}

// ---------------------------------------------------------------------------
// Scenario 3 — permission denied (real ACL, no mutation)
// ---------------------------------------------------------------------------

#[test]
fn measure_of_an_acl_denied_directory_reports_errors_not_panic() {
    let target = match permission_denied_system_target() {
        Ok(t) => t,
        Err(reason) => {
            eprintln!("SKIP: no usable ACL-denied target in this environment — {reason}");
            return;
        }
    };
    let m = bounded_measure(&target, Duration::from_secs(30));
    assert!(
        m.error_count >= 1,
        "an ACL-denied root must surface as walk errors (got {m:?})"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4 — long path
// ---------------------------------------------------------------------------

#[test]
fn measure_of_a_deep_max_path_tree_succeeds() {
    let dir = TempDir::new();
    let (plain, extended) =
        create_deep_tree(dir.path(), 12, "deep-dir-30chars-abcdefghijklmnop", 3);

    // The walker receives the plain path (as providers do); std transparently
    // reaches it through its own verbatim handling.
    let m = bounded_measure(&plain, Duration::from_secs(30));
    assert_eq!(
        m.file_count, 3,
        "all deep leaves measured (got {m:?}) — plain path {plain:?}"
    );
    assert_eq!(
        m.error_count, 0,
        "no errors expected on our own tree: {m:?}"
    );

    // Cleanup via the verbatim form (plain remove_dir_all would silently fail
    // past MAX_PATH in *our* helper; the product port extends internally).
    std::fs::remove_dir_all(&extended).expect("clean up the deep tree");
}

// ---------------------------------------------------------------------------
// Scenario 6 — junction / mount-point machinery (measure must not descend)
// ---------------------------------------------------------------------------

#[test]
fn measure_of_a_junction_root_returns_an_error_and_zero_bytes() {
    let dir = TempDir::new();
    let target = dir.child("real-dir");
    std::fs::create_dir_all(target.join("nested")).unwrap();
    std::fs::write(target.join("nested").join("data.bin"), vec![0u8; 500]).unwrap();
    let link = dir.child("root-junction");
    create_junction(&link, &target).expect("mklink /J");

    assert!(
        path_is_reparse_like(&link),
        "the junction root must be detected as reparse-like"
    );
    let m = measure_tree(&link, &|| true);
    assert_eq!(
        m.error_count, 1,
        "reparse root → one measurement error: {m:?}"
    );
    assert_eq!(m.logical_size, 0, "never size through a link: {m:?}");
    assert_eq!(m.file_count, 0);
}

#[test]
fn measure_never_descends_into_a_nested_junction() {
    let dir = TempDir::new();
    let outside = dir.child("outside-target");
    std::fs::create_dir_all(outside.join("deep")).unwrap();
    std::fs::write(outside.join("deep").join("big.bin"), vec![0u8; 900]).unwrap();

    let tree = dir.child("scanned-tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("own.txt"), b"own").unwrap();
    create_junction(&tree.join("link-out"), &outside).expect("mklink /J");

    let m = bounded_measure(&tree, Duration::from_secs(30));
    assert_eq!(
        m.file_count, 1,
        "only the tree's own file is counted; the junction target must not be \
         descended into (got {m:?})"
    );
    assert_eq!(m.logical_size, 3, "only 'own' bytes: {m:?}");
    assert_eq!(m.error_count, 0);
}

#[test]
fn measure_of_a_self_referential_junction_loop_terminates() {
    // a → b → a loop through junctions. A non-defensive walker would recurse
    // forever (or until path exhaustion); the reparse filter must stop it.
    let dir = TempDir::new();
    let a = dir.child("loop-a");
    let b = dir.child("loop-b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    std::fs::write(a.join("a.txt"), b"a").unwrap();
    std::fs::write(b.join("b.txt"), b"bb").unwrap();
    create_junction(&a.join("to-b"), &b).expect("mklink /J a→b");
    create_junction(&b.join("to-a"), &a).expect("mklink /J b→a");

    let start = Instant::now();
    let m = measure_tree(&a, &|| true);
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "the junction loop must not hang the walker"
    );
    assert_eq!(
        m.file_count, 1,
        "only a's own file is measured; the loop must be cut at the first \
         reparse boundary (got {m:?})"
    );
    assert_eq!(m.logical_size, 1);
}

// ---------------------------------------------------------------------------
// Scenario 7 — hardlinks (documented double-count semantics, F7)
// ---------------------------------------------------------------------------

#[test]
fn measure_counts_each_hard_link_entry_separately() {
    // Two directory entries → two measured entries even though they share one
    // NTFS file. This is the documented F7 double-count limitation, pinned
    // here at the real-FS layer so a future dedup change is noticed.
    let dir = TempDir::new();
    let tree = dir.child("links");
    std::fs::create_dir(&tree).unwrap();
    let content = vec![0x5A; 40];
    std::fs::write(tree.join("entry-a.bin"), &content).unwrap();
    std::fs::hard_link(tree.join("entry-a.bin"), tree.join("entry-b.bin")).unwrap();

    let m = bounded_measure(&tree, Duration::from_secs(30));
    assert_eq!(m.file_count, 2, "two link entries measured: {m:?}");
    assert_eq!(
        m.logical_size, 80,
        "the shared file is counted per entry: {m:?}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 10 — large-tree boundedness + cancellation
// ---------------------------------------------------------------------------

#[test]
fn measure_of_a_ten_thousand_file_tree_finishes_within_budget() {
    let dir = TempDir::new();
    let root = dir.child("big");
    for i in 0..20 {
        let sub = root.join(format!("d{i:03}"));
        std::fs::create_dir_all(&sub).unwrap();
        for j in 0..500 {
            std::fs::write(sub.join(format!("f{j:04}.bin")), b"x").unwrap();
        }
    }

    let m = bounded_measure(&root, Duration::from_secs(30));
    assert_eq!(m.file_count, 10_000, "all 10k files counted: {m:?}");
    assert_eq!(m.logical_size, 10_000);
    assert_eq!(m.error_count, 0);
}

#[test]
fn measure_stops_at_cancellation_and_reports_the_partial_walk() {
    let dir = TempDir::new();
    let root = dir.child("cancel-me");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    for i in 0..400 {
        std::fs::write(root.join(format!("f{i:03}.bin")), b"x").unwrap();
    }

    // Cancel after the first few entries: the walk stops and the count must
    // be strictly below the full total (partial results stay honest).
    let seen = Cell::new(0u32);
    let cancel_after = 8u32;
    let start = Instant::now();
    let m = measure_tree(&root, &|| {
        seen.set(seen.get() + 1);
        seen.get() < cancel_after
    });
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "cancellation must stop the walk promptly"
    );
    assert!(
        m.file_count < 400,
        "cancelled walk must not count the whole tree (got {m:?})"
    );
    assert!(
        m.file_count >= 1,
        "cancellation happens after entry N — some entries are still counted \
         (got {m:?})"
    );
}

// ---------------------------------------------------------------------------
// Local scaffolding (kept out of the shared common module on purpose)
// ---------------------------------------------------------------------------

/// Clears then sets the Win32 read-only attribute through a plain std call
/// (`std::fs::metadata` does not expose it, so use the file's attribute via
/// the windows FFI is avoided: read-only is irrelevant to std open-for-read,
/// and for the *measure* tests we only need the attribute to exist — a
/// directory entry attribute set through the `attrib` shell command is
/// enough for the walker to see it without any unsafe code here).
fn set_readonly_attr(path: &std::path::Path) {
    let out = std::process::Command::new("attrib.exe")
        .arg("+R")
        .arg(path)
        .output()
        .expect("run attrib");
    assert!(
        out.status.success(),
        "attrib +R failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Creates a >MAX_PATH deep tree (12 × 30-char components) using the
/// `\\?\`-verbatim form (plain create_dir_all would fail past MAX_PATH for
/// the *creation* phase). Returns the plain and the extended root.
fn create_deep_tree(
    base: &std::path::Path,
    depth: usize,
    component: &str,
    leaf_files: usize,
) -> (PathBuf, PathBuf) {
    let mut plain = base.to_path_buf();
    for _ in 0..depth {
        plain.push(component);
    }
    let text = plain.to_string_lossy();
    assert!(text.len() > 260, "fixture must exceed MAX_PATH ({text})");
    let extended = PathBuf::from(format!("\\\\?\\{text}"));
    std::fs::create_dir_all(&extended).expect("create deep tree (verbatim)");
    for i in 0..leaf_files {
        std::fs::write(extended.join(format!("leaf-{i}.bin")), b"x").expect("write deep leaf");
    }
    (plain, extended)
}

/// The unmodified system ACL target (`%SystemDrive%\System Volume
/// Information`) — the same surface the platform matrix uses, with zero ACL
/// mutation/cleanup hazard.
fn permission_denied_system_target() -> Result<PathBuf, String> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let target = PathBuf::from(format!("{drive}\\System Volume Information"));
    match std::fs::metadata(&target) {
        Ok(_) => Err(format!(
            "the current process can read '{}' — cannot use it as a denied target",
            target.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(target),
        Err(e) => Err(format!("'{}' unusable: {e}", target.display())),
    }
}
