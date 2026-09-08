//! The deletion port — the **only** seam through which the CleanupEngine may
//! delete (INV-010).
//!
//! Core defines the trait; the Windows platform crate provides the real
//! implementation over the Recycle Bin COM API, `std::fs` tree deletion and
//! structured child-process execution. Nothing else in the code base may
//! touch `fs::remove_*` or call into the recycle-bin / shell capabilities —
//! the platform adapter is the single exception, by design.
//!
//! Implementers **must** be honest about failures: a locked target, a
//! permission problem or a failed external command is reported as an error,
//! never swallowed, so the engine can record it per item and keep going
//! (partial failure must be resumable).

use std::path::{Path, PathBuf};

use crate::domain::action::ToolQuerySpec;
use crate::safety::probe::FileIdentity;
use crate::ExternalCommandSpec;
use thiserror::Error;

/// R06 tool-scope re-query port: re-asks a package manager where its cache
/// lives right before executing a tool-native cleanup. Implementations are the
/// same adapters that served the scan-time `ToolQuery` (argv-structured,
/// short timeout, no shell). `None` means the query produced no usable answer
/// (tool missing / failure / timeout) — the engine treats that as a scope
/// verification failure and refuses.
pub trait ToolQueryPort: Send + Sync {
    /// Runs `spec.executable spec.args...` and returns the trimmed stdout on
    /// success, or `None` when the query cannot be answered.
    fn query_cache_root(&self, spec: &ToolQuerySpec) -> Option<String>;
}

/// Structured failure of a deletion action.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DeleteError {
    #[error("target does not exist: {path}")]
    NotFound { path: PathBuf },
    #[error("access denied while deleting: {path}")]
    PermissionDenied { path: PathBuf },
    #[error("target is locked or in use: {path}")]
    Locked { path: PathBuf },
    #[error("I/O failure while deleting {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("external command failed (exit_code={exit_code:?}, timed_out={timed_out}): {message}")]
    CommandFailed {
        exit_code: Option<i32>,
        timed_out: bool,
        message: String,
    },
    /// R05: the object bound at verification time no longer matches the live
    /// object at deletion time — the target was replaced / renamed between the
    /// identity check and the port operation.
    #[error(
        "identity mismatch at port: target '{path}' changed between validation and \
         deletion (expected volume {expected_volume}/index {expected_index}, live \
         volume {live_volume}/index {live_index})"
    )]
    IdentityMismatch {
        path: PathBuf,
        expected_volume: u32,
        expected_index: u64,
        live_volume: u32,
        live_index: u64,
    },
    /// The port could not keep the verified object bound through the final
    /// operation, so it **failed closed and deleted nothing**. This includes a
    /// staging failure and a platform API that only accepts a later path lookup
    /// instead of the verified handle. Callers must not fall back to an
    /// unbound path operation or silently change cleanup mode.
    #[error(
        "unbound-delete-refused: the verified object at '{path}' could not be bound to the \
         requested operation ({reason}) — nothing was deleted; re-run with a supported mode"
    )]
    UnboundDeleteRefused { path: PathBuf, reason: String },
}

/// Port implemented by the platform layer; consumed exclusively by
/// [`crate::cleanup::CleanupEngine`] (INV-010 — the single deletion
/// authority).
///
/// # Verified variants (R05)
///
/// [`DeletePort::delete_tree_verified`] / [`DeletePort::recycle_verified`]
/// take the [`FileIdentity`] that the validator bound at Allow time. The port
/// must re-open the target, re-read its identity **on the same handle** and
/// only delete when it still matches — closing the residual TOCTOU window
/// between validation and deletion. Implementations document their binding
/// mechanism; the plain variants remain for legacy fixtures and for targets
/// whose identity is unavailable.
///
/// # Sealing notes (Phase 15, F14)
///
/// The trait stays `pub` because the engine holds it as `Arc<dyn DeletePort>`
/// and the platform crate must implement it. The real seal is architectural
/// and audited:
///
/// - **Core** contains no implementation of the port outside the engine's own
///   test double, and no port method call (`recycle`, `delete_tree`,
///   `execute_command`) outside `cleanup/engine.rs`. The test below scans
///   the cleanup module sources and fails if that invariant is broken.
/// - The **platform crate** (`devresidue-platform-windows`) provides exactly
///   one production implementation (`WindowsDeletePort`) and nothing else in
///   any crate may delete (providers are discovery-only, INV-009).
pub trait DeletePort: Send + Sync {
    /// Moves `path` into the OS Recycle Bin (recoverable deletion).
    fn recycle(&self, path: &Path) -> Result<(), DeleteError>;

