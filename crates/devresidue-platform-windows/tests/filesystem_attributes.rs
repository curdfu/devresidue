//! Filesystem attribute capability tests.

mod common;

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use devresidue_platform_windows::filesystem::attributes;
use devresidue_platform_windows::FileSystemError;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    FILE_FLAGS_AND_ATTRIBUTES,
};

use common::TempDir;

fn to_wide(path: &std::path::Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn set_attributes(path: &Path, flags: FILE_FLAGS_AND_ATTRIBUTES) {
    let wide = to_wide(path);
    // SAFETY: `wide` is a nul-terminated buffer owned by this frame and the
    // attribute flags are plain Win32 constants; SetFileAttributesW writes
    // nothing into user memory.
    #[allow(unsafe_code)]
    unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), flags) }
        .expect("SetFileAttributesW must succeed");
}

#[test]
fn fresh_file_reports_defaults() {
    let dir = TempDir::new();
    let file = dir.child("plain.txt");
    fs::write(&file, b"x").unwrap();

    let attrs = attributes(&file).expect("read attributes");
    assert!(!attrs.read_only);
    assert!(!attrs.hidden);
    assert!(!attrs.system);
    assert!(!attrs.reparse_point);
    assert!(!attrs.is_dir());
    assert!(!attrs.is_reparse_point());
    assert!(attrs.archive, "fresh files carry the archive bit");
}

#[test]
fn hidden_and_readonly_flags_round_trip() {
    let dir = TempDir::new();
    let file = dir.child("flags.bin");
    fs::write(&file, b"x").unwrap();

    set_attributes(&file, FILE_ATTRIBUTE_HIDDEN);
    let attrs = attributes(&file).expect("read attributes");
    assert!(attrs.hidden);
    assert!(!attrs.read_only);

    // Combine hidden | readonly.
    set_attributes(&file, FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_READONLY);
    let attrs = attributes(&file).expect("read attributes");
    assert!(attrs.hidden);
    assert!(attrs.read_only);

    // Reset to normal so TempDir cleanup can delete it.
    set_attributes(&file, FILE_ATTRIBUTE_NORMAL);
    let attrs = attributes(&file).expect("read attributes");
    assert!(!attrs.hidden);
    assert!(!attrs.read_only);
}

#[test]
fn directory_flag_is_set() {
    let dir = TempDir::new();
    let sub = dir.child("subdir");
    fs::create_dir(&sub).unwrap();

    let attrs = attributes(&sub).expect("read attributes");
    assert!(attrs.is_dir());
    assert!(!attrs.is_reparse_point());
}

#[test]
fn missing_path_returns_not_found() {
    let dir = TempDir::new();
    let ghost = dir.child("no-such-file");
    match attributes(&ghost) {
        Err(FileSystemError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}
