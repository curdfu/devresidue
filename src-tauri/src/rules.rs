//! `get_rules` / `validate_rules` commands (R11).
//!
//! Inspection and restricted user-rule removal for the **actual** merged rule registry (built-in
//! `resources/rules` + the user rules container under the data directory) —
//! the same assembly the scan rule gate uses, but lenient: rules that failed
//! validation are returned (or counted) instead of aborting the command, so
//! the frontend can show exactly what is loaded and what is broken.
//!
//! This replaces the demo-only rule listing the UI previously rendered from
//! hard-coded mock data (R11). The sole mutation is `delete_user_rule`, which
//! accepts only an exact user-rule id and delegates source ownership checks to
//! Core; disposition writes live in `disposition.rs`.

use std::path::Path;

use devresidue_core::rules::{load_rules, remove_user_rule, user_rules_dir, RuleSet, RuleSource};
use devresidue_core::ResidueCategory;
use devresidue_core::RiskLevel;
use tauri::State;

use crate::contract::{CommandError, ErrorCode, RuleDto, RuleIssueDto, RulesValidationDto};
use crate::state::AppState;
use crate::support;

/// `get_rules` command: the merged registry as the frontend renders it.
#[tauri::command]
pub fn get_rules(state: State<'_, AppState>) -> Result<Vec<RuleDto>, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    let set = load_merged(&data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    Ok(rule_dtos(&set))
}

/// `validate_rules` command: whole-registry health (CLI `rules validate`
/// semantics): totals + every finding.
#[tauri::command]
pub fn validate_rules(state: State<'_, AppState>) -> Result<RulesValidationDto, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    let set = load_merged(&data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e))?;

    let warnings = set
        .issues
        .iter()
        .filter(|i| i.severity == devresidue_core::rules::Severity::Warning)
        .count();
    let errors = set.error_count();

    let mut issues: Vec<RuleIssueDto> = set
        .issues
        .iter()
        .map(|issue| RuleIssueDto {
            rule_id: issue.rule_id.clone(),
            message: issue.message.clone(),
            severity: match issue.severity {
                devresidue_core::rules::Severity::Error => "error".to_string(),
                devresidue_core::rules::Severity::Warning => "warning".to_string(),
            },
        })
        .collect();
    // Deterministic presentation: blocking errors first, then warnings.
    issues.sort_by_key(|i| i.severity != "error");

    Ok(RulesValidationDto {
        total: set.rules.len(),
        errors,
        warnings,
        issues,
    })
}

/// Removes a user-authored rule by exact rule id. Built-in, community and
/// provider rules are rejected in Core. This never receives a filesystem path
/// and never changes scan data, cleanup plans or user files outside the rules
/// destination.
#[tauri::command]
pub fn delete_user_rule(state: State<'_, AppState>, rule_id: String) -> Result<(), CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    remove_user_rule(&user_rules_dir(&data_dir), &rule_id)
        .map_err(|error| CommandError::new(ErrorCode::Engine, error))
}

/// Machine-facing source label (kebab-case, matches the CLI's wire style).
fn source_label(source: RuleSource) -> &'static str {
    match source {
        RuleSource::BuiltinProtected => "builtin-protected",
        RuleSource::UserProtected => "user-protected",
        RuleSource::User => "user",
        RuleSource::Community => "community",
        RuleSource::BuiltinDetection => "builtin-detection",
    }
}

/// Kebab-case serialisation of a risk level (mirrors the core serde rename).
fn risk_label(risk: RiskLevel) -> String {
    serde_json::to_value(risk)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{risk:?}"))
}

/// Kebab-case serialisation of a category.
fn category_label(category: ResidueCategory) -> String {
    serde_json::to_value(category)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{category:?}"))
}

/// Flattens a merged [`RuleSet`] into DTOs. Every successfully loaded rule is
/// `valid: true` (its own warnings ride along in `issues`); rules whose
/// validation **failed** are not part of `rules` and are reconstructed from
/// the rule-level error issues with `valid: false` and no metadata (the
/// declaration did not load).
fn rule_dtos(set: &RuleSet) -> Vec<RuleDto> {
    let mut out: Vec<RuleDto> = set
        .rules
        .iter()
        .map(|rule| RuleDto {
            rule_id: rule.id.clone(),
            source: Some(source_label(rule.source).to_string()),
            risk: Some(risk_label(rule.risk)),
            category: Some(category_label(rule.category)),
            description: Some(rule.description.clone()),
            valid: true,
            issues: set
                .issues
                .iter()
                .filter(|i| i.rule_id.as_deref() == Some(rule.id.as_str()))
                .map(|i| i.message.clone())
                .collect(),
        })
        .collect();

    // Rule-level *error* issues whose rule never loaded → invalid entries.
    for issue in set.issues.iter().filter(|i| i.is_error()) {
        let Some(rule_id) = issue.rule_id.as_deref() else {
            continue; // file-level issues surface only in validate_rules.
        };
        if out.iter().any(|dto| dto.rule_id == rule_id) {
            continue;
        }
        out.push(RuleDto {
            rule_id: rule_id.to_string(),
            source: None,
            risk: None,
            category: None,
            description: None,
            valid: false,
            issues: set
                .issues
                .iter()
                .filter(|i| i.rule_id.as_deref() == Some(rule_id) && i.is_error())
                .map(|i| i.message.clone())
                .collect(),
        });
    }
    out
}

