//! Rule source priority and winner resolution (SPEC §14).

use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::{ResidueCategory, RiskLevel, RuleId};

use super::{loader::CompiledRule, matcher::rule_matches};

/// Where a rule came from. Priority order is fixed by SPEC §14:
///
/// ```text
/// Built-in Protected > User Protected > User > Community >
/// Built-in Detection > AI Suggestion
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleSource {
    /// Built-in rules protecting credentials/roots (folder `protected/`).
    BuiltinProtected,
    /// User-authored protected rules.
    UserProtected,
    /// User-authored detection rules.
    User,
    /// Community-contributed detection rules.
    Community,
    /// Built-in detection rules shipped with the product
    /// (folders `agents/ ide/ dev_cache/ packages/`).
    BuiltinDetection,
    /// AI analyzer suggestions (Phase 14).
    AiSuggestion,
}

impl RuleSource {
    /// Rank used for resolution: lower wins. Follows SPEC §14 exactly.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            RuleSource::BuiltinProtected => 0,
            RuleSource::UserProtected => 1,
            RuleSource::User => 2,
            RuleSource::Community => 3,
            RuleSource::BuiltinDetection => 4,
            RuleSource::AiSuggestion => 5,
        }
    }
}

impl fmt::Display for RuleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            RuleSource::BuiltinProtected => "Builtin Protected",
            RuleSource::UserProtected => "User Protected",
            RuleSource::User => "User",
            RuleSource::Community => "Community",
            RuleSource::BuiltinDetection => "Builtin Detection",
            RuleSource::AiSuggestion => "AI Suggestion",
        };
        f.write_str(label)
    }
}

/// The winning rule for a candidate path, carrying everything a later layer
/// needs to populate `ScanItem` (category/risk/product/explanation) and
/// `Evidence` (slug + numeric [`RuleId`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRule {
    /// Rule slug (globally unique).
    pub id: String,
    /// Numeric rule id (registry-assigned; used by `Evidence.rule_id`).
    pub rule_id: RuleId,
    /// Winning source.
    pub source: RuleSource,
    /// Risk assigned by the rule.
    pub risk: RiskLevel,
    /// Category assigned by the rule.
    pub category: ResidueCategory,
    /// Product owning the data (when the rule names one).
    pub product: Option<String>,
    /// Rule description (explainability).
    pub description: String,
}

/// Resolves the best matching rule for `candidate` among `rules`.
///
/// Tie-break order (after source priority):
///
/// 1. **specificity**: `exact` beats `glob`; longer glob patterns beat shorter
///    ones (a deeper path is more specific);
/// 2. **declaration order**: the rule declared first in the load order wins.
///
/// Returns `None` when no rule matches (the caller decides how to classify the
/// item, e.g. `RiskLevel::Unknown`).
#[must_use]
pub fn resolve(candidate: &Path, rules: &[CompiledRule]) -> Option<ResolvedRule> {
    let mut best: Option<&CompiledRule> = None;
    for rule in rules {
        if !rule_matches(rule, candidate) {
            continue;
        }
        let replace = match best {
            None => true,
            Some(current) => better(rule, current),
        };
        if replace {
            best = Some(rule);
        }
    }
    best.map(|rule| ResolvedRule {
        id: rule.id.clone(),
        rule_id: rule.numeric_id,
        source: rule.source,
        risk: rule.risk,
        category: rule.category,
        product: rule.product.clone(),
        description: rule.description.clone(),
    })
}

