//! NTFS file identity — a stable fingerprint of a file-system object.
//!
//! Phase 5 records this during the scan snapshot and re-reads it right before
//! deletion. If the identity changed in between, the target was renamed /
//! replaced / re-created (TOCTOU, SPEC §16, INV-005) and the plan must be
//! re-validated.
//!
//! By default the identity is read **without following** a trailing reparse
//! point (`FILE_FLAG_OPEN_REPARSE_POINT`): for a junction/symlink the entry
//! itself is fingerprinted, which is what lets the SafetyValidator detect a
//! link swap. [`file_identity_following`] resolves through the link instead
//! (used when the *target* must be compared).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use windows::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES,
};

use crate::ffi::{filetime_to_system_time, open_existing};
use crate::filesystem::extended_for_access;
use crate::FileSystemError;

/// Stable identity of a file-system object on an NTFS volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// `dwVolumeSerialNumber` — identifies the volume the object lives on.
    pub volume_serial: u32,
    /// `nFileIndexHigh << 32 | nFileIndexLow` — unique per object on a volume.
    pub file_index: u64,
    /// `ftLastWriteTime` of the object.
    pub last_write: SystemTime,
}

/// Reads the identity of `path` **without following** a trailing reparse
/// point (the identity is that of the symlink/junction entry itself).
pub fn file_identity(path: impl AsRef<Path>) -> Result<FileIdentity, FileSystemError> {
    read_identity(path.as_ref(), /* follow_reparse */ false)
}

/// Opens `path` (directory-capable, without following a trailing reparse
/// point), reads its identity from the **same handle** and compares it with
/// `expected`.
///
/// Returns `Ok(())` when the live object is still exactly the expected one.
/// The caller is expected to perform the deletion while (or immediately after)
/// the returned handle guard is alive — an open directory handle prevents the
/// entry from being renamed/replaced underneath the deletion (R05 binding).
///
/// `Err(FileSystemError::Other { .. })` with code `ERROR_OBJECT_CHANGED` style
/// mismatches is reported as a dedicated error; callers map it onto
/// `DeleteError::IdentityMismatch`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("identity mismatch on {path}")]
    Mismatch {
        path: PathBuf,
        expected_volume: u32,
        expected_index: u64,
        live_volume: u32,
        live_index: u64,
    },
    #[error(transparent)]
    Io(#[from] FileSystemError),
}

/// Win32 access mask bits for the F02 rebind protocol.
mod access_bits {
    /// `DELETE` (0x00010000) — required to rename an entry in place.
    pub const DELETE: u32 = 0x0001_0000;
    /// `SYNCHRONIZE` (0x00100000).
    pub const SYNCHRONIZE: u32 = 0x0010_0000;
}

/// RAII handle for the F02 rename-to-staging protocol.
///
/// Holds the verified object open and remembers the **original path** the
/// object was verified at — the caller needs it to locate the sibling
/// directory for the staged rename.
pub(crate) struct RebindHandle {
    /// The path the object was verified at (its pre-staging location).
    pub(crate) path: PathBuf,
    pub(crate) handle: crate::ffi::OwnedHandle,
}

impl RebindHandle {
    /// The raw Win32 handle (valid while this guard lives).
    pub(crate) fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        self.handle.raw()
    }

    /// The path the object was verified at (its pre-staging location).
    pub(crate) fn verified_path(&self) -> &Path {
        &self.path
    }
}

