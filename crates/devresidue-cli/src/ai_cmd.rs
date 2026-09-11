//! Optional remote-AI CLI surface.
//!
//! This module deliberately keeps AI analysis and any user-rule confirmation
//! in one process. Remote suggestions are never persisted or accepted back as
//! free-form rule data; the confirmation grammar names only registered scan
//! item ids and closed risk/category values.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use devresidue_ai::{
    AiAdvisorService, AiCancellationToken, AiConfirmationConfig, AiConfirmationSelection,
    AiProfileInput, AiProfileStore, AiReviewBatch, OpenAiCompatibleTransport,
};
use devresidue_core::ai::{AiApiProtocol, AiProfileId, RecoveryStatus, StructuredOutputMode};
use devresidue_core::{ResidueCategory, RiskLevel, ScanItem, ScanItemId};
use devresidue_platform_windows::env_key::WindowsUserEnvKeyStore;
use devresidue_platform_windows::profile_file::WindowsAiProfileFilePort;
use devresidue_platform_windows::user_rule_tx::WindowsUserRuleTransactionPort;
use devresidue_providers::scan_store::{ScanMode, ScanSnapshot};

use crate::{rules_cmd, support};
use crate::{AiCommand, AiProfileCommand, AiProfileInputArgs, AiRecoveryCommand};

/// Runs the optional remote AI command group. It performs no directory traversal
/// beyond the previously verified scan snapshot.
pub fn run(cmd: AiCommand) -> Result<(), String> {
    let data_dir = support::data_dir()?;
    match cmd {
        AiCommand::Status => report_status(&open_profile_store(&data_dir)?),
        AiCommand::Enable => {
            open_profile_store(&data_dir)?.set_master_enabled(true)?;
            println!("remote AI enabled");
            Ok(())
        }
        AiCommand::Disable => {
            open_profile_store(&data_dir)?.set_master_enabled(false)?;
            println!("remote AI disabled");
            Ok(())
        }
        AiCommand::Profile { cmd } => run_profile(&data_dir, cmd),
        AiCommand::Recovery { cmd } => run_recovery(&data_dir, cmd),
        AiCommand::Analyze { items, confirm } => run_analysis(&data_dir, &items, confirm),
    }
}

fn open_profile_store(data_dir: &Path) -> Result<AiProfileStore, String> {
    AiProfileStore::open_with_file_port(data_dir, Arc::new(WindowsAiProfileFilePort::new()))
}

fn report_status(store: &AiProfileStore) -> Result<(), String> {
    let state = store.load()?;
    println!(
        "remote AI: {}",
        if state.master_enabled() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "active profile: {}",
        state.active_profile_id().map_or("(none)", |id| id.as_str())
    );
    println!("profiles: {}", state.profiles().len());
    match store.recovery_status()? {
        RecoveryStatus::Clean => println!("recovery: clean"),
        RecoveryStatus::RecoveryRequired(marker) => println!(
            "recovery: required for profile {} ({:?})",
            marker.profile_id, marker.op
        ),
        RecoveryStatus::InvalidMarker => println!("recovery: invalid marker (remote AI blocked)"),
    }
    Ok(())
}

fn run_profile(data_dir: &Path, cmd: AiProfileCommand) -> Result<(), String> {
    let store = open_profile_store(data_dir)?;
    let keys = WindowsUserEnvKeyStore::new();
    match cmd {
        AiProfileCommand::List => {
            let state = store.load()?;
            if state.profiles().is_empty() {
                println!("no AI profiles configured");
            }
            for profile in state.profiles() {
                println!(
                    "{}  name={}  model={}  api_protocol={:?}  enabled={}  timeout_secs={}  structured_output={:?}",
                    profile.id(),
                    profile.name(),
                    profile.model(),
                    profile.api_protocol(),
                    profile.enabled(),
                    profile.timeout_secs(),
                    profile.structured_output_mode(),
                );
            }
            Ok(())
        }
        AiProfileCommand::Create { input } => {
            let api_key = read_profile_key(&input)?;
            let profile = store.upsert(profile_input(input)?, &api_key, &keys)?;
            println!("AI profile created: {}", profile.id());
            Ok(())
        }
        AiProfileCommand::Update { profile_id, input } => {
            let api_key = read_profile_key(&input)?;
            let profile = store.update(
                parse_profile_id(&profile_id)?,
                profile_input(input)?,
                &api_key,
                &keys,
            )?;
            println!("AI profile updated: {}", profile.id());
            Ok(())
        }
        AiProfileCommand::Delete { profile_id } => {
            let id = parse_profile_id(&profile_id)?;
            store.delete(id, &keys)?;
            println!("AI profile deleted");
            Ok(())
        }
        AiProfileCommand::Select { profile_id } => {
            let state = store.set_active_profile(Some(parse_profile_id(&profile_id)?))?;
            println!(
                "active AI profile: {}",
                state.active_profile_id().map_or("(none)", |id| id.as_str())
            );
            Ok(())
        }
        AiProfileCommand::ClearSelection => {
            store.set_active_profile(None)?;
            println!("active AI profile cleared");
            Ok(())
        }
    }
}

