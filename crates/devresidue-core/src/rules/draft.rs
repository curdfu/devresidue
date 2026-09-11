//! Exact-only AI rule drafts (Task 2 / AiAdvisor Phase A).
//!
//! # Boundary
//!
//! This module turns a user-confirmed [`AiRuleConfirmation`] — one trusted
//! [`ScanItem`] plus the user's final [`RiskLevel`] and a whitelisted
//! [`RuleProvenance`] — into the narrowest possible [`RuleDoc`]. The rule
//! grammar never comes from the model (DR-5): [`LocalRuleDraftBuilder::build`]
//! consumes only local, trusted `ScanItem` fields (path, product, evidence),
//! the user's closed-set final category, and the structured provenance. Model
//! `reason` / `product_guess` text is not part of the confirmation at all, so
//! it cannot influence the anchor, product or category.
//!
//! # Narrowness
//!
//! - The anchor is always `match.exact` on the scanned residue path (or a
//!   strictly-below provider/agent-root child anchor when structured evidence
//!   provides one); `glob` / `include` / `exclude` are always empty.
//! - Whole HOME / drive / protected / workspace / agent roots are rejected
//!   before the ordinary validator runs (INV-006 / SPEC §9).
//! - `Protected` final risk maps to `source: user-protected`; every other
//!   committable risk maps to `source: user`. `Unknown` is **not** a
//!   committable rule type (the user must pick a real classification).
//! - The draft id is deterministic: `user-ai/<canonical-suggestion-uuid>`, so
//!   re-submitting the same suggestion is idempotent.
//!
//! Every draft passes through the existing [`validate_rule_file`] before it is
//! returned, so a dangerous anchor or an inconsistent provenance fails closed
//! at build time, long before any file write.

use std::path::{Path, PathBuf};

use crate::ai::RuleProvenance;
use crate::domain::scan_item::ScanItem;
use crate::rules::matcher::expand_env;
use crate::rules::priority::RuleSource;
use crate::rules::schema::{MatchSpec, RuleDoc, RuleFile};
use crate::rules::validator::{
    validate_rule_file, AGENT_ROOT_DIR_NAMES, PROTECTED_ROOT_ENV_VARS,
};
use crate::safety::canonical;
use crate::{ResidueCategory, RiskLevel};

/// One user-confirmed AI review decision: the trusted scanned item, the risk
/// the user finally chose, and the whitelisted provenance to persist with the
/// rule. There is deliberately no `reason` / `product_guess` field: model text
/// is display-only and never becomes a rule field.
#[derive(Debug, Clone, PartialEq)]
pub struct AiRuleConfirmation {
    /// The trusted scan item whose residue the user decided to classify.
    pub item: ScanItem,
    /// The risk the user finally chose (never `Unknown`).
    pub final_risk: RiskLevel,
    /// The residue category the user finally chose (never `Unknown`).
    ///
    /// This is deliberately independent from the original scan category: AI
    /// review starts with `Unknown` candidates, and the remote model must not
    /// be allowed to supply this value.
    pub final_category: ResidueCategory,
    /// Non-sensitive AiAdvisor provenance attached to the resulting rule.
    pub provenance: RuleProvenance,
}

/// Builder that turns a confirmation into the narrowest exact-only [`RuleDoc`].
///
/// The zero-sized builder reads `%VAR%`/`${VAR}` values from the real process
/// environment for the root-safety checks; tests may inject a deterministic
/// lookup through the crate-visible [`LocalRuleDraftBuilder::build_with_lookup`]
/// so root-rejection behaviour is fully reproducible.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalRuleDraftBuilder;

/// Re-verifies a candidate user rule file against the confirmations before the
/// transaction is allowed to replace the live file (and again after the
/// replacement is reloaded).
///
/// Implementations compile the complete candidate documents with the existing
/// validation/compilation semantics and then prove, for every confirmation,
/// that the item's path resolves to its generated rule id with exactly the
/// user's final risk. The transaction is all-or-nothing: a verifier rejection
/// rolls the whole batch back.
pub trait CandidateRuleVerifier {
    /// Returns `Ok` only when every confirmation is faithfully represented by
    /// `candidate_user_file`.
    fn verify(
        &self,
        candidate_user_file: &RuleFile,
        confirmations: &[AiRuleConfirmation],
    ) -> Result<(), String>;
}

