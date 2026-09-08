//! R05 verified-deletion binding — real-FS matrix.
//!
//! Both verified operations must first bind to the identity recorded at
//! validation time: replacing the object between capture and the port call
//! yields `IdentityMismatch` and the replacement survives. An untouched object
//! is deleted by verified `DirectDelete`; verified `RecycleBin` binds through
//! the post-hoc protocol (staged rename + pinned identity check + outcome
//! verification) and recycles the exact verified object.

mod common;

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_core::safety::probe::FileIdentity;
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_platform_windows::identity::file_identity;

use common::TempDir;

fn port() -> WindowsDeletePort {
    WindowsDeletePort
}

fn identity_of(path: &std::path::Path) -> FileIdentity {
    file_identity(path)
        .map(|id| FileIdentity {
            volume_serial: id.volume_serial,
            file_index: id.file_index,
            last_write: Some(id.last_write),
        })
        .expect("read identity")
}

#[test]
fn r05_untouched_directory_deletes_verified() {
    let dir = TempDir::new();
    let target = dir.child("steady-clean");
    std::fs::create_dir_all(target.join("sub")).unwrap();
    std::fs::write(target.join("sub").join("f.txt"), b"x").unwrap();

    let expected = identity_of(&target);
    port()
        .delete_tree_verified(&target, &expected)
        .expect("verified delete of the untouched object");
    assert!(!target.exists(), "the object should be gone");
}

#[test]
fn r05_replaced_directory_is_refused_and_the_replacement_survives() {
    let dir = TempDir::new();
    let target = dir.child("swap-target");
    std::fs::create_dir_all(target.join("nested")).unwrap();
    std::fs::write(target.join("nested").join("data.txt"), b"original").unwrap();

    let expected = identity_of(&target);

    // Attacker replaces the whole directory at the same path after capture.
    std::fs::remove_dir_all(&target).expect("remove original");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("evil.txt"), b"replacement").unwrap();

    let err = port()
        .delete_tree_verified(&target, &expected)
        .expect_err("a replaced object must never be deleted verified");
    match err {
        DeleteError::IdentityMismatch { .. } => {}
        other => panic!("expected IdentityMismatch, got {other:?}"),
    }
    // The replacement object is untouched.
    assert!(target.join("evil.txt").exists(), "replacement must survive");
}

#[test]
fn r05_replaced_file_is_refused_and_the_replacement_survives() {
    let dir = TempDir::new();
    let target = dir.child("swap-file.bin");
    std::fs::write(&target, b"original").unwrap();

    let expected = identity_of(&target);

    // Replace the file at the same path.
    std::fs::remove_file(&target).unwrap();
    std::fs::write(&target, b"replacement").unwrap();

    let err = port()
        .delete_tree_verified(&target, &expected)
        .expect_err("a replaced file must never be deleted verified");
    assert!(matches!(err, DeleteError::IdentityMismatch { .. }));
    assert_eq!(std::fs::read(&target).unwrap(), b"replacement");
}

#[test]
fn r05_missing_target_is_not_a_mismatch_but_not_found() {
    let dir = TempDir::new();
    let ghost = dir.child("ghost");
    // A real identity from a *different* existing path — the verified port
    // must fail with NotFound (the path vanished) rather than a mismatch or
    // an accidental delete of the wrong object.
    let existing = dir.child("existing");
    std::fs::write(&existing, b"x").unwrap();
    let expected = identity_of(&existing);
    let err = port()
        .delete_tree_verified(&ghost, &expected)
        .expect_err("ghost must not delete");
    assert!(
        matches!(err, DeleteError::NotFound { .. }),
        "a vanished target is NotFound, not a mismatch: {err:?}"
    );
    assert!(existing.exists());
}

#[test]
fn r05_recycle_verified_moves_the_object_and_rejects_a_replacement() {
    let dir = TempDir::new();
    // Untouched object: the post-hoc protocol recycles it away.
    let target = dir.child("recycle-bound");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("f.txt"), b"x").unwrap();
    let expected = identity_of(&target);

    port()
        .recycle_verified(&target, &expected)
        .expect("the untouched verified object is recycled");
    assert!(!target.exists(), "the verified object is gone");

    // Replaced object: the recycle must refuse with IdentityMismatch before
    // any staging, and the replacement survives.
    let swapped = dir.child("recycle-swapped");
    std::fs::create_dir(&swapped).unwrap();
    std::fs::write(swapped.join("original.txt"), b"original").unwrap();
    let swap_expected = identity_of(&swapped);
    std::fs::remove_dir_all(&swapped).unwrap();
    std::fs::create_dir(&swapped).unwrap();
    std::fs::write(swapped.join("replacement.txt"), b"replacement").unwrap();

    let err = port()
        .recycle_verified(&swapped, &swap_expected)
        .expect_err("a replaced object must never be recycled verified");
    assert!(
        matches!(err, DeleteError::IdentityMismatch { .. }),
        "expected IdentityMismatch, got {err:?}"
    );
    assert_eq!(
        std::fs::read(swapped.join("replacement.txt")).unwrap(),
        b"replacement",
        "the replacement must survive"
    );
}
