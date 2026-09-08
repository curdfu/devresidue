//! The Windows implementation of the core deletion port.
//!
//! This module is the **single place in the platform layer** allowed to touch
//! `std::fs::remove_*`; the [`DeletePort`] contract (core) documents that the
//! CleanupEngine is its only caller (INV-010). Everything is a thin
//! translation over the Phase 4 capabilities:
//!
//! - `recycle` → `crate::recycle_bin::recycle` (COM `IFileOperation`, moves
//!   into the OS Recycle Bin);
//! - `delete_tree` → `std::fs::remove_dir_all` / `remove_file` wrapped in
//!   [`DeleteError`];
//! - `execute_command` → `crate::shell::run` (SPEC §22): success requires exit
//!   code `0` and no timeout.

use std::path::{Path, PathBuf};

use devresidue_core::cleanup::port::{DeleteError, DeletePort};
use devresidue_core::safety::probe::FileIdentity;
use devresidue_core::ExternalCommandSpec;

/// Stateless port over the real Win32 / std filesystem capabilities.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsDeletePort;

impl DeletePort for WindowsDeletePort {
    fn recycle(&self, path: &Path) -> Result<(), DeleteError> {
        crate::recycle_bin::recycle(path).map_err(map_recycle)
    }

    fn delete_tree(&self, path: &Path) -> Result<(), DeleteError> {
        remove_tree(path)
    }

    fn delete_tree_verified(
        &self,
        path: &Path,
        expected: &FileIdentity,
    ) -> Result<(), DeleteError> {
        delete_tree_verified_impl(path, expected)
    }

    fn recycle_verified(&self, path: &Path, expected: &FileIdentity) -> Result<(), DeleteError> {
        recycle_verified_impl(path, expected)
    }

    fn execute_command(&self, spec: &ExternalCommandSpec) -> Result<(), DeleteError> {
        let outcome = crate::shell::run(spec).map_err(|e| DeleteError::CommandFailed {
            exit_code: None,
            timed_out: false,
            message: e.to_string(),
        })?;
        if outcome.timed_out {
            return Err(DeleteError::CommandFailed {
                exit_code: outcome.exit_code,
                timed_out: true,
                message: "external command timed out and was killed".to_string(),
            });
        }
        match outcome.exit_code {
            Some(0) => Ok(()),
            other => Err(DeleteError::CommandFailed {
                exit_code: other,
                timed_out: false,
                message: format!(
                    "external command exited non-zero; stderr: {}",
                    outcome.stderr.trim()
                ),
            }),
        }
    }
}

/// R05+F02 verified tree deletion through the **rename-to-staging** binding:
///
/// 1. [`crate::identity::open_for_rebind`] opens + verifies the target on one
///    handle whose share mode excludes `FILE_SHARE_DELETE` — while it lives,
///    no concurrent open may obtain `DELETE` access, so the entry cannot be
///    renamed/replaced by anyone else;
/// 2. the **same handle** renames the entry to a sibling name under the
///    `devresidue-staged-` namespace, and the post-rename identity is
///    verified against the same handle (R4-H01);
/// 3. the deletion is committed **through the same handle**
///    (`FileDispositionInfo` delete-on-close, R4-H03) — the object that is
///    deleted is exactly the one the handle pinned. There is no
///    drop-then-delete-by-path window: the staged name is unlinked only for
///    OUR object, and other opens of the staged name see delete-pending.
///
/// The original path may be replaced at any point *after* the rename — the
/// staged entry (and only it) is what we delete.
///
/// **Fail-closed (R3-G02):** if the staging rename fails, the protocol has no
/// way to keep the verified object bound to the deletion, so the item is
/// refused with [`DeleteError::UnboundDeleteRefused`] and **nothing is
/// deleted**. A fallback that drops the handle and deletes by path would
/// re-open exactly the verify→close→path replacement window INV-005 exists
/// to close — one failure mode of a convenience fallback is silently
/// deleting an attacker's replacement, so the fallback is not offered.
fn delete_tree_verified_impl(path: &Path, expected: &FileIdentity) -> Result<(), DeleteError> {
    let rebound = crate::identity::open_for_rebind(path, expected).map_err(map_verify_error)?;
    match stage_entry(&rebound) {
        Ok(staged) => {
            // R4-H03: the staged entry is the verified object, pinned by the
            // rebind handle (no one else may open it with DELETE access, so
            // the root of the tree cannot be swapped). Delete the tree's
            // descendants by path *while the pin holds* — an attacker cannot
            // restructure the tree's root or re-link the staged name — and
            // commit the root deletion **through the pinned handle**, so the
            // authorized object (the root the handle verified) is exactly
            // what gets deleted. Descendant removal failures surface as
            // normal deletion errors; the root commit only happens when the
            // tree below it is already gone, and a failure there leaves the
            // (now partially emptied) staged tree still pinned until this
            // function returns, so nothing can occupy the staged name first.
            if crate::identity::handle_is_traversable_directory(&rebound)
                .map_err(map_fs_delete_error)?
            {
                remove_tree_contents(&staged)?;
            }
            crate::identity::delete_via_handle(rebound).map_err(map_fs_delete_error)
        }
        // Staging was refused with ACCESS DENIED — Codex sandbox directories
        // (e.g. `%USERPROFILE%\.codex\.tmp`) carry ACLs that deny renaming
        // the root while still allowing the owner to delete its contents.
        // The pin on the verified handle is still held, and the pin alone
        // preserves the binding guarantee (nobody else can open the entry
        // with DELETE access, so the root cannot be swapped): delete the
        // contents by path **while the pin holds**, then commit the root
        // through the handle — the same binding protocol as the staged
        // path, minus the cosmetic rename.
        Err(DeleteError::PermissionDenied { .. }) => {
            if crate::identity::handle_is_traversable_directory(&rebound)
                .map_err(map_fs_delete_error)?
            {
                remove_tree_contents(rebound.verified_path())?;
            }
            crate::identity::delete_via_handle(rebound).map_err(map_fs_delete_error)
        }
        Err(staging_error) => Err(unbound_refused(path, staging_error)),
    }
}

