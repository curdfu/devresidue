use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use devresidue_ai::{
    AiAdvisorService, AiCancellationToken, AiProfileStore, AiServiceError, AiServiceErrorKind,
    AiTransport, MetadataSanitizer,
};
use devresidue_core::ai::{
    AgeBucket, AiBatchId, AiEntryToken, AiKeyEnvName, AiProfile, AiProfileId, AiSuggestion, AiZone,
    EnvKeyStore, PreparedBatch, SanitizedEntry, SizeBucket,
};
use devresidue_core::{
    CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, ScanItemId, SourceKind,
};

const PROFILE_ID: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";

#[derive(Default)]
struct NoKeyStore;

impl EnvKeyStore for NoKeyStore {
    fn set(&self, _name: &AiKeyEnvName, _value: &str) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, _name: &AiKeyEnvName) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn remove(&self, _name: &AiKeyEnvName) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingTransport {
    calls: AtomicUsize,
    connection_calls: AtomicUsize,
}

impl AiTransport for RecordingTransport {
    fn test_connection(
        &self,
        _profile: &AiProfile,
        _api_key: &str,
        _cancellation: &AiCancellationToken,
    ) -> Result<(), AiServiceError> {
        self.connection_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn analyze(
        &self,
        _profile: &AiProfile,
        _api_key: &str,
        _batch: &PreparedBatch,
        _cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

struct StaticKeyStore;

impl EnvKeyStore for StaticKeyStore {
    fn set(&self, _name: &AiKeyEnvName, _value: &str) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, _name: &AiKeyEnvName) -> Result<Option<String>, String> {
        Ok(Some("sk-test-service-key".to_string()))
    }

    fn remove(&self, _name: &AiKeyEnvName) -> Result<(), String> {
        Ok(())
    }
}

struct ValidatingTransport;

impl AiTransport for ValidatingTransport {
    fn analyze(
        &self,
        profile: &AiProfile,
        _api_key: &str,
        batch: &PreparedBatch,
        _cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        Ok(vec![AiSuggestion {
            token: batch.entries[0].id.clone(),
            suggested_risk: RiskLevel::Review,
            confidence: 0.8,
            reason: "Likely local cache.".to_string(),
            product_guess: None,
            profile_id: profile.id().clone(),
            scan_generation: batch.scan_generation,
        }])
    }
}

#[derive(Default)]
struct PathRecordingTransport {
    received_paths: Mutex<Vec<PathBuf>>,
}

impl AiTransport for PathRecordingTransport {
    fn analyze(
        &self,
        profile: &AiProfile,
        _api_key: &str,
        batch: &PreparedBatch,
        _cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        Ok(vec![AiSuggestion {
            token: batch.entries[0].id.clone(),
            suggested_risk: RiskLevel::Review,
            confidence: 0.8,
            reason: "Likely local cache.".to_string(),
            product_guess: None,
            profile_id: profile.id().clone(),
            scan_generation: batch.scan_generation,
        }])
    }

    fn analyze_with_paths(
        &self,
        profile: &AiProfile,
        api_key: &str,
        batch: &PreparedBatch,
        paths: &[PathBuf],
        cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        *self.received_paths.lock().unwrap() = paths.to_vec();
        self.analyze(profile, api_key, batch, cancellation)
    }
}

fn prepared_batch() -> PreparedBatch {
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

fn protected_scan_item() -> ScanItem {
    ScanItem {
        id: ScanItemId::from_raw(900),
        path: PathBuf::from(r"C:\Users\alice\.ssh"),
        display_name: "protected credentials".to_string(),
        product: Some("ssh".to_string()),
        category: ResidueCategory::Credential,
        risk: RiskLevel::Protected,
        source: SourceKind::Rule,
        logical_size: 1,
        file_count: 1,
        last_modified: Some(SystemTime::now() - Duration::from_secs(60)),
        explanation: "protected by local rule".to_string(),
        cleanup_action: CleanupAction::None,
        evidence: vec![Evidence::new("builtin-protected", "local proof")],
        scan_snapshot: None,
        classification_rule_id: Some("builtin-protected/ssh".to_string()),
    }
}

fn configured_store(data_dir: &std::path::Path) -> AiProfileStore {
    configured_store_with_active_profile(data_dir, Some(PROFILE_ID))
}

fn configured_store_with_active_profile(
    data_dir: &std::path::Path,
    active_profile_id: Option<&str>,
) -> AiProfileStore {
    fs::create_dir_all(data_dir).unwrap();
    let active_profile = active_profile_id
        .map(|id| format!(r#""{id}""#))
        .unwrap_or_else(|| "null".to_string());
    fs::write(
        data_dir.join("ai-profiles.json"),
        format!(
            r#"{{"masterEnabled":true,"activeProfileId":{active_profile},"profiles":[{{"id":"{PROFILE_ID}","name":"test profile","baseUrl":"https://example.invalid/v1","model":"test-model","apiKeyEnv":"DEVRESIDUE_AI_KEY_{}","structuredOutputMode":"strict","timeoutSecs":120,"enabled":true}}]}}"#,
            PROFILE_ID.to_ascii_uppercase(),
        ),
    )
    .unwrap();
    AiProfileStore::open(data_dir).unwrap()
}

#[test]
fn disabled_master_switch_never_reads_a_key_or_contacts_the_transport() {
    let data_dir =
        std::env::temp_dir().join(format!("devresidue-task6-service-{}", std::process::id()));
    let store = AiProfileStore::open(&data_dir).unwrap();
    let transport = Arc::new(RecordingTransport::default());
    let service_transport: Arc<dyn AiTransport> = transport.clone();
    let service = AiAdvisorService::new(store, Arc::new(NoKeyStore), service_transport);

    let error = service
        .analyze(&prepared_batch(), 42, &AiCancellationToken::new())
        .unwrap_err();

    assert_eq!(error.kind(), AiServiceErrorKind::Disabled);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(service.suggestions(&prepared_batch().id).is_none());
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn connection_test_checks_profile_readiness_before_contacting_the_transport() {
    let disabled_dir = std::env::temp_dir().join(format!(
        "devresidue-task9-service-disabled-{}",
        std::process::id()
    ));
    let disabled_transport = Arc::new(RecordingTransport::default());
    let disabled = AiAdvisorService::new(
        AiProfileStore::open(&disabled_dir).unwrap(),
        Arc::new(NoKeyStore),
        disabled_transport.clone(),
    );
    let profile_id = AiProfileId::parse(PROFILE_ID).unwrap();
    let error = disabled
        .test_connection(&profile_id, &AiCancellationToken::new())
        .unwrap_err();
    assert_eq!(error.kind(), AiServiceErrorKind::Disabled);
    assert_eq!(
        disabled_transport.connection_calls.load(Ordering::SeqCst),
        0
    );
    assert_eq!(disabled_transport.calls.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(disabled_dir);

    let configured_dir = std::env::temp_dir().join(format!(
        "devresidue-task9-service-connection-{}",
        std::process::id()
    ));
    let configured_transport = Arc::new(RecordingTransport::default());
    let configured = AiAdvisorService::new(
        configured_store(&configured_dir),
        Arc::new(StaticKeyStore),
        configured_transport.clone(),
    );
    configured
        .test_connection(&profile_id, &AiCancellationToken::new())
        .unwrap();
    assert_eq!(
        configured_transport.connection_calls.load(Ordering::SeqCst),
        1
    );
    assert_eq!(configured_transport.calls.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(configured_dir);

    let missing_key_dir = std::env::temp_dir().join(format!(
        "devresidue-task9-service-connection-missing-key-{}",
        std::process::id()
    ));
    let missing_key_transport = Arc::new(RecordingTransport::default());
    let missing_key = AiAdvisorService::new(
        configured_store(&missing_key_dir),
        Arc::new(NoKeyStore),
        missing_key_transport.clone(),
    );
    let error = missing_key
        .test_connection(&profile_id, &AiCancellationToken::new())
        .unwrap_err();
    assert_eq!(error.kind(), AiServiceErrorKind::MissingKey);
    assert_eq!(
        missing_key_transport
            .connection_calls
            .load(Ordering::SeqCst),
        0
    );
    assert_eq!(missing_key_transport.calls.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(missing_key_dir);
}

#[test]
fn successful_validated_suggestions_exist_only_in_the_process_local_batch_cache() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-success-{}",
        std::process::id()
    ));
    let store = configured_store(&data_dir);
    let service = AiAdvisorService::new(
        store,
        Arc::new(StaticKeyStore),
        Arc::new(ValidatingTransport),
    );
    let batch = prepared_batch();

    let suggestions = service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap();

    assert_eq!(suggestions.len(), 1);
    assert_eq!(service.suggestions(&batch.id), Some(suggestions));
    assert!(!data_dir.join("ai-audit.jsonl").exists());
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn stale_generation_never_reads_profile_or_contacts_transport() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-stale-{}",
        std::process::id()
    ));
    let transport = Arc::new(RecordingTransport::default());
    let service = AiAdvisorService::new(
        AiProfileStore::open(&data_dir).unwrap(),
        Arc::new(NoKeyStore),
        transport.clone(),
    );
    let batch = prepared_batch();

    let error = service
        .analyze(
            &batch,
            batch.scan_generation + 1,
            &AiCancellationToken::new(),
        )
        .unwrap_err();

    assert_eq!(error.kind(), AiServiceErrorKind::StaleGeneration);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(service.suggestions(&batch.id).is_none());
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn missing_active_profile_or_key_never_contacts_transport() {
    let no_active_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-no-active-{}",
        std::process::id()
    ));
    let no_active_transport = Arc::new(RecordingTransport::default());
    let no_active_service = AiAdvisorService::new(
        configured_store_with_active_profile(&no_active_dir, None),
        Arc::new(StaticKeyStore),
        no_active_transport.clone(),
    );
    let batch = prepared_batch();

    let no_active_error = no_active_service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap_err();

    assert_eq!(no_active_error.kind(), AiServiceErrorKind::NoActiveProfile);
    assert_eq!(no_active_transport.calls.load(Ordering::SeqCst), 0);
    assert!(no_active_service.suggestions(&batch.id).is_none());
    let _ = fs::remove_dir_all(no_active_dir);

    let missing_key_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-missing-key-{}",
        std::process::id()
    ));
    let missing_key_transport = Arc::new(RecordingTransport::default());
    let missing_key_service = AiAdvisorService::new(
        configured_store(&missing_key_dir),
        Arc::new(NoKeyStore),
        missing_key_transport.clone(),
    );

    let missing_key_error = missing_key_service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap_err();

    assert_eq!(missing_key_error.kind(), AiServiceErrorKind::MissingKey);
    assert_eq!(missing_key_transport.calls.load(Ordering::SeqCst), 0);
    assert!(missing_key_service.suggestions(&batch.id).is_none());
    let _ = fs::remove_dir_all(missing_key_dir);
}

#[test]
fn empty_sanitized_batch_never_contacts_the_remote_transport() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-empty-batch-{}",
        std::process::id()
    ));
    let transport = Arc::new(RecordingTransport::default());
    let service = AiAdvisorService::new(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        transport.clone(),
    );
    let mut batch = prepared_batch();
    batch.entries.clear();

    let error = service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap_err();

    assert_eq!(error.kind(), AiServiceErrorKind::NoEligibleEntries);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(service.suggestions(&batch.id).is_none());
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn protected_scan_item_is_filtered_before_service_and_does_not_affect_later_local_candidates() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-protected-{}",
        std::process::id()
    ));
    let transport = Arc::new(RecordingTransport::default());
    let service = AiAdvisorService::new(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        transport.clone(),
    );
    let protected = protected_scan_item();
    let protected_before = protected.clone();
    let protected_batch = MetadataSanitizer::new()
        .prepare(
            std::slice::from_ref(&protected),
            42,
            AiProfileId::parse(PROFILE_ID).unwrap(),
        )
        .unwrap();

    assert!(protected_batch.entries.is_empty());
    let error = service
        .analyze(
            &protected_batch,
            protected_batch.scan_generation,
            &AiCancellationToken::new(),
        )
        .unwrap_err();

    assert_eq!(error.kind(), AiServiceErrorKind::NoEligibleEntries);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(service.suggestions(&protected_batch.id).is_none());
    assert_eq!(protected, protected_before);

    let mut later_local_candidate = protected_scan_item();
    later_local_candidate.id = ScanItemId::from_raw(901);
    later_local_candidate.risk = RiskLevel::Unknown;
    later_local_candidate.category = ResidueCategory::DeveloperCache;
    later_local_candidate.classification_rule_id = None;
    let later_batch = MetadataSanitizer::new()
        .prepare(
            &[later_local_candidate],
            43,
            AiProfileId::parse(PROFILE_ID).unwrap(),
        )
        .unwrap();
    assert_eq!(later_batch.entries.len(), 1);
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn preparation_binds_only_the_sanitized_snapshot_candidates_to_the_review_batch() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task8-service-preparation-{}",
        std::process::id()
    ));
    let service = AiAdvisorService::new(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        Arc::new(RecordingTransport::default()),
    );
    let protected = protected_scan_item();
    let mut locally_classified_review = protected_scan_item();
    locally_classified_review.id = ScanItemId::from_raw(901);
    locally_classified_review.risk = RiskLevel::Review;
    locally_classified_review.category = ResidueCategory::Session;
    locally_classified_review.classification_rule_id = Some("builtin-detection/session".into());
    locally_classified_review.cleanup_action = CleanupAction::RecycleBin;

    let review = service
        .prepare_review_batch(
            &[protected, locally_classified_review],
            43,
            AiProfileId::parse(PROFILE_ID).unwrap(),
        )
        .unwrap();

    assert_eq!(review.batch().scan_generation, 43);
    assert_eq!(review.batch().entries.len(), 1);
    assert_eq!(review.item_ids(), &[ScanItemId::from_raw(901)]);
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn full_paths_reach_the_transport_only_after_explicit_batch_authorization() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task14-service-path-disclosure-{}",
        std::process::id()
    ));
    let transport = Arc::new(PathRecordingTransport::default());
    let service = AiAdvisorService::new(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        transport.clone(),
    );
    let mut candidate = protected_scan_item();
    candidate.id = ScanItemId::from_raw(901);
    candidate.path = PathBuf::from(r"C:\Users\alice\AppData\Local\cache");
    candidate.risk = RiskLevel::Unknown;
    candidate.category = ResidueCategory::DeveloperCache;
    candidate.classification_rule_id = None;

    let disclosed = service
        .prepare_review_batch_with_paths(
            std::slice::from_ref(&candidate),
            43,
            AiProfileId::parse(PROFILE_ID).unwrap(),
            true,
        )
        .unwrap();
    assert!(disclosed.includes_paths());
    service
        .analyze(
            disclosed.batch(),
            disclosed.batch().scan_generation,
            &AiCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        *transport.received_paths.lock().unwrap(),
        vec![candidate.path.clone()]
    );

    let undisclosed = service
        .prepare_review_batch(
            std::slice::from_ref(&candidate),
            44,
            AiProfileId::parse(PROFILE_ID).unwrap(),
        )
        .unwrap();
    assert!(!undisclosed.includes_paths());
    service
        .analyze(
            undisclosed.batch(),
            undisclosed.batch().scan_generation,
            &AiCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        *transport.received_paths.lock().unwrap(),
        vec![candidate.path.clone()]
    );
    let _ = fs::remove_dir_all(data_dir);
}

#[test]
fn invalid_transport_result_is_not_cached_or_persisted_as_a_rule() {
    let data_dir = std::env::temp_dir().join(format!(
        "devresidue-task6-service-invalid-result-{}",
        std::process::id()
    ));
    let transport = Arc::new(RecordingTransport::default());
    let service = AiAdvisorService::new(
        configured_store(&data_dir),
        Arc::new(StaticKeyStore),
        transport.clone(),
    );
    let batch = prepared_batch();

    let error = service
        .analyze(&batch, batch.scan_generation, &AiCancellationToken::new())
        .unwrap_err();

    assert_eq!(error.kind(), AiServiceErrorKind::InvalidResponse);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    assert!(service.suggestions(&batch.id).is_none());
    assert!(!data_dir.join("user-rules.yaml").exists());
    let _ = fs::remove_dir_all(data_dir);
}