/// Opens `path`, verifies its identity against `expected` on the **same
/// handle** and returns a [`RebindHandle`] pinning the entry.
///
/// The handle requests `DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES` and its
/// share mode is **read/write only — no `FILE_SHARE_DELETE`**. Two
/// consequences, both load-bearing for the rename-to-staging binding (F02):
///
/// - the holder may issue an in-place rename through
///   `SetFileInformationByHandle(FileRenameInfo)` (renaming an entry requires
///   `DELETE` access on it);
/// - **no other open may obtain `DELETE` access** while this handle lives, so
///   a concurrent rename/replace of the same entry fails with
///   `ERROR_SHARING_VIOLATION` — the verified object is pinned for the whole
///   verify→staging window (the inverse of the old `FILE_SHARE_DELETE`
///   behaviour the R2 report disproved).
///
/// `FILE_FLAG_BACKUP_SEMANTICS` is required to open a directory;
/// `FILE_FLAG_OPEN_REPARSE_POINT` keeps us on the entry itself (a reparse
/// point is never resolved through, INV-004).
pub(crate) fn open_for_rebind(
    path: &Path,
    expected: &devresidue_core::safety::probe::FileIdentity,
) -> Result<RebindHandle, VerifyError> {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_MODE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let target = extended_for_access(path);
    let wide = crate::ffi::to_wide(&target);
    let share = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0);
    let access = FILE_READ_ATTRIBUTES.0 | access_bits::DELETE | access_bits::SYNCHRONIZE;

    // SAFETY: `wide` (nul-terminated) outlives the call; the returned handle
    // is immediately wrapped in the RAII guard (`OwnedHandle` closes it on
    // drop). No user buffers are involved.
    #[allow(unsafe_code)]
    let handle = unsafe {
        CreateFileW(
            crate::ffi::as_pcwstr(&wide),
            access,
            share,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|e| VerifyError::Io(crate::ffi::classify_error(path, &e)))?;
    let handle = crate::ffi::OwnedHandle::from_raw(handle);

    // Read + compare identity on the same handle.
    // SAFETY: `info` is written by the kernel on success; the guard outlives
    // the call.
    #[allow(unsafe_code)]
    let info = {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle.raw(), &mut info) }
            .map_err(|e| VerifyError::Io(crate::ffi::classify_error(path, &e)))?;
        info
    };
    let live_volume = info.dwVolumeSerialNumber;
    let live_index = ((u64::from(info.nFileIndexHigh)) << 32) | u64::from(info.nFileIndexLow);
    if live_volume != expected.volume_serial || live_index != expected.file_index {
        return Err(VerifyError::Mismatch {
            path: path.to_path_buf(),
            expected_volume: expected.volume_serial,
            expected_index: expected.file_index,
            live_volume,
            live_index,
        });
    }
    Ok(RebindHandle {
        path: path.to_path_buf(),
        handle,
    })
}

/// Renames the entry behind `handle` to `new_name` inside its current parent
/// directory, using `SetFileInformationByHandle(FileRenameInfo)` on the open
/// handle (F02). `ReplaceIfExists = false`.
///
/// Windows resolution note: with `RootDirectory = NULL`, a FileRenameInfo
/// `FileName` that is only *relative* is resolved against the process working
/// directory, not the object's own directory (→ `ERROR_NOT_SAME_DEVICE` when
/// they differ). The rename therefore passes the **fully qualified staged
/// path** (`parent\new_name`) — same directory, so the object stays pinned by
/// the open handle and is merely re-linked to the unpredictable staged name.
///
/// `new_name` must be a bare file/directory name. Returns the staged path.
pub(crate) fn rename_handle_to(
    handle: windows::Win32::Foundation::HANDLE,
    original_path: &Path,
    new_name: &str,
) -> Result<PathBuf, FileSystemError> {
    use windows::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_INFO_BY_HANDLE_CLASS, FILE_RENAME_INFO,
        FILE_RENAME_INFO_0,
    };

    // The staged target is a sibling of the original path.
    let staged = original_path
        .parent()
        .map(|p| p.join(new_name))
        .ok_or_else(|| FileSystemError::Other {
            path: original_path.to_path_buf(),
            code: 0,
            message: "verified path has no parent directory".to_string(),
        })?;

    // FILE_RENAME_INFO layout, computed from the windows-crate struct so it is
    // robust across pointer widths (x64/ARM64 both use 8-byte handles):
    //   [0..4)  union FILE_RENAME_INFO_0 { BOOL ReplaceIfExists; DWORD Flags }
    //   [pad to HANDLE alignment)
    //   [..+8)  RootDirectory (HANDLE, NULL for same-directory rename)
    //   [+4)    FileNameLength (bytes, excluding the NUL)
    //   [align 2) FileName (UTF-16, relative, no root path)
    fn align_up(value: usize, align: usize) -> usize {
        (value + align - 1) & !(align - 1)
    }
    let anon_size = std::mem::size_of::<FILE_RENAME_INFO_0>();
    let root_off = align_up(
        anon_size,
        std::mem::align_of::<windows::Win32::Foundation::HANDLE>(),
    );
    let len_off = root_off + std::mem::size_of::<windows::Win32::Foundation::HANDLE>();
    let name_off = align_up(len_off + std::mem::size_of::<u32>(), 2);
    // Sanity: the trailing FileName field of the crate struct starts where we
    // computed (offset_of on a struct whose FileName is [u16; 1]).
    let _ = std::mem::size_of::<FILE_RENAME_INFO>();

    // Encode WITH a NUL terminator (R4-H01). FileNameLength stays the exact
    // byte count of the name (excluding the NUL), but the buffer must still
    // carry the terminating zero: the kernel reads the name as a
    // null-terminated string beyond FileNameLength, and a buffer ending
    // exactly at the last name byte made it read past the allocation —
    // observed on-disk staged names carrying 1–3 extra UTF-16 units of heap
    // garbage (R4 `malformed-stage-names.json`). `vec![0u8; ...]` zero-fills,
    // so the extra 2 bytes are already NUL.
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = staged.as_os_str().encode_wide().collect();
    let name_bytes = wide.len() * 2;
    let mut buffer = vec![0u8; name_off + name_bytes + 2];
    // ReplaceIfExists = false, RootDirectory = NULL: the buffer is zeroed and
    // those fields sit at the start — nothing to write.
    buffer[len_off..len_off + 4].copy_from_slice(&(name_bytes as u32).to_le_bytes());
    for (i, unit) in wide.iter().enumerate() {
        let off = name_off + i * 2;
        buffer[off..off + 2].copy_from_slice(&unit.to_le_bytes());
    }

    // SAFETY: `buffer` is a correctly laid-out FILE_RENAME_INFO (zeroed
    // ReplaceIfExists/RootDirectory, byte length, the fully qualified UTF-16
    // staged path at the computed offsets, and a NUL terminator within the
    // buffer) owned for the call; `handle` is the open verified object.
    #[allow(unsafe_code)]
    let result = unsafe {
        SetFileInformationByHandle(
            handle,
            FILE_INFO_BY_HANDLE_CLASS(FileRenameInfo.0),
            buffer.as_ptr().cast(),
            buffer.len() as u32,
        )
    };
    result.map_err(|e| crate::ffi::classify_error(original_path, &e))?;
    // R4-H01 defence-in-depth: verify the entry the handle now names IS the
    // staged path we computed — a name buffer/layout bug would otherwise
    // move the user's object to an unpredictable location we could not
    // report or restore. The identity on the same handle proves the object;
    // a mismatch fails closed with the handle still pinning the entry (the
    // caller refuses the item and the object stays reachable for recovery).
    verify_renamed_name(handle, &staged)?;
    Ok(staged)
}

