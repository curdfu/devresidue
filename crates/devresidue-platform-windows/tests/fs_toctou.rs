//! Phase 15-B / SPEC §33 **concurrent rename** and **replacement** (TOCTOU)
//! matrix — the real-FS versions of SPEC §32's TOCTOU Replacement Test,
//! Junction Substitution Test and Symlink Substitution Test.
//!
//! Previous coverage drove a programmable *fake* probe; here the real
//! `WindowsPathProbe` + `SafetyValidator` run against genuine on-disk
//! mutations between capture and validate.

mod common;

use std::fs;
use std::sync::Arc;

use devresidue_core::safety::probe::ProcessProbe;
use devresidue_core::safety::process::ProcessGuard;
use devresidue_core::safety::validator::{DenyReason, SafetyValidator, ValidationRequest};
use devresidue_core::safety::ProtectedRootRegistry;
use devresidue_core::RiskLevel;
use devresidue_platform_windows::safety::{WindowsPathProbe, WindowsProcessProbe};

use common::{create_junction, TempDir};

/// Real-validator harness: real probes, empty protected-root registry (the
/// fixture paths are plain temp dirs) and an empty process watch-list.
fn real_validator() -> SafetyValidator {
    let path_probe: Arc<dyn devresidue_core::safety::probe::PathProbe + Send + Sync> =
        Arc::new(WindowsPathProbe);
    let process_probe: Arc<dyn ProcessProbe + Send + Sync> = Arc::new(WindowsProcessProbe);
    SafetyValidator::new(
        path_probe,
        process_probe,
        ProtectedRootRegistry::from_roots(vec![]),
        ProcessGuard::new(vec![], std::collections::HashMap::new()),
    )
}

// ---------------------------------------------------------------------------
// Scenario 8 — concurrent rename
// ---------------------------------------------------------------------------

#[test]
fn rename_between_capture_and_validate_is_denied_as_target_missing() {
    let dir = TempDir::new();
    let validator = real_validator();
    let target = dir.child("victim-dir");
    fs::create_dir_all(target.join("nested")).unwrap();
    fs::write(target.join("nested").join("data.txt"), b"x").unwrap();

    let snapshot = validator
        .capture(&target, None, None, RiskLevel::Safe)
        .expect("capture the pre-rename target");

    // Thread B renames the directory away while "the plan" waits.
    let moved = dir.child("moved-away");
    let src = target.clone();
    let dst = moved.clone();
    let renamer = std::thread::spawn(move || {
        fs::rename(&src, &dst).expect("rename the target away");
    });
    renamer.join().expect("renamer thread");

    let verdict = validator.validate(&ValidationRequest {
        path: &target,
        product: None,
        snapshot: &snapshot,
    });
    assert!(
        !verdict.is_allowed(),
        "a renamed target must never validate"
    );
    match verdict.deny_reason() {
        Some(DenyReason::TargetMissing { .. }) => {}
        Some(other) => panic!("expected TargetMissing for a renamed-away directory, got {other:?}"),
        None => panic!("expected a deny, got {verdict:?}"),
    }

    // The renamed object itself is untouched and still exists with content.
    assert!(moved.join("nested").join("data.txt").exists());
}

// ---------------------------------------------------------------------------
// Scenario 9 — replacement (delete + re-create / swap)
// ---------------------------------------------------------------------------

#[test]
fn deleted_and_recreated_directory_is_denied_as_identity_changed() {
    let dir = TempDir::new();
    let validator = real_validator();
    let target = dir.child("swap-dir");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("original.bin"), b"original").unwrap();

    let snapshot = validator
        .capture(&target, None, None, RiskLevel::Safe)
        .expect("capture the original directory");

    // Mutation: remove the whole directory, re-create the same path with
    // *different* content (a classic scan→delete replacement).
    fs::remove_dir_all(&target).expect("remove original");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("evil.bin"), b"replacement").unwrap();

    let verdict = validator.validate(&ValidationRequest {
        path: &target,
        product: None,
        snapshot: &snapshot,
    });
    assert!(
        !verdict.is_allowed(),
        "a re-created directory must never validate"
    );
    match verdict.deny_reason() {
        Some(DenyReason::IdentityChanged { .. }) => {}
        Some(other) => panic!("expected IdentityChanged for a re-created dir, got {other:?}"),
        None => panic!("expected a deny, got {verdict:?}"),
    }
}

