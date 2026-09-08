//! Directory analyzer (SPEC §26, Phase 14) — **default off**.
//!
//! A [`DirectoryAnalyzer`] inspects one directory and returns a
//! [`Suggestion`]. Its input is **metadata only**:
//!
//! ```text
//! directory name | first-level file names | extension histogram | size |
//! timestamps | one-level tree shape | manifest presence
//! ```
//!
//! File **contents are never read** (source code / credentials / tokens /
//! private documents stay out of the input, SPEC §26).
//!
//! The analyzer has **no deletion authority** (INV-003): a `Suggestion` is a
//! plain DTO for the UI to display. Turning it into a rule is an explicit,
//! separately-gated user action (`rules::user::upsert_detection_rule`) that
//! still runs the full rule validator.

pub mod heuristic;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use devresidue_core::{ResidueCategory, RiskLevel};

/// How many first-level names are captured (bounded: the analyzer never walks
/// a whole tree, and a name sample is enough for a guess).
pub const MAX_SAMPLED_NAMES: usize = 64;

/// Manifest file names recognised at the top level (strong product signals).
pub const MANIFEST_MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "composer.json",
    "Gemfile",
    "pom.xml",
    "build.gradle",
    "CMakeLists.txt",
];

/// Metadata-only facts about one directory — the sole analyzer input.
#[derive(Debug, Clone, Default)]
pub struct DirFacts {
    /// Directory (leaf) name.
    pub dir_name: String,
    /// First-level child *file* names (bounded sample).
    pub file_names: Vec<String>,
    /// First-level child *directory* names.
    pub child_dirs: Vec<String>,
    /// Extension histogram over the sampled first-level files.
    pub extensions: BTreeMap<String, usize>,
    /// Total measured logical size (bytes).
    pub logical_size: u64,
    /// Total file count.
    pub file_count: u64,
    /// Newest modification time (may be absent).
    pub last_modified: Option<SystemTime>,
    /// Number of first-level entries.
    pub entry_count: usize,
}

impl DirFacts {
    /// Whether a recognised manifest marker is present at the top level.
    #[must_use]
    pub fn has_manifest(&self) -> Option<&'static str> {
        MANIFEST_MARKERS
            .iter()
            .find(|m| self.file_names.iter().any(|f| f == *m))
            .copied()
    }

    /// The lower-cased, tokenised directory name.
    #[must_use]
    pub fn name_tokens(&self) -> Vec<String> {
        self.dir_name
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// One analyzer suggestion (a DTO — nothing here may delete or mutate).
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// Best-effort product/tool guess (`None` when nothing matched).
    pub product_guess: Option<String>,
    /// Confidence in `0.0..=1.0`.
    pub confidence: f32,
    /// Category the item would be classified into if the user accepts.
    pub category: ResidueCategory,
    /// Risk the item would carry if the user accepts.
    pub risk: RiskLevel,
    /// Human explanation of the reasoning.
    pub explanation: String,
    /// Suggested rule id slot (created only when the user accepts).
    pub suggested_rule_id: Option<String>,
}

/// Contract for directory analyzers (Phase 14 interface; the AI/network
/// implementation is a future version — this crate ships the local heuristic
/// one).
pub trait DirectoryAnalyzer {
    /// Produces a suggestion from metadata-only facts.
    fn analyze(&self, facts: &DirFacts) -> Suggestion;
}

/// Collects the metadata-only facts for one directory (a single top-level
/// read + the caller-provided measure). Never reads file contents.
pub fn collect_facts(dir: &Path, measure: crate::measure::Measure) -> Option<DirFacts> {
    let dir_name = dir.file_name()?.to_string_lossy().into_owned();
    let mut facts = DirFacts {
        dir_name,
        logical_size: measure.logical_size,
        file_count: measure.file_count,
        last_modified: measure.last_modified,
        ..DirFacts::default()
    };
    let entries = std::fs::read_dir(dir).ok()?;
    let mut entries = entries.flatten();
    while facts.entry_count < MAX_SAMPLED_NAMES {
        let Some(entry) = entries.next() else {
            break;
        };
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        facts.entry_count += 1;
        if ft.is_dir() {
            facts.child_dirs.push(name);
            continue;
        }
        if ft.is_file() {
            let ext = Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            *facts.extensions.entry(ext).or_default() += 1;
            facts.file_names.push(name);
        }
    }
    Some(facts)
}

/// Maps a category label onto a residue category (kebab-case, mirrors the
/// domain serde).
#[allow(dead_code)]
pub(crate) fn category_from_kebab(text: &str) -> ResidueCategory {
    match text {
        "ai-agent" => ResidueCategory::AiAgent,
        "ide" => ResidueCategory::Ide,
        "developer-cache" => ResidueCategory::DeveloperCache,
        "package-cache" => ResidueCategory::PackageCache,
        "build-artifact" => ResidueCategory::BuildArtifact,
        "dependency" => ResidueCategory::Dependency,
        "log" => ResidueCategory::Log,
        "temporary" => ResidueCategory::Temporary,
        "session" => ResidueCategory::Session,
        "workspace-state" => ResidueCategory::WorkspaceState,
        "configuration" => ResidueCategory::Configuration,
        "credential" => ResidueCategory::Credential,
        _ => ResidueCategory::Unknown,
    }
}
