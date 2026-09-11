//! Non-sensitive AI profile persistence and crash isolation.
//!
//! The profile file contains metadata only.  A key is accepted as a one-way
//! operation argument, passed to [`EnvKeyStore`](devresidue_core::ai::EnvKeyStore),
//! and never placed in a DTO, marker, error, or compensating write.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use devresidue_core::ai::{
    AiApiProtocol, AiProfile, AiProfileFilePort, AiProfileId, EnvKeyStore, PendingProfileTxn,
    ProfileFileLock, ProfileTxnOp, ProfileTxnState, RecoveryStatus, StructuredOutputMode,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const PROFILE_FILE_NAME: &str = "ai-profiles.json";
const PROFILE_TXN_FILE_NAME: &str = "ai-profiles.txn.json";
const PROFILE_LOCK_FILE_NAME: &str = "ai-profiles.lock";
const PROFILE_TXN_VERSION: u32 = 1;

/// Non-secret input used when creating or updating a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiProfileInput {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_protocol: AiApiProtocol,
    pub structured_output_mode: StructuredOutputMode,
    pub timeout_secs: u64,
    pub enabled: bool,
}

/// In-memory, non-secret profile state loaded from `ai-profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredProfiles {
    master_enabled: bool,
    active_profile_id: Option<AiProfileId>,
    profiles: Vec<AiProfile>,
}

impl StoredProfiles {
    #[must_use]
    pub const fn master_enabled(&self) -> bool {
        self.master_enabled
    }

    #[must_use]
    pub fn active_profile_id(&self) -> Option<&AiProfileId> {
        self.active_profile_id.as_ref()
    }

    #[must_use]
    pub fn profiles(&self) -> &[AiProfile] {
        &self.profiles
    }

    fn default_state() -> Self {
        Self {
            master_enabled: false,
            active_profile_id: None,
            profiles: Vec::new(),
        }
    }
}

/// File-backed non-sensitive AI profile store.
#[derive(Clone)]
pub struct AiProfileStore {
    data_dir: PathBuf,
    path: PathBuf,
    txn_path: PathBuf,
    lock_path: PathBuf,
    file_port: Arc<dyn AiProfileFilePort>,
}

impl fmt::Debug for AiProfileStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AiProfileStore")
            .field("data_dir", &self.data_dir)
            .field("path", &self.path)
            .field("txn_path", &self.txn_path)
            .field("lock_path", &self.lock_path)
            .finish_non_exhaustive()
    }
}