/// Fixed, content-free description for AI-written rules. A description must not
/// echo a raw path, the model's reason or any model text, so it carries no
/// dynamic content at all.
const AI_RULE_DESCRIPTION: &str = "user classification (from ai-advisor review)";

impl LocalRuleDraftBuilder {
    /// Builds the narrowest exact rule for `confirmation`, using the real
    /// process environment for root-safety checks.
    pub fn build(&self, confirmation: &AiRuleConfirmation) -> Result<RuleDoc, String> {
        self.build_with_lookup(confirmation, &|name| std::env::var(name).ok())
    }

    /// Same as [`LocalRuleDraftBuilder::build`] but with an explicit
    /// environment lookup, so tests are fully deterministic.
    pub(crate) fn build_with_lookup(
        &self,
        confirmation: &AiRuleConfirmation,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<RuleDoc, String> {
        // Unknown is not a committable classification: the review UI must make
        // the user pick a real final risk before this builder is ever called.
        if confirmation.final_risk == RiskLevel::Unknown {
            return Err("Unknown cannot be committed as an AI rule".to_string());
        }
        if confirmation.final_category == ResidueCategory::Unknown {
            return Err("Unknown category cannot be committed as an AI rule".to_string());
        }
        if confirmation.provenance.user_final_risk != confirmation.final_risk {
            return Err(format!(
                "provenance.user_final_risk `{:?}` must equal the user's final risk `{:?}`",
                confirmation.provenance.user_final_risk, confirmation.final_risk
            ));
        }

        let source = match confirmation.final_risk {
            RiskLevel::Protected => RuleSource::UserProtected,
            _ => RuleSource::User,
        };

        // The narrowest trusted anchor: a structured evidence child anchor
        // strictly below a provider/agent root when one is available, otherwise
        // the scanned residue path itself (which is already a leaf directory in
        // the current data model).
        let anchor = evidence_child_anchor(&confirmation.item)
            .unwrap_or_else(|| confirmation.item.path.clone());
        reject_dangerous_anchor(&anchor, lookup)?;
        let match_exact = anchor.to_string_lossy().into_owned();

        let rule = RuleDoc {
            id: format!("user-ai/{}", confirmation.provenance.suggestion_id.as_str()),
            description: AI_RULE_DESCRIPTION.to_string(),
            product: confirmation.item.product.clone(),
            category: confirmation.final_category,
            risk: confirmation.final_risk,
            source,
            match_spec: MatchSpec {
                exact: Some(match_exact),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: Vec::new(),
            exclude: Vec::new(),
            provenance: Some(confirmation.provenance.clone()),
        };

        // F-M4-1 style write-time gate: refuse the draft when the rule would
        // not survive the loader's own validation (dangerous roots,
        // source/risk inconsistency, provenance mismatch, non-absolute anchor).
        let single = RuleFile {
            rules: vec![rule.clone()],
        };
        let issues = validate_rule_file(&single, source, lookup);
        if let Some(issue) = issues.into_iter().find(|i| i.is_error()) {
            return Err(format!(
                "draft rule `{}` rejected before write: {}",
                rule.id, issue.message
            ));
        }
        Ok(rule)
    }
}

/// Returns a strictly-below provider/agent-root child anchor when the item's
/// structured evidence names one, otherwise `None` (the caller anchors the item
/// path itself).
///
/// In the current data model every provider/agent residue is already reported
/// as a concrete leaf directory, so no structured evidence kind carries a
/// *narrower* trusted child path. This is kept as a single decision point so a
/// future provider that reports a broad managed root can narrow the anchor here
/// without touching the transaction path.
fn evidence_child_anchor(_item: &ScanItem) -> Option<PathBuf> {
    None
}

/// Rejects anchors that would over-classify a protected area (INV-006 / SPEC
/// §9): relative paths, drive / UNC share roots, the whole HOME / system /
/// program protected roots, the whole `%LOCALAPPDATA%` / `%APPDATA%` user-data
/// roots and whole AI agent roots under `%USERPROFILE%`.
///
/// The authoritative safety net remains [`validate_rule_file`] (the builder
/// runs it afterwards); these are clearer, targeted early messages.
fn reject_dangerous_anchor(
    anchor: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let display = anchor.to_string_lossy().into_owned();

    if !canonical::is_absolute(anchor) {
        return Err(format!(
            "refusing to anchor an AI rule on a relative path `{display}`"
        ));
    }
    if canonical::unc_share_root(anchor).is_some() {
        return Err(format!(
            "refusing to anchor an AI rule on a UNC share root `{display}` (INV-006)"
        ));
    }
    let norm = norm_text(anchor);
    if is_drive_root(&norm) {
        return Err(format!(
            "refusing to anchor an AI rule on a drive root `{display}` (INV-006)"
        ));
    }

    // Whole protected / user-data roots (expanded from env templates).
    for template in PROTECTED_ROOT_ENV_VARS
        .iter()
        .copied()
        .chain(["%LOCALAPPDATA%", "%APPDATA%"])
    {
        if let Ok(root) = expand_env(template, lookup) {
            if canonical::normalized_eq_path(anchor, PathBuf::from(root)) {
                return Err(format!(
                    "refusing to anchor an AI rule on the whole protected root `{template}` \
                     (INV-006): `{display}`"
                ));
            }
        }
    }

    // Whole AI agent roots under %USERPROFILE% (SPEC §9): an AI rule must
    // classify a fine-grained cache/log/session item, never the whole agent.
    if let Some(home) = lookup("USERPROFILE") {
        let home_root = PathBuf::from(home);
        for agent in AGENT_ROOT_DIR_NAMES {
            let root = home_root.join(agent);
            if canonical::normalized_eq_path(anchor, &root) {
                return Err(format!(
                    "refusing to anchor an AI rule on the whole agent root `%USERPROFILE%\\{agent}` \
                     (SPEC §9): split agent roots into fine-grained cache/log/session items"
                ));
            }
        }
    }
    Ok(())
}

/// Lexically normalises a path to slash-separated text with no trailing slash
/// (the same shape `rules::validator` uses for its root checks).
fn norm_text(path: &Path) -> String {
    let canon = canonical::normalize(path);
    let text = crate::rules::matcher::normalize_slashes(&canon.to_string_lossy());
    text.trim_end_matches('/').to_string()
}

/// True when slash-normalised `text` is a bare drive root (`C:`).
fn is_drive_root(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiProfileId, AiSuggestionId, RuleOrigin};
    use crate::domain::source::SourceKind;
    use crate::{ResidueCategory, ScanItemId};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    const PROFILE_CANON: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";
    const SUGGESTION_CANON: &str = "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f";

    fn fake_env() -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = [
            ("USERPROFILE", r"C:\Users\me"),
            ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
            ("APPDATA", r"C:\Users\me\AppData\Roaming"),
            ("SYSTEMROOT", r"C:\Windows"),
            ("PROGRAMFILES", r"C:\Program Files"),
            ("PROGRAMFILES(X86)", r"C:\Program Files (x86)"),
            ("PROGRAMDATA", r"C:\ProgramData"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        move |name: &str| map.get(&name.to_ascii_uppercase()).cloned()
    }

    fn provenance(user_final_risk: RiskLevel) -> RuleProvenance {
        RuleProvenance {
            origin: RuleOrigin::AiAdvisor,
            profile_id: AiProfileId::parse(PROFILE_CANON).unwrap(),
            scan_generation: 7,
            suggestion_id: AiSuggestionId::parse(SUGGESTION_CANON).unwrap(),
            user_final_risk,
            created_at_epoch_secs: 1_760_000_000,
        }
    }

    fn item(path: &str, category: ResidueCategory, risk: RiskLevel) -> ScanItem {
        ScanItem {
            id: ScanItemId::from_raw(1),
            path: PathBuf::from(path),
            display_name: "reviewed residue".to_string(),
            product: None,
            category,
            risk,
            source: SourceKind::UnknownProvider,
            logical_size: 1024,
            file_count: 1,
            last_modified: Some(SystemTime::now() - Duration::from_secs(86_400)),
            explanation: "trusted local discovery evidence; not model text".to_string(),
            cleanup_action: crate::CleanupAction::None,
            evidence: Vec::new(),
            scan_snapshot: None,
            classification_rule_id: None,
        }
    }

    fn confirmation(path: &str, final_risk: RiskLevel) -> AiRuleConfirmation {
        AiRuleConfirmation {
            item: item(path, ResidueCategory::DeveloperCache, RiskLevel::Unknown),
            final_risk,
            final_category: ResidueCategory::DeveloperCache,
            provenance: provenance(final_risk),
        }
    }

    fn build_with_fake_env(confirmation: &AiRuleConfirmation) -> Result<RuleDoc, String> {
        LocalRuleDraftBuilder.build_with_lookup(confirmation, &fake_env())
    }

    #[test]
    fn draft_for_ai_confirmation_uses_exact_match_never_glob() {
        let rule =
            build_with_fake_env(&confirmation(r"C:\Users\me\.tool\cache", RiskLevel::Safe)).unwrap();
        assert_eq!(
            rule.match_spec.exact.as_deref(),
            Some(r"C:\Users\me\.tool\cache")
        );
        assert!(rule.match_spec.glob.is_none());
        assert!(rule.include.is_empty());
        assert!(rule.exclude.is_empty());
        assert_eq!(
            rule.id,
            format!("user-ai/{SUGGESTION_CANON}"),
            "draft id must be deterministic user-ai/<canonical-suggestion-uuid>"
        );
        assert_eq!(rule.risk, RiskLevel::Safe);
        assert_eq!(rule.source, RuleSource::User);
        assert_eq!(rule.category, ResidueCategory::DeveloperCache);
        // Description never carries a raw path / reason / model text.
        assert!(!rule.description.contains(r"C:\Users\me"));
    }

    #[test]
    fn draft_uses_user_selected_category_instead_of_unknown_scan_category() {
        let mut confirmation = confirmation(r"C:\Users\me\.tool\cache", RiskLevel::Safe);
        confirmation.item.category = ResidueCategory::Unknown;
        confirmation.final_category = ResidueCategory::DeveloperCache;

        let rule = build_with_fake_env(&confirmation).unwrap();
        assert_eq!(rule.category, ResidueCategory::DeveloperCache);
    }

    #[test]
    fn draft_description_and_product_never_come_from_model_text() {
        // Model reason/product_guess are not even part of the confirmation.
        // Anchor/product/category all come from the trusted ScanItem only —
        // extra evidence or explanation text cannot redirect the anchor.
        let mut conf = confirmation(r"C:\Users\me\.tool\cache", RiskLevel::Safe);
        conf.item.evidence = vec![crate::Evidence::new(
            "layout",
            r"model-ish text pointing at C:\Elsewhere\wrong",
        )];
        let rule = build_with_fake_env(&conf).unwrap();
        assert_eq!(
            rule.match_spec.exact.as_deref(),
            Some(r"C:\Users\me\.tool\cache")
        );
        assert_eq!(rule.product, None);
        assert_eq!(rule.category, ResidueCategory::DeveloperCache);
    }

    #[test]
    fn unknown_cannot_be_committed_as_an_ai_rule() {
        let err = build_with_fake_env(&confirmation(r"C:\Users\me\.tool\cache", RiskLevel::Unknown))
            .unwrap_err();
        assert!(err.contains("Unknown cannot be committed"), "{err}");
    }

    #[test]
    fn unknown_category_cannot_be_committed_as_an_ai_rule() {
        let mut confirmation = confirmation(r"C:\Users\me\.tool\cache", RiskLevel::Safe);
        confirmation.final_category = ResidueCategory::Unknown;

        let err = build_with_fake_env(&confirmation).unwrap_err();
        assert!(err.contains("Unknown category cannot be committed"), "{err}");
    }

    #[test]
    fn protected_final_risk_maps_to_user_protected() {
        let rule = build_with_fake_env(&confirmation(
            r"C:\Users\me\.tool\config-store",
            RiskLevel::Protected,
        ))
        .unwrap();
        assert_eq!(rule.source, RuleSource::UserProtected);
        assert_eq!(rule.risk, RiskLevel::Protected);
        assert_eq!(
            rule.match_spec.exact.as_deref(),
            Some(r"C:\Users\me\.tool\config-store")
        );
        let prov = rule.provenance.as_ref().expect("provenance present");
        assert_eq!(prov.origin, RuleOrigin::AiAdvisor);
        assert_eq!(prov.user_final_risk, RiskLevel::Protected);
    }

    #[test]
    fn review_and_regenerable_risks_map_to_user_detection() {
        for (risk, _) in [
            (RiskLevel::Safe, "safe"),
            (RiskLevel::RegenerableLocal, "regenerable-local"),
            (RiskLevel::RegenerableDownload, "regenerable-download"),
            (RiskLevel::Review, "review"),
        ] {
            let rule = build_with_fake_env(&confirmation(r"C:\Users\me\.cache\x", risk)).unwrap();
            assert_eq!(rule.source, RuleSource::User, "{risk:?}");
            assert_eq!(rule.risk, risk, "{risk:?}");
        }
    }

    #[test]
    fn provenance_risk_mismatch_is_rejected() {
        let mut conf = confirmation(r"C:\Users\me\.tool\cache", RiskLevel::Safe);
        conf.provenance = provenance(RiskLevel::Protected); // disagrees with final_risk
        let err = build_with_fake_env(&conf).unwrap_err();
        assert!(err.contains("user_final_risk"), "{err}");
    }

    #[test]
    fn drive_root_relative_and_share_root_anchors_are_rejected() {
        for path in [r"C:\", r"relative\path", r"\\srv\share"] {
            let err = build_with_fake_env(&confirmation(path, RiskLevel::Safe)).unwrap_err();
            assert!(
                err.contains("refusing to anchor") || err.contains("rejected before write"),
                "path {path} must be rejected, got {err}"
            );
        }
    }

    #[test]
    fn whole_home_and_protected_roots_are_rejected() {
        for path in [
            r"C:\Users\me",
            r"C:\Windows",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me\AppData\Roaming",
        ] {
            let err = build_with_fake_env(&confirmation(path, RiskLevel::Safe)).unwrap_err();
            assert!(
                err.contains("whole protected root") || err.contains("rejected before write"),
                "path {path} must be rejected as a whole protected root, got {err}"
            );
        }
    }

    #[test]
    fn whole_agent_root_is_rejected_even_when_not_safe_risk() {
        // SPEC §9's built-in guard only rejects `safe`; the AI builder must
        // reject a whole agent root for *every* committable risk (a protect on
        // the whole agent root is still too broad for an exact AI rule).
        for risk in [
            RiskLevel::Safe,
            RiskLevel::Protected,
            RiskLevel::Review,
            RiskLevel::RegenerableDownload,
        ] {
            let err = build_with_fake_env(&confirmation(r"C:\Users\me\.claude", risk)).unwrap_err();
            assert!(
                err.contains("whole agent root"),
                "whole agent root at {risk:?} must be rejected, got {err}"
            );
        }
    }

    #[test]
    fn fine_grained_agent_subdirectory_is_allowed() {
        for path in [
            r"C:\Users\me\.claude\shell-snapshots",
            r"C:\Users\me\.claude\projects",
            r"C:\Users\me\.codex\history",
        ] {
            let rule =
                build_with_fake_env(&confirmation(path, RiskLevel::Safe)).unwrap_or_else(|e| {
                    panic!("fine-grained agent subdir {path} must pass: {e}")
                });
            assert_eq!(rule.match_spec.exact.as_deref(), Some(path));
        }
    }
}
