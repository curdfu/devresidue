use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use devresidue_ai::{
    AiAdvisorService, AiAuditLog, AiAuditWarning, AiCancellationToken, AiConfirmationConfig,
    AiConfirmationSelection, AiProfileStore, AiServiceError, AiTransport,
};
use devresidue_core::ai::{
    AgeBucket, AiBatchId, AiEntryToken, AiKeyEnvName, AiProfile, AiProfileId, AiSuggestion, AiZone,
    EnvKeyStore, PreparedBatch, SanitizedEntry, SizeBucket,
};
use devresidue_core::rules::user::upsert_detection_rule;
use devresidue_core::{
    CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, ScanItemId, SourceKind,
};
use devresidue_platform_windows::user_rule_tx::WindowsUserRuleTransactionPort;

const PROFILE_ID: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";

struct StaticKeyStore;

impl EnvKeyStore for StaticKeyStore {
    fn set(&self, _name: &AiKeyEnvName, _value: &str) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, _name: &AiKeyEnvName) -> Result<Option<String>, String> {
        Ok(Some("sk-test-confirm-key".to_string()))
    }

    fn remove(&self, _name: &AiKeyEnvName) -> Result<(), String> {
        Ok(())
    }
}

struct ValidSuggestionTransport;

impl AiTransport for ValidSuggestionTransport {
    fn analyze(
        &self,
        profile: &AiProfile,
        _api_key: &str,
        batch: &PreparedBatch,
        _cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        Ok(batch
            .entries
            .iter()
            .map(|entry| AiSuggestion {
                token: entry.id.clone(),
                suggested_risk: RiskLevel::Review,
                confidence: 0.9,
                reason: "Likely a local cache.".to_string(),
                product_guess: None,
                profile_id: profile.id().clone(),
                scan_generation: batch.scan_generation,
            })
            .collect())
    }
}

fn data_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "devresidue-task7-confirm-{name}-{}",
        std::process::id(),
    ))
}

fn empty_builtin_rules_dir(data_dir: &std::path::Path) -> PathBuf {
    let rules_dir = data_dir.join("builtin-rules");
    fs::create_dir_all(&rules_dir).unwrap();
    rules_dir
}

fn confirmation_config(data_dir: &std::path::Path) -> AiConfirmationConfig {
    AiConfirmationConfig::new(
        data_dir,
        Arc::new(WindowsUserRuleTransactionPort::new()),
        empty_builtin_rules_dir(data_dir),
    )
}

fn configured_store(data_dir: &std::path::Path) -> AiProfileStore {
    fs::create_dir_all(data_dir).unwrap();
    fs::write(
        data_dir.join("ai-profiles.json"),
        format!(
            r#"{{"masterEnabled":true,"activeProfileId":"{PROFILE_ID}","profiles":[{{"id":"{PROFILE_ID}","name":"test profile","baseUrl":"https://example.invalid/v1","model":"test-model","apiKeyEnv":"DEVRESIDUE_AI_KEY_{}","structuredOutputMode":"strict","timeoutSecs":120,"enabled":true}}]}}"#,
            PROFILE_ID.to_ascii_uppercase()
        ),
    )
    .unwrap();
    AiProfileStore::open(data_dir).unwrap()
}

fn batch() -> PreparedBatch {
    PreparedBatch {
        id: AiBatchId::new("7a1b2c3d-4e5f-6a7b-8c9d-0e1f2a3b4c5d".to_string()),
        profile_id: AiProfileId::parse(PROFILE_ID).unwrap(),
        scan_generation: 42,
        entries: vec![SanitizedEntry {
            id: AiEntryToken::new("9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f".to_string()),
            zone: AiZone::LocalAppData,
            relative_depth: 2,
            display_name: "cache".to_string(),
            source_kind: "unknown-provider".to_string(),
            category_hint: "unknown".to_string(),
            product_hint: None,
            size_bucket: SizeBucket::Under64MiB,
            age_bucket: AgeBucket::Under30Days,
            signals: vec!["path-layout".to_string()],
        }],
    }
}

