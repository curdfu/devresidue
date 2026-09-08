//! `devresidue rules` — load, list and validate the rule registry (Phase 3).

use std::{env, path::Path, path::PathBuf};

use devresidue_core::rules::{load_rules, CompiledRule, RuleIssue, Severity};

use super::RulesCommand;
use crate::scan::{category_label, risk_label, truncate};

/// Env var override for the rules directory (useful when the CLI is invoked
/// from outside the repository).
const RULES_DIR_ENV: &str = "DEVRESIDUE_RULES_DIR";

/// Runs a `rules` subcommand.
pub fn run(cmd: RulesCommand) -> Result<(), String> {
    match cmd {
        RulesCommand::List { json } => list(json),
        RulesCommand::Validate => validate(),
    }
}

/// Locates the rules directory. Resolution order (portable-first):
///
/// 1. `DEVRESIDUE_RULES_DIR` env override (diagnostics);
/// 2. `resources/rules` **next to the running executable** (portable layout:
///    `DevResidue\DevResidue.exe` + `DevResidue\resources\rules`);
/// 3. `rules` next to the executable (flat portable layout);
/// 4. `resources/rules` found by walking up from the working directory (dev
///    mode, running from the repository).
///
/// `locate_rules_dir_at(base)` is the testable core: it probes the candidate
/// locations under an arbitrary `base` path.
pub fn locate_rules_dir() -> Option<PathBuf> {
    if let Ok(custom) = env::var(RULES_DIR_ENV) {
        let dir = PathBuf::from(custom);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            if let Some(found) = locate_rules_dir_at(exe_dir) {
                return Some(found);
            }
        }
    }
    let start = env::current_dir().ok()?;
    start.ancestors().find_map(|ancestor| {
        let candidate = ancestor.join("resources").join("rules");
        candidate.is_dir().then_some(candidate)
    })
}

/// Probes the exe-relative candidate locations under `base`:
/// `base/resources/rules` first (portable layout), then `base/rules`.
pub fn locate_rules_dir_at(base: &Path) -> Option<PathBuf> {
    let nested = base.join("resources").join("rules");
    if nested.is_dir() {
        return Some(nested);
    }
    let flat = base.join("rules");
    if flat.is_dir() {
        return Some(flat);
    }
    None
}

fn list(json: bool) -> Result<(), String> {
    let dir = locate_rules_dir().ok_or_else(|| {
        format!("could not locate `resources/rules` (set {RULES_DIR_ENV} to point at it)")
    })?;

    let set = load_using_real_env(&dir)?;

    if json {
        emit_json(&dir, &set.rules);
    } else {
        emit_table(&set.rules);
    }

    report_issues(&set.issues);
    if set.error_count() > 0 {
        return Err(format!(
            "rules load failed with {} error(s) (see messages above)",
            set.error_count()
        ));
    }
    Ok(())
}

fn validate() -> Result<(), String> {
    let dir = locate_rules_dir().ok_or_else(|| {
        format!(
            "rules directory missing: expected `resources/rules` under the \
                 repository or under {RULES_DIR_ENV}"
        )
    })?;

    let set = load_using_real_env(&dir)?;
    println!("Rules directory: {}", dir.display());
    report_issues(&set.issues);

    if set.error_count() == 0 {
        println!(
            "OK: {} rule(s) loaded and validated, 0 errors, {} warning(s).",
            set.rules.len(),
            set.issues
                .iter()
                .filter(|i| i.severity == Severity::Warning)
                .count()
        );
        Ok(())
    } else {
        println!(
            "FAILED: {} error(s), {} rule(s) loaded.",
            set.error_count(),
            set.rules.len()
        );
        Err(format!(
            "rule validation failed with {} error(s)",
            set.error_count()
        ))
    }
}

/// Loads the rules using the real process environment.
fn load_using_real_env(dir: &std::path::Path) -> Result<devresidue_core::rules::RuleSet, String> {
    let env = |name: &str| env::var(name).ok();
    Ok(load_rules(dir, &env))
}

