//! Shared scaffolding for the Phase 15-B real-filesystem matrix tests
//! (SPEC §33 Filesystem). New helpers only — the pre-existing `tests/common`
//! module is shared and intentionally left untouched.
//!
//! Everything here is test-only scaffolding: privilege probes, attribute
//! helpers and cleanup guards. No product code is exercised.

#![allow(dead_code)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Nul-terminated UTF-16 encoding of a path (Win32 call parameter).
pub fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Sets (or clears) the read-only attribute of a file **or** directory with
/// the real Win32 attribute API (`SetFileAttributesW`).
pub fn set_readonly(path: &Path, on: bool) -> std::io::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        SetFileAttributesW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    };
    let flags = if on {
        FILE_ATTRIBUTE_READONLY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let wide = to_wide(path);
    // SAFETY: `wide` is a nul-terminated buffer owned by this frame; the flags
    // are plain Win32 constants and SetFileAttributesW writes nothing into
    // user memory. Mirrors the existing `filesystem_attributes.rs` helper.
    #[allow(unsafe_code)]
    unsafe { SetFileAttributesW(PCWSTR(wide.as_ptr()), flags) }
        .map_err(|e| std::io::Error::from_raw_os_error((e.code().0) & 0xFFFF))
}

/// A file held open with **no sharing** (`share_mode(0)`), which makes every
/// other open of the same file fail with `ERROR_SHARING_VIOLATION` — the
/// classic "locked" attack surface.
pub struct ExclusiveLock {
    _file: std::fs::File,
}

/// Opens `path` for exclusive access (`dwShareMode = 0`).
pub fn lock_exclusive(path: &Path) -> std::io::Result<ExclusiveLock> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0) // deny read/write/delete sharing: fully exclusive.
        .open(path)?;
    Ok(ExclusiveLock { _file: file })
}

/// Resolves a path the *current* user is guaranteed to be denied access to:
/// `%SystemDrive%\System Volume Information` carries an ACL that rejects even
/// administrators unless they explicitly take backup/restore privileges.
///
/// This is a real, unmodified system ACL — no `icacls` mutation, hence no
/// cleanup hazard (see the Phase 15-B report for why the deny-ACE + restore
/// approach was rejected). Returns `Err(reason)` when the environment cannot
/// provide such a target (the caller then skips visibly).
pub fn permission_denied_system_target() -> Result<PathBuf, String> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let root = PathBuf::from(format!("{drive}\\"));
    let target = root.join("System Volume Information");
    match std::fs::metadata(&target) {
        Ok(_) => Err(format!(
            "the current process can read '{}' (elevated with backup \
             privileges?) — cannot use it as a permission-denied target",
            target.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(target),
        Err(e) => Err(format!(
            "'{}' is not a usable permission-denied target: {e}",
            target.display()
        )),
    }
}

/// Guards a deep (>260 char) directory tree so it is always removed through
/// the `\\?\` verbatim form (plain `remove_dir_all` would silently fail and
/// leak the tree when the plain path exceeds MAX_PATH).
pub struct DeepTree {
    /// Extended `\\?\` form of the tree root.
    pub extended: PathBuf,
    /// The original plain path (what product APIs normally receive).
    pub plain: PathBuf,
}

/// Creates `depth` nested directories (each named `component`) under `base`,
/// then `leaf_files` empty files in the deepest directory. The full path is
/// deliberately longer than `MAX_PATH`.
pub fn create_deep_tree(base: &Path, depth: usize, component: &str, leaf_files: usize) -> DeepTree {
    let mut plain = base.to_path_buf();
    for _ in 0..depth {
        plain.push(component);
    }
    let extended = to_extended(&plain);
    std::fs::create_dir_all(&extended).expect("create deep tree (verbatim)");
    for i in 0..leaf_files {
        std::fs::write(extended.join(format!("leaf-{i}.bin")), b"x").expect("write deep leaf");
    }
    assert!(
        plain.to_string_lossy().len() > 260,
        "fixture must exceed MAX_PATH ({} chars)",
        plain.to_string_lossy().len()
    );
    DeepTree { extended, plain }
}

impl Drop for DeepTree {
    fn drop(&mut self) {
        // Verbatim remove always works for our own tree.
        let _ = std::fs::remove_dir_all(&self.extended);
    }
}

/// `\\?\`-prefixes an absolute path (lexical; mirrors the platform layer's
/// `path::to_extended` but stays independent for scaffolding).
fn to_extended(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.starts_with("\\\\?\\") {
        return path.to_path_buf();
    }
    PathBuf::from(format!("\\\\?\\{text}"))
}

/// Reports whether the UNC admin share `\\127.0.0.1\c$` is reachable with the
/// current identity (requires an elevated/administrator context).
pub fn unc_admin_share_base() -> Result<PathBuf, String> {
    let share_root = PathBuf::from(r"\\127.0.0.1\c$");
    match std::fs::metadata(&share_root) {
        Ok(_) => Ok(share_root),
        Err(e) => Err(format!(
            "UNC admin share '{}' is not accessible (needs an elevated \
             context): {e}",
            share_root.display()
        )),
    }
}

/// Maps the local path onto the admin share (`C:\x` → `\\127.0.0.1\c$\x`).
/// `None` when the path is not on the `C:` volume.
pub fn map_to_unc_share(local: &Path) -> Option<PathBuf> {
    let text = local.to_string_lossy();
    let (drive, rest) = text.split_at(2);
    if !drive.eq_ignore_ascii_case("C:") {
        return None;
    }
    Some(PathBuf::from(format!("\\\\127.0.0.1\\c${rest}")))
}

/// Bounded execution guard: a test that must finish within a budget fails
/// with a clear message instead of hanging the suite.
pub fn run_bounded<T>(budget: Duration, what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    assert!(
        elapsed <= budget,
        "{what} exceeded the {budget:?} budget (took {elapsed:?})"
    );
    out
}
