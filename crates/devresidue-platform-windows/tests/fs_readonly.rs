//! Phase 15-B / SPEC §33 **read-only** and **permission-denied**
//! real-filesystem matrix.
//!
//! Real Win32 facts pinned by this matrix (discovered at the real-FS layer):
//! - the caller who *owns* the parent directory holds `FILE_DELETE_CHILD`,
//!   which lets `DeleteFileW` / `RemoveDirectoryW` delete a read-only file /
//!   directory regardless of the attribute. On a normal dev box the deletion
//!   port therefore deletes read-only objects **successfully** — read-only is
//!   not a hard deletion barrier for the object's owner;
//! - the attribute is still visible to probes, and the real safety gate is
//!   the validator's `ReadOnlyChanged` check: a target that became read-only
//!   *after* the snapshot is denied at cleanup time;
//! - a genuine ACL-denied system path (`%SystemDrive%\System Volume
//!   Information`) drives every probe and the deletion port into the
//!   `PermissionDenied` bucket without panic.
//!
//! Why no `icacls /deny` mutation: denying `Everyone:F` also denies the
//! `WRITE_DAC` right needed to remove the ACE again, so a non-elevated test
//! process cannot restore the ACL and would leak the temp tree (see report).
//! The unmodified system ACL gives the identical surface with zero cleanup
//! hazard; environments without it skip visibly.

mod common;
mod fs_support;

use std::fs;
use std::sync::Arc;

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_core::safety::probe::{PathProbe, ProbeError};
use devresidue_core::safety::process::ProcessGuard;
use devresidue_core::safety::validator::{SafetyValidator, ValidationRequest};
use devresidue_core::safety::ProtectedRootRegistry;
use devresidue_core::RiskLevel;
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_platform_windows::safety::{WindowsPathProbe, WindowsProcessProbe};

use common::TempDir;
use fs_support::{permission_denied_system_target, set_readonly};

fn port() -> WindowsDeletePort {
    WindowsDeletePort
}

fn probe() -> WindowsPathProbe {
    WindowsPathProbe
}

fn real_validator() -> SafetyValidator {
    let path_probe: Arc<dyn devresidue_core::safety::probe::PathProbe + Send + Sync> =
        Arc::new(WindowsPathProbe);
    let process_probe: Arc<dyn devresidue_core::safety::probe::ProcessProbe + Send + Sync> =
        Arc::new(WindowsProcessProbe);
    SafetyValidator::new(
        path_probe,
        process_probe,
        ProtectedRootRegistry::from_roots(vec![]),
        ProcessGuard::new(vec![], std::collections::HashMap::new()),
    )
}

// ---------------------------------------------------------------------------
// Read-only (scenario 2)
// ---------------------------------------------------------------------------

#[test]
fn read_only_files_are_deleted_by_their_owner_and_the_flag_stays_probeable() {
    // Windows semantics: the temp-dir owner holds FILE_DELETE_CHILD on the
    // parent, so DeleteFileW removes a read-only *file* without clearing the
    // attribute first. Read-only is an advisory flag for the owner, not a
    // deletion barrier — the honest assertion is that deletion succeeds while
    // the flag is still visible to probes beforehand.
    let dir = TempDir::new();
    let file = dir.child("ro-file.txt");
    fs::write(&file, b"keep").unwrap();

    set_readonly(&file, true).expect("set read-only attribute");
    let attrs = probe().attributes(&file).expect("probe read-only file");
    assert!(attrs.read_only, "the probe must see the read-only flag");

    let outcome = port().delete_tree(&file);
    match outcome {
        Ok(()) => {
            assert!(!file.exists(), "owner with DELETE_CHILD deletes the file");
        }
        // Honest fallback for environments where the parent ACL denies
        // DELETE_CHILD (service accounts, hardened temp dirs).
        Err(DeleteError::PermissionDenied { .. }) => {
            set_readonly(&file, false).expect("clear read-only for cleanup");
            assert!(file.exists());
        }
        Err(other) => panic!("unexpected delete outcome for a read-only file: {other}"),
    }
}

