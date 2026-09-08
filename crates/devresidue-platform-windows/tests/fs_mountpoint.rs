//! Phase 15-B / SPEC §33 **mount point** real-filesystem matrix.
//!
//! A volume mount point is an `IO_REPARSE_TAG_MOUNT_POINT` over a *volume*
//! (the same tag family as a directory junction, distinguished by the
//! `\??\Volume{...}` substitute name). Creating one needs an unmounted volume
//! plus an elevated context (`mountvol`), so this test skips visibly when the
//! machine cannot provide one.
//!
//! The contract under test here (per scenario 6) is the probe's *tag*
//! correctness at the real-FS layer; the DO-NOT-DESCEND / measure side is
//! exercised over junctions (same reparse machinery, no privilege needed) in
//! the providers matrix test.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use devresidue_platform_windows::filesystem::attributes;
use devresidue_platform_windows::filesystem::reparse::{probe, ReparseTag};

use common::{create_junction, TempDir};

/// Finds an unmounted volume GUID usable with `mountvol` (best effort), or
/// returns `Err(reason)`.
fn find_unmounted_volume() -> Result<String, String> {
    let out = Command::new("mountvol.exe")
        .output()
        .map_err(|e| format!("cannot run mountvol: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "mountvol failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Each volume block starts with a "\\?\Volume{...}\" line; the following
    // non-empty indented lines are its mount points. A block with no mount
    // point line is an unmounted volume we may bind to a folder.
    let mut current_volume: Option<String> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("\\\\?\\Volume{") {
            current_volume = Some(line.to_string());
            continue;
        }
        // Any other content under the current block = a mount point exists →
        // this volume is taken; keep looking.
        if current_volume.is_some() {
            current_volume = None;
        }
    }
    match current_volume {
        Some(vol) => Ok(vol),
        None => Err(
            "no unmounted volume found (only volumes with drive letters or \
             recovery partitions that refuse mounting)"
                .to_string(),
        ),
    }
}

/// RAII guard that unmounts the mount point and removes the empty dir.
struct MountGuard {
    dir: PathBuf,
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let _ = Command::new("mountvol.exe")
            .arg(&self.dir)
            .arg("/D")
            .status();
        let _ = std::fs::remove_dir(&self.dir);
    }
}

#[test]
fn volume_mount_point_is_probed_as_mountpoint_or_skips_visibly() {
    let dir = TempDir::new();
    let mount_dir = dir.child("volume-mount");
    fs::create_dir(&mount_dir).expect("create mount folder");

    let volume = match find_unmounted_volume() {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("SKIP: cannot create a volume mount point — {reason}");
            return;
        }
    };

    let status = match Command::new("mountvol.exe")
        .arg(&mount_dir)
        .arg(&volume)
        .status()
    {
        Ok(s) if s.success() => s,
        Ok(s) => {
            eprintln!(
                "SKIP: mountvol rejected the mount point (exit {}); likely missing \
                 privilege or an unsupported volume",
                s.code().unwrap_or(-1)
            );
            return;
        }
        Err(e) => {
            eprintln!("SKIP: mountvol could not run: {e}");
            return;
        }
    };
    let _ = status;
    let _guard = MountGuard {
        dir: mount_dir.clone(),
    };

    // Re-probe sanity: the folder is now a mount point.
    let attrs = attributes(&mount_dir).expect("attributes of the mount point");
    assert!(
        attrs.is_reparse_point(),
        "a mounted volume must carry the reparse attribute"
    );

    let info = probe(&mount_dir).expect("probe the volume mount point");
    assert_eq!(
        info.tag,
        Some(ReparseTag::MountPoint),
        "a volume mount point must be tagged MountPoint, not Junction \
         (same reparse tag, different substitute name)"
    );
    let target = info.target.expect("mount point reports its volume");
    assert!(
        target.to_string_lossy().starts_with("\\\\?\\Volume"),
        "mount-point target must name a volume object: {}",
        target.display()
    );
}

#[test]
fn junction_remains_distinct_from_a_mount_point_tag() {
    // Control for the tag classification: a directory junction (mklink /J)
    // shares the IO_REPARSE_TAG_MOUNT_POINT *tag value* but must be reported
    // as Junction because its substitute name is a directory path.
    let dir = TempDir::new();
    let target = dir.child("real-target");
    fs::create_dir_all(target.join("nested")).unwrap();
    let link = dir.child("plain-junction");
    create_junction(&link, &target).expect("mklink /J");

    let info = probe(&link).expect("probe junction");
    assert_eq!(info.tag, Some(ReparseTag::Junction));
    let probed = info.target.expect("junction target");
    assert!(
        !probed.to_string_lossy().starts_with("\\\\?\\Volume"),
        "a junction target is a directory, never a volume: {}",
        probed.display()
    );
}
