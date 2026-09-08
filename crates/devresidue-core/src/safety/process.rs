//! Process guard — defers cleanup while a related product/agent tool is
//! running (SPEC §12, INV-012).
//!
//! MVP keeps one fixed watch-list (the DevResidue "agents & runtimes" table
//! from SPEC §12); the list is injectable and extensible per product for
//! later phases. The guard never guesses: when the process probe cannot
//! enumerate (state Unknown) it reports an unknown outcome, which the
//! validator turns into a **Deny** (INV-012 forbids treating Unknown as
//! NotRunning).

use std::collections::HashMap;

use super::probe::{ProcessProbe, ProcessState};

/// Default watch-list (SPEC §12 / PLAN Phase 5): agent CLIs and runtimes
/// whose caches we must not sweep while they are live.
pub const DEFAULT_PROCESS_NAMES: [&str; 9] = [
    "codex", "claude", "opencode", "cursor", "windsurf", "node", "go", "gopls", "cargo",
];

/// Result of a process-guard check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardOutcome {
    /// Watch-list process is running → defer the item.
    Running,
    /// No watch-list process is running → may continue.
    NotRunning,
    /// Process state could not be determined → caller must deny (INV-012).
    Unknown,
}

/// Per-product watch-list resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessGuard {
    /// Names used when the item's product is not mapped.
    default_names: Vec<String>,
    /// Optional product → process-name overrides.
    per_product: HashMap<String, Vec<String>>,
}

impl ProcessGuard {
    /// The fixed MVP watch-list.
    #[must_use]
    pub fn default_list() -> Self {
        Self {
            default_names: DEFAULT_PROCESS_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            per_product: HashMap::new(),
        }
    }

    /// An explicit watch-list (used by tests and future per-product rules).
    #[must_use]
    pub fn new(default_names: Vec<String>, per_product: HashMap<String, Vec<String>>) -> Self {
        Self {
            default_names,
            per_product,
        }
    }

    /// Process names that apply to `product` (falls back to the default list
    /// when the product has no mapping).
    #[must_use]
    pub fn names_for<'a>(&'a self, product: Option<&str>) -> &'a [String] {
        match product.and_then(|p| self.per_product.get(p)) {
            Some(list) => list,
            None => &self.default_names,
        }
    }

    /// Checks the watch-list state for the given product.
    pub fn check(&self, probe: &dyn ProcessProbe, product: Option<&str>) -> GuardOutcome {
        let names = self.names_for(product);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        match probe.process_state(&refs) {
            ProcessState::Running => GuardOutcome::Running,
            ProcessState::NotRunning => GuardOutcome::NotRunning,
            ProcessState::Unknown => GuardOutcome::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProcessProbe(ProcessState);

    impl ProcessProbe for FakeProcessProbe {
        fn process_state(&self, _names: &[&str]) -> ProcessState {
            self.0
        }
    }

    #[test]
    fn running_defers_not_running_passes_unknown_denies() {
        let guard = ProcessGuard::default_list();
        assert_eq!(
            guard.check(
                &FakeProcessProbe(ProcessState::Running),
                Some("Claude Code")
            ),
            GuardOutcome::Running
        );
        assert_eq!(
            guard.check(
                &FakeProcessProbe(ProcessState::NotRunning),
                Some("Claude Code")
            ),
            GuardOutcome::NotRunning
        );
        assert_eq!(
            guard.check(
                &FakeProcessProbe(ProcessState::Unknown),
                Some("Claude Code")
            ),
            GuardOutcome::Unknown
        );
    }

    #[test]
    fn default_list_is_fixed_and_covers_spec_tools() {
        let guard = ProcessGuard::default_list();
        let names = guard.names_for(Some("anything"));
        for expected in DEFAULT_PROCESS_NAMES {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn per_product_override_is_preferred() {
        let mut map = HashMap::new();
        map.insert("Custom Tool".to_string(), vec!["customd".to_string()]);
        let guard = ProcessGuard::new(
            DEFAULT_PROCESS_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            map,
        );
        assert_eq!(
            guard.names_for(Some("Custom Tool")),
            &["customd".to_string()]
        );
        assert_eq!(
            guard.names_for(Some("Other")),
            &DEFAULT_PROCESS_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()[..]
        );
        assert_eq!(
            guard.names_for(None),
            &DEFAULT_PROCESS_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()[..]
        );
    }
}
