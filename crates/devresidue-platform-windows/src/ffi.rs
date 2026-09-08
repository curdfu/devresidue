//! Shared, low-level FFI plumbing for the platform layer.
//!
//! Everything in this module is crate-internal. It centralises the three
//! repetitive, easy-to-get-wrong pieces of the Win32 boundary:
//!
//! 1. `u16` (UTF-16, nul-terminated) path encoding,
//! 2. opening existing file-system objects with explicit flags while mapping
//!    the raw Win32 error to a structured [`FileSystemError`],
//! 3. RAII handle ownership (`OwnedHandle`), `FILETIME` -> [`SystemTime`].

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
    FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// Structured error produced whenever this layer opens / inspects a path.
///
/// The three coarse buckets (not-found / access-denied / locked) are what the
/// Phase 5 SafetyValidator needs for fail-closed decisions; anything else is
/// surfaced verbatim (Win32 code + message) so no information is swallowed.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FileSystemError {
    #[error("path does not exist: {path}")]
    NotFound { path: PathBuf },
    #[error("access denied for path: {path}")]
    PermissionDenied { path: PathBuf },
    #[error("path is locked or in use: {path}")]
    Locked { path: PathBuf },
    #[error("Win32 error 0x{code:08X} ({message}) on path: {path}")]
    Other {
        path: PathBuf,
        code: u32,
        message: String,
    },
}

/// Nul-terminated UTF-16 encoding of a path (for `*PWSTR` parameters).
pub(crate) fn to_wide(path: impl AsRef<Path>) -> Vec<u16> {
    path.as_ref()
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Borrows the wide buffer as a `PCWSTR`. The caller keeps `wide` alive for
/// the duration of the Win32 call.
pub(crate) fn as_pcwstr(wide: &[u16]) -> PCWSTR {
    PCWSTR(wide.as_ptr())
}

/// Maps a `windows`-crate error (already carrying the thread's last error)
/// onto the structured [`FileSystemError`] buckets.
pub(crate) fn classify_error(
    path: impl Into<PathBuf>,
    err: &windows::core::Error,
) -> FileSystemError {
    // `windows` reports Win32 errors as HRESULT_FROM_WIN32(code); the low
    // 16 bits are the original GetLastError() value.
    let code = (err.code().0 as u32) & 0xFFFF;
    let path = path.into();
    match code {
        // FILE_NOT_FOUND, PATH_NOT_FOUND, INVALID_NAME, BAD_PATHNAME
        2 | 3 | 123 | 161 => FileSystemError::NotFound { path },
        // ERROR_ACCESS_DENIED
        5 => FileSystemError::PermissionDenied { path },
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
        32 | 33 => FileSystemError::Locked { path },
        _ => FileSystemError::Other {
            path,
            code,
            message: err.message().to_string(),
        },
    }
}

/// RAII wrapper over a raw Win32 `HANDLE`.
///
/// `windows`-crate handles must be closed explicitly; this guard guarantees
/// it on every code path.
#[derive(Debug)]
pub(crate) struct OwnedHandle(HANDLE);

impl OwnedHandle {
    /// Wraps an already-open handle.
    pub(crate) fn from_raw(handle: HANDLE) -> Self {
        Self(handle)
    }

    /// Raw handle for Win32 calls (valid only while the guard is alive).
    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `CloseHandle` is called exactly once per handle (this guard
        // owns it exclusively) and never again afterwards — the guard is not
        // `Clone`/`Copy` and `raw()` borrows it. `CloseHandle` on an invalid
        // handle returns an error that we ignore, matching Win32 semantics.
        #[allow(unsafe_code)]
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Opens an existing file or directory with `FILE_FLAG_BACKUP_SEMANTICS`
/// (required to open directories) plus any caller-supplied flags such as
/// `FILE_FLAG_OPEN_REPARSE_POINT`.
///
/// `desired_access` is a raw `u32` Win32 access mask (e.g.
/// `FILE_READ_ATTRIBUTES.0`); the share mode allows read/write/delete so that
/// concurrent readers of the same tree do not spuriously block us.
pub(crate) fn open_existing(
    path: &Path,
    desired_access: u32,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
) -> Result<OwnedHandle, FileSystemError> {
    let wide = to_wide(path);
    let share = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);
    // SAFETY: `wide` outlives the call (a nul-terminated buffer is kept alive
    // on the stack), no other argument points into memory we do not own, and
    // the returned handle is immediately wrapped in `OwnedHandle`. The
    // `windows` wrapper turns INVALID_HANDLE_VALUE into the thread's last
    // Win32 error for us.
    #[allow(unsafe_code)]
    let handle = unsafe {
        CreateFileW(
            as_pcwstr(&wide),
            desired_access,
            share,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | flags,
            None,
        )
    }
    .map_err(|e| classify_error(path, &e))?;
    Ok(OwnedHandle::from_raw(handle))
}

/// Windows `FILETIME` (100 ns ticks since 1601-01-01 UTC) -> [`SystemTime`].
pub(crate) fn filetime_to_system_time(ft: FILETIME) -> SystemTime {
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let raw = ((u64::from(ft.dwHighDateTime)) << 32) | u64::from(ft.dwLowDateTime);
    if raw >= UNIX_EPOCH_AS_FILETIME {
        UNIX_EPOCH + Duration::from_nanos((raw - UNIX_EPOCH_AS_FILETIME) * 100)
    } else {
        UNIX_EPOCH - Duration::from_nanos((UNIX_EPOCH_AS_FILETIME - raw) * 100)
    }
}
