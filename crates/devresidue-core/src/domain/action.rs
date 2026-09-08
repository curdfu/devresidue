//! Cleanup actions and modes (SPEC §19 / §22).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How a scanned item *would* be cleaned, decided at scan/plan time.
///
/// This is an **intention**, never an execution. Only the future
/// `CleanupEngine` (Phase 6) may translate a plan into real deletion; the
/// `action` field exists so listings, dry runs and plans can explain what the
/// engine would do (SPEC §23).
///
/// Serialised as an internally tagged object, e.g.
/// `{"kind":"external-command","command":{...}}` or `{"kind":"defer",
/// "reason":"..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CleanupAction {
    /// No cleanup should be suggested/allowed for this item (e.g. Protected
    /// credentials, INV-002/INV-007).
    None,
    /// Move to the OS recycle bin (recoverable).
    RecycleBin,
    /// Permanent deletion through the CleanupEngine.
    DirectDelete,
    /// A trusted tool-native cleanup command should be run instead of a raw
    /// delete (SPEC §10, §22). The command is fully structured — never a shell
    /// string.
    ExternalCommand {
        /// Structured executable + args spec.
        command: ExternalCommandSpec,
    },
    /// Cleanup is deferred with a human/machine-readable reason (e.g. a
    /// related process is running, SPEC §12 / INV-012).
    Defer { reason: String },
}

/// The actual deletion modes supported by the CleanupEngine (SPEC §19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanupMode {
    /// Move the target into the OS recycle bin first.
    RecycleBin,
    /// Permanently delete the target.
    DirectDelete,
    /// Run a structured external cleanup command.
    ExternalCommand,
}

/// Structured, shell-less description of an external cleanup command.
///
/// Only **trusted built-in providers** may create this value (SPEC §22); rule
/// files and AI suggestions are never allowed to contribute arbitrary shell
/// strings. Execution (Phase 4/6) must pass `args` directly to the child
/// process without `cmd.exe /c` string concatenation, enforce `timeout_secs`,
/// capture stdout/stderr and check the exit code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCommandSpec {
    /// Executable to launch (an absolute path or a tool resolved on `PATH`,
    /// e.g. `npm`). Never contains arguments.
    pub executable: String,
    /// Arguments passed individually to the executable.
    pub args: Vec<String>,
    /// Working directory for the child process, when relevant.
    pub working_directory: Option<PathBuf>,
    /// Kill switch after this many seconds. `None` means no timeout configured
    /// (providers should prefer setting one).
    pub timeout_secs: Option<u64>,
    /// R06: the **frozen tool-native scope** the command was authorised against
    /// (present for cache-cleaning tools such as npm/pip/uv/bun). Before
    /// execution the engine re-runs `verify_query` through the injected tool
    /// port, canonical-compares the answer with `expected_cache_root`, and
    /// refuses the run on drift — so a configuration change between scan and
    /// cleanup can never make the tool clean a different cache than the one
    /// that was verified (SPEC §11 / INV-011). `None` for legacy specs and for
    /// tools with no queryable scope.
    #[serde(default)]
    pub scope: Option<ScopeBinding>,
}

/// The re-verification protocol for a tool-native cleanup (R06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeBinding {
    /// Tool display name (for messages).
    pub tool: String,
    /// The cache root that was verified at scan time; execution must re-derive
    /// exactly this canonical path (case/separator insensitive).
    pub expected_cache_root: PathBuf,
    /// Command that re-asks the tool where its cache is (e.g. `uv cache dir`,
    /// `npm config get cache`).
    pub verify_query: ToolQuerySpec,
}

/// A re-query command for tool-scope verification (argv-structured).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolQuerySpec {
    pub executable: String,
    pub args: Vec<String>,
}

impl ExternalCommandSpec {
    /// Creates a command spec from parts (legacy: no scope binding).
    #[must_use]
    pub const fn new(
        executable: String,
        args: Vec<String>,
        working_directory: Option<PathBuf>,
        timeout_secs: Option<u64>,
    ) -> Self {
        Self {
            executable,
            args,
            working_directory,
            timeout_secs,
            scope: None,
        }
    }

    /// Creates a command spec bound to a tool-native scope (R06).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn with_scope(
        executable: String,
        args: Vec<String>,
        working_directory: Option<PathBuf>,
        timeout_secs: Option<u64>,
        scope: ScopeBinding,
    ) -> Self {
        Self {
            executable,
            args,
            working_directory,
            timeout_secs,
            scope: Some(scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_mode_serde_round_trips() {
        for mode in [
            CleanupMode::RecycleBin,
            CleanupMode::DirectDelete,
            CleanupMode::ExternalCommand,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: CleanupMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
        }
        assert_eq!(
            serde_json::to_string(&CleanupMode::RecycleBin).unwrap(),
            "\"recycle-bin\""
        );
    }

    #[test]
    fn cleanup_action_serde_shapes_are_stable() {
        // Internally-tagged DTO shape, stable for JSON consumers.
        let none = serde_json::to_string(&CleanupAction::None).unwrap();
        assert_eq!(none, r#"{"kind":"none"}"#);

        let defer = serde_json::to_string(&CleanupAction::Defer {
            reason: "process running".into(),
        })
        .unwrap();
        assert_eq!(defer, r#"{"kind":"defer","reason":"process running"}"#);

        let cmd = CleanupAction::ExternalCommand {
            command: ExternalCommandSpec::new(
                "npm".into(),
                vec!["cache".into(), "clean".into(), "--force".into()],
                None,
                Some(120),
            ),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: CleanupAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmd);
    }
}
