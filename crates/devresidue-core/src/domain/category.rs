//! Residue categories (SPEC §6).

use serde::{Deserialize, Serialize};

/// What kind of developer residue an item is.
///
/// Mirrors SPEC §6 exactly; serialised kebab-case (`"ai-agent"`,
/// `"developer-cache"`, ...). The category drives user-facing grouping and,
/// together with rules, feeds risk classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResidueCategory {
    /// Data produced by AI coding agents (Codex, Claude Code, OpenCode, ...).
    AiAgent,
    /// IDE / coding-tool data.
    Ide,
    /// Shared developer tool caches (npm cache, pip cache, ...).
    DeveloperCache,
    /// Package manager caches & stores.
    PackageCache,
    /// Rebuildable build outputs (target/, bin/, obj/, ...).
    BuildArtifact,
    /// Project dependency trees (`node_modules`, `.venv`, ...).
    Dependency,
    /// Log files.
    Log,
    /// Temporary files/directories.
    Temporary,
    /// Agent/IDE sessions (conversations, history).
    Session,
    /// Workspace state (open editors, indexes, ...).
    WorkspaceState,
    /// Configuration files.
    Configuration,
    /// Credentials, secrets, tokens, keys.
    Credential,
    /// Could not be classified.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_uses_kebab_case_and_round_trips() {
        let cases = [
            (ResidueCategory::AiAgent, "ai-agent"),
            (ResidueCategory::Ide, "ide"),
            (ResidueCategory::DeveloperCache, "developer-cache"),
            (ResidueCategory::PackageCache, "package-cache"),
            (ResidueCategory::BuildArtifact, "build-artifact"),
            (ResidueCategory::Dependency, "dependency"),
            (ResidueCategory::Log, "log"),
            (ResidueCategory::Temporary, "temporary"),
            (ResidueCategory::Session, "session"),
            (ResidueCategory::WorkspaceState, "workspace-state"),
            (ResidueCategory::Configuration, "configuration"),
            (ResidueCategory::Credential, "credential"),
            (ResidueCategory::Unknown, "unknown"),
        ];
        for (variant, token) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{token}\""));
            let back: ResidueCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }
}
