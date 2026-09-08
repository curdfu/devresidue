//! `devresidue analyze <item-id> [--create-rule]` — the metadata-only
//! directory analyzer (SPEC §26, Phase 14, **default off**).
//!
//! The analyzer reads only metadata (name / one-level layout / size) — file
//! contents are never read — and returns a *suggestion* that has no deletion
//! authority (INV-003). `--create-rule` turns the suggestion into a user
//! detection rule through the ordinary validator-gated rule write; the next
//! scan's F-2-1 gate then applies it like any user rule.

use devresidue_core::rules::{upsert_detection_rule, user_rules_dir};
use devresidue_providers::analyze::heuristic::HeuristicAnalyzer;
use devresidue_providers::analyze::{collect_facts, DirectoryAnalyzer};
use devresidue_providers::measure::measure_tree;
use devresidue_providers::settings::{load_settings, settings_path};

use crate::scan::{category_label, risk_label};
use crate::support;

/// Runs the analyze command.
pub fn run(item_id: u64, create_rule: bool) -> Result<(), String> {
    let data_dir = support::data_dir()?;

    // SPEC §26: default off. A disabled analyzer errors with the way to enable
    // it (never silently runs against user data).
    let settings = load_settings(&data_dir)?;
    if !settings.analyzer_enabled {
        return Err(format!(
            "directory analyzer is disabled — enable it by writing \
             {{\"analyzerEnabled\": true}} to {} (or use the desktop app \
             settings)",
            settings_path(&data_dir).display()
        ));
    }

    let item = support::find_latest_item(item_id)?;
    let path = &item.path;
    if !path.is_dir() {
        return Err(format!(
            "item {item_id} is not a directory ({}); the analyzer inspects \
             directory data",
            path.display()
        ));
    }
    let measure = measure_tree(path, &|| true);
    let facts = collect_facts(path, measure)
        .ok_or_else(|| format!("cannot read directory {}", path.display()))?;

    let analyzer = HeuristicAnalyzer;
    let suggestion = analyzer.analyze(&facts);

    println!("Analyzer suggestion for {}:", path.display());
    println!(
        "  product    {}",
        suggestion.product_guess.as_deref().unwrap_or("(unknown)")
    );
    println!("  confidence {:.0}%", suggestion.confidence * 100.0);
    println!("  category   {}", category_label(suggestion.category));
    println!("  risk       {}", risk_label(suggestion.risk));
    println!("  reasoning  {}", suggestion.explanation);

    if create_rule {
        let user_dir = user_rules_dir(&data_dir);
        std::fs::create_dir_all(&user_dir)
            .map_err(|e| format!("create {}: {e}", user_dir.display()))?;
        let rule_id = upsert_detection_rule(
            &user_dir,
            path,
            suggestion.product_guess.as_deref(),
            suggestion.category,
            suggestion.risk,
        )?;
        println!(
            "  rule       written {rule_id} (applies on the next scan; still gated by \
             the full rule validator)"
        );
    } else if suggestion.suggested_rule_id.is_some() {
        println!("  hint       re-run with --create-rule to persist this as a user rule");
    }
    Ok(())
}
