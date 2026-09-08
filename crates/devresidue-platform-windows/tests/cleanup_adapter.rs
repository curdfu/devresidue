//! Real-filesystem tests for the Windows DeletePort adapter
//! (recycle / delete_tree / execute_command).

mod common;

use std::fs;

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_core::safety::probe::FileIdentity;
use devresidue_core::ExternalCommandSpec;
use devresidue_platform_windows::cleanup::WindowsDeletePort;

use common::{create_junction, TempDir};

fn port() -> WindowsDeletePort {
    WindowsDeletePort
}

fn identity(path: &std::path::Path) -> FileIdentity {
    let id = devresidue_platform_windows::identity::file_identity(path).expect("file identity");
    FileIdentity {
        volume_serial: id.volume_serial,
        file_index: id.file_index,
        last_write: Some(id.last_write),
    }
}

#[test]
fn recycle_moves_a_directory_into_the_recycle_bin() {
    let dir = TempDir::new();
    let target = dir.child("recycle-me");
    fs::create_dir_all(target.join("nested")).unwrap();
    fs::write(target.join("nested").join("file.txt"), b"data").unwrap();
    assert!(target.exists());

    port().recycle(&target).expect("recycle a directory");

    assert!(!target.exists(), "directory must be gone after recycling");
}

#[test]
fn delete_tree_removes_files_and_directories() {
    let dir = TempDir::new();

    // File.
    let file = dir.child("delete-me.txt");
    fs::write(&file, b"x").unwrap();
    port().delete_tree(&file).expect("delete file");
    assert!(!file.exists());

    // Whole directory tree.
    let tree = dir.child("tree");
    fs::create_dir_all(tree.join("a").join("b")).unwrap();
    fs::write(tree.join("a").join("b").join("f"), b"y").unwrap();
    port().delete_tree(&tree).expect("delete tree");
    assert!(!tree.exists());
}

#[test]
fn verified_delete_removes_a_regular_file() {
    let dir = TempDir::new();
    let file = dir.child("verified-delete-me.txt");
    fs::write(&file, b"verified-file").unwrap();
    let expected = identity(&file);

    port()
        .delete_tree_verified(&file, &expected)
        .expect("verified DirectDelete supports ordinary files");

    assert!(!file.exists(), "the selected file must be gone");
    let residue: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("devresidue-staged-")
        })
        .collect();
    assert!(residue.is_empty(), "no staged file may remain: {residue:?}");
}

#[test]
fn verified_recycle_moves_the_object_through_the_post_hoc_protocol() {
    // Post-hoc binding protocol: the verified object is staged (bound),
    // identity-checked on the pinned handle, handed to the real shell, and
    // the outcome verified — the object is recycled, no staged residue.
    let dir = TempDir::new();
    let target = dir.child("verified-recycle-me");
    fs::create_dir_all(target.join("nested")).unwrap();
    fs::write(target.join("nested").join("file.txt"), b"data").unwrap();
    let expected = identity(&target);

    port()
        .recycle_verified(&target, &expected)
        .expect("verified recycle succeeds through the post-hoc protocol");

    assert!(!target.exists(), "the verified object was recycled away");
    let residue: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("devresidue-staged-")
        })
        .collect();
    assert!(
        residue.is_empty(),
        "no staged entry may remain: {residue:?}"
    );
}

#[test]
fn delete_tree_never_follows_a_junction() {
    let dir = TempDir::new();
    let target = dir.child("real-target");
    fs::create_dir_all(target.join("keep-me")).unwrap();

    let link = dir.child("junction-link");
    create_junction(&link, &target).expect("mklink /J");

    // Deleting the junction removes the link entry only (INV-004: a reparse
    // point is never followed by the deletion port).
    port().delete_tree(&link).expect("delete junction link");
    assert!(!link.exists(), "link entry removed");
    assert!(
        target.exists() && target.join("keep-me").exists(),
        "junction target must survive"
    );
}

#[test]
fn execute_command_success_path() {
    let spec = ExternalCommandSpec::new("where.exe".into(), vec!["where.exe".into()], None, None);
    port().execute_command(&spec).expect("where.exe exits 0");
}

#[test]
fn execute_command_nonzero_exit_is_an_error() {
    let spec = ExternalCommandSpec::new(
        "where.exe".into(),
        vec!["devresidue-no-such-binary-xyz".into()],
        None,
        None,
    );
    let err = port()
        .execute_command(&spec)
        .expect_err("nonzero exit must fail");
    assert!(matches!(
        err,
        DeleteError::CommandFailed {
            exit_code: Some(1),
            ..
        }
    ));
}

#[test]
fn missing_targets_map_to_not_found() {
    let dir = TempDir::new();
    let ghost = dir.child("never-existed");
    assert!(matches!(
        port().delete_tree(&ghost),
        Err(DeleteError::NotFound { .. })
    ));
}
