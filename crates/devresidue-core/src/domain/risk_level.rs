//! Risk classification of a residue item (SPEC §8).

use serde::{Deserialize, Serialize};

/// How dangerous / disruptive it is to remove a residue item.
///
/// The variant set mirrors SPEC §8 exactly. Serialised form uses kebab-case
/// (`"regenerable-local"`), matching the risk tokens used across the CLI,
/// journal and (future) UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskLevel {
    /// Deleting causes no lasting impact (logs, temp files, crash dumps,
    /// pure caches).
    Safe,
    /// Deleting costs a local rebuild only (CMake build dirs, Rust `target`,
    /// .NET `bin`/`obj`, ...).
    RegenerableLocal,
    /// Deleting means re-downloading or re-installing dependencies
    /// (`node_modules`, `.venv`, npm/pnpm/Cargo/NuGet caches, ...).
    RegenerableDownload,
    /// May hold data the user values (agent sessions, history, workspace
    /// state, temporary worktrees, downloaded models). Requires explicit
    /// confirmation before any cleanup.
    Review,
    /// Never eligible for ordinary cleanup (config, settings, auth,
    /// credentials, SSH keys, API keys, MCP config, user scripts, project
    /// instructions). INV-002 / INV-007.
    Protected,
    /// Could not be classified reliably. Never deleted automatically
    /// (INV-001).
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_uses_kebab_case_and_round_trips() {
        let cases = [
            (RiskLevel::Safe, "safe"),
            (RiskLevel::RegenerableLocal, "regenerable-local"),
            (RiskLevel::RegenerableDownload, "regenerable-download"),
            (RiskLevel::Review, "review"),
            (RiskLevel::Protected, "protected"),
            (RiskLevel::Unknown, "unknown"),
        ];
        for (variant, token) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{token}\""));
            let back: RiskLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn ordering_places_unknown_and_protected_last() {
        // Ordering is used to sort items most-dangerous-last in listings.
        assert!(RiskLevel::Safe < RiskLevel::Unknown);
        assert!(RiskLevel::Review < RiskLevel::Protected);
        assert!(RiskLevel::Protected < RiskLevel::Unknown);
    }
}
