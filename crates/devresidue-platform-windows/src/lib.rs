//! DevResidue Windows platform layer.
//!
//! Provides **capabilities only** — no business classification, no deletion
//! orchestration (PLAN Phase 4):
//!
//! ```text
//! path/         normalization, containment, long-path/UNC handling (pure lexical)
//! filesystem/   file attributes + reparse-point probe (symlink/junction/mount)
//! identity/     NTFS file identity (volume serial + file index + last write)
//! process/      running-process enumeration for the Process Guard (SPEC §12)
//! recycle_bin/  Windows Recycle Bin capability (used only by the Phase 6
//!               CleanupEngine — the single deletion authority)
//! shell/        structured external command execution (SPEC §22)
//! ```
//!
//! Hard constraints honoured here:
//!
//! - Core never depends on this crate (the dependency points the other way).
//! - Pure capability: this layer never decides *what* is cleanable and never
//!   performs a cleanup by itself. Reparse points are probed, never followed
//!   by default (SPEC §17 / INV-004).
//! - `unsafe` is `deny` crate-wide (lints) and allowed only at the narrow FFI
//!   boundary, each call site carrying a `// SAFETY:` comment.
//!
//! Dependency note: Win32 bindings come from the [`windows`] crate v0.62
//! (windows-rs) with an explicit, minimal feature list in `Cargo.toml`.

pub mod cleanup;
pub mod env_key;
pub mod filesystem;
pub mod identity;
pub mod path;
pub mod process;
pub mod recycle_bin;
pub mod safety;
pub mod shell;
pub mod user_rule_tx;

mod ffi;

