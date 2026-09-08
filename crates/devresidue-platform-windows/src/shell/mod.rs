//! Structured external-command execution (SPEC §22).
//!
//! Runs a trusted provider's [`ExternalCommandSpec`] as a child process:
//!
//! - executable + argv are passed **individually** to `CreateProcessW` via
//!   `std::process::Command` — never through `cmd.exe /c` string
//!   concatenation;
//! - stdout/stderr are captured on reader threads (no pipe deadlock);
//! - an optional timeout kills the process (Windows: `TerminateProcess` via
//!   `Child::kill`) and reaps it before returning;
//! - the working directory is validated to exist first.
//!
//! This module executes cleanup commands, but only when invoked by the Phase 6
//! CleanupEngine for an approved plan — it never decides *whether* a cleanup
//! should happen.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use devresidue_core::ExternalCommandSpec;
use thiserror::Error;

/// Outcome of a completed (or timed-out) external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Process exit code. `None` when the process was killed and its exit
    /// status could not be reaped before the kill grace period elapsed.
    pub exit_code: Option<i32>,
    /// Captured stdout (lossy UTF-8; command output is byte-oriented).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8).
    pub stderr: String,
    /// True when the configured `timeout_secs` elapsed and the process had to
    /// be killed. Timed-out runs must be treated as failures by callers.
    pub timed_out: bool,
}

/// Errors that prevent the command from running at all.
#[derive(Debug, Error)]
pub enum ShellError {
    #[error("working directory does not exist: {0:?}")]
    WorkingDirectoryNotFound(PathBuf),
    #[error("failed to spawn '{executable}': {message}")]
    SpawnFailed { executable: String, message: String },
}

/// Kill grace period (seconds) granted after `kill()` before giving up on
/// reaping the child.
const KILL_REAP_GRACE: Duration = Duration::from_secs(2);
/// Poll interval while waiting for process exit.
const POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Runs `spec`, capturing output and enforcing the timeout.
pub fn run(spec: &ExternalCommandSpec) -> Result<CommandOutcome, ShellError> {
    if let Some(wd) = &spec.working_directory {
        if !wd.is_dir() {
            return Err(ShellError::WorkingDirectoryNotFound(wd.clone()));
        }
    }

    let mut cmd = Command::new(&spec.executable);
    // Windows GUI-mode guard: when DevResidue runs as a GUI app (Tauri) the
    // default creation flags would give console-less child processes (npm,
    // pip, uv, bun, ...) a brand-new console window, causing a black-window
    // flash for every tool query / external cleanup. CREATE_NO_WINDOW
    // (0x08000000) suppresses that new console. In CLI mode this flag has no
    // side effect: the parent already has a console and stdout/stderr are
    // captured through pipes, so the tool's output still reaches us.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.args(&spec.args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if let Some(wd) = &spec.working_directory {
        cmd.current_dir(wd);
    }

    // Windows: suppress the child's console window (CREATE_NO_WINDOW,
    // 0x08000000). Without it, spawning a console tool (npm/pip/uv/bun config
    // queries or cleanup commands) from the **GUI** app flashes a black console
    // window on every call. The flag is harmless under the CLI (which already
    // has a console): stdout/stderr are still captured through the pipes, so
    // output capture and all tests are unaffected.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    // CreateProcessW with per-argument argv; no shell string is ever built.
    // Windows shim resolution: `npm`, `pnpm`, ... ship as `npm.cmd` batch
    // shims, and CreateProcessW does NOT apply PATHEXT — a bare "npm" spawn
    // fails (the extensionless file is a sh script). Retry the literal name
    // first, then `<name>.cmd` / `<name>.bat`. A PATHEXT-ordered probe would
    // need a full PATH search; the two extensions cover every real tool shim
    // (npm/pnpm/yarn/pnpm.cmd, python wrappers use .exe and never land here).
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(first_error) => {
            let mut resolved = None;
            let mut last_error = first_error;
            for ext in [".cmd", ".bat"] {
                let shim = format!("{}{ext}", spec.executable);
                let mut retry = Command::new(&shim);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    retry.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                retry
                    .args(&spec.args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                if let Some(wd) = &spec.working_directory {
                    retry.current_dir(wd);
                }
                match retry.spawn() {
                    Ok(child) => {
                        resolved = Some(child);
                        break;
                    }
                    Err(e) => last_error = e,
                }
            }
            match resolved {
                Some(child) => child,
                None => {
                    return Err(ShellError::SpawnFailed {
                        executable: spec.executable.clone(),
                        message: last_error.to_string(),
                    })
                }
            }
        }
    };

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let out_reader = std::thread::spawn(move || read_to_end(stdout));
    let err_reader = std::thread::spawn(move || read_to_end(stderr));

    let deadline = spec
        .timeout_secs
        .map(|s| Instant::now() + Duration::from_secs(s));
    let mut timed_out = false;

    // Poll for exit (cheap on Windows: WaitForSingleObject under the hood),
    // kill on deadline, then reap.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    timed_out = true;
                    // SAFETY note: std::process::Child::kill() on Windows
                    // calls TerminateProcess — a hard, non-graceful kill. The
                    // child cannot ignore it; reaping afterwards prevents a
                    // zombie handle.
                    let _ = child.kill();
                    break reap_after_kill(&mut child);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                // try_wait error: attempt to reap once more before giving up.
                let _ = child.kill();
                let _ = child.wait();
                return Err(ShellError::SpawnFailed {
                    executable: spec.executable.clone(),
                    message: e.to_string(),
                });
            }
        }
    };

    // Reap the reader threads; pipes reach EOF once the process is gone.
    let stdout_bytes = out_reader
        .join()
        .unwrap_or_else(|_| String::new().into_bytes());
    let stderr_bytes = err_reader
        .join()
        .unwrap_or_else(|_| String::new().into_bytes());

    let exit_code = status.and_then(|s| s.code());
    Ok(CommandOutcome {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        timed_out,
    })
}