fn candidate() -> ScanItem {
    ScanItem {
        id: ScanItemId::from_raw(501),
        path: PathBuf::from(r"C:\ai-advisor-fixture\cache"),
        display_name: "cache".to_string(),
        product: Some("fixture-tool".to_string()),
        category: ResidueCategory::DeveloperCache,
        risk: RiskLevel::Unknown,
        source: SourceKind::UnknownProvider,
        logical_size: 12,
        file_count: 1,
        last_modified: Some(SystemTime::now() - Duration::from_secs(60)),
        explanation: "local fixture".to_string(),
        cleanup_action: CleanupAction::None,
        evidence: vec![Evidence::new("path-layout", "local fixture")],
        scan_snapshot: None,
        classification_rule_id: None,
    }
}

fn second_candidate() -> ScanItem {
    let mut item = candidate();
    item.id = ScanItemId::from_raw(502);
    item.path = PathBuf::from(r"C:\ai-advisor-fixture\other-cache");
    item
}

fn two_item_batch() -> PreparedBatch {
    let mut prepared = batch();
    prepared.entries.push(SanitizedEntry {
        id: AiEntryToken::new("0d3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f".to_string()),
        zone: AiZone::LocalAppData,
        relative_depth: 2,
        display_name: "other-cache".to_string(),
        source_kind: "unknown-provider".to_string(),
        category_hint: "unknown".to_string(),
        product_hint: None,
        size_bucket: SizeBucket::Under64MiB,
        age_bucket: AgeBucket::Under30Days,
        signals: vec!["path-layout".to_string()],
    });
    prepared
}

