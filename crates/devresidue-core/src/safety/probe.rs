//! Probing ports — the capability boundary between core validation and the
//! platform layer (ports & adapters).
//!
//! Core defines *what* it needs to know about a live target; the Windows
//! platform layer implements these traits over real Win32 APIs. Validators
//! code against the traits only, so their safety logic is unit-testable with
//! programmable fakes and does not pull any OS dependency into core.
//!
//! All DTOs here are deliberately platform-neutral and serialisable, so a
//! [`crate::TargetSnapshot`] can embed them.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Neutral view of the Windows attribute bits safety cares about. Produced by
/// the platform probe; consumed by snapshot capture and revalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct AttrFlags {
    pub read_only: bool,
    pub hidden: bool,
    pub system: bool,
    pub directory: bool,
    pub reparse: bool,
}

/// Kind of reparse point encountered on a path (SPEC §17 / INV-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReparseKind {
    Symlink,
    Junction,
    MountPoint,
    Other,
}

/// Result of a reparse probe: `kind` is `None` for a regular object.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReparseInfo {
    pub kind: Option<ReparseKind>,
    /// Resolved target (substitute name, device prefix stripped). Present for
    /// symlink/junction/mount-point; explanatory only — core never follows it.
    pub target: Option<PathBuf>,
}

/// Stable NTFS identity of a file-system object.
///
/// `volume_serial + file_index` uniquely identify an object on a volume and
/// are what revalidation compares to detect rename/replace/re-create (TOCTOU,
/// SPEC §16, INV-005). `last_write` is carried here because obtaining it
/// requires opening the object anyway; the canonical record in a snapshot is
/// [`crate::TargetSnapshot::last_write_time`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub volume_serial: u32,
    pub file_index: u64,
    #[serde(with = "crate::domain::serde_time::opt_system_time")]
    pub last_write: Option<SystemTime>,
}

/// Execution state of the processes a product depends on (SPEC §12 / INV-012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessState {
    /// At least one watched process is running → skip/defer the target.
    Running,
    /// Watched processes are confirmed not running.
    NotRunning,
    /// The process list could not be read — **never** treat as NotRunning
    /// (INV-012: unknown must fail closed).
    Unknown,
}

/// A cross-process **whole-file lock** (R3-G06): acquired from an
/// already-open file and released when dropped.
///
/// Core only defines the contract; the platform layer (the one crate allowed
/// `unsafe`) implements it over Win32 `LockFileEx` / POSIX `flock`. It is a
/// capability for state-store writers that must serialise a read-modify-write
/// transaction across processes (the scan-store generation allocator) — not a
/// general filesystem probe, and it never gates cleanup decisions.
pub trait FileLock: Send + Sync + std::fmt::Debug {}

/// Acquires an exclusive, blocking, whole-file lock on `file` (R3-G06).
///
/// The lock is held until the returned guard is dropped. The default core
/// implementation is a **poison** for builds without the platform adapter: it
/// returns an error rather than silently pretending exclusion exists — the
/// real implementation is injected by the platform layer through the
/// providers crate's wiring (see `devresidue-providers::scan_store`).
pub fn lock_file_exclusive(file: std::fs::File) -> Result<Box<dyn FileLock>, String> {
    Err(format!(
        "cross-process file lock is not available in this build (file metadata {:?}) — \
         the platform adapter must provide it",
        file.metadata().map(|m| m.is_file())
    ))
}

/// Structured reason a filesystem probe could not inspect a path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("target does not exist: {path}")]
    NotFound { path: PathBuf },
    #[error("access denied while probing {path}")]
    PermissionDenied { path: PathBuf },
    #[error("target is locked or in use: {path}")]
    Locked { path: PathBuf },
    #[error("probing {path} failed: {message}")]
    Other { path: PathBuf, message: String },
}

/// Reads live facts about one file-system object. All three queries must be
/// consistent about existence: a missing path is reported as
/// [`ProbeError::NotFound`].
pub trait PathProbe {
    /// Attribute flags of the object itself (never following a trailing
    /// reparse point).
    fn attributes(&self, path: &Path) -> Result<AttrFlags, ProbeError>;
    /// Whether the path is a reparse point and of which kind (never follows).
    fn reparse_info(&self, path: &Path) -> Result<ReparseInfo, ProbeError>;
    /// Stable identity of the object (never following a trailing reparse
    /// point, so a symlink/junction reports its own entry).
    fn file_identity(&self, path: &Path) -> Result<FileIdentity, ProbeError>;
}

/// Reports whether any of the given process names is running.
pub trait ProcessProbe {
    /// Three-state answer; implementers surface "could not enumerate" as
    /// [`ProcessState::Unknown`] rather than guessing.
    fn process_state(&self, names: &[&str]) -> ProcessState;
}
