//! Rule file schema (YAML).
//!
//! Schema contract (deny unknown fields everywhere, so a rule carrying
//! `external_command:` / shell strings is rejected by the parser — SPEC §22).
//!
//! ```yaml
//! rules:
//!   - id: builtin-protected/ssh          # globally unique slug <source>/<name>
//!     description: "SSH credentials"
//!     product: OpenSSH
//!     category: credential               # kebab-case ResidueCategory
//!     risk: protected                    # kebab-case RiskLevel
//!     source: builtin-protected          # RuleSource
//!     match:
//!       exact: "%USERPROFILE%/.ssh"      # or glob / parent_marker + exists
//!     include: []                        # optional sub-glob allow-list
//!     exclude: []                        # optional sub-glob deny-list
//! ```
//!
//! # Semantics of the `match` spec
//!
//! Conditions inside `match` are ANDed:
//!
//! - `exact` *or* `glob` is required (mutually exclusive) and anchors the
//!   rule to an absolute path (after `%VAR%` / `${VAR}` expansion).
//! - `parent_marker` (when set): the **parent directory of the candidate**
//!   must contain a file/dir named `<marker>`.
//! - `exists: true`: the candidate path itself must exist.
//!
//! `include` / `exclude` are **relative sub-globs** interpreted against the
//! rule's anchor root. If `include` is non-empty only sub-paths matching one
//! of its patterns match the rule; any sub-path matching an `exclude` pattern
//! never matches the rule. They refine scans that visit children of the root.

use serde::{Deserialize, Serialize};

use crate::{ResidueCategory, RiskLevel};

use super::priority::RuleSource;

/// Top-level shape of a rules YAML file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleFile {
    /// Ordered list of rules in this file.
    pub rules: Vec<RuleDoc>,
}

/// One declarative rule document (pre-validation / pre-compilation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDoc {
    /// Globally unique rule slug, convention `<source>/<name>`.
    pub id: String,
    /// Human-readable explanation used by ScanItem.explanation later.
    pub description: String,
    /// Owning product/tool (shown in listings).
    #[serde(default)]
    pub product: Option<String>,
    /// Residue category this rule classifies hits into.
    pub category: ResidueCategory,
    /// Risk level this rule assigns to hits.
    pub risk: RiskLevel,
    /// Rule source; determines priority (SPEC §14) and must match the rules
    /// directory the file lives in.
    pub source: RuleSource,
    /// Path matching conditions (see module docs).
    #[serde(rename = "match")]
    pub match_spec: MatchSpec,
    /// Optional relative sub-glob allow-list against the anchor root.
    #[serde(default)]
    pub include: Vec<String>,
    /// Optional relative sub-glob deny-list against the anchor root.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Optional AiAdvisor provenance (Phase A). Absent for every legacy /
    /// hand-authored / heuristic rule, so old files parse unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::ai::RuleProvenance>,
}

/// The `match:` block of a rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchSpec {
    /// Exact absolute path equality (after env expansion).
    pub exact: Option<String>,
    /// Glob pattern over an absolute path (after env expansion).
    pub glob: Option<String>,
    /// Name of a marker entry that must exist in the candidate's parent.
    pub parent_marker: Option<String>,
    /// When true the candidate itself must exist.
    #[serde(default)]
    pub exists: bool,
}

