//! Real-filesystem tests for the core safety probe adapters
//! (`crates/devresidue-platform-windows/src/safety`).

mod common;

use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use devresidue_core::safety::probe::{
    PathProbe, ProbeError, ProcessProbe, ProcessState, ReparseKind,
};
use devresidue_platform_windows::safety::{WindowsPathProbe, WindowsProcessProbe};

use common::{create_junction, TempDir};

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Sets the hidden attribute (real Win32) on a temp file to exercise the
/// adapter's attribute translation.
fn set_hidden(path: &Path) {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
    let wide = to_wide(path);
    // SAFETY: nul-terminated buffer alive for the call; no output pointers.
    #[allow(unsafe_code)]
    unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_HIDDEN) }
        .expect("set hidden");
}

#[test]
fn attributes_translate_to_core_attr_flags() {
    let dir = TempDir::new();
    let file = dir.child("flags.txt");
    fs::write(&file, b"x").unwrap();

    let probe = WindowsPathProbe;
    let attrs = probe.attributes(&file).expect("read attributes");
    assert!(!attrs.hidden);
    assert!(!attrs.reparse);

    set_hidden(&file);
    let attrs = probe.attributes(&file).expect("read hidden");
    assert!(attrs.hidden);
    assert!(!attrs.directory);
}

#[test]
fn junction_is_reported_through_the_adapter() {
    let dir = TempDir::new();
    let target = dir.child("target-dir");
    fs::create_dir_all(target.join("nested")).unwrap();
    let link = dir.child("junction");
    create_junction(&link, &target).expect("mklink /J");

    let probe = WindowsPathProbe;
    let attrs = probe.attributes(&link).expect("junction attrs");
    assert!(attrs.reparse, "junction must report the reparse flag");
    assert!(attrs.directory);

    let info = probe.reparse_info(&link).expect("probe junction");
    assert_eq!(info.kind, Some(ReparseKind::Junction));
    assert!(info.target.is_some());
}

#[test]
fn identity_translation_is_stable_and_distinguishes_entry_from_target() {
    let dir = TempDir::new();
    let target = dir.child("real-dir");
    fs::create_dir(&target).unwrap();
    let link = dir.child("identity-junction");
    create_junction(&link, &target).expect("mklink /J");

    let probe = WindowsPathProbe;
    let first = probe.file_identity(&link).expect("entry identity");
    let second = probe.file_identity(&link).expect("entry identity again");
    assert_eq!(first, second, "identity must be stable");

    let resolved = probe.file_identity(&target).expect("target identity");
    assert_ne!(
        first.file_index, resolved.file_index,
        "junction entry must not be confused with its target"
    );
    assert!(first.last_write.is_some());
}

#[test]
fn missing_paths_map_to_probe_error_not_found() {
    let dir = TempDir::new();
    let ghost = dir.child("does-not-exist");
    let probe = WindowsPathProbe;
    assert!(matches!(
        probe.attributes(&ghost),
        Err(ProbeError::NotFound { .. })
    ));
    assert!(matches!(
        probe.reparse_info(&ghost),
        Err(ProbeError::NotFound { .. })
    ));
    assert!(matches!(
        probe.file_identity(&ghost),
        Err(ProbeError::NotFound { .. })
    ));
}

#[test]
fn process_probe_reports_this_test_process() {
    let probe = WindowsProcessProbe;
    let exe = std::env::current_exe()
        .expect("current exe")
        .file_name()
        .expect("exe name")
        .to_string_lossy()
        .into_owned();
    let extensionless = exe.trim_end_matches(".exe");
    assert_eq!(
        probe.process_state(&[extensionless]),
        ProcessState::Running,
        "the test harness itself must be observed running"
    );
    assert_eq!(
        probe.process_state(&["devresidue-no-such-process-xyz"]),
        ProcessState::NotRunning
    );
}

#[test]
fn probe_traits_are_object_safe() {
    // Core programs against `&dyn PathProbe` / `&dyn ProcessProbe`; this test
    // pins object-safety of the Windows adapters.
    let path_probe: &dyn PathProbe = &WindowsPathProbe;
    let process_probe: &dyn ProcessProbe = &WindowsProcessProbe;
    let _ = path_probe;
    let _ = process_probe;

    let dir = TempDir::new();
    let file = dir.child("obj.txt");
    fs::write(&file, b"o").unwrap();
    let d: &dyn PathProbe = &WindowsPathProbe;
    assert!(d.attributes(&file).is_ok());
}

#[test]
fn platform_adapters_translate_a_real_directory_tree() {
    // Directory target (not a reparse point) through the whole probe surface.
    let dir = TempDir::new();
    let sub = dir.child("plain-tree");
    fs::create_dir_all(sub.join("a").join("b")).unwrap();

    let probe = WindowsPathProbe;
    let attrs = probe.attributes(&sub).expect("attrs");
    assert!(attrs.directory);
    let info = probe.reparse_info(&sub).expect("reparse");
    assert_eq!(info.kind, None);
    let id = probe.file_identity(&sub).expect("identity");
    assert_ne!(id.file_index, 0);
    assert!(id.volume_serial != 0);

    let ghost = PathBuf::from("Z:\\definitely\\missing\\devresidue");
    assert!(matches!(
        probe.file_identity(&ghost),
        Err(ProbeError::NotFound { .. })
    ));
}