/// Windows atomic file/lock port for `devresidue_ai::AiProfileStore`.
///
/// Kept in the platform crate so the AI crate does not depend on Windows APIs.
pub mod profile_file {
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use devresidue_core::ai::{AiProfileFilePort, ProfileFileLock};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        MOVE_FILE_FLAGS, REPLACEFILE_WRITE_THROUGH, REPLACE_FILE_FLAGS,
    };
    use windows::Win32::System::IO::OVERLAPPED;

    /// Handle-safe Windows profile file port.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct WindowsAiProfileFilePort;

    impl WindowsAiProfileFilePort {
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    struct ProfileLock {
        _file: File,
    }

    impl ProfileFileLock for ProfileLock {}

    impl AiProfileFilePort for WindowsAiProfileFilePort {
        fn acquire_exclusive(&self, lock_path: &Path) -> Result<Box<dyn ProfileFileLock>, String> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)
                .map_err(|_| "unable to open AI profile write lock".to_string())?;
            let mut overlapped = OVERLAPPED::default();
            // SAFETY: the file is open read/write, the handle remains alive in
            // the returned guard, and `overlapped` is a zeroed synchronous lock
            // request whose range covers the lock file.
            #[allow(unsafe_code)]
            let result = unsafe {
                LockFileEx(
                    HANDLE(file.as_raw_handle() as *mut _),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    None,
                    u32::MAX,
                    u32::MAX,
                    &mut overlapped,
                )
            };
            result.map_err(|_| "AI profile write lock is already held".to_string())?;
            Ok(Box::new(ProfileLock { _file: file }))
        }

        fn atomic_replace(&self, temporary: &Path, target: &Path) -> Result<(), String> {
            let temporary_wide = path_wide(temporary)?;
            let target_wide = path_wide(target)?;
            if target.exists() {
                // SAFETY: both paths are owned NUL-terminated UTF-16 buffers;
                // the replacement was fully flushed by the AI profile store.
                replace_with_move_fallback(
                    || {
                        #[allow(unsafe_code)]
                        unsafe {
                            ReplaceFileW(
                                PCWSTR(target_wide.as_ptr()),
                                PCWSTR(temporary_wide.as_ptr()),
                                None,
                                REPLACE_FILE_FLAGS(REPLACEFILE_WRITE_THROUGH.0),
                                None,
                                None,
                            )
                        }
                    },
                    || {
                        // ReplaceFileW may require WRITE_DAC while preserving
                        // attributes and ACLs. Retry the same-volume, flushed
                        // replacement when that preservation step is unavailable.
                        #[allow(unsafe_code)]
                        unsafe {
                            MoveFileExW(
                                PCWSTR(temporary_wide.as_ptr()),
                                PCWSTR(target_wide.as_ptr()),
                                MOVE_FILE_FLAGS(
                                    MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0,
                                ),
                            )
                        }
                    },
                )
                .map_err(|_| "unable to atomically replace AI profile file".to_string())
            } else {
                // SAFETY: both paths are owned NUL-terminated UTF-16 buffers;
                // the target is intentionally not overwritten in this branch.
                #[allow(unsafe_code)]
                unsafe {
                    MoveFileExW(
                        PCWSTR(temporary_wide.as_ptr()),
                        PCWSTR(target_wide.as_ptr()),
                        MOVE_FILE_FLAGS(MOVEFILE_WRITE_THROUGH.0),
                    )
                }
                .map_err(|_| "unable to atomically publish AI profile file".to_string())
            }
        }
    }

    fn replace_with_move_fallback<E>(
        replace: impl FnOnce() -> Result<(), E>,
        move_replace: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        replace().or_else(|_| move_replace())
    }

    fn path_wide(path: &Path) -> Result<Vec<u16>, String> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        if wide[..wide.len().saturating_sub(1)].contains(&0) {
            return Err("AI profile path contains an embedded NUL".to_string());
        }
        Ok(wide)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use devresidue_core::ai::AiProfileFilePort;
        use std::cell::RefCell;
        use std::fs;
        use std::path::PathBuf;
        use uuid::Uuid;

        struct TempDir(PathBuf);

        impl TempDir {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "devresidue-task4-profile-port-{}-{}",
                    std::process::id(),
                    Uuid::new_v4()
                ));
                fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn replace_file_and_move_file_use_flush_safe_port_without_temp_residue() {
            let dir = TempDir::new();
            let port = WindowsAiProfileFilePort::new();
            let existing = dir.0.join("ai-profiles.json");
            let existing_temp = dir.0.join(".existing.tmp");
            fs::write(&existing, b"old").unwrap();
            fs::write(&existing_temp, b"new").unwrap();
            port.atomic_replace(&existing_temp, &existing).unwrap();
            assert_eq!(fs::read(&existing).unwrap(), b"new");
            assert!(!existing_temp.exists());

            let absent = dir.0.join("new-target.json");
            let absent_temp = dir.0.join(".absent.tmp");
            fs::write(&absent_temp, b"first").unwrap();
            port.atomic_replace(&absent_temp, &absent).unwrap();
            assert_eq!(fs::read(&absent).unwrap(), b"first");
            assert!(!absent_temp.exists());
        }

        #[test]
        fn failed_primary_replacement_uses_move_fallback() {
            let attempts = RefCell::new(Vec::new());

            let result = replace_with_move_fallback(
                || {
                    attempts.borrow_mut().push("replace");
                    Err::<(), _>("ReplaceFileW failed")
                },
                || {
                    attempts.borrow_mut().push("move");
                    Ok::<(), &str>(())
                },
            );

            assert_eq!(result, Ok(()));
            assert_eq!(attempts.into_inner(), ["replace", "move"]);
        }

        #[test]
        fn profile_lock_is_exclusive_until_guard_drop() {
            let dir = TempDir::new();
            let lock = dir.0.join("ai-profiles.lock");
            let port = WindowsAiProfileFilePort::new();
            let first = port.acquire_exclusive(&lock).unwrap();
            assert!(port.acquire_exclusive(&lock).is_err());
            drop(first);
            assert!(port.acquire_exclusive(&lock).is_ok());
        }
    }
}

pub use ffi::FileSystemError;
