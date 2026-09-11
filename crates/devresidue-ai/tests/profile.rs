use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use devresidue_ai::{AiProfileInput, AiProfileStore};
use devresidue_core::ai::{
    AiApiProtocol, AiKeyEnvName, AiProfileFilePort, EnvKeyStore, PendingProfileTxn,
    ProfileFileLock, ProfileTxnOp, ProfileTxnState, RecoveryStatus, StructuredOutputMode,
};
use uuid::Uuid;

#[derive(Clone, Default)]
struct FakeEnvKeyStore {
    values: Arc<Mutex<BTreeMap<String, String>>>,
    fail_set: Arc<Mutex<bool>>,
    fail_remove: Arc<Mutex<bool>>,
    fail_process_refresh: Arc<Mutex<bool>>,
}

impl EnvKeyStore for FakeEnvKeyStore {
    fn set(&self, name: &AiKeyEnvName, value: &str) -> Result<(), String> {
        if *self.fail_set.lock().unwrap() {
            return Err("injected set failure".to_string());
        }
        self.values
            .lock()
            .unwrap()
            .insert(name.as_str().to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        Ok(self.values.lock().unwrap().get(name.as_str()).cloned())
    }

    fn remove(&self, name: &AiKeyEnvName) -> Result<(), String> {
        if *self.fail_remove.lock().unwrap() {
            return Err("injected remove failure".to_string());
        }
        self.values.lock().unwrap().remove(name.as_str());
        Ok(())
    }

    fn get_process(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        if *self.fail_process_refresh.lock().unwrap() {
            return Err("injected process refresh failure".to_string());
        }
        self.get(name)
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "devresidue-task4-profile-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
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

#[derive(Debug, Clone, Copy, Default)]
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

#[derive(Debug)]
struct FailingProfileFilePort {
    fail_on_call: usize,
    calls: AtomicUsize,
}

impl FailingProfileFilePort {
    fn new(fail_on_call: usize) -> Self {
        Self {
            fail_on_call,
            calls: AtomicUsize::new(0),
        }
    }
}

impl AiProfileFilePort for FailingProfileFilePort {
    fn acquire_exclusive(&self, lock_path: &Path) -> Result<Box<dyn ProfileFileLock>, String> {
        TestProfileFilePort.acquire_exclusive(lock_path)
    }

    fn atomic_replace(&self, temporary: &Path, target: &Path) -> Result<(), String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_on_call {
            return Err("injected atomic replacement failure".to_string());
        }
        TestProfileFilePort.atomic_replace(temporary, target)
    }
}

fn open_store(dir: &TempDir) -> AiProfileStore {
    AiProfileStore::open_with_file_port(dir.path(), Arc::new(TestProfileFilePort)).unwrap()
}

fn input(name: &str) -> AiProfileInput {
    AiProfileInput {
        name: name.to_string(),
        base_url: "https://example.invalid/v1".to_string(),
        model: "model-x".to_string(),
        api_protocol: AiApiProtocol::OpenAiCompatible,
        structured_output_mode: StructuredOutputMode::Auto,
        timeout_secs: 120,
        enabled: true,
    }
}

#[test]
fn profile_file_never_contains_the_api_key_and_round_trips_non_secret_fields() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let key = "sk-test-never-persist";

    let profile = store.upsert(input("gateway"), key, &keys).unwrap();
    let serialized = fs::read_to_string(store.path()).unwrap();

    assert!(!serialized.contains(key));
    assert!(serialized.contains(profile.id().as_str()));
    assert!(serialized.contains("\"baseUrl\""));
    assert!(serialized.contains("\"apiProtocol\": \"openai_compatible\""));
    assert!(serialized.contains("\"structuredOutputMode\": \"auto\""));
    assert_eq!(profile.name(), "gateway");
    assert_eq!(
        profile.api_key_env().as_str(),
        keys_name(profile.id().as_str())
    );

    let loaded = store.load().unwrap();
    assert_eq!(loaded.profiles().len(), 1);
    assert_eq!(loaded.profiles()[0].id(), profile.id());
}

#[test]
fn legacy_profile_without_api_protocol_defaults_to_openai_compatible() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let profile = store.upsert(input("legacy"), "legacy-key", &keys).unwrap();