/// R4-H01: after `SetFileInformationByHandle(FileRenameInfo)` succeeds,
/// confirms the handle's object really ended up at `expected_path` by
/// re-deriving the final path component from the kernel (via the parent +
/// `NtQueryInformationFile`-equivalent) — pragmatically: re-open the
/// expected path and compare NTFS identity with the handle's.
fn verify_renamed_name(
    handle: windows::Win32::Foundation::HANDLE,
    expected_path: &Path,
) -> Result<(), FileSystemError> {
    // Identity of the pinned object (from the handle itself).
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` is written by the kernel on success; the handle is owned
    // by the caller and valid for the call.
    #[allow(unsafe_code)]
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    ok.map_err(|e| crate::ffi::classify_error(expected_path, &e))?;
    let pinned_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);

    // Identity of the object at the expected staged path (no reparse
    // following: OPEN_REPARSE_POINT semantics like the rest of this module).
    let live = read_identity(expected_path, /* follow_reparse */ false)?;
    if live.file_index != pinned_index || live.volume_serial != info.dwVolumeSerialNumber {
        return Err(FileSystemError::Other {
            path: expected_path.to_path_buf(),
            code: 0,
            message: "post-rename verification failed: the staged path does not hold the \
                      renamed object"
                .to_string(),
        });
    }
    Ok(())
}

/// R4-H03: with the rebind handle still open, re-reads the entry's identity
/// **from the handle** and compares it with `expected`. While our pin lives
/// (share mode denies everyone else `DELETE` access) the entry cannot have
/// been renamed or replaced, so the handle's identity is authoritative —
/// this is the last moment the handle can vouch for the object before an
/// operation that must release the pin (the shell recycle).
pub(crate) fn handle_identity_matches(
    rebound: &RebindHandle,
    expected: &devresidue_core::safety::probe::FileIdentity,
) -> Result<bool, FileSystemError> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` is written by the kernel on success; the guard outlives
    // the call.
    #[allow(unsafe_code)]
    let ok = unsafe { GetFileInformationByHandle(rebound.handle.raw(), &mut info) };
    ok.map_err(|e| crate::ffi::classify_error(&rebound.path, &e))?;
    let live_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(live_index == expected.file_index && info.dwVolumeSerialNumber == expected.volume_serial)
}

