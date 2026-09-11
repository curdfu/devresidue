//! Cross-layer safety fixtures for the optional remote-AI advisor.
//!
//! These tests deliberately stop at local rule confirmation. They do not
//! construct a cleanup plan or a delete port: a later CleanupPlanner /
//! CleanupEngine flow remains the only route to execution.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use devresidue_ai::{
    AiAdvisorService, AiCancellationToken, AiConfirmationConfig, AiConfirmationSelection,
    AiProfileStore, AiServiceError, AiServiceErrorKind, AiTransport,
};
use devresidue_core::ai::{
    AiKeyEnvName, AiProfile, AiProfileFilePort, AiProfileId, AiSuggestion, EnvKeyStore,
    PreparedBatch, ProfileFileLock,
};
use devresidue_core::rules::user::upsert_detection_rule;
use devresidue_core::{
    CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, ScanItemId, SourceKind,
};
use devresidue_platform_windows::user_rule_tx::WindowsUserRuleTransactionPort;

const PROFILE_ID: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";
const TEST_KEY: &str = "e2e-one-shot-input";
const GENERATION: u64 = 77;
static NEXT_DIR: AtomicUsize = AtomicUsize::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let serial = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "devresidue-task11-e2e-{label}-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Portable test-only implementation. Windows profile-file atomicity has its
/// own platform tests; this fixture needs deterministic profile deletion in
/// order to prove it does not remove separately confirmed user rules.
#[derive(Clone, Copy, Default)]
struct TestProfileFilePort;

struct TestProfileLock {
    path: PathBuf,
    _file: File,
}

impl Drop for TestProfileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl ProfileFileLock for TestProfileLock {}

impl AiProfileFilePort for TestProfileFilePort {
    fn acquire_exclusive(&self, lock_path: &Path) -> Result<Box<dyn ProfileFileLock>, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
            .map_err(|_| "test profile lock held".to_string())?;
        Ok(Box::new(TestProfileLock {
            path: lock_path.to_path_buf(),
            _file: file,
        }))
    }

    fn atomic_replace(&self, temporary: &Path, target: &Path) -> Result<(), String> {
        let bytes = fs::read(temporary).map_err(|_| "test source read failed".to_string())?;
        fs::write(target, bytes).map_err(|_| "test target write failed".to_string())?;
        fs::remove_file(temporary).map_err(|_| "test temporary cleanup failed".to_string())
    }
}

#[derive(Clone, Default)]
struct TestKeyStore {
    values: Arc<Mutex<BTreeMap<String, String>>>,
}

impl TestKeyStore {
    fn with_profile_key() -> Self {
        let store = Self::default();
        store
            .values
            .lock()
            .unwrap()
            .insert(key_env_name(), TEST_KEY.to_string());
        store
    }
}

impl EnvKeyStore for TestKeyStore {
    fn set(&self, name: &AiKeyEnvName, value: &str) -> Result<(), String> {
        self.values
            .lock()
            .unwrap()
            .insert(name.as_str().to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        Ok(self.values.lock().unwrap().get(name.as_str()).cloned())
    }

    fn get_process(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        self.get(name)
    }

    fn remove(&self, name: &AiKeyEnvName) -> Result<(), String> {
        self.values.lock().unwrap().remove(name.as_str());
        Ok(())
    }
}

struct FixtureTransport {
    suggested_risk: RiskLevel,
    calls: AtomicUsize,
}

impl FixtureTransport {
    fn new(suggested_risk: RiskLevel) -> Self {
        Self {
            suggested_risk,
            calls: AtomicUsize::new(0),
        }
    }
}

impl AiTransport for FixtureTransport {
    fn analyze(
        &self,
        profile: &AiProfile,
        _api_key: &str,
        batch: &PreparedBatch,
        _cancellation: &AiCancellationToken,
    ) -> Result<Vec<AiSuggestion>, AiServiceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(batch
            .entries
            .iter()
            .map(|entry| AiSuggestion {
                token: entry.id.clone(),
                suggested_risk: self.suggested_risk,
                confidence: 0.72,
                reason: "fixture-only classification hint".to_string(),
                product_guess: Some("fixture product".to_string()),
                profile_id: profile.id().clone(),
                scan_generation: batch.scan_generation,
            })
            .collect())
    }
}

fn profile_id() -> AiProfileId {
    AiProfileId::parse(PROFILE_ID).unwrap()
}

fn key_env_name() -> String {
    format!("DEVRESIDUE_AI_KEY_{}", PROFILE_ID.to_ascii_uppercase())
}