    let mut serialized: serde_json::Value =
        serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
    serialized["profiles"][0]
        .as_object_mut()
        .unwrap()
        .remove("apiProtocol");
    fs::write(store.path(), serde_json::to_vec(&serialized).unwrap()).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(loaded.profiles()[0].id(), profile.id());
    assert_eq!(
        loaded.profiles()[0].api_protocol(),
        AiApiProtocol::OpenAiCompatible
    );
}

#[test]
fn profile_storage_rejects_corrupt_and_unknown_fields_fail_closed() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    fs::write(store.path(), b"not-json").unwrap();
    assert!(store.load().is_err());

    fs::write(
        store.path(),
        br#"{"masterEnabled":false,"activeProfileId":null,"profiles":[],"unexpected":true}"#,
    )
    .unwrap();
    assert!(store.load().is_err());

    let id = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";
    fs::write(
        store.path(),
        format!(
            r#"{{"masterEnabled":false,"activeProfileId":null,"profiles":[{{"id":"{id}","name":"x","baseUrl":"https://example.invalid/v1","model":"m","apiKeyEnv":"DEVRESIDUE_AI_KEY_NOT_THIS_ID","structuredOutputMode":"auto","timeoutSecs":120,"enabled":true}}]}}"#
        ),
    )
    .unwrap();
    assert!(store.load().is_err());
}

#[test]
fn profile_validation_rejects_insecure_urls_empty_fields_and_zero_timeout() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();

    let mut insecure = input("insecure");
    insecure.base_url = "http://remote.example/v1".to_string();
    assert!(store.upsert(insecure, "unused", &keys).is_err());

    let mut empty_host = input("empty-host");
    empty_host.base_url = "https://".to_string();
    assert!(store.upsert(empty_host, "unused", &keys).is_err());

    let mut zero_timeout = input("zero-timeout");
    zero_timeout.timeout_secs = 0;
    assert!(store.upsert(zero_timeout, "unused", &keys).is_err());
}

#[test]
fn profile_validation_accepts_loopback_ipv4_http() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let mut loopback = input("loopback");
    loopback.base_url = "http://127.0.0.1:8317/v1".to_string();

    let profile = store
        .upsert(loopback, "test-key", &keys)
        .expect("loopback HTTP should be accepted");

    assert_eq!(profile.base_url(), "http://127.0.0.1:8317/v1");
}

#[test]
fn failed_key_removal_keeps_marker_and_does_not_compensate_metadata_or_key() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let profile = store.upsert(input("keep"), "key-to-keep", &keys).unwrap();
    *keys.fail_remove.lock().unwrap() = true;

    assert!(store.delete(profile.id().clone(), &keys).is_err());
    assert!(store.txn_path().exists());
    assert_eq!(store.load().unwrap().profiles().len(), 0);
    assert_eq!(
        keys.get(profile.api_key_env()).unwrap().as_deref(),
        Some("key-to-keep")
    );
}

#[test]
fn failed_key_write_keeps_marker_and_does_not_compensate_previous_file_or_key() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let first = store.upsert(input("before"), "first-key", &keys).unwrap();
    *keys.fail_set.lock().unwrap() = true;

    let mut changed = input("after");
    changed.base_url = "http://localhost:43123/v1".to_string();
    assert!(store
        .update(first.id().clone(), changed, "second-key", &keys)
        .is_err());
    assert!(store.txn_path().exists());
    assert_eq!(store.load().unwrap().profiles()[0].name(), "after");
    assert_eq!(
        keys.get(first.api_key_env()).unwrap().as_deref(),
        Some("first-key")
    );
}

#[test]
fn deleting_profile_removes_only_its_generated_environment_variable() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let first = store.upsert(input("one"), "key-one", &keys).unwrap();
    let second = store.upsert(input("two"), "key-two", &keys).unwrap();

    store.delete(first.id().clone(), &keys).unwrap();

    assert_eq!(keys.get(first.api_key_env()).unwrap(), None);
    assert_eq!(
        keys.get(second.api_key_env()).unwrap().as_deref(),
        Some("key-two")
    );
    assert_eq!(store.load().unwrap().profiles().len(), 1);
}