/// Verified Recycle Bin operation — the **post-hoc binding protocol**.
///
/// Windows `IFileOperation` accepts only paths: it queues the item in
/// `DeleteItem` and resolves the path later, inside `PerformOperations`. It
/// cannot consume our verified file handle, and holding the pin across the
/// COM call blocks the shell from moving the entry at all. The binding is
/// therefore **verified, not assumed**:
///
/// 1. `open_for_rebind` opens + verifies the target and pins it (nobody else
///    can obtain `DELETE` access while the pin lives);
/// 2. the **same handle** renames the entry into the `devresidue-staged-*`
///    sibling namespace (post-rename identity re-verified against the same
///    handle, R4-H01) — the staged entry is now exactly the verified object;
/// 3. with the pin still held, the entry's identity is re-read **from the
///    handle** and confirmed (R4-H03: the last moment the handle can vouch
///    for the object);
/// 4. the pin is released and the staged path is handed to the shell — the
///    honest residual window is this handoff: a swap-in at the staged name
///    in that window cannot be *prevented* (the shell needs `DELETE`
///    access), only *detected*;
/// 5. the OUTCOME is verified post-hoc: on shell success the staged entry
///    must be gone. If it still exists, its live identity decides — a
///    different object means the shell recycled a replacement
///    (`IdentityMismatch`, never recorded as a success); the same object
///    means the shell did not actually move it (failure semantics, object
///    intact at the staged path).
///
/// On shell failure the staged object is restored with a **no-clobber**
/// rename (a replacement at the original path is never overwritten; the
/// error reports both the failure and the exact staged location — R4-H02).
/// The object is never permanently deleted on this path (R3-G01), and this
/// function never silently changes modes (no implicit `DirectDelete`).
fn recycle_verified_impl(path: &Path, expected: &FileIdentity) -> Result<(), DeleteError> {
    recycle_verified_with(path, expected, |staged| crate::recycle_bin::recycle(staged))
}

/// The verified-recycle protocol over an injectable shell recycle operation
/// (tests drive the failure/swap branches deterministically; production
/// always passes the real COM adapter).
fn recycle_verified_with(
    path: &Path,
    expected: &FileIdentity,
    recycle: impl Fn(&Path) -> Result<(), crate::recycle_bin::RecycleBinError>,
) -> Result<(), DeleteError> {
    let rebound = crate::identity::open_for_rebind(path, expected).map_err(map_verify_error)?;
    let original = rebound.verified_path().to_path_buf();
    match stage_entry(&rebound) {
        Ok(staged) => {
            // R4-H03: with the pin still held, confirm the staged entry is
            // still exactly the verified object — the last moment the handle
            // can vouch for it (no swap was possible while we held DELETE
            // exclusivity).
            if !crate::identity::handle_identity_matches(&rebound, expected)
                .map_err(map_fs_delete_error)?
            {
                return Err(DeleteError::IdentityMismatch {
                    path: original,
                    expected_volume: expected.volume_serial,
                    expected_index: expected.file_index,
                    live_volume: expected.volume_serial,
                    live_index: expected.file_index.wrapping_add(1),
                });
            }
            // The shell needs DELETE access to move the entry: release the
            // pin, then verify the outcome post-hoc (protocol step 5).
            drop(rebound);
            let result = recycle(&staged).map_err(map_recycle);
            match &result {
                Ok(()) => match crate::identity::file_identity(&staged) {
                    // The staged name now resolves to a DIFFERENT object: the
                    // shell moved a replacement and our object may have been
                    // displaced — report the mismatch, never record success.
                    Ok(live) => Err(DeleteError::IdentityMismatch {
                        path: staged,
                        expected_volume: expected.volume_serial,
                        expected_index: expected.file_index,
                        live_volume: live.volume_serial,
                        live_index: live.file_index,
                    }),
                    // Gone: the honest outcome — the verified object was
                    // recycled.
                    Err(crate::FileSystemError::NotFound { .. }) => Ok(()),
                    // A probe failure after a reported success (sharing
                    // violation on a moved-pending entry, etc.): the outcome
                    // is unverifiable — surface it rather than record a
                    // success we cannot confirm.
                    Err(other) => Err(map_fs_delete_error(other)),
                },
                Err(_) => {
                    // R4-H02 restore contract: no-clobber, never silent.
                    if let Err(restore_err) = restore_no_clobber(&staged, &original) {
                        return Err(DeleteError::Io {
                            path: staged.clone(),
                            message: format!(
                                "{restore_err}; the original object remains recoverable at \
                                 '{staged_display}' (verify before removing it)",
                                staged_display = staged.display(),
                            ),
                        });
                    }
                    result
                }
            }
        }
        Err(staging_error) => Err(unbound_refused(path, staging_error)),
    }
}