fn run_recovery(data_dir: &Path, cmd: AiRecoveryCommand) -> Result<(), String> {
    let store = open_profile_store(data_dir)?;
    let keys = WindowsUserEnvKeyStore::new();
    match cmd {
        AiRecoveryCommand::Status => report_status(&store),
        AiRecoveryCommand::Resubmit { profile_id, input } => {
            let api_key = read_profile_key(&input)?;
            let profile = store.resubmit_recovery(
                parse_profile_id(&profile_id)?,
                profile_input(input)?,
                &api_key,
                &keys,
            )?;
            println!("AI profile recovery re-submitted: {}", profile.id());
            Ok(())
        }
        AiRecoveryCommand::Delete { profile_id } => {
            store.delete_recovery(parse_profile_id(&profile_id)?, &keys)?;
            println!("AI profile deletion recovery completed");
            Ok(())
        }
        AiRecoveryCommand::Abandon { profile_id } => {
            store.abandon_recovery(parse_profile_id(&profile_id)?, &keys)?;
            println!("AI profile recovery abandoned; profile disabled");
            Ok(())
        }
    }
}

fn run_analysis(data_dir: &Path, ids: &str, confirm: bool) -> Result<(), String> {
    let snapshot = verified_ai_snapshot()?;
    let selected = select_snapshot_items(&snapshot, &support::parse_ids(ids)?)?;
    let store = open_profile_store(data_dir)?;
    let profile_id = store
        .load()?
        .active_profile_id()
        .cloned()
        .ok_or_else(|| "no active AI profile is selected".to_string())?;
    let service = advisor_service(data_dir, store)?;
    let review = service
        .prepare_review_batch(&selected, snapshot.generation, profile_id)
        .map_err(|error| error.to_string())?;
    let suggestions = service
        .analyze(
            review.batch(),
            snapshot.generation,
            &AiCancellationToken::new(),
        )
        .map_err(|error| error.to_string())?;
    render_suggestions(&review, &suggestions)?;

    if !confirm {
        service.clear_suggestions();
        println!("no rules were written; suggestions are transient and will not be persisted");
        return Ok(());
    }

    let selections = read_confirmation_line()?;
    if selections.is_empty() {
        service.clear_suggestions();
        println!("confirmation cancelled; no rules were written");
        return Ok(());
    }
    let allowed_ids: HashSet<ScanItemId> = review.item_ids().iter().copied().collect();
    if selections
        .iter()
        .any(|selection| !allowed_ids.contains(&selection.item_id()))
    {
        service.clear_suggestions();
        return Err("confirmation contains an item not present in this AI review".to_string());
    }

    let current_snapshot = verified_ai_snapshot()?;
    if current_snapshot.generation != snapshot.generation {
        service.clear_suggestions();
        return Err("a newer scan replaced this review; run AI analysis again".to_string());
    }
    let result = service
        .confirm_suggestions(&review.batch().id, current_snapshot.generation, &selections)
        .map_err(|error| error.to_string())?;
    println!(
        "local AI rules committed: {}",
        result.written_rule_ids.len()
    );
    if result.audit_warning.is_some() {
        eprintln!("warning: AI audit write failed after the rules were committed");
    }
    Ok(())
}

fn advisor_service(data_dir: &Path, store: AiProfileStore) -> Result<AiAdvisorService, String> {
    let builtin_rules_dir = rules_cmd::locate_rules_dir()
        .ok_or_else(|| "cannot locate the built-in rules needed for AI confirmation".to_string())?;
    Ok(AiAdvisorService::with_confirmation(
        store,
        Arc::new(WindowsUserEnvKeyStore::new()),
        Arc::new(OpenAiCompatibleTransport::new()),
        AiConfirmationConfig::new(
            data_dir,
            Arc::new(WindowsUserRuleTransactionPort::new()),
            builtin_rules_dir,
        ),
    ))
}

fn verified_ai_snapshot() -> Result<ScanSnapshot, String> {
    require_ai_snapshot(support::latest_snapshot()?)
}

fn require_ai_snapshot(snapshot: ScanSnapshot) -> Result<ScanSnapshot, String> {
    if !matches!(snapshot.mode, ScanMode::Real { .. }) {
        return Err("AI analysis requires a current real scan, not fixtures".to_string());
    }
    if snapshot.cancelled {
        return Err("AI analysis requires a completed scan; re-run devresidue scan".to_string());
    }
    Ok(snapshot)
}