/// Lenient merged load: built-in rules + the user rules container under the
/// data directory (same sources as the scan gate). Unlike
/// [`support::load_scan_rules`] this does **not** fail on issues — the caller
/// wants to report them.
fn load_merged(data_dir: &Path) -> Result<RuleSet, String> {
    let rules_dir = support::locate_rules_dir().ok_or_else(|| {
        "cannot locate `resources/rules` for rule inspection; set \
         DEVRESIDUE_RULES_DIR to point at it"
            .to_string()
    })?;
    let env_lookup = |name: &str| std::env::var(name).ok();
    let mut builtin = load_rules(&rules_dir, &env_lookup);
    let user_container = data_dir.join("rules");
    std::fs::create_dir_all(&user_container)
        .map_err(|e| format!("create {}: {e}", user_container.display()))?;
    let user_set = load_rules(&user_container, &env_lookup);
    builtin.merge(user_set);
    Ok(builtin)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal valid rule file into a loader-recognised partition
    /// (`agents/` = builtin detection, `user/` = user rules).
    fn write_rule(dir: &Path, subdir: &str, id: &str, source: &str) {
        let folder = dir.join(subdir);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(format!("{}.yaml", id.replace('/', "__"))),
            format!(
                "rules:\n  - id: {id}\n    description: test rule {id}\n    \
                 product: test\n    category: temporary\n    risk: safe\n    \
                 source: {source}\n    match:\n      exact: \
                 \"%USERPROFILE%\\\\{id}\"\n"
            ),
        )
        .expect("write rule file");
    }

    /// A rule file that fails parsing (the loader emits a file-level error).
    fn write_broken_rule(dir: &Path) {
        let folder = dir.join("agents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("broken.yaml"), "rules:\n  - id: [unclosed").expect("write");
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dr-tauri-rules-{tag}-{}", std::process::id()))
    }

    #[test]
    fn rule_dtos_mix_builtin_and_user_sources_with_valid_flags() {
        let rules_dir = tmp("builtin");
        write_rule(&rules_dir, "agents", "builtin-ok", "builtin-detection");
        // A user detection rule under the data rules container's `user/`.
        let data = tmp("data");
        write_rule(&data.join("rules"), "user", "user-detection/mine", "user");

        let env_lookup = |name: &str| std::env::var(name).ok();
        let mut merged = load_rules(&rules_dir, &env_lookup);
        merged.merge(load_rules(&data.join("rules"), &env_lookup));

        let dtos = rule_dtos(&merged);
        assert!(
            dtos.iter().any(|d| d.rule_id == "builtin-ok"
                && d.source.as_deref() == Some("builtin-detection")
                && d.valid),
            "builtin rule missing: {dtos:?}"
        );
        let user = dtos
            .iter()
            .find(|d| d.rule_id == "user-detection/mine")
            .expect("user rule present");
        assert_eq!(user.source.as_deref(), Some("user"));
        assert!(user.valid);
        assert_eq!(user.risk.as_deref(), Some("safe"));

        let _ = std::fs::remove_dir_all(&rules_dir);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn broken_rule_file_is_counted_as_an_error_issue() {
        // A malformed rule file in a known partition surfaces as a loading
        // error issue — the registry reports it (validate_rules) instead of
        // pretending everything is healthy.
        let rules_dir = tmp("broken-builtin");
        write_broken_rule(&rules_dir);
        write_rule(&rules_dir, "agents", "healthy", "builtin-detection");

        let env_lookup = |name: &str| std::env::var(name).ok();
        let set = load_rules(&rules_dir, &env_lookup);
        assert!(
            set.error_count() >= 1,
            "the broken file must produce an error: {:?}",
            set.issues
        );

        let dtos = rule_dtos(&set);
        assert!(dtos.iter().any(|d| d.rule_id == "healthy" && d.valid));
        let _ = std::fs::remove_dir_all(&rules_dir);
    }
}