fn configured_store(data_dir: &Path, base_url: &str) -> AiProfileStore {
    fs::write(
        data_dir.join("ai-profiles.json"),
        format!(
            r#"{{"masterEnabled":true,"activeProfileId":"{PROFILE_ID}","profiles":[{{"id":"{PROFILE_ID}","name":"e2e profile","baseUrl":"{base_url}","model":"fixture-model","apiKeyEnv":"{}","structuredOutputMode":"strict","timeoutSecs":30,"enabled":true}}]}}"#,
            key_env_name()
        ),
    )
    .unwrap();
    AiProfileStore::open_with_file_port(data_dir, Arc::new(TestProfileFilePort)).unwrap()
}

fn empty_builtin_rules_dir(data_dir: &Path) -> PathBuf {
    let path = data_dir.join("builtin-rules");
    fs::create_dir_all(&path).unwrap();
    path
}

fn confirmed_service(
    data_dir: &Path,
    base_url: &str,
    transport: Arc<dyn AiTransport>,
) -> (AiAdvisorService, AiProfileStore, TestKeyStore) {
    let profile_store = configured_store(data_dir, base_url);
    let keys = TestKeyStore::with_profile_key();
    let service = AiAdvisorService::with_confirmation(
        profile_store.clone(),
        Arc::new(keys.clone()),
        transport,
        AiConfirmationConfig::new(
            data_dir,
            Arc::new(WindowsUserRuleTransactionPort::new()),
            empty_builtin_rules_dir(data_dir),
        ),
    );
    (service, profile_store, keys)
}

fn candidate(id: u64, risk: RiskLevel, leaf: &str) -> ScanItem {
    ScanItem {
        id: ScanItemId::from_raw(id),
        path: PathBuf::from(format!(r"C:\ai-advisor-e2e\{leaf}")),
        display_name: leaf.to_string(),
        product: Some("fixture product".to_string()),
        category: ResidueCategory::DeveloperCache,
        risk,
        source: SourceKind::UnknownProvider,
        logical_size: 1024,
        file_count: 1,
        last_modified: Some(SystemTime::now() - Duration::from_secs(60)),
        explanation: "fixture scan item".to_string(),
        cleanup_action: CleanupAction::None,
        evidence: vec![Evidence::new("path-layout", "fixture evidence")],
        scan_snapshot: None,
        classification_rule_id: None,
    }
}

fn user_rule_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join("rules")
        .join("user")
        .join("user-dispositions.yaml")
}