fn select_snapshot_items(
    snapshot: &ScanSnapshot,
    ids: &[ScanItemId],
) -> Result<Vec<ScanItem>, String> {
    let mut selected = Vec::with_capacity(ids.len());
    let mut seen = HashSet::with_capacity(ids.len());
    for id in ids {
        if !seen.insert(*id) {
            return Err("AI analysis item ids must be unique".to_string());
        }
        let item = snapshot
            .items
            .iter()
            .find(|item| item.id == *id)
            .cloned()
            .ok_or_else(|| "an AI analysis item does not belong to the latest scan".to_string())?;
        selected.push(item);
    }
    Ok(selected)
}

fn render_suggestions(
    review: &AiReviewBatch,
    suggestions: &[devresidue_core::ai::AiSuggestion],
) -> Result<(), String> {
    let item_by_token: HashMap<&str, ScanItemId> = review
        .batch()
        .entries
        .iter()
        .zip(review.item_ids())
        .map(|(entry, item_id)| (entry.id.as_str(), *item_id))
        .collect();
    println!("AI suggestions (no cleanup plan or deletion is created):");
    for suggestion in suggestions {
        let item_id = item_by_token
            .get(suggestion.token.as_str())
            .copied()
            .ok_or_else(|| {
                "validated AI response could not be mapped to this review".to_string()
            })?;
        println!(
            "  item={}  suggested_risk={:?}  confidence={:.0}%  product={}  reason={}",
            item_id,
            suggestion.suggested_risk,
            suggestion.confidence * 100.0,
            suggestion.product_guess.as_deref().unwrap_or("(none)"),
            suggestion.reason,
        );
    }
    Ok(())
}

fn read_confirmation_line() -> Result<Vec<AiConfirmationSelection>, String> {
    eprint!(
        "type ITEM_ID=RISK:CATEGORY entries to confirm (or 'cancel'); allowed risks: safe, regenerable-local, regenerable-download, review, protected; allowed categories: ai-agent, ide, developer-cache, package-cache, build-artifact, dependency, log, temporary, session, workspace-state, configuration, credential: "
    );
    io::stderr()
        .flush()
        .map_err(|_| "unable to prompt for AI confirmation".to_string())?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|_| "unable to read AI confirmation".to_string())?;
    if line.trim().eq_ignore_ascii_case("cancel") {
        return Ok(Vec::new());
    }
    parse_confirmations(&line)
}

fn read_profile_key(args: &AiProfileInputArgs) -> Result<String, String> {
    if !args.key_stdin {
        return Err(
            "profile creation/update requires --key-stdin; API keys are never command arguments"
                .to_string(),
        );
    }
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|_| "unable to read the API key from standard input".to_string())?;
    let key = input.trim_end_matches(['\r', '\n']);
    if key.is_empty() {
        return Err("API key input is empty".to_string());
    }
    Ok(key.to_string())
}

fn profile_input(args: AiProfileInputArgs) -> Result<AiProfileInput, String> {
    Ok(AiProfileInput {
        name: args.name,
        base_url: args.base_url,
        model: args.model,
        api_protocol: parse_api_protocol(&args.api_protocol)?,
        structured_output_mode: parse_structured_output_mode(&args.structured_output)?,
        timeout_secs: args.timeout_secs,
        enabled: !args.disabled,
    })
}

fn parse_api_protocol(text: &str) -> Result<AiApiProtocol, String> {
    match text {
        "openai-compatible" => Ok(AiApiProtocol::OpenAiCompatible),
        "openai-responses" => Ok(AiApiProtocol::OpenAiResponses),
        _ => Err("API protocol must be openai-compatible or openai-responses".to_string()),
    }
}

fn parse_structured_output_mode(text: &str) -> Result<StructuredOutputMode, String> {
    match text {
        "auto" => Ok(StructuredOutputMode::Auto),
        "json-schema" => Ok(StructuredOutputMode::JsonSchema),
        "json-object" => Ok(StructuredOutputMode::JsonObject),
        _ => Err("structured output mode must be auto, json-schema, or json-object".to_string()),
    }
}

fn parse_profile_id(text: &str) -> Result<AiProfileId, String> {
    AiProfileId::parse(text).map_err(|_| "profile id must be a canonical UUID".to_string())
}