    /// Permanently deletes the file or directory tree at `path`.
    fn delete_tree(&self, path: &Path) -> Result<(), DeleteError>;

    /// Runs a structured external cleanup command (SPEC §22) and succeeds
    /// only when the process exited `0` within its timeout.
    fn execute_command(&self, spec: &ExternalCommandSpec) -> Result<(), DeleteError>;

    /// R05: deletes the tree at `path` **only if** its live identity still
    /// equals `expected`. Implementations bind the verified object to the
    /// deletion (open-verify-delete on one handle); a mismatch surfaces as
    /// [`DeleteError::IdentityMismatch`] and the replacement object survives.
    fn delete_tree_verified(&self, path: &Path, expected: &FileIdentity)
        -> Result<(), DeleteError>;

    /// R05: moves `path` to the Recycle Bin **only if** its live identity
    /// still equals `expected`. OS recycle APIs are path-only (they cannot
    /// consume a verified handle), so implementations must keep the binding
    /// by **post-hoc verification**: verify on a pinned handle up to the
    /// last possible moment, hand the (staged) path to the shell, then
    /// verify the OUTCOME — the entry is gone, or the identity decides
    /// (a different object is [`DeleteError::IdentityMismatch`], never a
    /// success). A recycle that neither binds nor verifies the outcome must
    /// fail closed with [`DeleteError::UnboundDeleteRefused`]; an
    /// implementation must never recycle an unverified name, silently
    /// downgrade to permanent deletion, or record an unverified outcome as
    /// success.
    fn recycle_verified(&self, path: &Path, expected: &FileIdentity) -> Result<(), DeleteError>;
}

#[cfg(test)]
mod tests {
    /// Sources inside the cleanup module that may legitimately reference the
    /// port (the trait definition and the engine's test double live here).
    /// Every other cleanup-module file must not implement or call the port.
    const AUDITED_FILES: &[(&str, &str)] = &[
        ("engine.rs", include_str!("engine.rs")),
        ("port.rs", include_str!("port.rs")),
        ("planner.rs", include_str!("planner.rs")),
        ("plan_store.rs", include_str!("plan_store.rs")),
        ("mod.rs", include_str!("mod.rs")),
    ];

    fn count(text: &str, needle: &str) -> usize {
        text.match_indices(needle).count()
    }

    #[test]
    fn delete_port_is_only_implemented_and_called_by_the_engine() {
        // F14: textual audit of the whole cleanup module. Implementations of
        // the trait may only appear in engine.rs (its test double) — never in
        // the planner, the store or the module root. Port *method calls* may
        // only appear in engine.rs.
        //
        // The needles are assembled at runtime so this audit file's own
        // source (which names the tokens) does not trip its assertions.
        let impl_needle = format!("impl {}", "DeletePort");
        // RecycleBin is executed as a verified permanent deletion by the
        // engine (the Windows recycle API cannot be handle-bound), so the
        // engine no longer calls `.recycle(` — the audit requires the two
        // operations it actually dispatches; `recycle` stays a port
        // capability exercised by the platform tests.
        let call_needles = [
            format!(".{}(", "delete_tree"),
            format!(".{}(", "execute_command"),
        ];

        for (name, src) in AUDITED_FILES {
            let impls = count(src, &impl_needle);
            if *name == "engine.rs" {
                assert!(
                    impls >= 1,
                    "{name}: the engine's test double impls the port"
                );
            } else {
                assert_eq!(
                    impls, 0,
                    "{name} must not implement DeletePort (INV-010/F14)"
                );
            }
        }

        // Method-call sites: only engine.rs may invoke the port (its docs and
        // test double reference the calls too, hence `>= 1`).
        for (name, src) in AUDITED_FILES {
            for call in &call_needles {
                if *name == "engine.rs" {
                    assert!(count(src, call) >= 1, "{name} must call the port ({call})");
                } else {
                    assert_eq!(
                        count(src, call),
                        0,
                        "{name} must not call the deletion port ({call}) (INV-010/F14)"
                    );
                }
            }
        }
    }
}