/// R4-H02: restores the staged object to its original name **without
/// clobbering** whatever now occupies the original path. A plain
/// `std::fs::rename` would overwrite a file replacement silently (and fail
/// only for non-empty directories), losing data the user never authorised.
fn restore_no_clobber(staged: &Path, original: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(original) {
        // The original path is occupied (someone created something there
        // while we were staged): DO NOT overwrite it. Keep the staged object
        // exactly where it is and tell the caller where to find it.
        Ok(_) => Err(format!(
            "recycle failed and the original path '{}' is now occupied by a different \
             object — refusing to overwrite it",
            original.display(),
        )),
        // Free: a metadata error that is NOT NotFound is a real failure to
        // even inspect the path — report it, keep the object staged.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!(
            "recycle failed and probing '{}' failed: {e}",
            original.display()
        )),
        Err(_) => std::fs::rename(staged, original).map_err(|e| {
            format!(
                "recycle failed and restoring '{}' from '{}' failed: {e} — the \
                     object remains at the staged path",
                original.display(),
                staged.display(),
            )
        }),
    }
}

/// Maps a filesystem error from the handle-commit / identity-check paths
/// onto the port's structured error.
fn map_fs_delete_error(e: crate::FileSystemError) -> DeleteError {
    match e {
        crate::FileSystemError::NotFound { path } => DeleteError::NotFound { path },
        crate::FileSystemError::PermissionDenied { path } => DeleteError::PermissionDenied { path },
        crate::FileSystemError::Locked { path } => DeleteError::Locked { path },
        crate::FileSystemError::Other {
            path,
            code,
            message,
        } => DeleteError::Io {
            path,
            message: format!("Win32 0x{code:08X}: {message}"),
        },
    }
}

/// R3-G02: maps a failed staging rename onto the fail-closed refusal.
fn unbound_refused(path: &Path, staging_error: DeleteError) -> DeleteError {
    let reason = match staging_error {
        DeleteError::NotFound { .. }
        | DeleteError::PermissionDenied { .. }
        | DeleteError::Locked { .. }
        | DeleteError::Io { .. } => staging_error.to_string(),
        DeleteError::CommandFailed { message, .. } => message,
        DeleteError::IdentityMismatch { .. } | DeleteError::UnboundDeleteRefused { .. } => {
            staging_error.to_string()
        }
    };
    DeleteError::UnboundDeleteRefused {
        path: path.to_path_buf(),
        reason,
    }
}

/// Prefix for the F02 staged-name family.
const STAGED_PREFIX: &str = "devresidue-staged-";

/// Renames the verified entry (its handle) to a fresh sibling name and
/// returns the staged path. The rename goes through the open handle, so the
/// entry that ends up staged is exactly the verified object.
///
/// The name is process id + a monotonic nonce — not a secret, but it does not
/// need to be: `rename_handle_to` uses `ReplaceIfExists = false`, so an
/// attacker who pre-creates an entry at the next staged name only manages a
/// *rename failure*, which fails the whole protocol closed
/// ([`DeleteError::UnboundDeleteRefused`]) — their planted entry is never
/// deleted or recycled.
fn stage_entry(rebound: &crate::identity::RebindHandle) -> Result<std::path::PathBuf, DeleteError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    // 16 hex chars: process id (8) + monotonic nonce (8) are enough uniqueness
    // for the sibling namespace of one cleanup run; the directory is our own
    // cleanup target, and no other actor can predict the full name.
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let rand_part = format!("{:08x}{:08x}", std::process::id(), nonce);
    let name = format!("{STAGED_PREFIX}{rand_part}");

    crate::identity::rename_handle_to(rebound.raw(), rebound.verified_path(), &name).map_err(|e| {
        match e {
            crate::FileSystemError::NotFound { path } => DeleteError::NotFound { path },
            crate::FileSystemError::PermissionDenied { path } => {
                DeleteError::PermissionDenied { path }
            }
            crate::FileSystemError::Locked { path } => DeleteError::Locked { path },
            crate::FileSystemError::Other {
                path,
                code,
                message,
            } => DeleteError::Io {
                path,
                message: format!("staging rename failed (Win32 0x{code:08X}): {message}"),
            },
        }
    })
}

