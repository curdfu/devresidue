//! Running-process detection for the Process Guard (SPEC §12 / INV-012).
//!
//! Enumerates processes via `Toolhelp32Snapshot` (same mechanism Task Manager
//! uses). This layer only reports what it observes — deciding to skip or defer
//! a target because a tool is running belongs to the Phase 5 safety layer.
//! Errors are surfaced honestly so the caller can fail closed on "unknown".

use std::mem::size_of;

use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

use crate::ffi::OwnedHandle;

/// One running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Executable file name as reported by the OS, e.g. `opencode.exe`.
    pub name: String,
    /// Process id.
    pub pid: u32,
}

/// Win32 `ERROR_NO_MORE_FILES` — signals end of the snapshot enumeration.
const ERROR_NO_MORE_FILES: u32 = 18;

/// Returns the full list of running processes (name + pid).
///
/// Never silently degrades: a failed snapshot is reported as an error so the
/// Phase 5 Process Guard can fail closed (SPEC §12: unknown ≠ not running).
pub fn running_processes() -> Result<Vec<ProcessInfo>, String> {
    let snapshot = take_snapshot()?;
    let mut out = Vec::new();
    // `dwSize` must be set before every enumeration call; the value never
    // changes, so setting it once in the initializer is sufficient.
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };

    let mut first = true;
    loop {
        // SAFETY: `entry.dwSize` is set before each call as required by the
        // API; `entry` is a valid, zeroed struct large enough for a process
        // entry; `snapshot` is our own open snapshot handle kept alive for
        // the whole enumeration.
        #[allow(unsafe_code)]
        let step = unsafe {
            if first {
                Process32FirstW(snapshot.raw(), &mut entry)
            } else {
                Process32NextW(snapshot.raw(), &mut entry)
            }
        };
        first = false;

        match step {
            Ok(()) => {
                let name = read_exe_name(&entry);
                out.push(ProcessInfo {
                    name,
                    pid: entry.th32ProcessID,
                });
            }
            Err(e) => {
                let code = (e.code().0 as u32) & 0xFFFF;
                if code == ERROR_NO_MORE_FILES {
                    break;
                }
                return Err(format!(
                    "process snapshot enumeration failed (Win32 0x{code:08X}): {}",
                    e.message()
                ));
            }
        }
    }
    Ok(out)
}

fn read_exe_name(entry: &PROCESSENTRY32W) -> String {
    let len = entry
        .szExeFile
        .iter()
        .position(|&u| u == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..len])
}

fn take_snapshot() -> Result<OwnedHandle, String> {
    // SAFETY: TH32CS_SNAPPROCESS only snapshots the process list; no data is
    // written anywhere. A null/invalid handle is turned into a Win32 error by
    // the `windows` wrapper.
    #[allow(unsafe_code)]
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.map_err(|e| {
        format!(
            "CreateToolhelp32Snapshot failed (Win32 0x{:08X}): {}",
            (e.code().0 as u32) & 0xFFFF,
            e.message()
        )
    })?;
    Ok(OwnedHandle::from_raw(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Case-insensitive name comparison form used by the tests below
    /// (mirrors what production callers do on `ProcessInfo.name`).
    fn normalize_name(name: &str) -> String {
        let lower = name.to_lowercase();
        lower
            .strip_suffix(".exe")
            .map_or_else(|| lower.clone(), str::to_owned)
    }

    /// Whether `name` is running according to a fresh snapshot.
    ///
    /// Test-only stand-in for the removed `is_running` convenience wrapper.
    /// Unlike the old boolean API it is **tri-state**: `Ok(true/false)` when
    /// the snapshot succeeded, `Err` when the process list could not be read
    /// (INV-012: unknown ≠ not running — a caller must fail closed on `Err`).
    fn is_running_tri_state(name: &str) -> Result<bool, String> {
        let needle = normalize_name(name);
        Ok(running_processes()?
            .iter()
            .any(|p| normalize_name(&p.name) == needle))
    }

    #[test]
    fn name_normalisation_strips_exe_and_lowercases() {
        assert_eq!(normalize_name("OpenCode.EXE"), "opencode");
        assert_eq!(normalize_name("opencode"), "opencode");
        assert_eq!(normalize_name("ping.exe"), "ping");
    }

    #[test]
    fn snapshot_contains_current_process() {
        let processes = running_processes().expect("process snapshot must succeed");
        let exe = std::env::current_exe()
            .expect("current exe path")
            .file_name()
            .expect("current exe name")
            .to_string_lossy()
            .into_owned();
        assert!(
            processes.iter().any(|p| p.name.eq_ignore_ascii_case(&exe)),
            "expected {} among running processes; got {processes:?}",
            exe
        );
        // The tri-state helper agrees (its own process is obviously running),
        // and is explicit about the snapshot having succeeded.
        assert_eq!(is_running_tri_state(&exe), Ok(true));
        // Extension-less name also matches.
        assert_eq!(is_running_tri_state(exe.trim_end_matches(".exe")), Ok(true));
    }

    #[test]
    fn unknown_process_not_running() {
        // A successful snapshot that simply does not contain the name is the
        // honest "not running" answer (distinct from an Err = unknown).
        assert_eq!(
            is_running_tri_state("devresidue-definitely-not-a-real-process-xyz"),
            Ok(false)
        );
    }
}
