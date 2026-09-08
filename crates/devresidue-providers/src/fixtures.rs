//! Built-in fixture provider for Phase 2 CLI acceptance.
//!
//! Returns 3-5 hard-coded, clearly-synthetic [`ScanItem`]s (Claude Code
//! cache, npm cache, Rust `target`, `.ssh` credentials, Claude session) that
//! exercise distinct categories, risk levels, sources and cleanup actions.
//!
//! The paths point at a **sample** user profile (`C:\Users\demo`) and do not
//! need to exist. No real filesystem access happens; this provider exists so
//! the CLI pipeline can be demonstrated before real providers land (Phase 7+).

use std::time::{Duration, SystemTime};

use devresidue_core::{
    CleanupAction, Evidence, ExternalCommandSpec, ProviderId, ResidueCategory, RiskLevel, ScanItem,
    ScanItemId, SourceKind,
};

use crate::ResidueProvider;

/// Numeric registration id of the fixture provider (session-local registry).
pub const FIXTURE_PROVIDER_ID: ProviderId = ProviderId::from_raw(1);

/// Yields the synthetic demo items.
#[derive(Debug, Default)]
pub struct FixtureProvider;

impl ResidueProvider for FixtureProvider {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn scan(&self) -> Vec<ScanItem> {
        fixture_scan_items()
    }
}

/// Builds the demo scan items (stable order & ids 1..=5).
pub fn fixture_scan_items() -> Vec<ScanItem> {
    let days_ago = |days: u64| Some(SystemTime::now() - Duration::from_secs(days * 86_400));

    let claude_cache = ScanItem {
        id: ScanItemId::from_raw(1),
        path: r"C:\Users\demo\.claude\shell-snapshots".into(),
        display_name: "Claude Code shell-snapshot cache".into(),
        product: Some("Claude Code".into()),
        category: ResidueCategory::AiAgent,
        risk: RiskLevel::Safe,
        source: SourceKind::AgentProvider,
        logical_size: 384 * 1024 * 1024,
        file_count: 91,
        last_modified: days_ago(12),
        explanation: "Claude Code shell-snapshot cache (agent layout split, SPEC §9); \
                      pure cache, safe to clean; regenerated on demand."
            .into(),
        cleanup_action: CleanupAction::RecycleBin,
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
        evidence: vec![
            Evidence::new(
                "path-layout",
                "agent cache subfolder (.claude/shell-snapshots)",
            ),
            Evidence::new("fixture", "synthetic sample item for Phase 2 acceptance"),
        ],
    };

    let npm_cache = ScanItem {
        id: ScanItemId::from_raw(2),
        path: r"C:\Users\demo\AppData\Local\npm-cache".into(),
        display_name: "npm cache".into(),
        product: Some("npm".into()),
        category: ResidueCategory::PackageCache,
        risk: RiskLevel::RegenerableDownload,
        source: SourceKind::DeveloperCacheProvider,
        logical_size: 1_258_291_200,
        file_count: 28_431,
        last_modified: days_ago(3),
        explanation: "npm dependency cache; deleting saves ~1.2 GiB but triggers \
                      re-downloads for future installs (SPEC §8, regenerable-download). \
                      Cleanup via the tool-native command instead of a raw delete \
                      (SPEC §10/§22)."
            .into(),
        cleanup_action: CleanupAction::ExternalCommand {
            command: ExternalCommandSpec::new(
                "npm".into(),
                vec!["cache".into(), "clean".into(), "--force".into()],
                None,
                Some(300),
            ),
        },
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
        evidence: vec![
            Evidence::new(
                "tool-reported-path",
                "npm cache location (Mole Windows branch layout reference)",
            ),
            Evidence::new("fixture", "synthetic sample item for Phase 2 acceptance"),
        ],
    };

    let rust_target = ScanItem {
        id: ScanItemId::from_raw(3),
        path: r"D:\Code\demo-app\target".into(),
        display_name: "Rust build artifacts (target)".into(),
        product: Some("Rust".into()),
        category: ResidueCategory::BuildArtifact,
        risk: RiskLevel::RegenerableLocal,
        source: SourceKind::Kondo,
        logical_size: 847_249_408,
        file_count: 19_204,
        last_modified: days_ago(21),
        explanation: "Rust project build output (kondo discovery, read-only, INV-008); \
                      locally regenerable with cargo build/clean."
            .into(),
        cleanup_action: CleanupAction::RecycleBin,
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
        evidence: vec![
            Evidence::new("kondo-project-type", "Rust/Cargo project target dir"),
            Evidence::new("fixture", "synthetic sample item for Phase 2 acceptance"),
        ],
    };

    let ssh = ScanItem {
        id: ScanItemId::from_raw(4),
        path: r"C:\Users\demo\.ssh".into(),
        display_name: "SSH keys & config".into(),
        product: Some("OpenSSH".into()),
        category: ResidueCategory::Credential,
        risk: RiskLevel::Protected,
        source: SourceKind::Rule,
        logical_size: 24_576,
        file_count: 6,
        last_modified: days_ago(400),
        explanation: "SSH credential directory; permanently protected \
                      (SPEC §18, INV-007). Never cleanable."
            .into(),
        cleanup_action: CleanupAction::None,
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
        evidence: vec![
            Evidence::new(
                "builtin-protected",
                "user-profile/.ssh is a built-in permanent protected path",
            ),
            Evidence::new("fixture", "synthetic sample item for Phase 2 acceptance"),
        ],
    };

    let claude_session = ScanItem {
        id: ScanItemId::from_raw(5),
        path: r"C:\Users\demo\.claude\projects\demo-app".into(),
        display_name: "Claude Code session history".into(),
        product: Some("Claude Code".into()),
        category: ResidueCategory::Session,
        risk: RiskLevel::Review,
        source: SourceKind::AgentProvider,
        logical_size: 16_384_000,
        file_count: 248,
        last_modified: days_ago(60),
        explanation: "Agent session transcripts for one project; may contain \
                      valuable context, requires explicit user review before any \
                      cleanup (SPEC §8 review)."
            .into(),
        cleanup_action: CleanupAction::Defer {
            reason: "Review risk class; user confirmation required before cleanup".into(),
        },
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
        evidence: vec![
            Evidence::new("path-layout", "agent session history (.claude/projects)"),
            Evidence::new("fixture", "synthetic sample item for Phase 2 acceptance"),
        ],
    };

    vec![claude_cache, npm_cache, rust_target, ssh, claude_session]
}