#[test]
fn remote_advice_reclassifies_only_after_local_confirmation_and_retains_rules_after_profile_delete()
{
    let data_dir = TempDir::new("confirm");
    let transport = Arc::new(FixtureTransport::new(RiskLevel::Review));
    let (service, profiles, keys) = confirmed_service(
        data_dir.path(),
        "https://example.invalid/v1",
        transport.clone(),
    );
    let unknown = candidate(101, RiskLevel::Unknown, "unknown-cache");
    let review = candidate(102, RiskLevel::Review, "review-state");

    let review_batch = service
        .prepare_review_batch(&[unknown.clone(), review.clone()], GENERATION, profile_id())
        .unwrap();
    assert_eq!(review_batch.item_ids(), &[unknown.id, review.id]);
    let batch_wire = serde_json::to_string(review_batch.batch()).unwrap();
    assert!(!batch_wire.contains(&unknown.path.display().to_string()));
    assert!(!batch_wire.contains(TEST_KEY));

    let suggestions = service
        .analyze(
            review_batch.batch(),
            GENERATION,
            &AiCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(suggestions.len(), 2);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

    // The only local mutation is a Core-owned all-or-nothing user-rule
    // transaction. This service has no CleanupPlanner or deletion import.
    let result = service
        .confirm_suggestions(
            &review_batch.batch().id,
            GENERATION,
            &[
                AiConfirmationSelection::new(
                    unknown.id,
                    RiskLevel::Safe,
                    ResidueCategory::DeveloperCache,
                ),
                AiConfirmationSelection::new(
                    review.id,
                    RiskLevel::Review,
                    ResidueCategory::DeveloperCache,
                ),
            ],
        )
        .unwrap();
    assert_eq!(result.written_rule_ids.len(), 2);
    assert_eq!(unknown.cleanup_action, CleanupAction::None);
    assert_eq!(review.cleanup_action, CleanupAction::None);

    let rules_before_delete = fs::read_to_string(user_rule_path(data_dir.path())).unwrap();
    assert!(rules_before_delete.contains("user-ai/"));
    assert!(rules_before_delete.contains("risk: safe"));
    assert!(rules_before_delete.contains("risk: review"));
    let profile_text = fs::read_to_string(data_dir.path().join("ai-profiles.json")).unwrap();
    let audit_text = fs::read_to_string(data_dir.path().join("ai-audit.jsonl")).unwrap();
    for private_text in [
        TEST_KEY,
        r"C:\ai-advisor-e2e",
        "fixture-only classification hint",
        "fixture evidence",
    ] {
        assert!(!profile_text.contains(private_text));
        assert!(!audit_text.contains(private_text));
    }

    profiles.delete(profile_id(), &keys).unwrap();
    assert!(profiles.load().unwrap().profiles().is_empty());
    assert!(!keys.values.lock().unwrap().contains_key(&key_env_name()));
    assert_eq!(
        fs::read_to_string(user_rule_path(data_dir.path())).unwrap(),
        rules_before_delete,
        "deleting an AI profile never removes confirmed local rules"
    );
}

#[test]
fn protected_item_never_reaches_the_transport() {
    let data_dir = TempDir::new("protected");
    let transport = Arc::new(FixtureTransport::new(RiskLevel::Review));
    let (service, _profiles, _keys) = confirmed_service(
        data_dir.path(),
        "https://example.invalid/v1",
        transport.clone(),
    );
    let mut protected = candidate(201, RiskLevel::Protected, "credential-dir");
    protected.path = PathBuf::from(r"C:\Users\alice\.ssh");
    protected.category = ResidueCategory::Credential;
    protected.classification_rule_id = Some("builtin-protected/ssh".to_string());

    let error = service
        .prepare_review_batch(&[protected], GENERATION, profile_id())
        .unwrap_err();
    assert_eq!(error.kind(), AiServiceErrorKind::NoEligibleEntries);
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn unknown_model_risk_requires_a_concrete_user_final_risk() {
    let data_dir = TempDir::new("unknown-final-risk");
    let transport = Arc::new(FixtureTransport::new(RiskLevel::Unknown));
    let (service, _profiles, _keys) =
        confirmed_service(data_dir.path(), "https://example.invalid/v1", transport);
    let unknown = candidate(301, RiskLevel::Unknown, "needs-human-choice");
    let review_batch = service
        .prepare_review_batch(std::slice::from_ref(&unknown), GENERATION, profile_id())
        .unwrap();
    let suggestions = service
        .analyze(
            review_batch.batch(),
            GENERATION,
            &AiCancellationToken::new(),
        )
        .unwrap();
    assert_eq!(suggestions[0].suggested_risk, RiskLevel::Unknown);

    let error = service
        .confirm_suggestions(
            &review_batch.batch().id,
            GENERATION,
            &[AiConfirmationSelection::new(
                unknown.id,
                RiskLevel::Unknown,
                ResidueCategory::DeveloperCache,
            )],
        )
        .unwrap_err();
    assert_eq!(error.kind(), AiServiceErrorKind::Configuration);
    assert!(!user_rule_path(data_dir.path()).exists());

    service
        .confirm_suggestions(
            &review_batch.batch().id,
            GENERATION,
            &[AiConfirmationSelection::new(
                unknown.id,
                RiskLevel::Review,
                ResidueCategory::DeveloperCache,
            )],
        )
        .unwrap();
    assert!(user_rule_path(data_dir.path()).exists());
}

#[test]
fn manual_same_anchor_conflict_rolls_back_the_whole_confirmation_batch() {
    let data_dir = TempDir::new("manual-conflict");
    let transport = Arc::new(FixtureTransport::new(RiskLevel::Review));
    let (service, _profiles, _keys) =
        confirmed_service(data_dir.path(), "https://example.invalid/v1", transport);
    let first = candidate(501, RiskLevel::Unknown, "first-cache");
    let second = candidate(502, RiskLevel::Unknown, "manual-rule-cache");
    let user_rules_dir = data_dir.path().join("rules").join("user");
    fs::create_dir_all(&user_rules_dir).unwrap();
    upsert_detection_rule(
        &user_rules_dir,
        &second.path,
        second.product.as_deref(),
        second.category,
        RiskLevel::Review,
    )
    .unwrap();
    let rules_before_confirmation = fs::read_to_string(user_rule_path(data_dir.path())).unwrap();

    let review_batch = service
        .prepare_review_batch(&[first.clone(), second.clone()], GENERATION, profile_id())
        .unwrap();
    service
        .analyze(
            review_batch.batch(),
            GENERATION,
            &AiCancellationToken::new(),
        )
        .unwrap();
    let error = service
        .confirm_suggestions(
            &review_batch.batch().id,
            GENERATION,
            &[
                AiConfirmationSelection::new(
                    first.id,
                    RiskLevel::Safe,
                    ResidueCategory::DeveloperCache,
                ),
                AiConfirmationSelection::new(
                    second.id,
                    RiskLevel::Review,
                    ResidueCategory::DeveloperCache,
                ),
            ],
        )
        .unwrap_err();

    assert_eq!(error.kind(), AiServiceErrorKind::ConfirmationConflict);
    let rules = fs::read_to_string(user_rule_path(data_dir.path())).unwrap();
    assert_eq!(rules, rules_before_confirmation);
}