impl MatchSpec {
    /// True when the rule anchors via `exact`.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        self.exact.is_some()
    }

    /// True when the rule anchors via `glob`.
    #[must_use]
    pub const fn is_glob(&self) -> bool {
        self.glob.is_some()
    }

    /// True when neither `exact` nor `glob` is present (invalid per schema
    /// semantics — a rule must be anchored somewhere).
    #[must_use]
    pub const fn has_no_anchor(&self) -> bool {
        self.exact.is_none() && self.glob.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RiskLevel;

    const GOOD: &str = r#"
rules:
  - id: builtin-protected/ssh
    description: SSH credentials
    product: OpenSSH
    category: credential
    risk: protected
    source: builtin-protected
    match:
      exact: "%USERPROFILE%/.ssh"
"#;

    #[test]
    fn parses_canonical_document() {
        let file: RuleFile = serde_yaml_ng::from_str(GOOD).unwrap();
        assert_eq!(file.rules.len(), 1);
        let rule = &file.rules[0];
        assert_eq!(rule.id, "builtin-protected/ssh");
        assert_eq!(rule.category, ResidueCategory::Credential);
        assert_eq!(rule.risk, RiskLevel::Protected);
        assert_eq!(rule.source, RuleSource::BuiltinProtected);
        assert!(rule.match_spec.is_exact());
        assert!(!rule.match_spec.is_glob());
        assert!(rule.include.is_empty() && rule.exclude.is_empty());
    }

    #[test]
    fn rejects_unknown_fields_including_external_command() {
        // SPEC §22: rules must never be able to carry an external command /
        // shell string. deny_unknown_fields makes this a hard parse error.
        for payload in [
            r#"
rules:
  - id: evil
    description: injected command
    product: x
    category: temporary
    risk: safe
    source: builtin-detection
    match: { exact: "C:/x" }
    external_command: "rm -rf /"
"#,
            r#"
rules:
  - id: evil2
    description: injected shell field
    product: x
    category: temporary
    risk: safe
    source: builtin-detection
    match:
      exact: "C:/x"
      run_script: "del /q"
"#,
            r#"
external_command: "powershell -c evil"
rules: []
"#,
        ] {
            let err = serde_yaml_ng::from_str::<RuleFile>(payload).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("external_command") || msg.contains("run_script"),
                "unexpected error: {msg}"
            );
        }
    }

    #[test]
    fn rejects_unknown_field_inside_match() {
        let payload = r#"
rules:
  - id: r
    description: d
    category: temporary
    risk: safe
    source: builtin-detection
    match:
      exact: "C:/x"
      fuzz: true
"#;
        assert!(serde_yaml_ng::from_str::<RuleFile>(payload).is_err());
    }

    #[test]
    fn rejects_missing_required_fields() {
        // Missing risk/category/source/match must fail deserialisation.
        let payload = r#"
rules:
  - id: incomplete
    description: missing everything else
"#;
        assert!(serde_yaml_ng::from_str::<RuleFile>(payload).is_err());
    }

    // ---- optional `provenance` (AiAdvisor Phase A, backward compatible) ----

    const LEGACY_RULE_YAML: &str = r#"
rules:
  - id: builtin-protected/ssh
    description: SSH credentials
    product: OpenSSH
    category: credential
    risk: protected
    source: builtin-protected
    match:
      exact: "%USERPROFILE%/.ssh"
"#;

    const AI_PROVENANCE_YAML: &str = r#"
rules:
  - id: user-ai/2026-01-01-0001
    description: "user classification (from ai-advisor review): tool cache"
    category: developer-cache
    risk: safe
    source: user
    provenance:
      origin: ai-advisor
      profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1760000000
    match:
      exact: "C:/Users/demo/.tool/cache"
