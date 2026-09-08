//! File-identity capability tests (TOCTOU fingerprint, SPEC §16).

mod common;

use std::fs;

use devresidue_platform_windows::filesystem::reparse;
use devresidue_platform_windows::identity::{file_identity, file_identity_following};
use devresidue_platform_windows::FileSystemError;

use common::{create_junction, TempDir};

#[test]
fn identity_is_stable_across_reads() {
    let dir = TempDir::new();
    let file = dir.child("stable.txt");
    fs::write(&file, b"hello").unwrap();

    let a = file_identity(&file).expect("first read");
    let b = file_identity(&file).expect("second read");
    assert_eq!(a, b);
    assert_eq!(a.volume_serial, b.volume_serial);
    assert_eq!(a.file_index, b.file_index);
}

#[test]
fn distinct_files_have_distinct_identities() {
    let dir = TempDir::new();
    let f1 = dir.child("one.txt");
    let f2 = dir.child("two.txt");
    fs::write(&f1, b"1").unwrap();
    fs::write(&f2, b"2").unwrap();

    let id1 = file_identity(&f1).unwrap();
    let id2 = file_identity(&f2).unwrap();
    assert_ne!(
        id1.file_index, id2.file_index,
        "two files on the same volume must not share a file index"
    );
    assert_eq!(id1.volume_serial, id2.volume_serial, "same temp volume");
}

#[test]
fn directories_are_supported() {
    let dir = TempDir::new();
    let sub = dir.child("dir-object");
    fs::create_dir(&sub).unwrap();
    let id = file_identity(&sub).expect("identity of a directory");
    assert_ne!(id.file_index, 0);
}

#[test]
fn junction_identity_is_own_entry_not_target() {
    let dir = TempDir::new();
    let target = dir.child("real-dir");
    fs::create_dir(&target).unwrap();
    let link = dir.child("junction-identity");
    create_junction(&link, &target).expect("mklink /J");

    let info = reparse::probe(&link).unwrap();
    assert!(info.tag.is_some(), "link must be a reparse point");

    // file_identity (no-follow) fingerprints the link entry itself ...
    let link_id = file_identity(&link).expect("junction entry identity");
    // ... while file_identity_following resolves to the target object.
    let resolved_id = file_identity_following(&link).expect("junction target identity");
    let target_id = file_identity(&target).expect("real dir identity");

    assert_ne!(
        link_id.file_index, resolved_id.file_index,
        "junction entry (open-reparse-point) must differ from its target"
    );
    assert_eq!(
        resolved_id, target_id,
        "following the junction must yield the real directory's identity"
    );
}

#[test]
fn missing_path_reports_not_found() {
    let dir = TempDir::new();
    let ghost = dir.child("missing");
    match file_identity(&ghost) {
        Err(FileSystemError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}
