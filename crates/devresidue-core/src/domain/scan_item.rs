//! `ScanItem` — the unit of discovery.

use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use super::{
    action::CleanupAction, category::ResidueCategory, evidence::Evidence, ids::ScanItemId,
    risk_level::RiskLevel, snapshot::TargetSnapshot, source::SourceKind,
};

/// One residue item discovered during a scan (SPEC §6).
///
/// Providers **only** produce `ScanItem`s; they never delete (INV-009). All
/// downstream operations — user selection, planning, dry-run, journaling,
/// cleanup — reference the item exclusively through its strongly typed
/// [`ScanItemId`], never through an arbitrary path (INV-013).
///
/// Serialised DTO field names are stable snake_case and may be consumed by the
/// CLI (`scan --json`) and later the Tauri UI. `last_modified` is emitted as
/// Unix epoch seconds or `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanItem {
    /// Opaque id minted for this item within the current scan. Never derived
    /// from the path.
    pub id: ScanItemId,
    /// Logical path of the residue, as discovered by the trusted provider.
    /// The path travels *with* the registered item; callers never supply one.
    pub path: PathBuf,
    /// Short human-friendly name, e.g. `"npm cache"`.
    pub display_name: String,
    /// The product/tool that owns the data (e.g. `"Claude Code"`, `"npm"`).
    pub product: Option<String>,
    /// Residue category (drives grouping/UI).
    pub category: ResidueCategory,
    /// Risk level (drives cleanup policy).
    pub risk: RiskLevel,
    /// Which producer detected this item.
    pub source: SourceKind,
    /// Logical size in bytes (sum of the contained files when known).
    pub logical_size: u64,
    /// Number of contained files/directories (0 when not counted).
    pub file_count: u64,
    /// Last modification time observed at scan time (`None` when unknown).
    #[serde(with = "crate::domain::serde_time::opt_system_time")]
    pub last_modified: Option<SystemTime>,
    /// Human-readable reason for the classification (what/why/impact).
    pub explanation: String,
    /// The cleanup action this item *would* receive — never executed here.
    pub cleanup_action: CleanupAction,
    /// Supporting evidence for the discovery/classification.
    pub evidence: Vec<Evidence>,
    /// **Authorisation snapshot** captured at *scan* time (R01). When a real
    /// provider (with an injected [`crate::safety::probe::PathProbe`]) emits
    /// the item, it records the live identity/attributes/reparse state here.
    /// The planner prefers this record over a fresh capture, so a Scan→Plan
    /// replacement of the on-disk object is detected instead of silently
    /// re-authorising the new object. `None` for legacy fixtures/demo items.
    #[serde(default)]
    pub scan_snapshot: Option<TargetSnapshot>,
    /// R2-F06: the slug of the **classification** rule that last reclassified
    /// this item (F-2-1 `apply_rule_classification`), when any. Distinct from
    /// the discovery `Evidence.rule_id`: a Provider-sourced item may be
    /// reclassified by a user rule (risk/category rewritten, source kept), and
    /// that rule must still match at plan/execution time. Structured so the
    /// authorisation check can re-verify the exact rule, not just a source
    /// enum. `None` when the item carries its original provider classification.
    #[serde(default)]
    pub classification_rule_id: Option<String>,
}

impl ScanItem {
    /// Total logical bytes held by this item.
    #[must_use]
    pub const fn size_bytes(&self) -> u64 {
        self.logical_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{action::CleanupAction, category::ResidueCategory, evidence::Evidence};
    use crate::domain::{ids::ScanItemId, risk_level::RiskLevel, source::SourceKind};
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn sample_item() -> ScanItem {
        ScanItem {
            id: ScanItemId::from_raw(1),
            path: PathBuf::from(r"C:\Users\demo\.claude\projects\cache"),
            display_name: "Claude Code cache".into(),
            product: Some("Claude Code".into()),
            category: ResidueCategory::AiAgent,
            risk: RiskLevel::Safe,
            source: SourceKind::AgentProvider,
            logical_size: 128 * 1024 * 1024,
            file_count: 2048,
            last_modified: Some(SystemTime::now() - Duration::from_secs(86_400)),
            explanation: "Conversation replay cache; regenerable".into(),
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![Evidence::new("path-layout", "cache subfolder layout")],
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    #[test]
    fn constructs_with_explicit_semantics() {
        let item = sample_item();
        assert_eq!(item.id.raw(), 1);
        assert_eq!(item.size_bytes(), 128 * 1024 * 1024);
        assert_eq!(item.product.as_deref(), Some("Claude Code"));
        assert!(item.last_modified.is_some());
        assert_eq!(item.evidence.len(), 1);
    }

    #[test]
    fn defaults_use_explicit_none_not_fabricated_values() {
        let item = ScanItem {
            last_modified: None,
            product: None,
            evidence: Vec::new(),
            ..sample_item()
        };
        assert_eq!(item.product, None);
        assert_eq!(item.last_modified, None);
        assert!(item.evidence.is_empty());
        // Size/count fields are explicit u64s and stay as authored (0 is a
        // meaningful "unknown/not counted" state, never auto-filled).
        let zero = ScanItem {
            logical_size: 0,
            file_count: 0,
            ..sample_item()
        };
        assert_eq!(zero.logical_size, 0);
        assert_eq!(zero.file_count, 0);
    }

    #[test]
    fn id_does_not_change_when_path_is_replaced() {
        let mut item = sample_item();
        let id = item.id;
        item.path = PathBuf::from(r"D:\elsewhere");
        assert_eq!(item.id, id, "item id is independent of its path");
    }

    #[test]
    fn json_dto_has_stable_snake_case_fields() {
        let item = sample_item();
        let json = serde_json::to_string(&item).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = value.as_object().unwrap();
        for field in [
            "id",
            "path",
            "display_name",
            "product",
            "category",
            "risk",
            "source",
            "logical_size",
            "file_count",
            "last_modified",
            "explanation",
            "cleanup_action",
            "evidence",
        ] {
            assert!(obj.contains_key(field), "missing stable field {field}");
        }
        // kebab-case enum values inside the DTO.
        assert_eq!(obj["category"], "ai-agent");
        assert_eq!(obj["risk"], "safe");
        assert_eq!(obj["source"], "agent-provider");
        assert!(obj["last_modified"].is_number());
    }
}
