//! Phase 15-B / SPEC §33 **concurrent delete** real-filesystem matrix.
//!
//! Two threads deleting the same directory through the same
//! `WindowsDeletePort` must race without panic and converge on honest
//! outcomes: at least one side wins and the tree no longer exists; a losing
//! racer reports the target as gone (`NotFound`) — never a fabricated success
//! over a ghost.

mod common;

use std::fs;

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_platform_windows::cleanup::WindowsDeletePort;

use common::TempDir;

#[test]
fn concurrent_delete_tree_races_without_panic_and_converges() {
    let dir = TempDir::new();
    let target = dir.child("contested-tree");
    // A tree deep enough that both racers overlap real work.
    for i in 0..50 {
        let sub = target.join(format!("d{i:03}"));
        fs::create_dir_all(&sub).unwrap();
        for j in 0..10 {
            fs::write(sub.join(format!("f{j:03}.bin")), b"x").unwrap();
        }
    }

    let racer =
        |path: std::path::PathBuf| std::thread::spawn(move || WindowsDeletePort.delete_tree(&path));
    let t1 = racer(target.clone());
    let t2 = racer(target.clone());
    let r1 = t1.join().expect("racer 1 did not panic");
    let r2 = t2.join().expect("racer 2 did not panic");

    assert!(
        !target.exists(),
        "the tree must be gone after the race (r1={r1:?}, r2={r2:?})"
    );

    let wins = [&r1, &r2].iter().filter(|r| r.is_ok()).count();
    assert!(
        wins >= 1,
        "at least one racer must succeed deleting the tree (r1={r1:?}, r2={r2:?})"
    );
    for (who, result) in [("racer 1", &r1), ("racer 2", &r2)] {
        match result {
            Ok(()) => {}
            // Honest loser outcomes on Windows: the tree is gone by the time
            // this racer acts (NotFound), or the racer collides with the
            // winner mid-walk and hits a transient ACCESS_DENIED while
            // another thread removes the very directory it is deleting
            // (PermissionDenied). Both must be reported — never a fabricated
            // success over a ghost.
            Err(DeleteError::NotFound { .. }) => {}
            Err(DeleteError::PermissionDenied { .. }) => {}
            Err(other) => {
                panic!("{who} reported an unexpected outcome for a lost delete race: {other}")
            }
        }
    }
}

#[test]
fn delete_tree_on_a_ghost_reports_not_found_honestly() {
    // Sequential analogue documenting the NotFound side of the race: once the
    // tree is gone, a second delete reports NotFound — never a silent success.
    let dir = TempDir::new();
    let target = dir.child("once-only");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("x.txt"), b"x").unwrap();

    WindowsDeletePort
        .delete_tree(&target)
        .expect("first delete wins");
    assert!(!target.exists());

    match WindowsDeletePort.delete_tree(&target) {
        Err(DeleteError::NotFound { .. }) => {}
        other => panic!("second delete of a ghost must be NotFound, got {other:?}"),
    }
}