#[test]
fn confirmed_risk_becomes_a_user_rule_only_after_local_rematch() {
    let data_dir = data_dir("success");
    let service = AiAdvisorService::with_confirmation(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        Arc::new(ValidSuggestionTransport),
        confirmation_config(&data_dir),
    );
    let batch = batch();
    let item = candidate();
    service
        .bind_trusted_candidates(&batch, std::slice::from_ref(&item))
        .unwrap();
    service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap();

    let result = service
        .confirm_suggestions(
            &batch.id,
            batch.scan_generation,
            &[AiConfirmationSelection::new(
                item.id,
                RiskLevel::RegenerableLocal,
                ResidueCategory::BuildArtifact,
            )],
        )
        .unwrap();

    assert_eq!(result.written_rule_ids.len(), 1);
    let rules = fs::read_to_string(
        data_dir
            .join("rules")
            .join("user")
            .join("user-dispositions.yaml"),
    )
    .unwrap();
    assert!(rules.contains("user-ai/"));
    assert!(rules.contains("risk: regenerable-local"));
    assert!(rules.contains("category: build-artifact"));
    let audit = fs::read_to_string(data_dir.join("ai-audit.jsonl")).unwrap();
    assert!(audit.contains("confirm-written"));
    assert!(!audit.contains(r"C:\ai-advisor-fixture"));
    assert!(!audit.contains("Likely a local cache."));
    assert!(!audit.contains("sk-test-confirm-key"));
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn reloaded_builtin_protection_blocks_the_whole_confirmation_batch() {
    let data_dir = data_dir("builtin-protected");
    let builtin_rules = empty_builtin_rules_dir(&data_dir);
    let protected_dir = builtin_rules.join("protected");
    fs::create_dir_all(&protected_dir).unwrap();
    fs::write(
        protected_dir.join("fixture.yaml"),
        r#"rules:
  - id: builtin-protected/fixture
    description: fixture protection
    product: fixture-tool
    category: credential
    risk: protected
    source: builtin-protected
    match:
      exact: "C:/ai-advisor-fixture/cache"
"#,
    )
    .unwrap();
    let service = AiAdvisorService::with_confirmation(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        Arc::new(ValidSuggestionTransport),
        AiConfirmationConfig::new(
            &data_dir,
            Arc::new(WindowsUserRuleTransactionPort::new()),
            &builtin_rules,
        ),
    );
    let batch = batch();
    let item = candidate();
    service
        .bind_trusted_candidates(&batch, std::slice::from_ref(&item))
        .unwrap();
    service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap();

    let error = service
        .confirm_suggestions(
            &batch.id,
            batch.scan_generation,
            &[AiConfirmationSelection::new(
                item.id,
                RiskLevel::Review,
                ResidueCategory::DeveloperCache,
            )],
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        devresidue_ai::AiServiceErrorKind::ConfirmationFailed
    );
    assert!(!data_dir
        .join("rules")
        .join("user")
        .join("user-dispositions.yaml")
        .exists());
    assert!(fs::read_to_string(data_dir.join("ai-audit.jsonl"))
        .unwrap()
        .contains("confirm-rolled-back"));
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn one_manual_anchor_conflict_rejects_the_whole_confirmation_batch() {
    let data_dir = data_dir("conflict");
    let service = AiAdvisorService::with_confirmation(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        Arc::new(ValidSuggestionTransport),
        confirmation_config(&data_dir),
    );
    let batch = two_item_batch();
    let first = candidate();
    let second = second_candidate();
    let user_rules = data_dir.join("rules").join("user");
    fs::create_dir_all(&user_rules).unwrap();
    upsert_detection_rule(
        &user_rules,
        &second.path,
        second.product.as_deref(),
        second.category,
        RiskLevel::Review,
    )
    .unwrap();
    service
        .bind_trusted_candidates(&batch, &[first.clone(), second.clone()])
        .unwrap();
    service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap();

    let error = service
        .confirm_suggestions(
            &batch.id,
            batch.scan_generation,
            &[
                AiConfirmationSelection::new(
                    first.id,
                    RiskLevel::Review,
                    ResidueCategory::DeveloperCache,
                ),
                AiConfirmationSelection::new(
                    second.id,
                    RiskLevel::Safe,
                    ResidueCategory::DeveloperCache,
                ),
            ],
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        devresidue_ai::AiServiceErrorKind::ConfirmationConflict
    );
    let rules = fs::read_to_string(user_rules.join("user-dispositions.yaml")).unwrap();
    assert!(!rules.contains("user-ai/"));
    assert!(fs::read_to_string(data_dir.join("ai-audit.jsonl"))
        .unwrap()
        .contains("confirm-rolled-back"));
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn stale_generation_rejects_confirmation_without_consuming_the_review_batch() {
    let data_dir = data_dir("stale");
    let service = AiAdvisorService::with_confirmation(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        Arc::new(ValidSuggestionTransport),
        confirmation_config(&data_dir),
    );
    let batch = batch();
    let item = candidate();
    service
        .bind_trusted_candidates(&batch, std::slice::from_ref(&item))
        .unwrap();
    service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap();

    let error = service
        .confirm_suggestions(
            &batch.id,
            batch.scan_generation + 1,
            &[AiConfirmationSelection::new(
                item.id,
                RiskLevel::Review,
                ResidueCategory::DeveloperCache,
            )],
        )
        .unwrap_err();

    assert_eq!(
        error.kind(),
        devresidue_ai::AiServiceErrorKind::StaleGeneration
    );
    assert!(service.suggestions(&batch.id).is_some());
    assert!(!data_dir
        .join("rules")
        .join("user")
        .join("user-dispositions.yaml")
        .exists());
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn audit_write_failure_warns_without_reversing_a_committed_rule_batch() {
    let data_dir = data_dir("audit-failure");
    let audit_blocker = data_dir.join("audit-blocker");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&audit_blocker, "not a directory").unwrap();
    let service = AiAdvisorService::with_confirmation(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        Arc::new(ValidSuggestionTransport),
        AiConfirmationConfig::with_audit_log(
            &data_dir,
            Arc::new(WindowsUserRuleTransactionPort::new()),
            empty_builtin_rules_dir(&data_dir),
            AiAuditLog::open(&audit_blocker),
        ),
    );
    let batch = batch();
    let item = candidate();
    service
        .bind_trusted_candidates(&batch, std::slice::from_ref(&item))
        .unwrap();
    service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap();

    let result = service
        .confirm_suggestions(
            &batch.id,
            batch.scan_generation,
            &[AiConfirmationSelection::new(
                item.id,
                RiskLevel::Review,
                ResidueCategory::DeveloperCache,
            )],
        )
        .unwrap();

    assert_eq!(result.audit_warning, Some(AiAuditWarning::WriteFailed));
    assert!(data_dir
        .join("rules")
        .join("user")
        .join("user-dispositions.yaml")
        .exists());
    let _ = fs::remove_dir_all(data_dir);
}