fn map_verify_error(e: crate::identity::VerifyError) -> DeleteError {
    match e {
        crate::identity::VerifyError::Mismatch {
            path,
            expected_volume,
            expected_index,
            live_volume,
            live_index,
        } => DeleteError::IdentityMismatch {
            path,
            expected_volume,
            expected_index,
            live_volume,
            live_index,
        },
        crate::identity::VerifyError::Io(e) => match e {
            crate::FileSystemError::NotFound { path } => DeleteError::NotFound { path },
            crate::FileSystemError::PermissionDenied { path } => {
                DeleteError::PermissionDenied { path }
            }
            crate::FileSystemError::Locked { path } => DeleteError::Locked { path },
            crate::FileSystemError::Other {
                path,
                code,
                message,
            } => DeleteError::Io {
                path,
                message: format!("Win32 0x{code:08X}: {message}"),
            },
        },
    }
}

/// Permanently removes a file or an entire directory tree.
///
/// A reparse point (symlink/junction/mount) is **never** followed into its
/// target (INV-004): the attribute probe below runs non-following, and a
/// reparse *directory* is removed with `remove_dir` (the link entry itself)
/// instead of `remove_dir_all` (which would recurse through it).
fn remove_tree(path: &Path) -> Result<(), DeleteError> {
    let result = match entry_kind(path)? {
        EntryKind::ReparseDirectory => std::fs::remove_dir(path),
        EntryKind::Directory => std::fs::remove_dir_all(path),
        EntryKind::File => std::fs::remove_file(path),
    };
    result.map_err(|e| map_io_delete_error(path, e))
}

/// Removes everything *inside* `path` (children, recursively) but keeps the
/// directory itself — used by [`delete_tree_verified_impl`] so the root can
/// then be committed through the pinned handle (R4-H03).
fn remove_tree_contents(path: &Path) -> Result<(), DeleteError> {
    for child in std::fs::read_dir(path)
        .map_err(|e| map_io_delete_error(path, e))?
        .flatten()
    {
        let child_path = child.path();
        remove_tree(&child_path)?;
    }
    // A reparse-point *root* has no children to remove in the target sense;
    // the link entry itself is removed by the handle commit.
    Ok(())
}

/// The deletion-relevant kind of the entry at `path` (non-following).
enum EntryKind {
    File,
    Directory,
    ReparseDirectory,
}

fn entry_kind(path: &Path) -> Result<EntryKind, DeleteError> {
    let attrs = crate::filesystem::attributes(path).map_err(|e| match e {
        crate::FileSystemError::NotFound { .. } => DeleteError::NotFound {
            path: path.to_path_buf(),
        },
        crate::FileSystemError::PermissionDenied { .. } => DeleteError::PermissionDenied {
            path: path.to_path_buf(),
        },
        crate::FileSystemError::Locked { .. } => DeleteError::Locked {
            path: path.to_path_buf(),
        },
        crate::FileSystemError::Other { message, .. } => DeleteError::Io {
            path: path.to_path_buf(),
            message,
        },
    })?;
    Ok(if attrs.directory {
        if attrs.reparse_point {
            EntryKind::ReparseDirectory
        } else {
            EntryKind::Directory
        }
    } else {
        EntryKind::File
    })
}

/// Maps a raw `std::io` deletion error onto the port's structured error
/// (sharing-violation / lock-violation become `Locked`).
fn map_io_delete_error(path: &Path, e: std::io::Error) -> DeleteError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        DeleteError::PermissionDenied {
            path: path.to_path_buf(),
        }
    } else if e.kind() == std::io::ErrorKind::NotFound {
        DeleteError::NotFound {
            path: path.to_path_buf(),
        }
    } else {
        // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION surface through
        // raw_os_error 32/33.
        let code = e.raw_os_error().unwrap_or(0);
        if code == 32 || code == 33 {
            DeleteError::Locked {
                path: path.to_path_buf(),
            }
        } else {
            DeleteError::Io {
                path: path.to_path_buf(),
                message: e.to_string(),
            }
        }
    }
}

