//! Windows filesystem capabilities — attributes + reparse probing.
//!
//! Consumers are the Phase 5 SafetyValidator (snapshot / revalidation) and
//! measurement code. This module is pure capability: it reports what is on
//! disk, it never deletes anything and never decides what *should* be
//! cleanable.
//!
//! Reparse points are probed, never followed by default (SPEC §17, INV-004).

pub mod reparse;

use std::os::windows::fs::MetadataExt;
use std::path::Path;

use crate::path;
use crate::FileSystemError;

/// Windows file-attribute flags relevant to DevResidue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAttributes {
    /// `FILE_ATTRIBUTE_READONLY` (0x1)
    pub read_only: bool,
    /// `FILE_ATTRIBUTE_HIDDEN` (0x2)
    pub hidden: bool,
    /// `FILE_ATTRIBUTE_SYSTEM` (0x4)
    pub system: bool,
    /// `FILE_ATTRIBUTE_DIRECTORY` (0x10)
    pub directory: bool,
    /// `FILE_ATTRIBUTE_REPARSE_POINT` (0x400) — symlink/junction/mount point
    pub reparse_point: bool,
    /// `FILE_ATTRIBUTE_ARCHIVE` (0x20)
    pub archive: bool,
    /// Raw attribute mask as returned by `GetFileAttributes`.
    pub raw: u32,
}

impl FileAttributes {
    /// True when the path is a directory (per its attributes).
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.directory
    }

    /// True when the path is a reparse point of any kind (INV-004).
    #[must_use]
    pub fn is_reparse_point(&self) -> bool {
        self.reparse_point
    }
}

// Win32 FILE_ATTRIBUTE_* values (documented constants).
const ATTRIBUTE_READONLY: u32 = 0x0001;
const ATTRIBUTE_HIDDEN: u32 = 0x0002;
const ATTRIBUTE_SYSTEM: u32 = 0x0004;
const ATTRIBUTE_DIRECTORY: u32 = 0x0010;
const ATTRIBUTE_ARCHIVE: u32 = 0x0020;
const ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// Reads the file attributes of `path` **without following** a trailing
/// reparse point (uses the non-following metadata call), so a symlink /
/// junction reports its own `reparse_point` flag rather than its target's.
pub fn attributes(path: impl AsRef<Path>) -> Result<FileAttributes, FileSystemError> {
    // Pre-extend absolute paths so paths longer than MAX_PATH work; lexical
    // dot-components have already been resolved by callers that normalise
    // first (safe for canonical inputs).
    let target = extended_for_access(path.as_ref());
    let md = std::fs::symlink_metadata(&target).map_err(|e| io_to_fs_error(path.as_ref(), &e))?;
    let raw = md.file_attributes();
    Ok(FileAttributes {
        read_only: raw & ATTRIBUTE_READONLY != 0,
        hidden: raw & ATTRIBUTE_HIDDEN != 0,
        system: raw & ATTRIBUTE_SYSTEM != 0,
        directory: raw & ATTRIBUTE_DIRECTORY != 0,
        reparse_point: raw & ATTRIBUTE_REPARSE_POINT != 0,
        archive: raw & ATTRIBUTE_ARCHIVE != 0,
        raw,
    })
}

/// Maps a `std::io` error to the shared structured [`FileSystemError`].
pub(crate) fn io_to_fs_error(path: &Path, e: &std::io::Error) -> FileSystemError {
    match e.kind() {
        std::io::ErrorKind::NotFound => FileSystemError::NotFound {
            path: path.to_path_buf(),
        },
        std::io::ErrorKind::PermissionDenied => FileSystemError::PermissionDenied {
            path: path.to_path_buf(),
        },
        _ => FileSystemError::Other {
            path: path.to_path_buf(),
            code: e.raw_os_error().unwrap_or(0) as u32,
            message: e.to_string(),
        },
    }
}

/// Returns the path in extended `\\?\` form when it is absolute, so Win32 /
/// std access survives `MAX_PATH`; otherwise returns it unchanged.
pub(crate) fn extended_for_access(path: &Path) -> std::path::PathBuf {
    let ext = path::to_extended(path);
    if ext == path {
        path.to_path_buf()
    } else {
        ext
    }
}

// ---- R3-G06: cross-process whole-file lock ---------------------------------

/// Win32 implementation of the core [`FileLock`] contract: an exclusive,
/// blocking byte-range lock over the whole file, held until the guard drops
/// (dropping the guard closes the handle, which releases the range).
///
/// Used by the scan-store generation allocator to serialise
/// read-last-generation → assign → publish across processes.
#[derive(Debug)]
pub struct WindowsFileLock {
    _file: std::fs::File,
}

impl devresidue_core::safety::FileLock for WindowsFileLock {}

/// Acquires an exclusive, blocking, whole-file lock on `file`
/// (R3-G06, Win32 `LockFileEx`).
pub fn lock_file_exclusive(
    file: std::fs::File,
) -> Result<Box<dyn devresidue_core::safety::FileLock>, String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
    // Whole-file byte range: 0..=u32::MAX low and high — far beyond any lock
    // file's size, so one call excludes every other exclusive request on the
    // file. `LockFileEx` without LOCKFILE_FAIL_IMMEDIATELY blocks until the
    // range is granted.
    let mut overlapped = windows::Win32::System::IO::OVERLAPPED::default();
    // SAFETY: `file` is an open handle owned by this function (moved into
    // the returned guard); `overlapped` is a valid stack struct for the call
    // duration; the byte range is the whole file; the call blocks until the
    // exclusive range is granted.
    #[allow(unsafe_code)]
    let result = unsafe {
        LockFileEx(
            windows::Win32::Foundation::HANDLE(file.as_raw_handle() as *mut _),
            LOCKFILE_EXCLUSIVE_LOCK,
            None,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    result.map_err(|e| format!("LockFileEx failed: {e}"))?;
    // The guard owns the file; dropping it closes the handle, which releases
    // the byte range (documented Win32 handle-close semantics).
    Ok(Box::new(WindowsFileLock { _file: file }))
}

/// Attempts an exclusive whole-file lock and returns immediately when another
/// process owns it. The `busy:` prefix is consumed by the command layer and
/// mapped to the structured Busy error instead of waiting on the UI thread.
pub fn try_lock_file_exclusive(
    file: std::fs::File,
) -> Result<Box<dyn devresidue_core::safety::FileLock>, String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    let mut overlapped = windows::Win32::System::IO::OVERLAPPED::default();
    #[allow(unsafe_code)]
    let result = unsafe {
        LockFileEx(
            windows::Win32::Foundation::HANDLE(file.as_raw_handle() as *mut _),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            None,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    match result {
        Ok(()) => Ok(Box::new(WindowsFileLock { _file: file })),
        Err(error) if (error.code().0 as u32 & 0xFFFF) == 33 => {
            Err("busy: application data operation lock is already held".to_string())
        }
        Err(error) => Err(format!("LockFileEx try-lock failed: {error}")),
    }
}
