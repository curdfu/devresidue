//! Phase 15-B / SPEC §33 **long path** real-filesystem matrix.
//!
//! A >260-character tree (12 nested 30-char components) exercises the Win32
//! long-path surface end to end against the real adapters: attribute probe,
//! identity probe and `delete_tree` must all survive `MAX_PATH` because the
//! platform layer pre-extends absolute paths to the `\\?\` verbatim form.

mod common;
mod fs_support;

use std::fs;
use std::time::Duration;

use devresidue_core::cleanup::port::DeletePort;
use devresidue_core::safety::probe::PathProbe;
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_platform_windows::identity::file_identity;
use devresidue_platform_windows::path;
use devresidue_platform_windows::safety::WindowsPathProbe;

use common::TempDir;
use fs_support::{create_deep_tree, run_bounded};

fn port() -> WindowsDeletePort {
    WindowsDeletePort
}

fn probe() -> WindowsPathProbe {
    WindowsPathProbe
}

const DEPTH: usize = 12;
const COMPONENT: &str = "deep-dir-30chars-abcdefghijklmnop";
const LEAF_FILES: usize = 4;

#[test]
fn deep_tree_probe_and_delete_survive_max_path() {
    let dir = TempDir::new();
    let tree = create_deep_tree(dir.path(), DEPTH, COMPONENT, LEAF_FILES);

    // Probes receive the *plain* (non-prefixed) path — product code paths do;
    // the adapters must extend internally.
    let attrs = probe()
        .attributes(&tree.plain)
        .expect("attributes on a >260 path must not fail");
    assert!(attrs.directory, "deep root must be seen as a directory");

    let id = file_identity(&tree.plain).expect("identity on a >260 path");
    assert_ne!(id.file_index, 0, "deep tree root must have an identity");

    // delete_tree removes the whole deep tree.
    run_bounded(Duration::from_secs(30), "deep tree delete_tree", || {
        port()
            .delete_tree(&tree.plain)
            .expect("delete_tree must remove a >260 path")
    });
    assert!(!tree.plain.exists(), "deep tree must be gone");
}

#[test]
fn lexical_normalisation_round_trips_extended_forms() {
    // Pure lexical checks (no disk) pin the long-path *path* semantics that
    // the real-FS tests above depend on.
    let plain = path::normalize(r"C:\work\some\nested\dir");
    assert_eq!(plain, std::path::PathBuf::from(r"C:\work\some\nested\dir"));

    // to_extended prefixes plain absolute paths…
    let ext = path::to_extended(r"C:\work\deep");
    assert_eq!(ext, std::path::PathBuf::from(r"\\?\C:\work\deep"));

    // …and leaves already-extended paths untouched (round trip stable).
    let again = path::to_extended(&ext);
    assert_eq!(again, ext, "extended form must round-trip");
}

#[test]
fn lexical_is_within_uses_component_boundaries_on_long_paths() {
    let ancestor = std::path::PathBuf::from(r"C:\a\b");
    assert!(path::is_within(r"C:\a\b\c\d\e", &ancestor));
    assert!(
        !path::is_within(r"C:\a\b-c", &ancestor),
        "sibling prefix must not match"
    );
}

#[test]
fn fs_round_trip_write_through_plain_deep_path_uses_std_verbatim_support() {
    // Documents what plain std does with >MAX_PATH paths in this toolchain:
    // the fixture above creates via the verbatim form; this asserts the same
    // tree is reachable through plain std calls so the drop guard semantics
    // hold (std auto-prefixes absolute paths on Windows).
    let dir = TempDir::new();
    let tree = create_deep_tree(dir.path(), DEPTH, COMPONENT, LEAF_FILES);
    let leaves = fs::read_dir(&tree.plain).expect("read_dir must reach a >260 path");
    assert_eq!(leaves.count(), LEAF_FILES, "all deep leaves visible");
}
