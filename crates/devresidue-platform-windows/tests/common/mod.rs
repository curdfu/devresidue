//! Shared fixtures for integration tests (`tests/`). Never shipped.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// A uniquely named temporary directory that removes itself on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a fresh empty directory under the OS temp root.
    pub fn new() -> Self {
        let unique = format!(
            "devresidue-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        );
        let mut path = std::env::temp_dir();
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A child path inside the fixture directory.
    pub fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Default for TempDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Creates a directory junction `link` → `target` via `cmd /c mklink /J`.
///
/// Test scaffolding only — product code never creates links. Fails with a
/// message when the caller lacks the privilege to create links.
pub fn create_junction(link: &Path, target: &Path) -> Result<(), String> {
    let out = Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .map_err(|e| format!("failed to run mklink: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "mklink /J failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Creates a directory symlink. Fails when developer mode / the
/// SeCreateSymbolicLinkPrivilege is unavailable.
pub fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

/// True when the `io::Error` is ERROR_PRIVILEGE_NOT_HELD (1314) — the classic
/// "symlink creation requires developer mode / elevated privileges" failure.
pub fn is_privilege_error(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(1314)
}
