//! Phase 15-B / SPEC §33 **UNC** real-filesystem matrix.
//!
//! Two layers:
//! - pure-lexical UNC semantics of the path module (`normalize` /
//!   `is_within` / `to_extended`), which need no disk and no privilege;
//! - live probing over the `\\127.0.0.1\c$` admin share (identity / delete
//!   tree / recycle), skipped visibly when the current identity lacks admin
//!   access to the default share.
//!
//! The *root* share itself is deliberately not exercised here: rejecting the
//! UNC share root is a registry concern being implemented in the core safety
//! layer by a parallel task — this matrix owns the non-root UNC path surface
//! only (see the Phase 15-B report).

mod common;
mod fs_support;

use std::fs;
use std::path::PathBuf;

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_core::safety::probe::PathProbe;
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_platform_windows::identity::file_identity;
use devresidue_platform_windows::path;
use devresidue_platform_windows::safety::WindowsPathProbe;

use common::TempDir;
use fs_support::{map_to_unc_share, unc_admin_share_base};

fn port() -> WindowsDeletePort {
    WindowsDeletePort
}

fn probe() -> WindowsPathProbe {
    WindowsPathProbe
}

// ---------------------------------------------------------------------------
// Lexical UNC semantics (no disk access)
// ---------------------------------------------------------------------------

#[test]
fn lexical_unc_normalise_round_trips_extended_and_plain() {
    let plain_unc = PathBuf::from(r"\\server\share\dir\file.txt");
    let normalized = path::normalize(&plain_unc);
    assert_eq!(normalized, plain_unc);

    let ext = path::to_extended(&plain_unc);
    assert_eq!(
        ext,
        PathBuf::from(r"\\?\UNC\server\share\dir\file.txt"),
        "plain UNC must extend to the verbatim UNC form"
    );
    assert_eq!(path::to_extended(&ext), ext, "verbatim UNC round-trips");

    // Drive-relative and rooted-relative forms stay outside the comparison
    // domain of an absolute UNC path (lexical floor rules).
    assert!(!path::is_within(r"C:\foo", r"\\server\share"));
}

#[test]
fn lexical_unc_is_within_respects_share_boundaries() {
    let share = PathBuf::from(r"\\server\share");
    assert!(path::is_within(r"\\server\share\a\b", &share));
    assert!(
        path::is_within(r"\\server\share", &share),
        "equal path is within"
    );
    assert!(
        !path::is_within(r"\\server\share-other\a", &share),
        "a sibling share name must not be within"
    );
    assert!(
        !path::is_within(r"\\server-other\share\a", &share),
        "a sibling server must not be within"
    );
    // Case-insensitive component match on UNC.
    assert!(path::is_within(r"\\SERVER\Share\a", &share));
    // Extended spelling of the same root compares equal.
    let ext_share = PathBuf::from(r"\\?\UNC\server\share");
    assert!(path::is_within(r"\\server\share\a\b", &ext_share));
}

// ---------------------------------------------------------------------------
// Live UNC admin-share surface (requires elevation; skips visibly)
// ---------------------------------------------------------------------------

#[test]
fn unc_admin_share_probe_and_delete_full_chain() {
    if let Err(reason) = unc_admin_share_base() {
        eprintln!("SKIP: UNC live matrix unavailable — {reason}");
        return;
    }
    let dir = TempDir::new();
    let local_child = dir.child("unc-target");
    fs::create_dir(&local_child).unwrap();

    let Some(unc_child) = map_to_unc_share(&local_child) else {
        eprintln!("SKIP: temp dir is not on the C: volume; cannot map to c$");
        return;
    };

    // Create + probe over the UNC path.
    fs::write(unc_child.join("probe.txt"), b"data").expect("write over UNC");
    let attrs = probe()
        .attributes(&unc_child)
        .expect("attributes probe over UNC");
    assert!(attrs.directory, "UNC directory visible");
    let id = file_identity(&unc_child).expect("identity probe over UNC");
    assert_ne!(id.file_index, 0, "UNC object has an NTFS identity");

    // delete_tree removes the UNC subtree entirely.
    port()
        .delete_tree(&unc_child)
        .expect("delete_tree over UNC");
    assert!(!unc_child.exists(), "UNC subtree removed");
}

#[test]
fn unc_admin_share_recycle_does_not_panic_and_reports_shell_outcome() {
    if let Err(reason) = unc_admin_share_base() {
        eprintln!("SKIP: UNC live matrix unavailable — {reason}");
        return;
    }
    let dir = TempDir::new();
    let local_file = dir.child("unc-recycle-me.txt");
    fs::write(&local_file, b"payload").unwrap();
    let Some(unc_file) = map_to_unc_share(&local_file) else {
        eprintln!("SKIP: temp dir is not on the C: volume");
        return;
    };

    match port().recycle(&unc_file) {
        Ok(()) => {
            assert!(!unc_file.exists(), "recycled UNC file is gone");
        }
        Err(DeleteError::Io { message, .. }) => {
            // The shell may refuse to recycle a remote (admin-share) path —
            // that is shell surface behaviour, not a safety regression. What
            // must never happen is a permanent delete mis-reported as OK.
            eprintln!(
                "note: shell refused to recycle the UNC file (documented shell \
                 limitation): {message}"
            );
            // Clean up our own file via delete_tree so no fixture leaks.
            port()
                .delete_tree(&unc_file)
                .expect("cleanup delete over UNC");
        }
        Err(other) => panic!("unexpected recycle error over UNC: {other}"),
    }
}
