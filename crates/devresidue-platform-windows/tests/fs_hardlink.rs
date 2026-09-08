//! Phase 15-B / SPEC §33 **hardlink** real-filesystem matrix.
//!
//! Two hard links are two directory entries for one underlying NTFS file: the
//! identity probe must report the *same* `(volume_serial, file_index)` for
//! both. This pins the documented limitation — an NTFS identity cannot
//! distinguish hard links, so a scanner that measures both entries counts the
//! underlying file twice (known F7 low-priority double-count semantics, now
//! test-pinned at the real-FS layer).

mod common;

use std::fs;

use devresidue_platform_windows::identity::file_identity;

use common::TempDir;

#[test]
fn hard_linked_entries_share_one_identity() {
    let dir = TempDir::new();
    let first = dir.child("link-a.bin");
    let second = dir.child("link-b.bin");
    fs::write(&first, b"same underlying content").unwrap();
    fs::hard_link(&first, &second).expect("CreateHardLinkW via std");

    // Real NTFS: both entries resolve to one file object.
    let id_a = file_identity(&first).expect("identity of the first link");
    let id_b = file_identity(&second).expect("identity of the second link");
    assert_eq!(
        (id_a.volume_serial, id_a.file_index),
        (id_b.volume_serial, id_b.file_index),
        "hard links share volume_serial + file_index"
    );

    // Deleting one entry leaves the other fully intact (and with the same
    // content) — deleting a link is not deleting the file.
    fs::remove_file(&first).unwrap();
    assert!(!first.exists());
    assert!(second.exists(), "the other link survives");
    assert_eq!(
        fs::metadata(&second).unwrap().len(),
        "same underlying content".len() as u64
    );
}

#[test]
fn identity_probe_distinguishes_hard_links_from_distinct_files() {
    // Control: two *separate* files (same bytes) must keep distinct
    // identities — otherwise identity would be useless against TOCTOU.
    let dir = TempDir::new();
    let one = dir.child("plain-1.bin");
    let two = dir.child("plain-2.bin");
    fs::write(&one, b"identical bytes").unwrap();
    fs::write(&two, b"identical bytes").unwrap();

    let id_one = file_identity(&one).unwrap();
    let id_two = file_identity(&two).unwrap();
    assert_ne!(
        id_one.file_index, id_two.file_index,
        "same-byte distinct files must not share an identity"
    );
}

#[test]
fn deleting_one_hard_link_entry_keeps_the_other_intact_and_countable() {
    // The deletion consequence of the shared identity: removing one entry
    // (what a plan id targets) must not destroy the file the other link
    // points at.
    let dir = TempDir::new();
    let a = dir.child("rm-a.bin");
    let b = dir.child("rm-b.bin");
    fs::write(&a, b"shared bytes").unwrap();
    fs::hard_link(&a, &b).unwrap();

    fs::remove_file(&a).unwrap();
    assert!(b.exists(), "the surviving link still resolves");
    assert_eq!(
        fs::metadata(&b).unwrap().len(),
        "shared bytes".len() as u64,
        "surviving link reads the same content"
    );
    // The surviving entry still has a real, probeable NTFS identity.
    file_identity(&b).expect("identity of the surviving link");
}