#[test]
fn master_switch_and_active_profile_are_non_secret_persistent_state() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let profile = store.upsert(input("active"), "key-active", &keys).unwrap();

    store.set_master_enabled(true).unwrap();
    store
        .set_active_profile(Some(profile.id().clone()))
        .unwrap();
    let loaded = store.load().unwrap();
    assert!(loaded.master_enabled());
    assert_eq!(loaded.active_profile_id(), Some(profile.id()));
    let serialized = fs::read_to_string(store.path()).unwrap();
    assert!(serialized.contains("\"masterEnabled\": true"));
    assert!(serialized.contains("\"activeProfileId\":"));
    assert!(!serialized.contains("key-active"));
}

fn keys_name(id: &str) -> String {
    format!("DEVRESIDUE_AI_KEY_{}", id.to_ascii_uppercase())
}

#[test]
fn active_profile_marker_is_strict_non_secret_and_isolates_only_target() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let first = store.upsert(input("first"), "first-key", &keys).unwrap();
    let second = store.upsert(input("second"), "second-key", &keys).unwrap();
    store.set_active_profile(Some(first.id().clone())).unwrap();

    let marker = PendingProfileTxn {
        version: 1,
        state: ProfileTxnState::Active,
        op: ProfileTxnOp::Update,
        profile_id: first.id().clone(),
    };
    fs::write(store.txn_path(), serde_json::to_vec(&marker).unwrap()).unwrap();

    let status = store.recovery_status().unwrap();
    assert_eq!(status, RecoveryStatus::RecoveryRequired(marker.clone()));
    assert!(store.resolve_usable_profile(first.id()).unwrap().is_none());
    assert_eq!(
        store
            .resolve_usable_profile(second.id())
            .unwrap()
            .unwrap()
            .id(),
        second.id()
    );
    assert_eq!(store.load().unwrap().active_profile_id(), None);

    let marker_text = fs::read_to_string(store.txn_path()).unwrap();
    assert!(!marker_text.contains("first-key"));
    assert!(!marker_text.contains("second-key"));
    assert!(!marker_text.contains("https://"));
    assert!(!marker_text.contains("model-x"));
}

#[test]
fn corrupt_profile_marker_blocks_resolution_and_allordinary_writes() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let profile = store
        .upsert(input("existing"), "existing-key", &keys)
        .unwrap();
    fs::write(
        store.txn_path(),
        br#"{"version":1,"state":"active","op":"update","profileId":"bad","extra":true}"#,
    )
    .unwrap();

    assert_eq!(
        store.recovery_status().unwrap(),
        RecoveryStatus::InvalidMarker
    );
    assert!(store.resolve_usable_profile(profile.id()).is_err());
    assert!(store.set_master_enabled(true).is_err());
    assert!(store
        .upsert(input("blocked"), "blocked-key", &keys)
        .is_err());
    assert!(store.txn_path().exists());
}

#[test]
fn process_refresh_failure_keeps_marker_and_never_compensates_key_or_metadata() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    *keys.fail_process_refresh.lock().unwrap() = true;

    assert!(store
        .upsert(input("partial"), "partial-key", &keys)
        .is_err());
    assert!(store.txn_path().exists());
    assert!(!String::from_utf8_lossy(&fs::read(store.path()).unwrap()).contains("partial-key"));
    let marker_text = fs::read_to_string(store.txn_path()).unwrap();
    assert!(!marker_text.contains("partial-key"));
}

#[test]
fn profile_file_read_error_fails_closed_instead_of_becoming_default_state() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    fs::create_dir(store.path()).unwrap();
    assert!(store.load().is_err());
}

