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
pub mod filesystem;
pub mod identity;
pub mod path;
pub mod process;
pub mod recycle_bin;
pub mod safety;
pub mod shell;

mod ffi;

pub use ffi::FileSystemError;
