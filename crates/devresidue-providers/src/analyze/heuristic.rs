//! `HeuristicAnalyzer` — the shipped local, zero-network, zero-LLM analyzer
//! implementation (SPEC §26: the AI network analyzer is a future version; the
//! interface is in place and this implementation keeps the feature usable and
//! testable offline).

use devresidue_core::{ResidueCategory, RiskLevel};

use super::{DirFacts, DirectoryAnalyzer, Suggestion};

/// Product signals: a token appearing in the directory name / manifest marker
/// maps onto a product guess + category + risk suggestion.
const PRODUCT_SIGNALS: &[(&[&str], &str, ResidueCategory, RiskLevel)] = &[
    (
        &[
            "node", "npm", "npx", "yarn", "pnpm", "deno", "bun", "corepack",
        ],
        "node",
        ResidueCategory::Dependency,
        RiskLevel::RegenerableDownload,
    ),
    (
        &["rust", "cargo", "rustup"],
        "rust",
        ResidueCategory::BuildArtifact,
        RiskLevel::RegenerableLocal,
    ),
    (
        &[
            "python", "pip", "uv", "poetry", "conda", "pixi", "pyenv", "pytest",
        ],
        "python",
        ResidueCategory::Dependency,
        RiskLevel::RegenerableDownload,
    ),
    (
        &[
            "java", "jdk", "gradle", "maven", "mvn", "sbt", "scala", "kotlin",
        ],
        "java",
        ResidueCategory::BuildArtifact,
        RiskLevel::RegenerableLocal,
    ),
    (
        &["dotnet", "nuget", "msbuild"],
        ".net",
        ResidueCategory::BuildArtifact,
        RiskLevel::RegenerableLocal,
    ),
    (
        &["go", "golang", "goenv"],
        "go",
        ResidueCategory::BuildArtifact,
        RiskLevel::RegenerableLocal,
    ),
    (
        &["docker", "container"],
        "docker",
        ResidueCategory::DeveloperCache,
        RiskLevel::RegenerableDownload,
    ),
    (
        &["terraform"],
        "terraform",
        ResidueCategory::WorkspaceState,
        RiskLevel::Review,
    ),
    (
        &["vscode"],
        "vscode",
        ResidueCategory::Ide,
        RiskLevel::Review,
    ),
    (
        &["android", "gradle"],
        "android",
        ResidueCategory::BuildArtifact,
        RiskLevel::RegenerableLocal,
    ),
    (
        &["git"],
        "git",
        ResidueCategory::WorkspaceState,
        RiskLevel::Review,
    ),
];

/// The shipped analyzer.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicAnalyzer;

impl DirectoryAnalyzer for HeuristicAnalyzer {
    fn analyze(&self, facts: &DirFacts) -> Suggestion {
        let tokens = facts.name_tokens();
        // Prefer a manifest marker (strongest), then name tokens.
        let manifest = facts.has_manifest();
        for (names, product, category, risk) in PRODUCT_SIGNALS {
            let product = *product;
            let marker_hit = manifest.is_some_and(|m| {
                (product == "node" && matches!(m, "package.json" | "yarn.lock"))
                    || (product == "rust" && m == "Cargo.toml")
                    || (product == "go" && m == "go.mod")
                    || (product == "python" && matches!(m, "pyproject.toml" | "requirements.txt"))
                    || (product == "java" && matches!(m, "pom.xml" | "build.gradle"))
                    || (product == ".net" && m == "pom.xml")
            });
            let token_hit = tokens.iter().any(|t| names.contains(&t.as_str()));
            if token_hit {
                let confidence = if marker_hit { 0.9 } else { 0.6 };
                return Suggestion {
                    product_guess: Some(product.to_string()),
                    confidence,
                    category: *category,
                    risk: *risk,
                    explanation: explain(facts, product, marker_hit),
                    suggested_rule_id: Some(format!("user-analysis/{product}")),
                };
            }
        }
        // Nothing matched: unknown developer data, like the unknown provider.
        Suggestion {
            product_guess: None,
            confidence: 0.3,
            category: ResidueCategory::Unknown,
            risk: RiskLevel::Unknown,
            explanation: format!(
                "No developer tool signature in '{}'{} — treat as unknown developer \
                 data pending manual review.",
                facts.dir_name,
                if facts.entry_count == 0 {
                    " (empty)"
                } else {
                    ""
                }
            ),
            suggested_rule_id: None,
        }
    }
}

fn explain(facts: &DirFacts, product: &str, marker: bool) -> String {
    if marker {
        format!(
            "'{}' contains a {} manifest marker; layout matches {product} developer data.",
            facts.dir_name,
            facts.has_manifest().unwrap_or("")
        )
    } else {
        format!(
            "Directory name '{}' matches {product} developer tool conventions.",
            facts.dir_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(name: &str, files: &[&str], dirs: &[&str]) -> DirFacts {
        DirFacts {
            dir_name: name.to_string(),
            file_names: files.iter().map(|s| s.to_string()).collect(),
            child_dirs: dirs.iter().map(|s| s.to_string()).collect(),
            ..DirFacts::default()
        }
    }

    #[test]
    fn node_cache_guesses_node_with_manifest_confidence() {
        let analyzer = HeuristicAnalyzer;
        let f = facts(".npm", &["package.json"], &["_cacache", "logs"]);
        let s = analyzer.analyze(&f);
        assert_eq!(s.product_guess.as_deref(), Some("node"));
        assert!(s.confidence >= 0.8);
        assert_eq!(s.risk, RiskLevel::RegenerableDownload);
    }

    #[test]
    fn cargo_layout_guesses_rust() {
        let analyzer = HeuristicAnalyzer;
        let f = facts("rust-project", &["Cargo.toml"], &["src", "target"]);
        let s = analyzer.analyze(&f);
        assert_eq!(s.product_guess.as_deref(), Some("rust"));
        assert_eq!(s.category, ResidueCategory::BuildArtifact);
    }

    #[test]
    fn unidentifiable_directory_returns_unknown() {
        let analyzer = HeuristicAnalyzer;
        let f = facts("photos-backup", &["a.jpg"], &[]);
        let s = analyzer.analyze(&f);
        assert_eq!(s.product_guess, None);
        assert_eq!(s.risk, RiskLevel::Unknown);
        assert!(s.suggested_rule_id.is_none());
    }
}