#[test]
fn failed_marker_publish_removes_profile_temporary_file_and_leaves_no_marker() {
    let dir = TempDir::new();
    let store =
        AiProfileStore::open_with_file_port(dir.path(), Arc::new(FailingProfileFilePort::new(1)))
            .unwrap();
    let keys = FakeEnvKeyStore::default();

    assert!(store
        .upsert(input("marker-failure"), "marker-key", &keys)
        .is_err());
    assert!(!store.txn_path().exists());
    let leftovers = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "temporary files remain: {leftovers:?}"
    );
}

#[test]
fn failed_metadata_replace_keeps_non_secret_marker_and_cleans_temp_file() {
    let dir = TempDir::new();
    let store =
        AiProfileStore::open_with_file_port(dir.path(), Arc::new(FailingProfileFilePort::new(2)))
            .unwrap();
    let keys = FakeEnvKeyStore::default();

    assert!(store
        .upsert(input("metadata-failure"), "metadata-key", &keys)
        .is_err());
    assert!(store.txn_path().exists());
    assert!(!store.path().exists());
    assert!(fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .all(|name| !name.ends_with(".tmp")));
}

#[test]
fn recovery_resubmit_reuses_marker_target_and_clears_only_after_process_verification() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    *keys.fail_process_refresh.lock().unwrap() = true;
    assert!(store
        .upsert(input("recoverable"), "old-key", &keys)
        .is_err());
    let marker: PendingProfileTxn = match store.recovery_status().unwrap() {
        RecoveryStatus::RecoveryRequired(marker) => marker,
        status => panic!("expected recovery marker, got {status:?}"),
    };
    let id = marker.profile_id.clone();
    *keys.fail_process_refresh.lock().unwrap() = false;
    let profile = store
        .resubmit_recovery(id.clone(), input("recovered"), "fresh-key", &keys)
        .unwrap();
    assert_eq!(profile.id(), &id);
    assert!(matches!(
        store.recovery_status().unwrap(),
        RecoveryStatus::Clean
    ));
    assert_eq!(
        store.resolve_usable_profile(&id).unwrap().unwrap().name(),
        "recovered"
    );
    assert_eq!(
        keys.get(profile.api_key_env()).unwrap().as_deref(),
        Some("fresh-key")
    );
}

#[test]
fn delete_recovery_rejects_a_non_delete_marker_without_changing_metadata_or_key() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let profile = store
        .upsert(input("keep-on-update-recovery"), "keep-key", &keys)
        .unwrap();
    fs::write(
        store.txn_path(),
        serde_json::to_vec(&PendingProfileTxn {
            version: 1,
            state: ProfileTxnState::Active,
            op: ProfileTxnOp::Update,
            profile_id: profile.id().clone(),
        })
        .unwrap(),
    )
    .unwrap();

    assert!(store.delete_recovery(profile.id().clone(), &keys).is_err());
    assert!(store.txn_path().exists());
    assert_eq!(store.load().unwrap().profiles()[0].id(), profile.id());
    assert_eq!(
        keys.get(profile.api_key_env()).unwrap().as_deref(),
        Some("keep-key")
    );
}

#[test]
fn abandon_recovery_disables_profile_clears_active_and_removes_derived_key() {
    let dir = TempDir::new();
    let store = open_store(&dir);
    let keys = FakeEnvKeyStore::default();
    let profile = store
        .upsert(input("abandon"), "abandon-key", &keys)
        .unwrap();
    store
        .set_active_profile(Some(profile.id().clone()))
        .unwrap();
    fs::write(
        store.txn_path(),
        serde_json::to_vec(&PendingProfileTxn {
            version: 1,
            state: ProfileTxnState::Active,
            op: ProfileTxnOp::Update,
            profile_id: profile.id().clone(),
        })
        .unwrap(),
    )
    .unwrap();

    store.abandon_recovery(profile.id().clone(), &keys).unwrap();
    assert!(matches!(
        store.recovery_status().unwrap(),
        RecoveryStatus::Clean
    ));
    assert_eq!(store.load().unwrap().active_profile_id(), None);
    assert_eq!(keys.get(profile.api_key_env()).unwrap(), None);
    assert!(store
        .resolve_usable_profile(profile.id())
        .unwrap()
        .is_none());
}