/// True when `a` outranks `b` under (source rank, specificity, order).
fn better(a: &CompiledRule, b: &CompiledRule) -> bool {
    match a.source.rank().cmp(&b.source.rank()) {
        std::cmp::Ordering::Less => return true,
        std::cmp::Ordering::Greater => return false,
        std::cmp::Ordering::Equal => {}
    }
    match specificity(a).cmp(&specificity(b)) {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    a.order < b.order
}

/// Specificity score: higher wins. `exact` > any glob; globs are ranked by
/// pattern length (longer == deeper/more constrained == more specific).
fn specificity(rule: &CompiledRule) -> usize {
    match &rule.pattern {
        super::loader::CompiledPattern::Exact { .. } => usize::MAX,
        super::loader::CompiledPattern::Glob { pattern_norm, .. } => pattern_norm.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{
        loader::{CompiledPattern, CompiledRule},
        matcher::compile_glob,
    };

    /// Builds a compiled rule without going through the loader (resolve-only).
    fn rule(
        id: &str,
        numeric: u64,
        source: RuleSource,
        risk: RiskLevel,
        pattern: CompiledPattern,
        order: usize,
    ) -> CompiledRule {
        CompiledRule {
            id: id.into(),
            numeric_id: RuleId::from_raw(numeric),
            source,
            risk,
            category: ResidueCategory::Temporary,
            product: None,
            description: id.into(),
            pattern,
            parent_marker: None,
            requires_existing: false,
            include_raw: Vec::new(),
            exclude_raw: Vec::new(),
            include_matchers: Vec::new(),
            exclude_matchers: Vec::new(),
            order,
        }
    }

    fn glob_rule(
        id: &str,
        numeric: u64,
        source: RuleSource,
        pattern: &str,
        order: usize,
    ) -> CompiledRule {
        rule(
            id,
            numeric,
            source,
            RiskLevel::Safe,
            CompiledPattern::Glob {
                matcher: compile_glob(pattern).expect("valid glob"),
                pattern_norm: pattern.to_string(),
                root: pattern
                    .split(['*', '?'])
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .to_string(),
            },
            order,
        )
    }

    #[test]
    fn source_priority_wins_over_specificity() {
        // Same target: a Detection glob is more specific but a Protected
        // exact rule outranks it purely on source (SPEC §14).
        let detection = glob_rule(
            "detection/cache",
            1,
            RuleSource::BuiltinDetection,
            "c:/root/cache/**",
            0,
        );
        let protected = rule(
            "protected/cache",
            2,
            RuleSource::BuiltinProtected,
            RiskLevel::Protected,
            CompiledPattern::Exact {
                target: "c:/root/cache".into(),
            },
            1,
        );
        let hit = resolve(Path::new(r"c:\root\cache"), &[detection, protected]).unwrap();
        assert_eq!(hit.id, "protected/cache");
        assert_eq!(hit.risk, RiskLevel::Protected);
    }

    #[test]
    fn longer_glob_beats_shorter_glob_on_same_source() {
        let broad = glob_rule(
            "detection/broad",
            1,
            RuleSource::BuiltinDetection,
            "c:/root/**",
            0,
        );
        let narrow = glob_rule(
            "detection/narrow",
            2,
            RuleSource::BuiltinDetection,
            "c:/root/x/*",
            1,
        );
        // Candidate matches both patterns.
        let hit = resolve(Path::new(r"c:\root\x\y"), &[broad, narrow]).unwrap();
        assert_eq!(hit.id, "detection/narrow");
    }

    #[test]
    fn declaration_order_breaks_exact_ties() {
        // Identical rules cannot both exist (unique ids), but two globs with
        // equal specificity on the same candidate are resolved by order.
        let first = glob_rule(
            "detection/first",
            1,
            RuleSource::BuiltinDetection,
            "c:/root/x/**",
            0,
        );
        let second = glob_rule(
            "detection/second",
            2,
            RuleSource::BuiltinDetection,
            "c:/root/x/**",
            1,
        );
        let hit = resolve(Path::new(r"c:\root\x\deep"), &[second, first]).unwrap();
        assert_eq!(hit.id, "detection/first");
    }

    #[test]
    fn rank_follows_spec_section_14() {
        assert!(RuleSource::BuiltinProtected.rank() < RuleSource::UserProtected.rank());
        assert!(RuleSource::UserProtected.rank() < RuleSource::User.rank());
        assert!(RuleSource::User.rank() < RuleSource::Community.rank());
        assert!(RuleSource::Community.rank() < RuleSource::BuiltinDetection.rank());
        assert!(RuleSource::BuiltinDetection.rank() < RuleSource::AiSuggestion.rank());
    }

    #[test]
    fn source_serde_uses_kebab_case() {
        for (source, token) in [
            (RuleSource::BuiltinProtected, "builtin-protected"),
            (RuleSource::UserProtected, "user-protected"),
            (RuleSource::User, "user"),
            (RuleSource::Community, "community"),
            (RuleSource::BuiltinDetection, "builtin-detection"),
            (RuleSource::AiSuggestion, "ai-suggestion"),
        ] {
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{token}\"")
            );
        }
    }
}
