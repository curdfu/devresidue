//! Reparse-point probing tests: junction (mklink /J) and symlink (std API),
//! plus DO-NOT-FOLLOW behaviour on a plain directory.

mod common;

use std::fs;

use devresidue_platform_windows::filesystem::{attributes, reparse};
use devresidue_platform_windows::path;

use common::{create_junction, is_privilege_error, symlink_dir, TempDir};

#[test]
fn plain_directory_is_not_a_reparse_point() {
    let dir = TempDir::new();
    let sub = dir.child("sub");
    fs::create_dir(&sub).unwrap();

    let info = reparse::probe(&sub).expect("probe plain dir");
    assert_eq!(info.tag, None, "plain dir must not be a reparse point");
    assert_eq!(info.target, None);

    let attrs = attributes(&sub).unwrap();
    assert!(!attrs.is_reparse_point());
}

#[test]
fn junction_is_reported_with_real_target() {
    let dir = TempDir::new();
    let target = dir.child("target-dir");
    let nested = target.join("nested");
    fs::create_dir_all(&nested).unwrap();

    let link = dir.child("junction-link");
    create_junction(&link, &target).expect("mklink /J must succeed in this environment");

    let attrs = attributes(&link).expect("read attributes");
    assert!(
        attrs.is_reparse_point(),
        "junction must carry the reparse-point attribute"
    );
    assert!(attrs.is_dir());

    let info = reparse::probe(&link).expect("probe junction");
    let tag = info.tag.expect("junction must be a reparse point");
    assert_eq!(tag, reparse::ReparseTag::Junction, "mklink /J → junction");
    let probed_target = info.target.expect("junction must have a target");

    // The substitute name is absolute; compare case-insensitively on the
    // normalised key so drive letter case differences cannot fail the test.
    assert_eq!(
        path::as_key(probed_target),
        path::as_key(&target),
        "junction must point at the created target dir"
    );
}

#[test]
fn symlink_dir_is_reported_with_target_or_skips_visibly() {
    let dir = TempDir::new();
    let target = dir.child("target-dir");
    fs::create_dir(&target).unwrap();
    let link = dir.child("symlink-dir");

    if let Err(e) = symlink_dir(&target, &link) {
        if is_privilege_error(&e) {
            eprintln!(
                "SKIP: cannot create directory symlinks \
                 (needs Developer Mode or an elevated shell): {e}"
            );
            return;
        }
        panic!("symlink_dir failed for an unexpected reason: {e}");
    }

    let attrs = attributes(&link).expect("read attributes");
    assert!(attrs.is_reparse_point());

    let info = reparse::probe(&link).expect("probe symlink");
    assert_eq!(info.tag, Some(reparse::ReparseTag::Symlink));
    let probed = info.target.expect("symlink must have a target");
    assert_eq!(path::as_key(probed), path::as_key(&target));
}