"#;

    #[test]
    fn legacy_rule_without_provenance_still_parses() {
        let doc: RuleFile = serde_yaml_ng::from_str(LEGACY_RULE_YAML).unwrap();
        assert_eq!(doc.rules.len(), 1);
        assert!(doc.rules[0].provenance.is_none());
    }

    #[test]
    fn rule_with_provenance_parses_and_round_trips() {
        let file: RuleFile = serde_yaml_ng::from_str(AI_PROVENANCE_YAML).unwrap();
        let prov = file.rules[0]
            .provenance
            .as_ref()
            .expect("provenance must be present");
        assert_eq!(prov.origin, crate::ai::RuleOrigin::AiAdvisor);
        assert_eq!(
            prov.profile_id.as_str(),
            "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72"
        );
        assert_eq!(prov.scan_generation, 7);
        assert_eq!(
            prov.suggestion_id.as_str(),
            "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f"
        );
        assert_eq!(prov.user_final_risk, RiskLevel::Safe);
        assert_eq!(prov.created_at_epoch_secs, 1_760_000_000);

        // YAML round trip preserves provenance exactly.
        let text = serde_yaml_ng::to_string(&file).unwrap();
        let back: RuleFile = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(back.rules[0].provenance, file.rules[0].provenance);
    }

    #[test]
    fn provenance_with_a_path_or_secret_field_is_rejected() {
        // RuleProvenance is deny_unknown_fields: a path/reason/model/key/
        // request/response field is a hard parse error (never silently kept).
        for payload in [
            r#"
rules:
  - id: r
    description: d
    category: temporary
    risk: safe
    source: user
    provenance:
      origin: ai-advisor
      profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1
      path: "C:/Users/demo/.ssh"
    match: { exact: "C:/x" }
"#,
            r#"
rules:
  - id: r2
    description: d
    category: temporary
    risk: safe
    source: user
    provenance:
      origin: ai-advisor
      profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1
      reason: "because node_modules is rebuildable"
    match: { exact: "C:/x" }
"#,
            r#"
rules:
  - id: r3
    description: d
    category: temporary
    risk: safe
    source: user
    provenance:
      origin: ai-advisor
      profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1
      api_key: "sk-test-never-persist"
    match: { exact: "C:/x" }
"#,
            r#"
rules:
  - id: r4
    description: d
    category: temporary
    risk: safe
    source: user
    provenance:
      origin: ai-advisor
      profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1
      request: "{ \"raw\": true }"
    match: { exact: "C:/x" }
"#,
        ] {
            assert!(
                serde_yaml_ng::from_str::<RuleFile>(payload).is_err(),
                "provenance with a non-whitelisted field must be rejected"
            );
        }
    }

    #[test]
    fn provenance_with_invalid_profile_id_or_origin_is_rejected_at_parse() {
        // profile_id must be canonical UUID text (AiProfileId parse).
        let bad_profile = r#"
rules:
  - id: r
    description: d
    category: temporary
    risk: safe
    source: user
    provenance:
      origin: ai-advisor
      profile_id: not-a-uuid
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1
    match: { exact: "C:/x" }
"#;
        assert!(serde_yaml_ng::from_str::<RuleFile>(bad_profile).is_err());

        // Only `origin: ai-advisor` exists; anything else fails deserialisation.
        let bad_origin = r#"
rules:
  - id: r2
    description: d
    category: temporary
    risk: safe
    source: user
    provenance:
      origin: builtin
      profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72
      scan_generation: 7
      suggestion_id: 9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f
      user_final_risk: safe
      created_at_epoch_secs: 1
    match: { exact: "C:/x" }
"#;
        assert!(serde_yaml_ng::from_str::<RuleFile>(bad_origin).is_err());
    }

    #[test]
    fn provenance_rejects_free_text_suggestion_id_at_parse() {
        // Fix-round 1: suggestion_id must be a canonical UUID, never free
        // text. A path, a key-looking value, a multi-line response string and
        // an empty string are all rejected at the serde parse layer (the value
        // is parsed through AiSuggestionId, not a raw String).
        let suggestion_uuid = "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f";
        // suggestion_id is rendered as a literal block scalar so backslashes,
        // quotes and newlines in the candidate are preserved verbatim (no YAML
        // escape ambiguity) and the parse error, when it comes, is guaranteed
        // to be about the suggestion_id value itself.
        let mk = |suggestion_id: &str| {
            let mut s = String::from(
                "rules:\n  - id: r\n    description: d\n    category: temporary\n    \
                 risk: safe\n    source: user\n    provenance:\n      origin: ai-advisor\n      \
                 profile_id: 018f7e21-9d15-7b17-a5fd-4f0f2bcadc72\n      scan_generation: 7\n      \
                 suggestion_id: |-\n",
            );
            for line in suggestion_id.split('\n') {
                s.push_str("        ");
                s.push_str(line);
                s.push('\n');
            }
            s.push_str("      user_final_risk: safe\n      created_at_epoch_secs: 1\n    match:\n      exact: \"C:/x\"\n");
            s
        };
        for bad in [
            r"C:\Users\alice\.ssh",
            "sk-secret-value",
            r#"{"role":"system","content":"multi line"}"#,
            "",
        ] {
            let result = serde_yaml_ng::from_str::<RuleFile>(&mk(bad));
            let err = match result {
                Err(e) => e,
                Ok(_) => panic!("free-text suggestion_id must be rejected, got {bad:?}"),
            };
            // serde_yaml_ng reports the provenance sub-object path (it does not
            // descend into the custom suggestion_id parser), so the definitive
            // signal is the AiSuggestionId parse error itself.
            let msg = err.to_string();
            assert!(
                msg.contains("invalid AI suggestion id"),
                "expected the AiSuggestionId parse error for {bad:?}, got: {msg}"
            );
        }

        // The canonical value round-trips through a real rule document.
        let file: RuleFile = serde_yaml_ng::from_str(&mk(suggestion_uuid)).unwrap();
        assert_eq!(file.rules[0].provenance.as_ref().unwrap().suggestion_id.as_str(), suggestion_uuid);
    }
}
