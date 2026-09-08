//! Shared fixtures for provider integration tests (`tests/`). Never shipped.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devresidue_providers::scan_ctx::{EnvMap, ScanContext, ToolQuery};

/// A uniquely named temporary directory that removes itself on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let unique = format!(
            "devresidue-prov-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let mut path = std::env::temp_dir();
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

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

/// Creates a directory junction via `cmd /c mklink /J` (test scaffolding).
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

/// A fake tool that answers cache-dir queries from a canned map.
pub struct FakeTool {
    answers: HashMap<String, Result<String, String>>,
}

impl FakeTool {
    pub fn new(answers: HashMap<String, Result<String, String>>) -> Self {
        Self { answers }
    }

    pub fn answering(exe: &str, out: &str) -> Self {
        let mut m = HashMap::new();
        m.insert(exe.to_string(), Ok(out.to_string()));
        Self::new(m)
    }
}

impl ToolQuery for FakeTool {
    fn run(&self, executable: &str, _args: &[&str], _timeout: Duration) -> Result<String, String> {
        match self.answers.get(executable) {
            Some(result) => result.clone(),
            None => Err(format!("no canned answer for {executable}")),
        }
    }
}

/// A tool that always fails (falls back to known defaults).
pub struct FailingTool;

impl ToolQuery for FailingTool {
    fn run(&self, executable: &str, _args: &[&str], _timeout: Duration) -> Result<String, String> {
        Err(format!("tool '{executable}' failed (test)"))
    }
}

/// Default fake env: a stable sample profile.
pub fn sample_env() -> EnvMap {
    let mut m = HashMap::new();
    m.insert("USERPROFILE".into(), r"C:\Users\alice".into());
    m.insert(
        "LOCALAPPDATA".into(),
        r"C:\Users\alice\AppData\Local".into(),
    );
    m.insert("APPDATA".into(), r"C:\Users\alice\AppData\Roaming".into());
    m.insert("SYSTEMROOT".into(), r"C:\Windows".into());
    m.insert("PROGRAMFILES".into(), r"C:\Program Files".into());
    m.insert("PROGRAMFILES(X86)".into(), r"C:\Program Files (x86)".into());
    m.insert("PROGRAMDATA".into(), r"C:\ProgramData".into());
    m
}

/// Builds a context with a canned tool and roots.
pub fn ctx(
    env: EnvMap,
    tool: Box<dyn ToolQuery>,
    roots: Vec<PathBuf>,
    continue_scan: bool,
) -> ScanContext {
    ScanContext::with_env(env, tool, roots, Box::new(move || continue_scan))
}

/// Builds a context with a canned tool and roots (cancellable from outside).
pub fn cancellable_ctx(env: EnvMap, roots: Vec<PathBuf>) -> (ScanContext, SharedFlag) {
    let flag = SharedFlag::new(true);
    let c = flag.clone();
    let ctx = ScanContext::with_env(env, Box::new(FailingTool), roots, Box::new(move || c.get()));
    (ctx, flag)
}

/// A shared boolean (cancellation probe).
#[derive(Clone)]
pub struct SharedFlag {
    value: std::sync::Arc<Mutex<bool>>,
}

impl SharedFlag {
    pub fn new(initial: bool) -> Self {
        Self {
            value: std::sync::Arc::new(Mutex::new(initial)),
        }
    }

    pub fn set(&self, v: bool) {
        *self.value.lock().unwrap() = v;
    }

    pub fn get(&self) -> bool {
        *self.value.lock().unwrap()
    }
}

/// Tiny helper to write a marker + nested content.
pub fn make_dir_with_files(base: &Path, files: &[&str]) {
    for file in files {
        let p = base.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
    }
}
