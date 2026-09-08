//! `get_settings` / `set_analyzer_enabled` / `analyze_item` commands
//! (SPEC §26, Phase 14).
//!
//! The directory analyzer is **opt-in** (`settings.json` -> analyzer_enabled,
//! default false). It reads metadata only and returns a [`SuggestionDto`] —
//! it has no deletion authority; turning a suggestion into a rule is the
//! `set_disposition`-adjacent user-rule write (see `rules::user`).

use devresidue_core::rules::{upsert_detection_rule, user_rules_dir};
use devresidue_core::RiskLevel;
use devresidue_providers::analyze::heuristic::HeuristicAnalyzer;
use devresidue_providers::analyze::{collect_facts, DirectoryAnalyzer};
use devresidue_providers::measure::measure_tree;
use devresidue_providers::settings::{
    load_settings, set_analyzer_enabled as persist_analyzer_enabled, settings_path,
};
use tauri::State;

use crate::contract::{CommandError, DispositionResultDto, ErrorCode, SettingsDto, SuggestionDto};
use crate::state::AppState;

/// `get_settings` command.
#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<SettingsDto, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    let settings = load_settings(&data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    Ok(SettingsDto {
        analyzer_enabled: settings.analyzer_enabled,
    })
}

/// `set_analyzer_enabled` command (SPEC §26 opt-in).
#[tauri::command]
pub fn set_analyzer_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    persist_analyzer_enabled(&data_dir, enabled)
        .map_err(|e| CommandError::new(ErrorCode::Engine, e))
}

/// `analyze_item` command: suggest a classification for one scan item's
/// directory (metadata only). Errors with [`ErrorCode::AnalyzerDisabled`] when
/// the analyzer is off.
#[tauri::command]
pub fn analyze_item(
    state: State<'_, AppState>,
    item_id: u64,
) -> Result<SuggestionDto, CommandError> {
    let (data_dir, item) = {
        let model = state.model.lock().unwrap();
        let item = model
            .latest()
            .and_then(|snap| snap.items.iter().find(|i| i.id.raw() == item_id).cloned());
        let data_dir = model.data_dir().to_path_buf();
        let item = item.ok_or_else(|| {
            CommandError::new(
                ErrorCode::InvalidItem,
                format!(
                    "item id {item_id} does not belong to the latest scan — re-scan then \
                     retry"
                ),
            )
        })?;
        (data_dir, item)
    };

    let settings = load_settings(&data_dir).map_err(|e| CommandError::new(ErrorCode::Engine, e))?;
    if !settings.analyzer_enabled {
        return Err(CommandError::new(
            ErrorCode::AnalyzerDisabled,
            format!(
                "directory analyzer is disabled (SPEC §26 default); enable it in the \
                 settings — settings file: {}",
                settings_path(&data_dir).display()
            ),
        ));
    }

    let path = &item.path;
    if !path.is_dir() {
        return Err(CommandError::new(
            ErrorCode::Engine,
            format!("item {item_id} is not a directory: {}", path.display()),
        ));
    }
    let measure = measure_tree(path, &|| true);
    let facts = collect_facts(path, measure)
        .ok_or_else(|| CommandError::new(ErrorCode::Engine, "cannot read directory"))?;
    let suggestion = HeuristicAnalyzer.analyze(&facts);

    // Kebab-case labels match the domain serde used everywhere else.
    let category = kebab(&suggestion.category);
    let suggested_risk = kebab(&suggestion.risk);
    Ok(SuggestionDto {
        item_id,
        product_guess: suggestion.product_guess.clone(),
        confidence: suggestion.confidence,
        category,
        suggested_risk,
        explanation: suggestion.explanation.clone(),
        suggested_rule_id: suggestion.suggested_rule_id.clone(),
        path: path.display().to_string(),
    })
}

/// `create_rule_from_suggestion` command: persist the *user-confirmed*
/// analyzer suggestion as a user detection rule (SPEC §26 "Create Rule").
///
/// The user explicitly accepts a suggested risk level for the item; the rule
/// is written through the ordinary validator-gated path (`upsert_detection_rule`,
/// core `rules::user`), so it still runs the full rule validator on the next
/// load. This writes a classification rule only — the analyzer keeps **no**
/// deletion authority (INV-003).
#[tauri::command]
pub fn create_rule_from_suggestion(
    state: State<'_, AppState>,
    item_id: u64,
    suggested_risk: String,
) -> Result<DispositionResultDto, CommandError> {
    // Parse the kebab-case risk the UI round-tripped from SuggestionDto.
    let risk: RiskLevel = parse_kebab_risk(&suggested_risk).ok_or_else(|| {
        CommandError::new(
            ErrorCode::InvalidItem,
            format!(
                "invalid suggested_risk `{suggested_risk}` (expected a risk level such as \
                 safe, regenerable-local, regenerable-download, review, protected, unknown)"
            ),
        )
    })?;

    let (data_dir, item) = {
        let model = state.model.lock().unwrap();
        let item = model
            .latest()
            .and_then(|snap| snap.items.iter().find(|i| i.id.raw() == item_id).cloned());
        let data_dir = model.data_dir().to_path_buf();
        let item = item.ok_or_else(|| {
            CommandError::new(
                ErrorCode::InvalidItem,
                format!(
                    "item id {item_id} does not belong to the latest scan — re-scan then \
                     retry"
                ),
            )
        })?;
        (data_dir, item)
    };

    let user_dir = user_rules_dir(&data_dir);
    std::fs::create_dir_all(&user_dir).map_err(|e| {
        CommandError::new(
            ErrorCode::Engine,
            format!("create {}: {e}", user_dir.display()),
        )
    })?;
    let rule_id = upsert_detection_rule(
        &user_dir,
        &item.path,
        item.product.as_deref(),
        item.category,
        risk,
    )
    .map_err(|e| CommandError::new(ErrorCode::Engine, e))?;

    Ok(DispositionResultDto {
        item_id,
        rule_id,
        path: item.path.display().to_string(),
        effect: format!(
            "user rule written from analyzer suggestion (risk {suggested_risk}); applies on \
             the next scan (F-2-1 gate)"
        ),
    })
}

/// Parses a kebab-case risk string into a [`RiskLevel`] (the domain serde
/// vocabulary).
fn parse_kebab_risk(text: &str) -> Option<RiskLevel> {
    serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
}

/// Serialises a serde-able label into its kebab-case string form.
fn kebab<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_serialises_labels() {
        assert_eq!(
            kebab(&devresidue_core::ResidueCategory::BuildArtifact),
            "build-artifact"
        );
        assert_eq!(
            kebab(&devresidue_core::RiskLevel::RegenerableDownload),
            "regenerable-download"
        );
    }

    #[test]
    fn risk_parsing_accepts_kebab_and_rejects_garbage() {
        assert_eq!(
            parse_kebab_risk("regenerable-local"),
            Some(RiskLevel::RegenerableLocal)
        );
        assert_eq!(parse_kebab_risk("safe"), Some(RiskLevel::Safe));
        assert_eq!(parse_kebab_risk("protected"), Some(RiskLevel::Protected));
        assert_eq!(parse_kebab_risk("not-a-risk"), None);
        assert_eq!(parse_kebab_risk(""), None);
    }
}