/// After `kill()`, polls `try_wait` until the child is reaped (bounded by the
/// kill grace period) and returns the final exit status.
fn reap_after_kill(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let give_up = Instant::now() + KILL_REAP_GRACE;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= give_up {
            // Last resort: a blocking wait must succeed once TerminateProcess
            // has been issued.
            return child.wait().ok();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn read_to_end(mut reader: impl std::io::Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    buf
}

// ---- npm .cmd shim resolution (regression) ------------------------------
// `npm` on Windows is npm.cmd; CreateProcessW does not apply PATHEXT, so the
// bare-name spawn fails and the runner must retry the .cmd/.bat shims.
#[cfg(test)]
mod shim_tests {
    use super::*;
    use devresidue_core::ExternalCommandSpec;

    #[test]
    fn cmd_shim_tools_spawn_and_answer() {
        // npm ships as npm.cmd on every Node install; if npm is absent on
        // this machine the test is skipped (dev boxes without Node).
        if which_npm().is_none() {
            return;
        }
        let spec = ExternalCommandSpec::new(
            "npm".into(),
            vec!["config".into(), "get".into(), "cache".into()],
            None,
            Some(10),
        );
        let outcome = run(&spec).expect("npm (npm.cmd) spawns through the shim retry");
        assert!(!outcome.timed_out, "npm config get cache must not time out");
        assert!(
            matches!(outcome.exit_code, Some(0)),
            "npm exits 0: {:?}",
            outcome.exit_code
        );
        assert!(
            outcome.stdout.trim().to_lowercase().contains("cache"),
            "stdout names the cache dir: {}",
            outcome.stdout
        );
    }

    fn which_npm() -> Option<std::path::PathBuf> {
        // Minimal PATH probe for npm.cmd (test-only; production resolution is
        // the spawn retry above).
        let path = std::env::var("PATH").ok()?;
        for dir in path.split(';') {
            for name in ["npm.cmd", "npm.exe"] {
                let candidate = std::path::Path::new(dir).join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::ExternalCommandSpec;

    fn spec(executable: &str, args: &[&str], timeout_secs: Option<u64>) -> ExternalCommandSpec {
        ExternalCommandSpec::new(
            executable.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
            None,
            timeout_secs,
        )
    }

    #[test]
    fn arguments_are_passed_individually() {
        // where.exe prints the resolved path of every argument.
        let outcome = run(&spec("where.exe", &["where.exe"], None)).expect("spawn");
        assert_eq!(outcome.exit_code, Some(0));
        assert!(
            outcome.stdout.to_lowercase().contains("where.exe"),
            "expected where.exe path in output, got: {}",
            outcome.stdout
        );
    }

    #[test]
    fn non_zero_exit_code_is_captured() {
        let outcome =
            run(&spec("where.exe", &["devresidue-no-such-tool-xyz"], None)).expect("spawn");
        // where.exe exits 1 when nothing matches.
        assert_eq!(outcome.exit_code, Some(1));
    }

    #[test]
    fn timeout_kills_long_running_process() {
        // ping -n 30 to the loopback would run ~29s; a 1s timeout must kill it.
        let outcome = run(&spec("ping.exe", &["-n", "30", "127.0.0.1"], Some(1))).expect("spawn");
        assert!(
            outcome.timed_out,
            "process should have timed out: {outcome:?}"
        );
        // The child must be fully reaped (not left running). The check goes
        // through `running_processes()` (Result-shaped): an Err here would be
        // "state unknown", which must fail the assertion rather than silently
        // pass (INV-012).
        let processes = crate::process::running_processes().expect("process snapshot must succeed");
        assert!(
            !processes
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case("ping.exe")
                    || p.name.eq_ignore_ascii_case("ping")),
            "ping still running: {processes:?}"
        );
    }

    #[test]
    fn missing_working_directory_is_rejected() {
        let spec = ExternalCommandSpec::new(
            "where.exe".into(),
            vec!["where.exe".into()],
            Some(PathBuf::from("C:\\devresidue\\no\\such\\dir")),
            None,
        );
        let err = run(&spec).expect_err("must fail");
        assert!(matches!(err, ShellError::WorkingDirectoryNotFound(_)));
    }

    #[test]
    fn missing_executable_is_rejected() {
        let err = run(&spec("devresidue-no-such-exe.exe", &[], None)).expect_err("must fail");
        assert!(matches!(err, ShellError::SpawnFailed { .. }));
    }
}
