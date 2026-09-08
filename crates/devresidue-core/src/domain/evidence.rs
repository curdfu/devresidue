//! Evidence attached to a `ScanItem`.

use serde::{Deserialize, Serialize};

use super::ids::RuleId;

/// A single piece of evidence explaining *why* an item was discovered and
/// classified the way it was.
///
/// Evidence is the backbone of the product's explainability promise
/// (SPEC §35 / PLAN §23: every result must answer "what / who / why /
/// impact / detected-by / which rule"). The structure is intentionally open —
/// new evidence kinds only add string keys, no schema change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// The rule that produced this evidence, once the Rule Engine exists
    /// (Phase 3). `None` when the finding came from a provider heuristic.
    pub rule_id: Option<RuleId>,
    /// Machine-readable evidence kind, e.g. `"path-layout"`,
    /// `"tool-reported-path"`, `"kondo-project-type"`, `"manifest"`,
    /// `"builtin-protected"`.
    pub source: String,
    /// Human-readable detail backing the classification.
    pub detail: String,
}

impl Evidence {
    /// Creates an evidence entry without a rule reference.
    #[must_use]
    pub fn new(source: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            rule_id: None,
            source: source.into(),
            detail: detail.into(),
        }
    }

    /// Attaches a rule reference (used by the Rule Engine in Phase 3+).
    #[must_use]
    pub fn with_rule(mut self, rule_id: RuleId) -> Self {
        self.rule_id = Some(rule_id);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_with_and_without_rule() {
        let e = Evidence::new("path-layout", "matched claude code cache layout");
        assert_eq!(e.rule_id, None);

        let e2 = e.with_rule(RuleId::from_raw(9));
        assert_eq!(e2.rule_id, Some(RuleId::from_raw(9)));
        assert_eq!(e2.source, "path-layout");
    }
}
