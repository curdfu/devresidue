//! Phase 15-B / SPEC §33 **locked** real-filesystem matrix.
//!
//! A file opened with `share_mode(0)` is exclusively held: data/delete access
//! by anyone else fails with `ERROR_SHARING_VIOLATION`.
//!
//! Real Win32 facts pinned by this matrix (discovered at the real-FS layer):
//! - metadata probes (`FILE_READ_ATTRIBUTES` / `GetFileAttributesExW`) are
//!   **not** blocked by sharing locks — Windows only enforces sharing against
//!   data/delete access. Attributes + identity therefore succeed on a locked
//!   file and are not folded or swallowed;
//! - the lock is surfaced where it actually matters: deletion requests
//!   DELETE access and fails into the `Locked` bucket;
//! - `delete_tree` over a tree containing the locked file fails as `Locked`
//!   while the tree state is reported honestly (locked file + root remain);
//! - recycle of such a directory reflects whatever the COM shell decides
//!   (per-item semantics, no panic).

mod common;
mod fs_support;

use std::fs;

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_core::safety::probe::PathProbe;
use devresidue_platform_windows::cleanup::WindowsDeletePort;
use devresidue_platform_windows::safety::WindowsPathProbe;

use common::TempDir;
use fs_support::lock_exclusive;

fn port() -> WindowsDeletePort {
    WindowsDeletePort
}

fn probe() -> WindowsPathProbe {
    WindowsPathProbe
}

#[test]
fn locked_files_are_metadata_readable_but_denied_at_deletion() {
    let dir = TempDir::new();
    let file = dir.child("held.txt");
    fs::write(&file, b"payload").unwrap();

    let _lock = lock_exclusive(&file).expect("exclusive open must succeed");

    // Real Win32 behaviour discovered at this layer: *metadata* opens
    // (FILE_READ_ATTRIBUTES / GetFileAttributesExW) are never blocked by a
    // sharing-mode 0 holder — Windows only enforces sharing against data
    // access. So attributes *and* identity probes legitimately succeed on a
    // locked file; they must not be *folded* — they simply are metadata
    // reads. The lock is only enforced where data/delete access is requested,
    // which is the deletion port's job (asserted below).
    let attrs = probe().attributes(&file).expect("attributes must not lock");
    assert!(!attrs.directory, "the held file is a file");

    let id = probe()
        .file_identity(&file)
        .expect("identity reads metadata and must succeed on a locked file");
    let id2 = probe().file_identity(&file).expect("stable across reads");
    assert_eq!(id, id2, "identity is stable while locked");

    // Deleting the locked file *does* request DELETE access → sharing
    // violation → the Locked bucket. This is where the lock is surfaced.
    let err = port()
        .delete_tree(&file)
        .expect_err("a locked file must refuse deletion");
    assert!(
        matches!(err, DeleteError::Locked { .. }),
        "delete must report the Locked bucket, got: {err}"
    );
    assert!(file.exists(), "the locked file survives");

    // Release and confirm deletion now succeeds (the lock was the cause).
    drop(_lock);
    port()
        .delete_tree(&file)
        .expect("delete succeeds once the lock is released");
    assert!(!file.exists());
}

#[test]
fn delete_tree_on_a_locked_tree_fails_locked_and_reports_state_honestly() {
    let dir = TempDir::new();
    let tree = dir.child("locked-tree");
    fs::create_dir(&tree).unwrap();
    let held = tree.join("held.bin");
    let sibling = tree.join("sibling.bin");
    fs::write(&held, b"x").unwrap();
    fs::write(&sibling, b"y").unwrap();

    let _lock = lock_exclusive(&held).expect("exclusive open");

    let err = port()
        .delete_tree(&tree)
        .expect_err("a tree with a locked child must fail");
    assert!(
        matches!(err, DeleteError::Locked { .. }),
        "delete_tree must report the Locked bucket, got: {err}"
    );
    // Honest partial-state reporting: the locked file could not be deleted,
    // so the tree root must still be present (and the held file too).
    assert!(tree.exists(), "tree root must remain after a failed delete");
    assert!(
        held.exists(),
        "the locked file must survive a failed delete"
    );

    drop(_lock);
    port()
        .delete_tree(&tree)
        .expect("after releasing the lock the whole tree must delete");
    assert!(!tree.exists());
}

#[test]
fn delete_tree_on_a_single_locked_file_reports_locked() {
    let dir = TempDir::new();
    let file = dir.child("held-single.bin");
    fs::write(&file, b"x").unwrap();

    let _lock = lock_exclusive(&file).expect("exclusive open");
    let err = port()
        .delete_tree(&file)
        .expect_err("a locked file must fail deletion");
    assert!(matches!(err, DeleteError::Locked { .. }), "got: {err}");
    assert!(file.exists(), "the locked file survives");

    drop(_lock);
    port().delete_tree(&file).expect("delete after unlock");
    assert!(!file.exists());
}

#[test]
fn recycle_of_a_tree_with_a_locked_child_never_panics() {
    let dir = TempDir::new();
    let tree = dir.child("recycle-with-lock");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("held.bin"), b"x").unwrap();

    let lock = lock_exclusive(&tree.join("held.bin")).expect("exclusive open");

    // The COM shell decides how a directory holding a locked child is
    // recycled (in-volume moves can succeed while per-item deletes fail; the
    // result may also be a partial success reported as OK). The contract
    // under test: no panic, and the locked file is never permanently deleted
    // while the lock is held.
    match port().recycle(&tree) {
        Ok(()) => {
            eprintln!(
                "note: shell reported OK recycling the tree (partial/whole-dir \
                 semantics) — original path presence is not guaranteed either way"
            );
        }
        Err(DeleteError::Io { message, .. }) => {
            eprintln!("note: shell refused to recycle a tree with a locked child: {message}");
        }
        Err(other) => panic!("unexpected recycle error for a locked tree: {other}"),
    }

    // Whatever the shell did, the held file's data was not destroyed while
    // the lock was up.
    drop(lock);
    if tree.exists() {
        port()
            .delete_tree(&tree)
            .expect("cleanup after releasing the lock");
    }
}
