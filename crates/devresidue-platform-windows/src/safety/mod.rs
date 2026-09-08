//! Ports & adapters — the Windows implementations of the core safety probe
//! traits.
//!
//! Core (`devresidue-core::safety`) defines what the SafetyValidator needs
//! (attributes / reparse / identity / process state) and this crate provides
//! the real Win32-backed implementations. The adapters are thin translations
//! over the Phase 4 capability modules:
//!
//! ```text
//! core::safety::PathProbe   ←  WindowsPathProbe (filesystem + identity)
//! core::safety::ProcessProbe ← WindowsProcessProbe (Toolhelp snapshot)
//! ```
//!
//! They map structured [`crate::FileSystemError`]s onto core's
//! [`devresidue_core::safety::ProbeError`] so the validator can fail closed on
//! not-found/permission/lock exactly as the Phase 5 contract demands.

use std::path::Path;

use devresidue_core::safety::probe::{
    AttrFlags, FileIdentity, PathProbe, ProbeError, ProcessProbe, ProcessState, ReparseInfo,
    ReparseKind,
};

use crate::FileSystemError;

/// Real filesystem probe: attributes, reparse state and NTFS identity via the
/// Phase 4 capability modules. Zero-sized and stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPathProbe;

impl PathProbe for WindowsPathProbe {
    fn attributes(&self, path: &Path) -> Result<AttrFlags, ProbeError> {
        crate::filesystem::attributes(path)
            .map(|a| AttrFlags {
                read_only: a.read_only,
                hidden: a.hidden,
                system: a.system,
                directory: a.directory,
                reparse: a.reparse_point,
            })
            .map_err(|e| map_error(path, e))
    }

    fn reparse_info(&self, path: &Path) -> Result<ReparseInfo, ProbeError> {
        crate::filesystem::reparse::probe(path)
            .map(|info| ReparseInfo {
                kind: info.tag.map(map_kind),
                target: info.target,
            })
            .map_err(|e| map_error(path, e))
    }

    fn file_identity(&self, path: &Path) -> Result<FileIdentity, ProbeError> {
        crate::identity::file_identity(path)
            .map(|id| FileIdentity {
                volume_serial: id.volume_serial,
                file_index: id.file_index,
                last_write: Some(id.last_write),
            })
            .map_err(|e| map_error(path, e))
    }
}

fn map_kind(tag: crate::filesystem::reparse::ReparseTag) -> ReparseKind {
    match tag {
        crate::filesystem::reparse::ReparseTag::Symlink => ReparseKind::Symlink,
        crate::filesystem::reparse::ReparseTag::Junction => ReparseKind::Junction,
        crate::filesystem::reparse::ReparseTag::MountPoint => ReparseKind::MountPoint,
        crate::filesystem::reparse::ReparseTag::Other => ReparseKind::Other,
    }
}

fn map_error(path: &Path, e: FileSystemError) -> ProbeError {
    match e {
        FileSystemError::NotFound { .. } => ProbeError::NotFound {
            path: path.to_path_buf(),
        },
        FileSystemError::PermissionDenied { .. } => ProbeError::PermissionDenied {
            path: path.to_path_buf(),
        },
        FileSystemError::Locked { .. } => ProbeError::Locked {
            path: path.to_path_buf(),
        },
        FileSystemError::Other { message, .. } => ProbeError::Other {
            path: path.to_path_buf(),
            message,
        },
    }
}

/// Real process probe over the Toolhelp32 process snapshot.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsProcessProbe;

impl ProcessProbe for WindowsProcessProbe {
    fn process_state(&self, names: &[&str]) -> ProcessState {
        if names.is_empty() {
            return ProcessState::NotRunning;
        }
        let processes = match crate::process::running_processes() {
            Ok(list) => list,
            // Honest failure: unknown ≠ not running (INV-012).
            Err(_) => return ProcessState::Unknown,
        };
        for proc in &processes {
            if names
                .iter()
                .any(|wanted| exe_name_matches(&proc.name, wanted))
            {
                return ProcessState::Running;
            }
        }
        ProcessState::NotRunning
    }
}

/// Case-insensitive executable-name comparison that tolerates the `.exe`
/// suffix on either side (`"opencode"` matches `"OpenCode.EXE"`).
fn exe_name_matches(actual: &str, wanted: &str) -> bool {
    fn fold(name: &str) -> String {
        let lower = name.to_lowercase();
        match lower.strip_suffix(".exe") {
            Some(without) => without.to_string(),
            None => lower,
        }
    }
    fold(actual) == fold(wanted)
}

// ---- R3 §4: OS CSPRNG adapter (integrity-key generation) -------------------

/// Fills `buf` with cryptographically strong random bytes from the OS CSPRNG
/// (Win32 `BCryptGenRandom` with `BCRYPT_USE_SYSTEM_PREFERRED_RNG`, no
/// algorithm handle needed).
///
/// Used for the scan-store HMAC key generation (R2-F01 / R3 §4: the previous
/// pragmatic time+pid+address seeding was honestly flagged by the review as
/// "not the OS CSPRNG the doc claimed"; this is the real one).
pub fn os_random(buf: &mut [u8]) -> Result<(), String> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    // SAFETY: `buf` is a valid mutable slice for the call duration; the
    // system-preferred RNG flag needs no algorithm handle (None).
    #[allow(unsafe_code)]
    let status = unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() {
        Ok(())
    } else {
        Err(format!(
            "BCryptGenRandom failed (NTSTATUS 0x{:08X})",
            status.0 as u32
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_matching_is_case_and_suffix_insensitive() {
        assert!(exe_name_matches("OpenCode.EXE", "opencode"));
        assert!(exe_name_matches("opencode", "opencode.exe"));
        assert!(exe_name_matches("PING.EXE", "ping.exe"));
        assert!(!exe_name_matches("ping2.exe", "ping"));
    }
}
