//! Rule-driven static cache provider (Mole-extracted knowledge).
//!
//! Discovery source for the `resources/rules/dev_cache/*` / `ide/*`
//! builtin-detection rules: enumerates every **exact-match** rule in the
//! installed rule set, expands its `%ENV%` templates against the scan
//! context, and emits each directory that actually exists as a measured
//! [`ScanItem`] carrying the rule's own risk / category / product.
//!
//! This keeps a single source of truth — adding a tool cache to the scan
//! means adding one YAML rule, no code change (the Mole import path,
//! `scripts/import-mole-rules.md`). Rules with `risk: protected` /
//! `risk: unknown` are never emitted (the rule gate handles those); glob
//! rules are left to the unknown provider's heuristic discovery. Every
//! emitted item is recyclable by default (`RecycleBin`) — no tool-native
//! command is invented for tools we only know by location.

use std::path::PathBuf;

use devresidue_core::rules::loader::CompiledPattern;
use devresidue_core::rules::priority::RuleSource;
use devresidue_core::{CleanupAction, RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

pub const PROVIDER: &str = "static-cache";

/// Scans for every existing exact-match builtin cache location.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    ctx.progress(crate::scan_ctx::ProgressEvent::ProviderStart(PROVIDER));
    let items = scan_impl(ctx);
    if !ctx.cancelled() {
        ctx.progress(crate::scan_ctx::ProgressEvent::ProviderDone(PROVIDER));
    }
    items
}

fn scan_impl(ctx: &ScanContext) -> Vec<ScanItem> {
    // The rule set is only visible through resolved_rule(path) on the
    // context; enumeration needs the RuleSet itself. ScanContext keeps it
    // private — extend with a read accessor instead of re-loading here.
    let Some(rules) = ctx.rule_set() else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for rule in &rules.rules {
        // Only literal exact targets (globs stay with the unknown provider).
        let CompiledPattern::Exact { target } = &rule.pattern else {
            continue;
        };
        // NOTE: the loader already expanded `%ENV%` templates against the
        // process environment at load time (`compile_rule` → `expand_env`),
        // so `target` is an absolute slash-normalised path. Do NOT expand
        // again — the scan context's env may differ (injected test
        // environments), and double expansion silently fails to match.
        // Only builtin detection rules — user rules express dispositions over
        // already-discovered data, not new locations.
        if rule.source != RuleSource::BuiltinDetection {
            continue;
        }
        // Protected / unknown rules never emit cleanable items.
        if matches!(rule.risk, RiskLevel::Protected | RiskLevel::Unknown) {
            continue;
        }
        let path = PathBuf::from(target.replace('/', "\\"));
        if !path.is_dir() {
            continue;
        }
        // Dedupe against every earlier provider (seen-set).
        if ctx.already_seen(&path) || !ctx.seen_insert(&path) {
            continue;
        }
        let product = rule
            .product
            .clone()
            .unwrap_or_else(|| "Developer tool".into());
        let measure = crate::measure::measure_tree_parallel(&path, ctx.cancel_fn());
        if ctx.cancelled() {
            return items;
        }
        items.push(ScanItem {
            id: ctx.allocate_id(),
            path,
            display_name: format!("{} cache", product),
            product: Some(product),
            category: rule.category,
            risk: rule.risk,
            source: SourceKind::Rule,
            logical_size: measure.logical_size,
            file_count: measure.file_count,
            last_modified: measure.last_modified,
            explanation: format!(
                "{} — location from the built-in rule gate (Mole reference, \
                 SPEC §25 discovery); deleting is {}.",
                rule.description,
                risk_hint(rule.risk)
            ),
            cleanup_action: CleanupAction::RecycleBin,
            evidence: vec![
                devresidue_core::Evidence::new(crate::registry::evidence_tag(PROVIDER), &rule.id),
                devresidue_core::Evidence::new(
                    "rule-source",
                    format!("builtin detection rule {}", rule.id),
                )
                .with_rule(rule.numeric_id),
            ],
            scan_snapshot: None, // filled by the scan assembler when a probe is wired
            classification_rule_id: Some(rule.id.clone()),
        });
    }
    items
}

/// One-line impact phrasing per risk level (explainability, SPEC §8).
fn risk_hint(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Safe => "without lasting impact",
        RiskLevel::RegenerableLocal => "rebuildable locally on next use",
        RiskLevel::RegenerableDownload => "re-downloadable on next install",
        RiskLevel::Review => "irreversible — review the contents first",
        RiskLevel::Protected | RiskLevel::Unknown => "not authorised",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_ctx::{ScanContext, ToolQuery};
    use devresidue_core::rules::load_rules;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct NoTool;
    impl ToolQuery for NoTool {
        fn run(&self, _e: &str, _a: &[&str], _t: std::time::Duration) -> Result<String, String> {
            Err("no tool".into())
        }
    }

    #[test]
    fn emits_only_existing_rule_locations_from_the_injected_env() {
        let base = std::env::temp_dir().join(format!("dr-static-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let profile = base.join("profile");
        std::fs::create_dir_all(profile.join(".hex").join("cache")).unwrap();
        std::fs::write(profile.join(".hex").join("cache").join("d"), b"x").unwrap();

        let mut env = HashMap::new();
        env.insert("USERPROFILE".into(), profile.display().to_string());
        env.insert(
            "LOCALAPPDATA".into(),
            profile.join("la").display().to_string(),
        );
        env.insert("APPDATA".into(), profile.join("ra").display().to_string());

        // Real built-in rule FILES, loaded with the INJECTED env so every
        // %ENV% anchor expands to the fixture profile (mirrors production,
        // where the loader uses the process env).
        let rules_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/rules");
        let set = load_rules(&rules_dir, &|k| env.get(k).cloned());
        assert!(set.is_clean(), "rules must load: {:?}", set.issues);

        let mut ctx = ScanContext::with_env(env, Box::new(NoTool), Vec::new(), Box::new(|| true));
        ctx.set_rules(Arc::new(set));

        let items = scan(&ctx);
        let hex = items
            .iter()
            .find(|it| it.path.to_string_lossy().contains(".hex"))
            .expect("the .hex cache must be discovered from the INJECTED profile");
        assert_eq!(hex.risk, RiskLevel::RegenerableDownload);
        assert_eq!(
            hex.classification_rule_id.as_deref(),
            Some("builtin-detection/hex-cache")
        );
        // No item may point outside the injected profile (real-machine dirs
        // must never leak through process env).
        for it in &items {
            assert!(
                it.path.starts_with(&profile),
                "item outside injected profile: {}",
                it.path.display()
            );
        }

        let _ = std::fs::remove_dir_all(&base);
    }
}
