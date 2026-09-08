//! Source of a residue item (SPEC §6).

use serde::{Deserialize, Serialize};

/// Which producer detected/classified an item.
///
/// Mirrors SPEC §6 exactly; serialised kebab-case (`"developer-cache-provider"`).
/// Note: `Kondo` is a discovery-only source — kondo-lib is never allowed to
/// delete anything (INV-008).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// Produced by a rule from the Rule Engine (Phase 3+).
    Rule,
    /// Produced by kondo-lib project/artifact discovery (Phase 7). Read-only.
    Kondo,
    /// Produced by a developer cache provider (npm/pip/... knowledge).
    DeveloperCacheProvider,
    /// Reported directly by a package manager / tool-native query.
    PackageManager,
    /// Produced by an agent provider.
    AgentProvider,
    /// Produced by the unknown developer data provider (Phase 13). Heuristic
    /// discovery only — every hit is `Unknown` risk and never auto-deleted.
    UnknownProvider,
    /// Produced by the optional AI analyzer (Phase 14). Suggestions only.
    AiAnalysis,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_uses_kebab_case_and_round_trips() {
        let cases = [
            (SourceKind::Rule, "rule"),
            (SourceKind::Kondo, "kondo"),
            (
                SourceKind::DeveloperCacheProvider,
                "developer-cache-provider",
            ),
            (SourceKind::PackageManager, "package-manager"),
            (SourceKind::AgentProvider, "agent-provider"),
            (SourceKind::UnknownProvider, "unknown-provider"),
            (SourceKind::AiAnalysis, "ai-analysis"),
        ];
        for (variant, token) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, format!("\"{token}\""));
            let back: SourceKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }
}
