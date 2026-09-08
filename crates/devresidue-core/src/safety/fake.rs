//! Programmable fake probes for core safety unit tests (test-only module;
//! compiled under `cfg(test)` via `mod.rs`).
//!
//! `FakeFs` is an in-memory file-system whose state a test mutates *between*
//! capture and validate to simulate TOCTOU races:
//!
//! ```text
//! capture (scan)          validate (pre-delete)
//!       |                        |
//!   state v1  ---------->    state v2   → IdentityChanged / StaleSnapshot / ...
//! ```
//!
//! Entries are keyed by the canonical, case-folded path so lookups behave like
//! the real Windows filesystem (case-insensitive). Creating a file or
//! directory materialises its ancestors automatically.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use super::canonical;
use super::probe::{AttrFlags, FileIdentity, PathProbe, ProbeError, ReparseInfo, ReparseKind};

/// One virtual object in the fake filesystem.
#[derive(Debug, Clone)]
pub struct FakeEntry {
    pub attrs: AttrFlags,
    pub reparse: Option<ReparseKind>,
    pub volume_serial: u32,
    pub file_index: u64,
    pub last_write: Option<SystemTime>,
}

/// The fake filesystem itself.
#[derive(Debug, Default)]
pub struct FakeFs {
    entries: HashMap<String, FakeEntry>,
    next_file_index: u64,
}

/// Shared, lockable fake filesystem behind a [`PathProbe`].
pub struct FakeProbe {
    fs: Mutex<FakeFs>,
}

impl FakeProbe {
    /// A fresh, empty fake filesystem.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            fs: Mutex::new(FakeFs::default()),
        })
    }

    /// Exclusive access to mutate state between probes.
    pub fn lock(&self) -> MutexGuard<'_, FakeFs> {
        self.fs.lock().expect("fake probe mutex")
    }
}

impl Default for FakeProbe {
    fn default() -> Self {
        Self {
            fs: Mutex::new(FakeFs::default()),
        }
    }
}

impl FakeFs {
    fn key(path: &Path) -> String {
        canonical::normalize(path).to_string_lossy().to_lowercase()
    }

    fn dir_attrs() -> AttrFlags {
        AttrFlags {
            directory: true,
            ..AttrFlags::default()
        }
    }

    /// A fixed reference timestamp for created entries.
    fn base_time() -> SystemTime {
        UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
    }

    /// Creates every missing ancestor of `path` as a plain directory.
    pub fn ensure_ancestors(&mut self, path: &Path) {
        let prefixes = canonical::ancestor_prefixes(path);
        for p in prefixes {
            self.create_dir(p.as_path());
        }
    }

    /// Creates a plain directory entry (and ancestors).
    pub fn create_dir(&mut self, path: &Path) {
        self.ensure_ancestors(path);
        self.insert_entry(path, Self::dir_attrs(), None, Self::base_time());
    }

    /// Creates a plain file entry (and ancestors).
    pub fn create_file(&mut self, path: &Path, last_write: Option<SystemTime>) {
        self.ensure_ancestors(path);
        self.insert_entry(
            path,
            AttrFlags {
                directory: false,
                ..AttrFlags::default()
            },
            None,
            last_write.unwrap_or_else(Self::base_time),
        );
    }

    fn insert_entry(
        &mut self,
        path: &Path,
        mut attrs: AttrFlags,
        reparse: Option<ReparseKind>,
        last_write: SystemTime,
    ) {
        if reparse.is_some() {
            attrs.reparse = true;
            if attrs.directory
                && matches!(
                    reparse,
                    Some(ReparseKind::Junction | ReparseKind::MountPoint)
                )
            {
                attrs.directory = true;
            }
        }
        self.next_file_index += 1;
        self.entries.insert(
            Self::key(path),
            FakeEntry {
                attrs,
                reparse,
                volume_serial: 0xC0FFEE,
                file_index: self.next_file_index,
                last_write: Some(last_write),
            },
        );
    }

    /// Turns an existing plain entry into a reparse point of `kind`.
    pub fn set_reparse(&mut self, path: &Path, kind: ReparseKind) {
        self.ensure_ancestors(path);
        match self.entries.get_mut(&Self::key(path)) {
            Some(e) => {
                e.reparse = Some(kind);
                e.attrs.reparse = true;
            }
            None => {
                let attrs = Self::dir_attrs();
                self.insert_entry(path, attrs, Some(kind), Self::base_time());
            }
        }
    }

    /// Clears a reparse point (the object becomes a plain directory again,
    /// keeping its identity — simulating a link replaced by a real folder).
    pub fn clear_reparse(&mut self, path: &Path) {
        if let Some(e) = self.entries.get_mut(&Self::key(path)) {
            e.reparse = None;
            e.attrs.reparse = false;
        }
    }

    /// Removes the entry (and everything below it) — simulating deletion.
    /// Component-boundary aware: `c:\foo` removal does not touch `c:\foo-bar`.
    pub fn remove(&mut self, path: &Path) {
        let key = Self::key(path);
        let prefix = format!("{key}\\");
        let keys: Vec<String> = self
            .entries
            .keys()
            .filter(|k| **k == key || k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in keys {
            self.entries.remove(&k);
        }
    }

    /// Removes the target entry and re-creates it as a *different* object
    /// (new file index) — the "delete then re-create" TOCTOU race.
    pub fn delete_and_recreate(&mut self, path: &Path) {
        let key = Self::key(path);
        self.entries.remove(&key);
        self.create_file(path, Some(Self::base_time()));
    }

    /// Bumps the last-write time of an entry (in-place modification).
    pub fn bump_last_write(&mut self, path: &Path) {
        let key = Self::key(path);
        if let Some(e) = self.entries.get_mut(&key) {
            let base = e.last_write.unwrap_or_else(Self::base_time);
            e.last_write = Some(base + std::time::Duration::from_secs(1));
        }
    }

    /// Marks an entry read-only.
    pub fn set_read_only(&mut self, path: &Path, ro: bool) {
        if let Some(e) = self.entries.get_mut(&Self::key(path)) {
            e.attrs.read_only = ro;
        }
    }

    fn attributes_of(&self, path: &Path) -> Option<AttrFlags> {
        self.entries.get(&Self::key(path)).map(|e| e.attrs)
    }

    fn reparse_of(&self, path: &Path) -> Option<ReparseInfo> {
        self.entries.get(&Self::key(path)).map(|e| ReparseInfo {
            kind: e.reparse,
            target: None,
        })
    }

    fn identity_of(&self, path: &Path) -> Option<FileIdentity> {
        self.entries.get(&Self::key(path)).map(|e| FileIdentity {
            volume_serial: e.volume_serial,
            file_index: e.file_index,
            last_write: e.last_write,
        })
    }
}

impl PathProbe for FakeProbe {
    fn attributes(&self, path: &Path) -> Result<AttrFlags, ProbeError> {
        let fs = self.fs.lock().expect("fake probe mutex");
        fs.attributes_of(path).ok_or_else(|| ProbeError::NotFound {
            path: path.to_path_buf(),
        })
    }

    fn reparse_info(&self, path: &Path) -> Result<ReparseInfo, ProbeError> {
        let fs = self.fs.lock().expect("fake probe mutex");
        fs.reparse_of(path).ok_or_else(|| ProbeError::NotFound {
            path: path.to_path_buf(),
        })
    }

    fn file_identity(&self, path: &Path) -> Result<FileIdentity, ProbeError> {
        let fs = self.fs.lock().expect("fake probe mutex");
        fs.identity_of(path).ok_or_else(|| ProbeError::NotFound {
            path: path.to_path_buf(),
        })
    }
}