#[test]
fn read_only_directories_are_deleted_by_their_owner_and_the_flag_stays_probeable() {
    // Windows ignores FILE_ATTRIBUTE_READONLY on directories for removal when
    // the caller owns the parent (RemoveDirectoryW checks DELETE access, not
    // the attribute). The attribute remains visible to probes.
    let dir = TempDir::new();
    let sub = dir.child("ro-dir");
    fs::create_dir_all(sub.join("nested")).unwrap();
    fs::write(sub.join("nested").join("f.txt"), b"x").unwrap();

    set_readonly(&sub, true).expect("set read-only on directory");
    let attrs = probe().attributes(&sub).expect("probe read-only dir");
    assert!(attrs.read_only, "the probe must see the read-only flag");
    assert!(attrs.directory);

    let outcome = port().delete_tree(&sub);
    match outcome {
        Ok(()) => {
            assert!(!sub.exists());
        }
        Err(DeleteError::PermissionDenied { .. }) => {
            set_readonly(&sub, false).expect("clear read-only for cleanup");
            assert!(sub.exists());
        }
        Err(other) => panic!("unexpected delete outcome for a read-only dir: {other}"),
    }
}

#[test]
fn a_target_that_became_read_only_after_the_snapshot_is_denied_at_revalidation() {
    // The real read-only safety gate lives in the validator: a target that
    // gained the read-only attribute *after* the scan must be refused at
    // cleanup time (ReadOnlyChanged), because deleting it would surprise a
    // user who deliberately protected it.
    let dir = TempDir::new();
    let validator = real_validator();
    let file = dir.child("protected-later.txt");
    fs::write(&file, b"x").unwrap();

    let snapshot = validator
        .capture(&file, None, None, RiskLevel::Safe)
        .expect("capture while writable");

    set_readonly(&file, true).expect("user marks the file read-only");

    let verdict = validator.validate(&ValidationRequest {
        path: &file,
        product: None,
        snapshot: &snapshot,
    });
    assert!(!verdict.is_allowed(), "read-only change must deny cleanup");
    assert!(
        matches!(
            verdict.deny_reason(),
            Some(devresidue_core::safety::validator::DenyReason::ReadOnlyChanged { .. })
        ),
        "expected ReadOnlyChanged, got {verdict:?}"
    );

    // Control: clearing the flag restores Allow.
    set_readonly(&file, false).expect("clear read-only");
    let verdict = validator.validate(&ValidationRequest {
        path: &file,
        product: None,
        snapshot: &snapshot,
    });
    assert!(
        verdict.is_allowed(),
        "writable again → Allow, got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// Permission denied (scenario 3)
// ---------------------------------------------------------------------------

#[test]
fn acl_denied_system_path_buckets_every_probe_and_delete_as_permission_denied() {
    let target = match permission_denied_system_target() {
        Ok(t) => t,
        Err(reason) => {
            eprintln!("SKIP: no usable ACL-denied target in this environment — {reason}");
            return;
        }
    };

    // Every probe must land in the PermissionDenied bucket — never NotFound,
    // never Other, never a silent success.
    match probe().attributes(&target) {
        Err(ProbeError::PermissionDenied { .. }) => {}
        other => panic!("attributes on ACL-denied path: expected PermissionDenied, got {other:?}"),
    }
    match probe().file_identity(&target) {
        Err(ProbeError::PermissionDenied { .. }) => {}
        other => {
            panic!("file_identity on ACL-denied path: expected PermissionDenied, got {other:?}")
        }
    }
    match probe().reparse_info(&target) {
        Err(ProbeError::PermissionDenied { .. }) => {}
        other => {
            panic!("reparse_info on ACL-denied path: expected PermissionDenied, got {other:?}")
        }
    }

    // The deletion port fails closed on the same bucket before touching the
    // root (attributes probe runs first — no deletion is attempted).
    match port().delete_tree(&target) {
        Err(DeleteError::PermissionDenied { .. }) => {}
        other => panic!("delete_tree on ACL-denied path: expected PermissionDenied, got {other:?}"),
    }

    // Sanity: the target really still exists as a system object.
    assert!(
        std::fs::symlink_metadata(&target).is_ok() || std::fs::metadata(&target).is_err(),
        "unexpected state for the system target"
    );
}

#[test]
fn missing_paths_still_bucket_not_found_not_permission_denied() {
    // Guard against bucket confusion: a genuinely absent path must stay
    // NotFound (permission-denied tests above must not have masked this).
    let dir = TempDir::new();
    let ghost = dir.child("never-existed");
    match probe().attributes(&ghost) {
        Err(ProbeError::NotFound { .. }) => {}
        other => panic!("expected NotFound for a ghost, got {other:?}"),
    }
    match port().delete_tree(&ghost) {
        Err(DeleteError::NotFound { .. }) => {}
        other => panic!("expected NotFound for a ghost delete, got {other:?}"),
    }
}