impl AiProfileStore {
    /// Opens a store rooted at `data_dir` using the local portable file port.
    ///
    /// On Windows production callers should use [`Self::open_with_file_port`]
    /// with the platform `ReplaceFileW`/`MoveFileExW` port.  The portable
    /// implementation is intentionally fail-closed on Windows rather than
    /// silently using remove-then-rename.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, String> {
        Self::open_with_file_port(data_dir, Arc::new(PortableProfileFilePort))
    }

    /// Opens a store with an injected lock/atomic-file port.
    pub fn open_with_file_port(
        data_dir: impl AsRef<Path>,
        file_port: Arc<dyn AiProfileFilePort>,
    ) -> Result<Self, String> {
        let data_dir = data_dir.as_ref().to_path_buf();
        fs::create_dir_all(&data_dir)
            .map_err(|_| "unable to create AI profile data directory".to_string())?;
        Ok(Self {
            path: data_dir.join(PROFILE_FILE_NAME),
            txn_path: data_dir.join(PROFILE_TXN_FILE_NAME),
            lock_path: data_dir.join(PROFILE_LOCK_FILE_NAME),
            data_dir,
            file_port,
        })
    }

    /// The metadata file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The non-sensitive transaction marker path.
    #[must_use]
    pub fn txn_path(&self) -> &Path {
        &self.txn_path
    }

    /// The exclusive write-lock path.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Returns the crash-isolation status without exposing a key or metadata.
    pub fn recovery_status(&self) -> Result<RecoveryStatus, String> {
        match self.read_marker()? {
            MarkerRead::Missing => Ok(RecoveryStatus::Clean),
            MarkerRead::Valid(marker) => Ok(RecoveryStatus::RecoveryRequired(marker)),
            MarkerRead::Invalid => Ok(RecoveryStatus::InvalidMarker),
        }
    }

    /// Resolves one profile only when it is safe for remote-AI use.
    ///
    /// A valid active marker isolates only its `profileId`; if that id was the
    /// active profile, `load` also reports no active profile at runtime.  A
    /// malformed marker disables resolution of every profile and is preserved.
    pub fn resolve_usable_profile(&self, id: &AiProfileId) -> Result<Option<AiProfile>, String> {
        let marker = match self.read_marker()? {
            MarkerRead::Missing => None,
            MarkerRead::Valid(marker) => Some(marker),
            MarkerRead::Invalid => return Err(
                "AI profile transaction marker is invalid; remote profile resolution is disabled"
                    .to_string(),
            ),
        };
        if marker
            .as_ref()
            .is_some_and(|marker| marker.profile_id == *id)
        {
            return Ok(None);
        }
        let state = self.load_unlocked()?;
        Ok(state
            .profiles
            .into_iter()
            .find(|profile| profile.id() == id && profile.enabled()))
    }

    /// Loads profile metadata.  Missing metadata is an empty state.  All other
    /// metadata read errors fail closed; malformed metadata and unknown fields
    /// are rejected.  A valid marker affects only the runtime active reference.
    pub fn load(&self) -> Result<StoredProfiles, String> {
        // A marker read failure is itself a recovery failure.  A syntactically
        // invalid marker closes metadata loading too; callers use
        // `recovery_status` and an explicit recovery API to inspect/repair it.
        if matches!(self.read_marker()?, MarkerRead::Invalid) {
            return Err(
                "AI profile transaction marker is invalid; profile loading is disabled".to_string(),
            );
        }
        let mut state = self.load_unlocked()?;
        if let MarkerRead::Valid(marker) = self.read_marker()? {
            if state.active_profile_id.as_ref() == Some(&marker.profile_id) {
                state.active_profile_id = None;
            }
        }
        Ok(state)
    }

    /// Creates a profile with a locally generated UUID and writes its key only
    /// through the supplied environment-key port.
    pub fn upsert(
        &self,
        input: AiProfileInput,
        api_key: &str,
        keys: &dyn EnvKeyStore,
    ) -> Result<AiProfile, String> {
        self.validate_input(&input)?;
        let _lock = self.acquire_clean_write()?;
        let mut state = self.load_unlocked()?;
        let id = new_profile_id()?;
        let profile = profile_from_input(id, input);
        state.profiles.push(profile.clone());
        self.complete_set_transaction(
            &state,
            ProfileTxnOp::Create,
            profile.id().clone(),
            &profile,
            api_key,
            keys,
        )?;
        Ok(profile)
    }

    /// Updates an existing profile.  The generated environment name is
    /// retained because the profile id is retained.
    pub fn update(
        &self,
        id: AiProfileId,
        input: AiProfileInput,
        api_key: &str,
        keys: &dyn EnvKeyStore,
    ) -> Result<AiProfile, String> {
        self.validate_input(&input)?;
        let _lock = self.acquire_clean_write()?;
        let mut state = self.load_unlocked()?;
        let index = state
            .profiles
            .iter()
            .position(|profile| profile.id() == &id)
            .ok_or_else(|| "AI profile does not exist".to_string())?;
        let profile = profile_from_input(id, input);
        state.profiles[index] = profile.clone();
        self.complete_set_transaction(
            &state,
            ProfileTxnOp::Update,
            profile.id().clone(),
            &profile,
            api_key,
            keys,
        )?;
        Ok(profile)
    }

    /// Deletes exactly one profile and exactly its generated key.  No user
    /// rule or unrelated environment variable is touched.
    pub fn delete(&self, id: AiProfileId, keys: &dyn EnvKeyStore) -> Result<(), String> {
        let _lock = self.acquire_clean_write()?;
        let mut state = self.load_unlocked()?;
        let index = state
            .profiles
            .iter()
            .position(|profile| profile.id() == &id)
            .ok_or_else(|| "AI profile does not exist".to_string())?;
        let profile = state.profiles.remove(index);
        if state.active_profile_id.as_ref() == Some(&id) {
            state.active_profile_id = None;
        }

        self.publish_marker(ProfileTxnOp::Delete, id.clone())?;
        self.persist_state(&state)?;
        self.remove_key_and_verify(profile.api_key_env(), keys)?;
        self.clear_marker()
    }

    /// Changes only the non-secret master switch.  Ordinary writes are blocked
    /// while any recovery marker exists.
    pub fn set_master_enabled(&self, enabled: bool) -> Result<StoredProfiles, String> {
        let _lock = self.acquire_clean_write()?;
        let mut state = self.load_unlocked()?;
        state.master_enabled = enabled;
        self.persist_state(&state)?;
        Ok(state)
    }

    /// Changes only the active profile reference.
    pub fn set_active_profile(
        &self,
        profile_id: Option<AiProfileId>,
    ) -> Result<StoredProfiles, String> {
        let _lock = self.acquire_clean_write()?;
        let mut state = self.load_unlocked()?;
        if let Some(id) = profile_id.as_ref() {
            if !state.profiles.iter().any(|profile| profile.id() == id) {
                return Err("AI profile does not exist".to_string());
            }
        }
        state.active_profile_id = profile_id;
        self.persist_state(&state)?;
        Ok(state)
    }

    /// Explicit recovery re-submission for a valid marker.  The marker's id is
    /// retained; callers must provide metadata and a fresh key again.
    pub fn resubmit_recovery(
        &self,
        id: AiProfileId,
        input: AiProfileInput,
        api_key: &str,
        keys: &dyn EnvKeyStore,
    ) -> Result<AiProfile, String> {
        self.validate_input(&input)?;
        let _lock = self.acquire_recovery_write(&id)?;
        let marker = self.require_recovery_marker(&id)?;
        if matches!(marker.op, ProfileTxnOp::Delete) {
            return Err("delete recovery must use delete_recovery or abandon_recovery".to_string());
        }
        let mut state = self.load_unlocked()?;
        let profile = profile_from_input(id.clone(), input);
        match state
            .profiles
            .iter()
            .position(|existing| existing.id() == &id)
        {
            Some(index) => state.profiles[index] = profile.clone(),
            None => {
                if !matches!(marker.op, ProfileTxnOp::Create | ProfileTxnOp::Update) {
                    return Err("AI profile recovery target does not exist".to_string());
                }
                state.profiles.push(profile.clone());
            }
        }
        self.complete_existing_set_transaction(&state, marker, &profile, api_key, keys)?;
        Ok(profile)
    }

    /// Explicit recovery deletion.  It is idempotent for a missing metadata
    /// profile and always targets only the marker's derived environment name.
    pub fn delete_recovery(&self, id: AiProfileId, keys: &dyn EnvKeyStore) -> Result<(), String> {
        let _lock = self.acquire_recovery_write(&id)?;
        let marker = self.require_recovery_marker(&id)?;
        if !matches!(marker.op, ProfileTxnOp::Delete) {
            return Err("delete recovery requires a delete transaction marker".to_string());
        }
        let mut state = self.load_unlocked()?;
        state.profiles.retain(|profile| profile.id() != &id);
        if state.active_profile_id.as_ref() == Some(&id) {
            state.active_profile_id = None;
        }
        self.persist_state(&state)?;
        let id_for_key = if let Some(profile) = state.profiles.iter().find(|p| p.id() == &id) {
            profile.id().clone()
        } else {
            marker.profile_id.clone()
        };
        let env = devresidue_core::ai::derived_api_key_env(&id_for_key);
        self.remove_key_and_verify(&env, keys)?;
        self.clear_marker()
    }

    /// Explicitly abandons a valid recovery marker: disable the remaining
    /// profile, clear an active reference, remove the derived key, verify the
    /// process environment, then remove the marker.
    pub fn abandon_recovery(&self, id: AiProfileId, keys: &dyn EnvKeyStore) -> Result<(), String> {
        let _lock = self.acquire_recovery_write(&id)?;
        let _marker = self.require_recovery_marker(&id)?;
        let mut state = self.load_unlocked()?;
        if let Some(profile) = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id() == &id)
        {
            // `AiProfile` is immutable by design.  Rebuild the non-secret
            // value with the same settings and disabled state.
            *profile = AiProfile::with_api_protocol(
                profile.id().clone(),
                profile.name().to_string(),
                profile.base_url().to_string(),
                profile.model().to_string(),
                profile.api_protocol(),
                profile.structured_output_mode(),
                profile.timeout_secs(),
                false,
            );
        }
        if state.active_profile_id.as_ref() == Some(&id) {
            state.active_profile_id = None;
        }
        self.persist_state(&state)?;
        let env = devresidue_core::ai::derived_api_key_env(&id);
        self.remove_key_and_verify(&env, keys)?;
        self.clear_marker()
    }

    fn load_unlocked(&self) -> Result<StoredProfiles, String> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(StoredProfiles::default_state())
            }
            Err(_) => return Err("unable to read AI profile file".to_string()),
        };
        let raw: PersistedProfiles = serde_json::from_slice(&bytes)
            .map_err(|_| "AI profile file is corrupt or contains unsupported fields".to_string())?;
        self.validate_raw(&raw)?;
        let profiles = raw
            .profiles
            .into_iter()
            .map(PersistedProfile::into_profile)
            .collect::<Result<Vec<_>, _>>()?;
        let active_profile_id = raw.active_profile_id;
        if let Some(active) = active_profile_id.as_ref() {
            if !profiles.iter().any(|profile| profile.id() == active) {
                return Err(
                    "AI profile activeProfileId does not identify a stored profile".to_string(),
                );
            }
        }
        Ok(StoredProfiles {
            master_enabled: raw.master_enabled,
            active_profile_id,
            profiles,
        })
    }

    fn acquire_clean_write(&self) -> Result<Box<dyn ProfileFileLock>, String> {
        let guard = self
            .file_port
            .acquire_exclusive(&self.lock_path)
            .map_err(|_| "unable to acquire AI profile write lock".to_string())?;
        match self.read_marker()? {
            MarkerRead::Missing => Ok(guard),
            MarkerRead::Valid(_) | MarkerRead::Invalid => Err(
                "AI profile recovery is required; ordinary profile writes are blocked".to_string(),
            ),
        }
    }

    fn acquire_recovery_write(&self, id: &AiProfileId) -> Result<Box<dyn ProfileFileLock>, String> {
        let guard = self
            .file_port
            .acquire_exclusive(&self.lock_path)
            .map_err(|_| "unable to acquire AI profile write lock".to_string())?;
        match self.read_marker()? {
            MarkerRead::Valid(marker) if marker.profile_id == *id => Ok(guard),
            MarkerRead::Valid(_) => {
                Err("AI profile recovery target does not match marker".to_string())
            }
            MarkerRead::Missing => Err("AI profile recovery marker is missing".to_string()),
            MarkerRead::Invalid => Err("AI profile recovery marker is invalid".to_string()),
        }
    }

    fn require_recovery_marker(&self, id: &AiProfileId) -> Result<PendingProfileTxn, String> {
        match self.read_marker()? {
            MarkerRead::Valid(marker) if marker.profile_id == *id => Ok(marker),
            MarkerRead::Valid(_) => {
                Err("AI profile recovery target does not match marker".to_string())
            }
            MarkerRead::Missing => Err("AI profile recovery marker is missing".to_string()),
            MarkerRead::Invalid => Err("AI profile recovery marker is invalid".to_string()),
        }
    }

    fn complete_set_transaction(
        &self,
        state: &StoredProfiles,
        op: ProfileTxnOp,
        id: AiProfileId,
        profile: &AiProfile,
        api_key: &str,
        keys: &dyn EnvKeyStore,
    ) -> Result<(), String> {
        self.publish_marker(op, id)?;
        self.complete_set_after_marker(state, profile, api_key, keys)
    }

    fn complete_existing_set_transaction(
        &self,
        state: &StoredProfiles,
        marker: PendingProfileTxn,
        profile: &AiProfile,
        api_key: &str,
        keys: &dyn EnvKeyStore,
    ) -> Result<(), String> {
        // The marker is already the durable recovery classification.  A
        // resubmission never replaces it with a key-bearing or metadata-bearing
        // payload; it only retries the prescribed metadata/key sequence.
        self.complete_set_after_marker(state, profile, api_key, keys)
            .map_err(|error| format!("profile recovery {:?} failed: {error}", marker.op))
    }

    fn complete_set_after_marker(
        &self,
        state: &StoredProfiles,
        profile: &AiProfile,
        api_key: &str,
        keys: &dyn EnvKeyStore,
    ) -> Result<(), String> {
        self.persist_state(state)?;
        keys.set(profile.api_key_env(), api_key)
            .map_err(|_| "unable to store the AI profile key".to_string())?;
        let process_value = keys
            .get_process(profile.api_key_env())
            .map_err(|_| "unable to verify the current process environment".to_string())?;
        if process_value.as_deref() != Some(api_key) {
            return Err(
                "current process environment did not receive the AI profile key".to_string(),
            );
        }
        self.clear_marker()
    }

    fn remove_key_and_verify(
        &self,
        env: &devresidue_core::ai::AiKeyEnvName,
        keys: &dyn EnvKeyStore,
    ) -> Result<(), String> {
        keys.remove(env)
            .map_err(|_| "unable to remove the selected AI profile key".to_string())?;
        let process_value = keys.get_process(env).map_err(|_| {
            "unable to verify removal from the current process environment".to_string()
        })?;
        if process_value.is_some() {
            return Err(
                "the selected AI profile key remains in the current process environment"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn publish_marker(&self, op: ProfileTxnOp, profile_id: AiProfileId) -> Result<(), String> {
        let marker = PendingProfileTxn {
            version: PROFILE_TXN_VERSION,
            state: ProfileTxnState::Active,
            op,
            profile_id,
        };
        let bytes = serde_json::to_vec(&marker)
            .map_err(|_| "unable to serialize AI profile transaction marker".to_string())?;
        self.atomic_write(&self.txn_path, &bytes, "transaction marker")
    }

    fn clear_marker(&self) -> Result<(), String> {
        match fs::remove_file(&self.txn_path) {
            Ok(()) => sync_directory(&self.data_dir),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("unable to remove AI profile transaction marker".to_string()),
        }
    }

    fn persist_state(&self, state: &StoredProfiles) -> Result<(), String> {
        let persisted = PersistedProfiles::from_state(state);
        let bytes = serde_json::to_vec_pretty(&persisted)
            .map_err(|_| "unable to serialize AI profile metadata".to_string())?;
        self.atomic_write(&self.path, &bytes, "profile metadata")
    }

    fn atomic_write(&self, target: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
        let temp = self.data_dir.join(format!(
            ".ai-profiles.{}.{}.tmp",
            label.replace(' ', "-"),
            Uuid::new_v4()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| format!("unable to create temporary AI profile {label}"))?;
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| format!("unable to flush temporary AI profile {label}"))?;
            self.file_port.atomic_replace(&temp, target)
        })();
        if let Err(error) = result {
            match fs::remove_file(&temp) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(format!(
                        "{error}; unable to remove temporary AI profile {label}"
                    ))
                }
            }
            return Err(error);
        }
        sync_directory(&self.data_dir)
    }

    fn read_marker(&self) -> Result<MarkerRead, String> {
        let bytes = match fs::read(&self.txn_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(MarkerRead::Missing)
            }
            Err(_) => return Err("unable to read AI profile transaction marker".to_string()),
        };
        let marker = match serde_json::from_slice::<PendingProfileTxn>(&bytes) {
            Ok(marker)
                if marker.version == PROFILE_TXN_VERSION
                    && marker.state == ProfileTxnState::Active =>
            {
                marker
            }
            Ok(_) | Err(_) => return Ok(MarkerRead::Invalid),
        };
        Ok(MarkerRead::Valid(marker))
    }

    fn validate_input(&self, input: &AiProfileInput) -> Result<(), String> {
        if input.name.trim().is_empty() || input.model.trim().is_empty() {
            return Err("AI profile name and model must not be empty".to_string());
        }
        if input.timeout_secs == 0 {
            return Err("AI profile timeout must be positive".to_string());
        }
        validate_base_url(&input.base_url)
    }

    fn validate_raw(&self, raw: &PersistedProfiles) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        for profile in &raw.profiles {
            if !ids.insert(profile.id.as_str()) {
                return Err("AI profile file contains duplicate profile IDs".to_string());
            }
            profile.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkerRead {
    Missing,
    Valid(PendingProfileTxn),
    Invalid,
}

fn profile_from_input(id: AiProfileId, input: AiProfileInput) -> AiProfile {
    AiProfile::with_api_protocol(
        id,
        input.name,
        input.base_url,
        input.model,
        input.api_protocol,
        input.structured_output_mode,
        input.timeout_secs,
        input.enabled,
    )
}

fn new_profile_id() -> Result<AiProfileId, String> {
    AiProfileId::parse(&Uuid::new_v4().to_string())
        .map_err(|_| "unable to generate an AI profile ID".to_string())
}

fn validate_base_url(base_url: &str) -> Result<(), String> {
    if base_url.is_empty()
        || base_url.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_control() || byte == b'\\' || byte == b'@'
        })
    {
        return Err("AI profile base URL must use HTTPS or explicit loopback HTTP".to_string());
    }
    let (scheme, remainder) = base_url.split_once("://").ok_or_else(|| {
        "AI profile base URL must use HTTPS or explicit loopback HTTP".to_string()
    })?;
    let scheme = scheme.to_ascii_lowercase();
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() {
        return Err("AI profile base URL must use HTTPS or explicit loopback HTTP".to_string());
    }
    let is_https = scheme == "https";
    let is_local_http = scheme == "http" && is_loopback_http_authority(authority);
    if is_https || is_local_http {
        Ok(())
    } else {
        Err("AI profile base URL must use HTTPS or explicit loopback HTTP".to_string())
    }
}