/// Prints every issue to stderr (one line per issue). Silent when clean —
/// the summary lines printed by each subcommand carry the happy-path result.
fn report_issues(issues: &[RuleIssue]) {
    for issue in issues {
        let severity = match issue.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let file = issue
            .file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into());
        let rule = issue.rule_id.as_deref().unwrap_or("-");
        let field = issue.field.as_deref().unwrap_or("-");
        eprintln!(
            "[{severity}] {file} rule={rule} field={field}: {}",
            issue.message
        );
    }
}

/// Renders a fixed-column table: Id / Source / Risk / Category / Description.
fn emit_table(rules: &[CompiledRule]) {
    const COLUMNS: [&str; 5] = ["Id", "Source", "Risk", "Category", "Description"];
    // Column content caps (0 = uncapped).
    const CAPS: [usize; 5] = [46, 18, 21, 16, 72];

    let mut rows: Vec<[String; 5]> = Vec::with_capacity(rules.len());
    for rule in rules {
        rows.push([
            truncate(&rule.id, CAPS[0]),
            truncate(&rule.source.to_string(), CAPS[1]),
            truncate(risk_label(rule.risk), CAPS[2]),
            truncate(category_label(rule.category), CAPS[3]),
            truncate(&rule.description, CAPS[4]),
        ]);
    }

    let mut widths = [0_usize; 5];
    for (i, header) in COLUMNS.iter().enumerate() {
        widths[i] = header.len();
    }
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            let cap = if CAPS[i] == 0 { usize::MAX } else { CAPS[i] };
            widths[i] = widths[i].max(cell.len().min(cap));
        }
    }

    let header: Vec<String> = COLUMNS.iter().zip(widths).map(|(h, w)| pad(h, w)).collect();
    println!("{}", header.join("  "));
    println!(
        "{}",
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for row in &rows {
        let cells: Vec<String> = row.iter().zip(widths).map(|(c, w)| pad(c, w)).collect();
        println!("{}", cells.join("  "));
    }
    println!();
    println!("{} rule(s) loaded.", rules.len());
}

/// Emits the rules as a stable JSON array (snake_case fields, kebab-case
/// enums), matching the domain DTO conventions used by `scan --json`.
fn emit_json(dir: &std::path::Path, rules: &[CompiledRule]) {
    let items: Vec<serde_json::Value> = rules
        .iter()
        .map(|rule| {
            serde_json::json!({
                "id": rule.id,
                "source": rule.source,
                "risk": rule.risk,
                "category": rule.category,
                "product": rule.product,
                "description": rule.description,
                "anchor": rule.describe_anchor(),
                "parent_marker": rule.parent_marker,
                "requires_existing": rule.requires_existing,
                "include": rule.include_raw,
                "exclude": rule.exclude_raw,
            })
        })
        .collect();

    let out = serde_json::json!({
        "rules_dir": dir.display().to_string(),
        "count": rules.len(),
        "rules": items,
    });
    match serde_json::to_string_pretty(&out) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("error: failed to serialise rule listing: {err}"),
    }
}

/// Pads `s` to `width` for left-aligned column output.
fn pad(s: &str, width: usize) -> String {
    let mut out = String::with_capacity(width);
    out.push_str(s);
    if s.len() < width {
        out.extend(std::iter::repeat(' ').take(width - s.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_aligns_left() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("abcd", 2), "abcd");
    }

    /// Builds a tempdir with one of the portable layouts present.
    fn tmp(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("dr-rules-loc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn portable_layout_nested_resources_rules_wins() {
        let base = tmp("nested");
        // Portable layout: exe dir + resources/rules.
        std::fs::create_dir_all(base.join("resources").join("rules")).unwrap();
        // A flat rules/ dir also exists — nested must win (priority 2 > 3).
        std::fs::create_dir_all(base.join("rules")).unwrap();
        assert_eq!(
            locate_rules_dir_at(&base),
            Some(base.join("resources").join("rules"))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn portable_layout_flat_rules_is_the_fallback() {
        let base = tmp("flat");
        std::fs::create_dir_all(base.join("rules")).unwrap();
        assert_eq!(locate_rules_dir_at(&base), Some(base.join("rules")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_layout_returns_none() {
        let base = tmp("none");
        assert_eq!(locate_rules_dir_at(&base), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