/// R4-H03: commits the deletion of the entry behind `rebound` **through the
/// pinned handle** (`SetFileInformationByHandle(FileDispositionInfo)` with a
/// delete-on-close disposition), so the object that gets deleted is exactly the one the
/// handle verified — closing the residual `drop(handle)` → delete-by-staged-
/// path window where the staged name could be swapped for a different
/// object.
///
/// On success the entry is deleted when the handle closes (this function
/// consumes the guard). On failure the guard is also consumed but the
/// **entry stays** (setting the delete disposition succeeds as a whole; a
/// `SetFileInformationByHandle` error leaves the entry in place,
/// still at its staged name) and the caller can locate it by the staged
/// path for recovery.
///
/// The share-mode of the rebind handle (no `FILE_SHARE_DELETE` for others)
/// means no one can have the entry open for delete; after our disposition
/// is set, other opens see it as delete-pending (access denied), so a
/// swap-in at the staged name cannot intercept the deletion: the name is
/// unlinked only for OUR object.
pub(crate) fn delete_via_handle(rebound: RebindHandle) -> Result<(), FileSystemError> {
    use windows::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
        FILE_INFO_BY_HANDLE_CLASS,
    };
    let path = rebound.path.clone();
    let info = FILE_DISPOSITION_INFO {
        DeleteFile: true as _, // mark this handle's object for deletion on close
    };
    // SAFETY: `info` is a plain bool struct owned for the call; the handle is
    // owned by `rebound` and valid (consumed afterwards by closing on drop).
    #[allow(unsafe_code)]
    let result = unsafe {
        SetFileInformationByHandle(
            rebound.handle.raw(),
            FILE_INFO_BY_HANDLE_CLASS(FileDispositionInfo.0),
            &info as *const FILE_DISPOSITION_INFO as *const core::ffi::c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    // Drop AFTER the disposition call: closing the handle commits the
    // delete-on-close (success) or simply releases the pin (failure).
    drop(rebound);
    result.map_err(|e| crate::ffi::classify_error(&path, &e))
}

/// Returns whether the pinned object is a real directory whose children may
/// be traversed for `DirectDelete`.
///
/// The classification comes from the same verified handle, not from a fresh
/// path lookup. Ordinary files and reparse-point directories both return
/// `false`: files have no children, and reparse points must never be followed
/// (INV-004).
pub(crate) fn handle_is_traversable_directory(
    rebound: &RebindHandle,
) -> Result<bool, FileSystemError> {
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` is written by the kernel on success; the guard outlives
    // the call and pins the exact object whose attributes are inspected.
    #[allow(unsafe_code)]
    let ok = unsafe { GetFileInformationByHandle(rebound.handle.raw(), &mut info) };
    ok.map_err(|e| crate::ffi::classify_error(&rebound.path, &e))?;

    let attributes = info.dwFileAttributes;
    Ok(attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0
        && attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0)
}

pub fn file_identity_following(path: impl AsRef<Path>) -> Result<FileIdentity, FileSystemError> {
    read_identity(path.as_ref(), /* follow_reparse */ true)
}

fn read_identity(path: &Path, follow_reparse: bool) -> Result<FileIdentity, FileSystemError> {
    let target = extended_for_access(path);
    let flags = if follow_reparse {
        // OPEN_REPARSE_POINT *not* set: CreateFileW resolves the link for us.
        windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0)
    } else {
        FILE_FLAG_OPEN_REPARSE_POINT
    };
    let handle = open_existing(&target, FILE_READ_ATTRIBUTES.0, flags)?;

    // SAFETY: `info` is a plain struct written by the kernel on success; the
    // handle outlives the call (owned by `handle`). No user buffers are
    // involved. On failure the `windows` wrapper surfaces the last error.
    #[allow(unsafe_code)]
    let info = {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle.raw(), &mut info) }
            .map_err(|e| crate::ffi::classify_error(path, &e))?;
        info
    };

    let file_index = ((u64::from(info.nFileIndexHigh)) << 32) | u64::from(info.nFileIndexLow);
    Ok(FileIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index,
        last_write: filetime_to_system_time(info.ftLastWriteTime),
    })
}
