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
}