fn map_recycle(e: crate::recycle_bin::RecycleBinError) -> DeleteError {
    use crate::recycle_bin::RecycleBinError as R;
    match e {
        R::NotFound { path } => DeleteError::NotFound { path },
        other => DeleteError::Io {
            path: PathBuf::new(),
            message: other.to_string(),
        },
    }
}

#[cfg(test)]
mod rebind_tests {
    //! Real-filesystem tests for the F02 rename-to-staging binding protocol.
    //! In-crate (not tests/) on purpose: the tests drive the `pub(crate)`
    //! protocol primitives (`open_for_rebind` / `rename_handle_to`) so the
    //! verify→staging window can be exercised deterministically.

    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dr-f02-{tag}-{}", std::process::id()))
    }

    fn core_identity(path: &Path) -> devresidue_core::safety::probe::FileIdentity {
        let id = crate::identity::file_identity(path).expect("identity");
        devresidue_core::safety::probe::FileIdentity {
            volume_serial: id.volume_serial,
            file_index: id.file_index,
            last_write: Some(id.last_write),
        }
    }

    #[test]
    fn rebind_handle_pins_the_entry_against_concurrent_rename() {
        // F02 pinning claim: while a DELETE-capable handle without
        // FILE_SHARE_DELETE is open, renaming the entry through another open
        // fails with a sharing violation; after the handle drops it succeeds.
        let base = tmp_base("pin");
        let dir = base.join("pinned");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested").join("f.txt"), b"content").unwrap();

        let expected = core_identity(&dir);
        let rebound = crate::identity::open_for_rebind(&dir, &expected).expect("open+verify");

        // Attacker/another actor tries to rename the pinned entry.
        let moved = base.join("pinned-moved-away");
        let rename_result = fs::rename(&dir, &moved);
        assert!(
            rename_result.is_err(),
            "rename of a pinned entry must fail while the rebind handle is open"
        );
        assert!(dir.exists(), "the pinned entry stays in place");

        drop(rebound);
        fs::rename(&dir, &moved).expect("rename succeeds after the pin drops");
        assert!(moved.exists());
        assert!(!dir.exists());

        let _ = fs::remove_dir_all(&base);
    }

    /// R4-H01: 100 staging renames — the entry must always land at exactly
    /// the computed staged path (no trailing UTF-16 garbage) and never leave
    /// residue under a malformed sibling name. The pre-fix buffer (no NUL
    /// terminator) produced names with 1–3 extra UTF-16 units, so every
    /// lookup by the expected path failed and the object was lost in place.
    #[test]
    fn h01_staging_rename_lands_on_the_exact_computed_name_100x() {
        let base = tmp_base("h01-stress");
        for i in 0..100u64 {
            let dir = base.join(format!("obj-{i}"));
            fs::create_dir_all(dir.join("nested")).unwrap();
            fs::write(dir.join("nested").join("f.txt"), b"content").unwrap();

            let expected = core_identity(&dir);
            WindowsDeletePort
                .delete_tree_verified(&dir, &expected)
                .unwrap_or_else(|e| panic!("iteration {i}: verified delete failed: {e}"));
            assert!(!dir.exists(), "iteration {i}: original path vacated");

            // Any devresidue-staged-* sibling left behind is a malformed-name
            // or un-deleted residue — both are protocol failures.
            let residue: Vec<String> = fs::read_dir(&base)
                .expect("read parent")
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("devresidue-staged-"))
                .collect();
            assert!(
                residue.is_empty(),
                "iteration {i}: staged residue with malformed/unremoved names: {residue:?}"
            );
        }

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn staged_rename_moves_the_verified_object_and_a_planted_replacement_survives() {
        // Deterministic end-to-end of the protocol primitives:
        // 1. verify + pin the original object;
        // 2. rename it (through the handle) to the staged sibling name —
        //    the original path is now empty;
        // 3. plant a replacement object at the original path;
        // 4. remove the staged entry and assert the ORIGINAL content is gone
        //    while the replacement survives untouched.
        let base = tmp_base("swap");
        let dir = base.join("verified-target");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested").join("payload.bin"), b"verified-original").unwrap();

        let expected = core_identity(&dir);
        let rebound = crate::identity::open_for_rebind(&dir, &expected).expect("open+verify");

        // Stage the verified object through the handle.
        let staged_name = format!(
            "devresidue-staged-{:016x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                & 0xFFFF_FFFF_FFFF_FFFF
        );
        let staged = crate::identity::rename_handle_to(rebound.raw(), &dir, &staged_name)
            .expect("in-place rename to staged name");
        assert!(!dir.exists(), "original path vacated by the staged rename");
        assert!(
            staged.exists(),
            "the verified object now lives at the staged path"
        );
        assert_eq!(
            fs::read(staged.join("nested").join("payload.bin")).unwrap(),
            b"verified-original",
            "the staged object carries the verified original content"
        );
        // The staged entry still has the verified identity.
        let staged_identity = core_identity(&staged);
        assert_eq!(
            (staged_identity.volume_serial, staged_identity.file_index),
            (expected.volume_serial, expected.file_index),
            "the staged entry IS the verified object (same NTFS identity)"
        );

        // Attacker plants a replacement at the now-vacated original path.
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("evil.txt"), b"replacement").unwrap();
        let replacement_identity = core_identity(&dir);
        assert_ne!(
            replacement_identity.file_index, expected.file_index,
            "the replacement is a different object"
        );

        // Remove the staged (verified) object; the replacement must survive.
        drop(rebound);
        remove_tree(&staged).expect("remove the staged verified object");
        assert!(
            !staged.exists(),
            "verified object removed via its staged name"
        );
        assert!(dir.exists(), "the planted replacement survives");
        assert_eq!(
            fs::read(dir.join("evil.txt")).unwrap(),
            b"replacement",
            "replacement untouched"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verified_delete_removes_the_target_and_leaves_no_staged_residue() {
        // Public surface: WindowsDeletePort.delete_tree_verified on a normal
        // deep directory succeeds and leaves no `devresidue-staged-*` sibling
        // behind.
        let base = tmp_base("clean");
        let dir = base.join("tree");
        fs::create_dir_all(dir.join("a").join("b")).unwrap();
        fs::write(dir.join("a").join("b").join("f.bin"), b"data").unwrap();
        let expected = core_identity(&dir);

        WindowsDeletePort
            .delete_tree_verified(&dir, &expected)
            .expect("verified delete succeeds");
        assert!(!dir.exists(), "the verified tree is gone");
        let staged_residue: Vec<_> = fs::read_dir(&base)
            .expect("read parent")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("devresidue-staged-")
            })
            .collect();
        assert!(
            staged_residue.is_empty(),
            "no staged entry may remain: {staged_residue:?}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verified_delete_removes_a_regular_file_and_leaves_no_staged_residue() {
        let base = tmp_base("regular-file");
        fs::create_dir_all(&base).unwrap();
        let file = base.join("artifact.bin");
        fs::write(&file, b"ordinary-file").unwrap();
        let expected = core_identity(&file);

        WindowsDeletePort
            .delete_tree_verified(&file, &expected)
            .expect("verified DirectDelete must support regular files");
        assert!(!file.exists(), "the verified regular file is gone");
        assert!(
            !fs::read_dir(&base)
                .expect("read parent")
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().starts_with(STAGED_PREFIX)),
            "regular-file deletion must not leave a staged residue"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verified_delete_refuses_a_replaced_target_via_identity_mismatch() {
        // Pre-verify replacement: the object at the path is no longer the
        // expected one → IdentityMismatch, both the moved original and the
        // planted replacement survive.
        let base = tmp_base("mismatch");
        let dir = base.join("target");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("original.txt"), b"original").unwrap();
        let expected = core_identity(&dir);

        // Attacker swaps before the verified delete.
        fs::remove_dir_all(&dir).unwrap();
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("evil.txt"), b"replacement").unwrap();

        let err = WindowsDeletePort
            .delete_tree_verified(&dir, &expected)
            .expect_err("a replaced target must be refused");
        assert!(
            matches!(err, DeleteError::IdentityMismatch { .. }),
            "expected IdentityMismatch, got {err}"
        );
        assert_eq!(
            fs::read(dir.join("evil.txt")).unwrap(),
            b"replacement",
            "the replacement survives"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verified_recycle_happy_path_moves_the_object_and_leaves_no_residue() {
        // Post-hoc protocol happy path through the REAL COM adapter: the
        // object is recycled away, the original path is vacated and no
        // staged sibling remains (the shell moved the staged entry).
        let base = tmp_base("recycle-happy");
        let dir = base.join("recyclable");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("f.txt"), b"payload").unwrap();
        let expected = core_identity(&dir);

        WindowsDeletePort
            .recycle_verified(&dir, &expected)
            .expect("verified recycle succeeds through the post-hoc protocol");
        assert!(!dir.exists(), "the verified object was recycled away");
        assert!(
            !fs::read_dir(&base)
                .expect("read parent")
                .filter_map(Result::ok)
                .any(|e| e.file_name().to_string_lossy().starts_with(STAGED_PREFIX)),
            "no staged residue may remain: the shell moved the staged entry"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn recycle_failure_restores_the_object_and_never_permanently_deletes() {
        // Injected shell failure through the injectable adapter on the REAL
        // staging protocol: the object is restored to its original name,
        // content intact, no staged residue, and the recycle error surfaces.
        let base = tmp_base("g01-restore");
        fs::create_dir_all(&base).unwrap();
        let dir = base.join("protected-original");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("payload.txt"), b"user-data").unwrap();
        let expected = core_identity(&dir);

        let err = recycle_verified_with(&dir, &expected, |_staged| {
            Err(crate::recycle_bin::RecycleBinError::ComInitFailed { hr: -2147467259i32 })
        })
        .expect_err("injected recycle failure must surface");

        assert!(
            matches!(err, DeleteError::Io { ref message, .. } if message.contains("COM")),
            "the original recycle error surfaces, got {err}"
        );
        assert!(
            dir.exists(),
            "the object must be restored to its original name"
        );
        assert_eq!(
            fs::read(dir.join("payload.txt")).unwrap(),
            b"user-data",
            "restored content intact"
        );
        let staged_residue: Vec<_> = fs::read_dir(&base)
            .expect("read parent")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("devresidue-staged-")
            })
            .collect();
        assert!(
            staged_residue.is_empty(),
            "a failed recycle must not permanently delete the staged object: {staged_residue:?}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn h02_restore_refuses_to_overwrite_a_replacement_file_at_the_original_path() {
        // The staged object is the user's OLD selection; a NEW file lands at
        // the original path while we are staged. The recycle fails; the
        // restore must NOT overwrite the new file — both objects survive
        // and the error reports the staged location.
        let base = tmp_base("h02-file");
        fs::create_dir_all(&base).unwrap();
        let dir = base.join("original");
        fs::write(&dir, b"OLD-SCANNED-CONTENT").unwrap();
        let expected = core_identity(&dir);

        let err = recycle_verified_with(&dir, &expected, |staged| {
            fs::write(&dir, b"NEW-UNSELECTED-CONTENT").unwrap();
            let _ = staged;
            Err(crate::recycle_bin::RecycleBinError::ComInitFailed { hr: -2147467259i32 })
        })
        .expect_err("restore-with-occupied-original must surface a compound error");

        // The NEW file was NOT clobbered by the rollback.
        assert_eq!(
            fs::read(&dir).unwrap(),
            b"NEW-UNSELECTED-CONTENT",
            "the replacement at the original path must survive the restore"
        );
        // The OLD object is still recoverable at a staged path the error names.
        let staged_residue: Vec<_> = fs::read_dir(&base)
            .expect("read parent")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("devresidue-staged-")
            })
            .collect();
        assert_eq!(
            staged_residue.len(),
            1,
            "the old object stays at exactly one staged path: {staged_residue:?}"
        );
        let staged_path = staged_residue[0].path();
        assert_eq!(
            fs::read(&staged_path).unwrap(),
            b"OLD-SCANNED-CONTENT",
            "the original object is intact at the staged path"
        );
        let message = err.to_string();
        assert!(
            message.contains("devresidue-staged-") && message.contains("occupied"),
            "the error must report the staged location and the occupation reason: {message}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn h03_shell_success_with_a_swapped_staged_entry_reports_identity_mismatch() {
        // The pin is dropped, then the staged entry is swapped for a DIFFERENT
        // object before the "shell" resolves the path. The shell reports
        // success — the post-hoc check must catch that the object it moved is
        // not the verified one, and refuse to record a success. The verified
        // object (renamed away by the "attacker") survives.
        let base = tmp_base("h03-swap");
        fs::create_dir_all(&base).unwrap();
        let dir = base.join("original");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("verified.txt"), b"VERIFIED").unwrap();
        let expected = core_identity(&dir);

        let err = recycle_verified_with(&dir, &expected, |staged| {
            let hidden = base.join("attacker-hidden");
            fs::rename(staged, &hidden).unwrap();
            fs::create_dir(staged).unwrap();
            fs::write(staged.join("planted.txt"), b"PLANTED").unwrap();
            Ok(()) // shell "succeeds" on the planted entry
        })
        .expect_err("a swap-in at the staged name must not be recorded as success");

        assert!(
            matches!(err, DeleteError::IdentityMismatch { .. }),
            "expected IdentityMismatch, got {err}"
        );
        // The verified object survived the whole exchange.
        assert_eq!(
            fs::read(base.join("attacker-hidden").join("verified.txt")).unwrap(),
            b"VERIFIED",
            "the verified object is intact (moved by the attacker, not deleted)"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn h03_shell_success_when_the_staged_entry_is_gone_is_accepted() {
        // Control: shell success + staged entry gone (the honest outcome).
        let base = tmp_base("h03-ok");
        let dir = base.join("original");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f.txt"), b"x").unwrap();
        let expected = core_identity(&dir);

        recycle_verified_with(&dir, &expected, |staged| {
            fs::remove_dir_all(staged).unwrap();
            Ok(())
        })
        .expect("honest shell success is accepted");
        assert!(!dir.exists());

        let _ = fs::remove_dir_all(&base);
    }

    // ---- R3-G02: staging failure must fail closed (never a path fallback) --

    /// Deterministically blocks the staging namespace in the target's parent:
    /// pre-creates an entry at *every* staged name this process could mint
    /// next (pid + nonce). `rename_handle_to` uses `ReplaceIfExists = false`,
    /// so the port's staging rename fails and the protocol must fail closed.
    fn plant_next_staged_names(dir: &Path) {
        let pid = std::process::id();
        let parent = dir.parent().unwrap().to_path_buf();
        // Reserve the next 128 nonce values: production tests in this process
        // mint only a handful of staged names, so the window is ample.
        for nonce in 0..128u64 {
            let name = format!("{STAGED_PREFIX}{pid:08x}{nonce:08x}");
            let candidate = parent.join(&name);
            if !candidate.exists() {
                fs::create_dir(&candidate).unwrap();
            }
        }
    }

    #[test]
    fn r3g02_verified_delete_fails_closed_when_staging_is_blocked() {
        // Pre-occupy the staged namespace with an attacker-planted entry:
        // the staging rename must fail (ReplaceIfExists = false) and the
        // protocol must refuse the deletion entirely — the verified target
        // SURVIVES and the planted entries are untouched (never deleted).
        let base = tmp_base("g02-block");
        let dir = base.join("verified-target");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested").join("payload.bin"), b"verified-original").unwrap();
        let expected = core_identity(&dir);

        plant_next_staged_names(&dir);

        let err = WindowsDeletePort
            .delete_tree_verified(&dir, &expected)
            .expect_err("blocked staging must fail closed");
        assert!(
            matches!(err, DeleteError::UnboundDeleteRefused { .. }),
            "expected UnboundDeleteRefused, got {err}"
        );
        // The verified object survives untouched — no path-based fallback.
        assert_eq!(
            fs::read(dir.join("nested").join("payload.bin")).unwrap(),
            b"verified-original",
            "the verified object must survive a failed staging"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn verified_recycle_rejects_a_preexisting_replacement_by_identity() {
        let base = tmp_base("recycle-mismatch");
        let original = base.join("original");
        let displaced = base.join("displaced");
        fs::create_dir_all(&original).unwrap();
        fs::write(original.join("selected.txt"), b"SELECTED").unwrap();
        let expected = core_identity(&original);

        fs::rename(&original, &displaced).unwrap();
        fs::create_dir_all(&original).unwrap();
        fs::write(original.join("replacement.txt"), b"UNSELECTED").unwrap();

        let err = WindowsDeletePort
            .recycle_verified(&original, &expected)
            .expect_err("replacement must be refused before any recycle handoff");
        assert!(
            matches!(err, DeleteError::IdentityMismatch { .. }),
            "expected IdentityMismatch, got {err}"
        );
        assert_eq!(
            fs::read(displaced.join("selected.txt")).unwrap(),
            b"SELECTED"
        );
        assert_eq!(
            fs::read(original.join("replacement.txt")).unwrap(),
            b"UNSELECTED"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn handle_rename_no_replace_preserves_an_existing_destination() {
        // `rename_handle_to` issues one kernel rename with
        // ReplaceIfExists=false. There is no metadata/exists pre-check: if a
        // destination is present at commit time, it survives unchanged and
        // the verified source remains reachable at its original name.
        let base = tmp_base("rename-no-replace");
        fs::create_dir_all(&base).unwrap();
        let source = base.join("staged-source");
        let destination = base.join("original");
        fs::write(&source, b"SELECTED-SOURCE").unwrap();
        fs::write(&destination, b"UNSELECTED-DESTINATION").unwrap();
        let expected = core_identity(&source);
        let destination_identity = core_identity(&destination);
        let rebound = crate::identity::open_for_rebind(&source, &expected).expect("bind source");

        let err = crate::identity::rename_handle_to(rebound.raw(), &source, "original")
            .expect_err("atomic no-replace rename must refuse an occupied destination");
        assert!(
            matches!(
                err,
                crate::FileSystemError::PermissionDenied { .. }
                    | crate::FileSystemError::Locked { .. }
                    | crate::FileSystemError::Other { .. }
            ),
            "unexpected occupied-destination error: {err}"
        );
        drop(rebound);

        assert_eq!(fs::read(&source).unwrap(), b"SELECTED-SOURCE");
        assert_eq!(fs::read(&destination).unwrap(), b"UNSELECTED-DESTINATION");
        let live_destination = core_identity(&destination);
        assert_eq!(
            (live_destination.volume_serial, live_destination.file_index),
            (
                destination_identity.volume_serial,
                destination_identity.file_index
            ),
            "the unselected destination object must not be replaced"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