#[test]
fn plain_directory_swapped_for_a_junction_is_denied_as_reparse_target() {
    let dir = TempDir::new();
    let validator = real_validator();
    let target = dir.child("swap-to-junction");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep.txt"), b"x").unwrap();

    let snapshot = validator
        .capture(&target, None, None, RiskLevel::Safe)
        .expect("capture the plain directory");

    // Swap: the directory is deleted and replaced by a junction pointing
    // somewhere else (junction substitution attack).
    let elsewhere = dir.child("elsewhere");
    fs::create_dir_all(elsewhere.join("secret")).unwrap();
    fs::remove_dir_all(&target).expect("remove plain dir");
    create_junction(&target, &elsewhere).expect("mklink /J");

    let verdict = validator.validate(&ValidationRequest {
        path: &target,
        product: None,
        snapshot: &snapshot,
    });
    assert!(
        !verdict.is_allowed(),
        "a junction must never validate as a dir"
    );
    // Real control flow: the reparse guard runs *before* the fingerprint and
    // refuses the target itself as a reparse point (DO-NOT-FOLLOW). Core's
    // `ReparseSubstitution` variant is for the reverse direction — the
    // *recorded* reparse state vanishing — exercised in the next test.
    match verdict.deny_reason() {
        Some(DenyReason::ReparseTarget { kind, .. }) => {
            assert_eq!(
                *kind,
                devresidue_core::safety::probe::ReparseKind::Junction,
                "the substituted object must be seen as a junction"
            );
        }
        Some(other) => panic!("expected ReparseTarget for a junction swap, got {other:?}"),
        None => panic!("expected a deny, got {verdict:?}"),
    }
    // The junction's target is never touched by the validator.
    assert!(elsewhere.join("secret").exists());
}

#[test]
fn recorded_junction_replaced_by_plain_dir_is_denied() {
    // Reverse substitution: the snapshot records a junction entry, and by
    // validation time the entry is a plain directory — whatever was scanned
    // is no longer what sits on disk.
    let dir = TempDir::new();
    let validator = real_validator();
    let real = dir.child("real-content");
    fs::create_dir_all(real.join("files")).unwrap();
    let link = dir.child("recorded-junction");
    create_junction(&link, &real).expect("mklink /J");

    let snapshot = validator
        .capture(&link, None, None, RiskLevel::Safe)
        .expect("capture the junction entry");
    assert!(
        snapshot.reparse_state.is_some(),
        "the recorded snapshot must carry the junction reparse state"
    );

    // Mutation: the junction entry is removed and a plain directory appears
    // at the same path.
    fs::remove_dir(&link).expect("remove the junction entry");
    fs::create_dir(&link).unwrap();
    fs::write(link.join("plain.txt"), b"x").unwrap();

    let verdict = validator.validate(&ValidationRequest {
        path: &link,
        product: None,
        snapshot: &snapshot,
    });
    assert!(
        !verdict.is_allowed(),
        "a substituted entry must never validate"
    );
    // Actual flow: the fingerprint of the new plain directory differs from
    // the recorded junction-entry identity, so IdentityChanged fires first;
    // ReparseSubstitution is accepted as an equivalent fail-closed reason.
    match verdict.deny_reason() {
        Some(DenyReason::IdentityChanged { .. }) => {}
        Some(DenyReason::ReparseSubstitution { .. }) => {}
        Some(other) => panic!("expected IdentityChanged/ReparseSubstitution, got {other:?}"),
        None => panic!("expected a deny, got {verdict:?}"),
    }
}

#[test]
fn untouched_control_directory_validates_allow() {
    // Control: no mutation between capture and validate → Allow.
    let dir = TempDir::new();
    let validator = real_validator();
    let target = dir.child("steady");
    fs::create_dir_all(target.join("sub")).unwrap();
    fs::write(target.join("sub").join("f.txt"), b"x").unwrap();

    let snapshot = validator
        .capture(&target, None, None, RiskLevel::Safe)
        .expect("capture steady dir");
    let verdict = validator.validate(&ValidationRequest {
        path: &target,
        product: None,
        snapshot: &snapshot,
    });
    assert!(
        verdict.is_allowed(),
        "an untouched directory must validate, got {verdict:?}"
    );
}