fn is_loopback_http_authority(authority: &str) -> bool {
    let (host, port) = authority.split_once(':').unwrap_or((authority, ""));
    let has_valid_port = !authority.contains(':')
        || (!port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()));
    let is_localhost = host.eq_ignore_ascii_case("localhost");
    let is_loopback_ipv4 = host
        .parse::<std::net::Ipv4Addr>()
        .is_ok_and(|address| address.is_loopback());

    has_valid_port && (is_localhost || is_loopback_ipv4)
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|_| "unable to persist AI profile directory update".to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Portable profile file port.  Unix uses atomic rename after durable sibling
/// writes.  Windows deliberately refuses the operation so callers cannot
/// accidentally fall back to remove-then-rename; the Windows platform crate
/// supplies the `ReplaceFileW`/`MoveFileExW` implementation.
#[derive(Debug, Clone, Copy, Default)]
struct PortableProfileFilePort;

struct PortableProfileLock {
    path: PathBuf,
    _file: File,
}

impl Drop for PortableProfileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl ProfileFileLock for PortableProfileLock {}

impl AiProfileFilePort for PortableProfileFilePort {
    fn acquire_exclusive(&self, lock_path: &Path) -> Result<Box<dyn ProfileFileLock>, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
            .map_err(|_| "AI profile write lock is already held".to_string())?;
        Ok(Box::new(PortableProfileLock {
            path: lock_path.to_path_buf(),
            _file: file,
        }))
    }

    fn atomic_replace(&self, temporary: &Path, target: &Path) -> Result<(), String> {
        #[cfg(not(windows))]
        {
            fs::rename(temporary, target)
                .map_err(|_| "unable to atomically publish AI profile file".to_string())
        }
        #[cfg(windows)]
        {
            let _ = (temporary, target);
            Err("Windows AI profile atomic file port was not injected".to_string())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedProfiles {
    master_enabled: bool,
    active_profile_id: Option<AiProfileId>,
    profiles: Vec<PersistedProfile>,
}

impl PersistedProfiles {
    fn from_state(state: &StoredProfiles) -> Self {
        Self {
            master_enabled: state.master_enabled,
            active_profile_id: state.active_profile_id.clone(),
            profiles: state
                .profiles
                .iter()
                .map(PersistedProfile::from_profile)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedProfile {
    id: AiProfileId,
    name: String,
    base_url: String,
    model: String,
    api_key_env: String,
    #[serde(default)]
    api_protocol: PersistedAiApiProtocol,
    structured_output_mode: PersistedStructuredOutputMode,
    timeout_secs: u64,
    enabled: bool,
}

impl PersistedProfile {
    fn from_profile(profile: &AiProfile) -> Self {
        Self {
            id: profile.id().clone(),
            name: profile.name().to_string(),
            base_url: profile.base_url().to_string(),
            model: profile.model().to_string(),
            api_key_env: profile.api_key_env().as_str().to_string(),
            api_protocol: PersistedAiApiProtocol::from_core(profile.api_protocol()),
            structured_output_mode: PersistedStructuredOutputMode::from_core(
                profile.structured_output_mode(),
            ),
            timeout_secs: profile.timeout_secs(),
            enabled: profile.enabled(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.model.trim().is_empty() {
            return Err("AI profile file contains an empty name or model".to_string());
        }
        if self.timeout_secs == 0 {
            return Err("AI profile file contains a nonpositive timeout".to_string());
        }
        validate_base_url(&self.base_url)?;
        let expected = format!(
            "DEVRESIDUE_AI_KEY_{}",
            self.id.as_str().to_ascii_uppercase()
        );
        if self.api_key_env != expected {
            return Err("AI profile file contains an invalid generated key name".to_string());
        }
        Ok(())
    }

    fn into_profile(self) -> Result<AiProfile, String> {
        self.validate()?;
        Ok(AiProfile::with_api_protocol(
            self.id,
            self.name,
            self.base_url,
            self.model,
            self.api_protocol.to_core(),
            self.structured_output_mode.to_core(),
            self.timeout_secs,
            self.enabled,
        ))
    }
}

/// Stable profile-file protocol values. Absent values are legacy profiles and
/// therefore preserve the original Chat Completions compatibility behavior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum PersistedAiApiProtocol {
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
}

impl Default for PersistedAiApiProtocol {
    fn default() -> Self {
        Self::OpenAiCompatible
    }
}

impl PersistedAiApiProtocol {
    fn from_core(protocol: AiApiProtocol) -> Self {
        match protocol {
            AiApiProtocol::OpenAiResponses => Self::OpenAiResponses,
            AiApiProtocol::OpenAiCompatible => Self::OpenAiCompatible,
        }
    }

    fn to_core(self) -> AiApiProtocol {
        match self {
            Self::OpenAiResponses => AiApiProtocol::OpenAiResponses,
            Self::OpenAiCompatible => AiApiProtocol::OpenAiCompatible,
        }
    }
}

/// Stable on-disk names. `strict` remains compatible with existing profile
/// files; `auto` preserves the user's fallback preference without rewriting
/// prior configurations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PersistedStructuredOutputMode {
    Auto,
    #[serde(rename = "strict")]
    Strict,
    JsonObject,
}

impl PersistedStructuredOutputMode {
    fn from_core(mode: StructuredOutputMode) -> Self {
        match mode {
            StructuredOutputMode::Auto => Self::Auto,
            StructuredOutputMode::JsonObject => Self::JsonObject,
            StructuredOutputMode::JsonSchema => Self::Strict,
        }
    }

    fn to_core(self) -> StructuredOutputMode {
        match self {
            Self::Auto => StructuredOutputMode::Auto,
            Self::Strict => StructuredOutputMode::JsonSchema,
            Self::JsonObject => StructuredOutputMode::JsonObject,
        }
    }
}