/// Parses one explicit local confirmation line. The grammar is a comma-
/// separated sequence of `ScanItemId=risk:category` values; it has no path or
/// rule-text production and intentionally refuses `unknown` risk or category.
fn parse_confirmations(text: &str) -> Result<Vec<AiConfirmationSelection>, String> {
    let mut selections = Vec::new();
    let mut seen = HashSet::new();
    for pair in text.split(',') {
        let (raw_id, raw_decision) = pair
            .trim()
            .split_once('=')
            .ok_or_else(|| "confirmation entries must use ITEM_ID=RISK:CATEGORY".to_string())?;
        if raw_decision.contains('=') {
            return Err("confirmation entries must use ITEM_ID=RISK:CATEGORY".to_string());
        }
        let (raw_risk, raw_category) = raw_decision
            .split_once(':')
            .ok_or_else(|| "confirmation entries must use ITEM_ID=RISK:CATEGORY".to_string())?;
        if raw_category.contains(':') {
            return Err("confirmation entries must use ITEM_ID=RISK:CATEGORY".to_string());
        }
        let item_id = raw_id
            .trim()
            .parse::<u64>()
            .map(ScanItemId::from_raw)
            .map_err(|_| "confirmation item ids must be numbers".to_string())?;
        if !seen.insert(item_id) {
            return Err("confirmation item ids must be unique".to_string());
        }
        let final_risk = match raw_risk.trim() {
            "safe" => RiskLevel::Safe,
            "regenerable-local" => RiskLevel::RegenerableLocal,
            "regenerable-download" => RiskLevel::RegenerableDownload,
            "review" => RiskLevel::Review,
            "protected" => RiskLevel::Protected,
            _ => {
                return Err(
                    "confirmation risk must be safe, regenerable-local, regenerable-download, review, or protected"
                        .to_string(),
                )
            }
        };
        let final_category = parse_confirmation_category(raw_category.trim())?;
        selections.push(AiConfirmationSelection::new(
            item_id,
            final_risk,
            final_category,
        ));
    }
    if selections.is_empty() {
        return Err("at least one confirmation selection is required".to_string());
    }
    Ok(selections)
}

fn parse_confirmation_category(text: &str) -> Result<ResidueCategory, String> {
    match text {
        "ai-agent" => Ok(ResidueCategory::AiAgent),
        "ide" => Ok(ResidueCategory::Ide),
        "developer-cache" => Ok(ResidueCategory::DeveloperCache),
        "package-cache" => Ok(ResidueCategory::PackageCache),
        "build-artifact" => Ok(ResidueCategory::BuildArtifact),
        "dependency" => Ok(ResidueCategory::Dependency),
        "log" => Ok(ResidueCategory::Log),
        "temporary" => Ok(ResidueCategory::Temporary),
        "session" => Ok(ResidueCategory::Session),
        "workspace-state" => Ok(ResidueCategory::WorkspaceState),
        "configuration" => Ok(ResidueCategory::Configuration),
        "credential" => Ok(ResidueCategory::Credential),
        _ => Err(
            "confirmation category must be ai-agent, ide, developer-cache, package-cache, build-artifact, dependency, log, temporary, session, workspace-state, configuration, or credential"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use devresidue_core::{ResidueCategory, RiskLevel};
    use devresidue_providers::scan_store::{ScanMode, ScanSnapshot};

    use super::{parse_confirmations, require_ai_snapshot};

    #[test]
    fn confirmation_parser_requires_explicit_closed_risk_and_category() {
        let selections = parse_confirmations(
            "501=review:session, 502=regenerable-local:developer-cache",
        )
        .unwrap();

        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].item_id().raw(), 501);
        assert_eq!(selections[0].final_risk(), RiskLevel::Review);
        assert_eq!(selections[0].final_category(), ResidueCategory::Session);
        assert_eq!(selections[1].item_id().raw(), 502);
        assert_eq!(selections[1].final_risk(), RiskLevel::RegenerableLocal);
        assert_eq!(
            selections[1].final_category(),
            ResidueCategory::DeveloperCache
        );
    }

    #[test]
    fn confirmation_parser_rejects_paths_unknown_and_duplicate_ids() {
        for text in [
            r"C:\\Users\\alice\\cache=review",
            "501=unknown",
            "501=review",
            "501=review:unknown",
            "501=review:session,501=safe:temporary",
        ] {
            assert!(parse_confirmations(text).is_err(), "must reject {text}");
        }
    }

    #[test]
    fn remote_ai_refuses_fixture_and_partial_snapshots_before_candidate_selection() {
        let fixtures = ScanSnapshot {
            scanned_at: 1,
            generation: 4,
            mode: ScanMode::Fixtures,
            items: Vec::new(),
            warnings: Vec::new(),
            cancelled: false,
            integrity: None,
        };
        let partial = ScanSnapshot {
            mode: ScanMode::Real {
                workspace_roots: Vec::new(),
            },
            cancelled: true,
            ..fixtures.clone()
        };

        assert!(require_ai_snapshot(fixtures).is_err());
        assert!(require_ai_snapshot(partial).is_err());
    }
}
