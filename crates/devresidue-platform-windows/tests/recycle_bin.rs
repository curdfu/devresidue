//! Recycle-bin capability test.
//!
//! The recycle-bin capability is a *primitive*; outside tests the only
//! legitimate caller is the Phase 6 CleanupEngine (INV-010).

mod common;

use std::fs;

use devresidue_platform_windows::recycle_bin::recycle;

use common::TempDir;

#[test]
fn temp_file_disappears_after_recycle() {
    let dir = TempDir::new();
    let file = dir.child("recycle-me.txt");
    fs::write(&file, b"payload").unwrap();
    assert!(file.exists());

    recycle(&file).expect("recycle must succeed on a plain temp file");
    assert!(!file.exists(), "file must be gone after recycling");
}

#[test]
fn missing_path_is_reported() {
    let dir = TempDir::new();
    let ghost = dir.child("never-existed.txt");
    let err = recycle(&ghost).expect_err("recycling a missing path must fail");
    assert!(
        format!("{err}").contains("does not exist"),
        "unexpected error message: {err}"
    );
}
