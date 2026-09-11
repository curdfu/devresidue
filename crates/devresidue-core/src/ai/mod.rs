//! Pure AI domain types and ports (Task 1 / AiAdvisor Phase A).
//!
//! # Boundary
//!
//! This module holds **no network, platform, HTTP, TLS or UI dependency**:
//! only pure data types, string newtypes, fixed enums and trait ports that
//! richer layers (`devresidue-ai`, platform adapters, CLI, Tauri) implement.
//! The crate does not depend on the `uuid` crate; [`AiProfileId`] validates
//! canonical UUID *text* by hand (36 chars, 8-4-4-4-12 ASCII hex, lower-case
//! canonical display) and never stores a secret. Random UUID generation lives
//! in the `devresidue-ai` crate.
//!
//! # No-secret contract
//!
//! None of these types may carry a path, raw evidence text, raw request/
//! response text or an API key value. [`RuleProvenance`] is the persistence
//! face of that contract: it is an optional [`RuleDoc`](crate::rules::RuleDoc)
//! field that only accepts the non-sensitive fields `origin` (only
//! `ai-advisor`), `profile_id`, `scan_generation`, `suggestion_id`,
//! `user_final_risk` and `created_at_epoch_secs`. Any unknown YAML field
//! (path / reason / model / key / request / response ...) is rejected at the
//! serde parse layer via `deny_unknown_fields`; residual semantic problems are
//! rejected by the rule validator (`crate::rules::validator`).

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::RiskLevel;

// ---- canonical-UUID string newtypes ------------------------------------------

/// True when `text` is exactly `hhhhhhhh-hhhh-hhhh-hhhh-hhhhhhhhhhhh`
/// (36 chars, hyphens at 8/13/18/23, ASCII hex everywhere else). A hex digit on
/// a separator position (or a `-` anywhere else) is invalid.
fn is_canonical_uuid_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, &byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Defines one validated canonical-UUID string newtype plus its parse error.
/// Core only *parses/validates* UUID text — random UUID generation lives in the
/// `devresidue-ai` crate.
macro_rules! define_canonical_uuid_id {
    ($(#[$doc:meta])* $name:ident, $err:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        /// Error returned when [`$name::parse`] sees text that is not a
        /// 36-character canonical UUID (8-4-4-4-12 ASCII hex). The offending
        /// text is intentionally not echoed (a caller could pass a secret by
        /// mistake).
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $err;

        impl fmt::Display for $err {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(
                    "invalid ",
                    $label,
                    " id: expected canonical UUID text (36 chars, 8-4-4-4-12 ASCII hex)"
                ))
            }
        }

        impl std::error::Error for $err {}

        impl $name {
            /// Parses and validates a canonical UUID text representation.
            ///
            /// Accepts ASCII hex in either case and stores the lower-case
            /// canonical form for display and comparison.
            pub fn parse(text: &str) -> Result<Self, $err> {
                if !is_canonical_uuid_text(text) {
                    return Err($err);
                }
                Ok(Self(text.to_ascii_lowercase()))
            }

            /// Returns the canonical (lower-case) UUID text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::parse(&text).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_canonical_uuid_id! {
    /// Identifies one AI profile. Validated canonical UUID text; Core never
    /// mints random UUIDs (that happens in `devresidue-ai`).
    AiProfileId, AiProfileIdParseError, "AI profile"
}

define_canonical_uuid_id! {
    /// Correlates one AI suggestion (the model's answer for one sanitised
    /// entry). Validated canonical UUID text — never free text, a path or a
    /// secret-looking value — so a remote cannot smuggle arbitrary content into
    /// a persisted rule's provenance.
    AiSuggestionId, AiSuggestionIdParseError, "AI suggestion"
}

/// Prefix of the app-generated, exclusive user-level environment-variable name
/// that holds one profile's API key (DR-1). Never user-supplied.
pub const AI_KEY_ENV_PREFIX: &str = "DEVRESIDUE_AI_KEY_";

/// Strong type for the app-generated, exclusive user-level environment-variable
/// name that holds one profile's API key (DR-1). The inner value is **private**
/// and there is no `From<String>`, no public raw constructor and no public
/// `Deserialize`: the only way to obtain a value is the deterministic
/// derivation [`derived_api_key_env`], which always yields exactly
/// `DEVRESIDUE_AI_KEY_<UPPERCASE_CANONICAL_UUID>` for the profile id. An
/// arbitrary path / secret value / another profile's name therefore can never
/// be injected through a public struct literal or a parse path.
///
/// ```compile_fail
/// use devresidue_core::ai::{AiProfile, AiProfileId, StructuredOutputMode};
/// // An arbitrary String can never be injected through the public struct
/// // literal: `api_key_env` is the private `AiKeyEnvName` type, whose only
/// // public origin is `derived_api_key_env(&AiProfileId)`.
/// let _profile = AiProfile {
///     id: AiProfileId::parse("018f7e21-9d15-7b17-a5fd-4f0f2bcadc72").unwrap(),
///     name: String::new(),
///     base_url: String::new(),
///     model: String::new(),
///     api_key_env: "DEVRESIDUE_AI_KEY_ARBITRARY".to_string(),
///     structured_output_mode: StructuredOutputMode::Auto,
///     timeout_secs: 120,
///     enabled: true,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AiKeyEnvName(String);

impl AiKeyEnvName {
    /// Returns the derived environment-variable name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for AiKeyEnvName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for AiKeyEnvName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Returns the exact derived environment-variable name for `id`:
/// `DEVRESIDUE_AI_KEY_<UPPERCASE_CANONICAL_UUID>`.
///
/// This is the **only** public way to obtain an [`AiKeyEnvName`].
#[must_use]
pub fn derived_api_key_env(id: &AiProfileId) -> AiKeyEnvName {
    AiKeyEnvName(format!(
        "{AI_KEY_ENV_PREFIX}{}",
        id.as_str().to_ascii_uppercase()
    ))
}

/// The only operation recorded by the non-sensitive AI profile transaction
/// marker.  The marker never contains metadata, a key, a key hash, or a key
/// environment value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileTxnOp {
    /// A profile is being created.
    Create,
    /// A profile is being updated.
    Update,
    /// A profile is being deleted.
    Delete,
}

/// Persistent profile marker state.  This release has one active state; the
/// explicit enum prevents callers from inventing a second wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileTxnState {
    /// A metadata/environment operation is incomplete and requires isolation.
    Active,
}

/// Strict, non-sensitive profile transaction marker payload.
///
/// This type is deliberately pure so the storage crate can expose recovery
/// state without exposing a key or any old/new profile contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingProfileTxn {
    /// Marker schema version.
    pub version: u32,
    /// Marker lifecycle state; currently always [`ProfileTxnState::Active`].
    pub state: ProfileTxnState,
    /// The non-sensitive operation being isolated.
    pub op: ProfileTxnOp,
    /// The only profile affected by this transaction.
    pub profile_id: AiProfileId,
}

/// Runtime gate for AI profile resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryStatus {
    /// No profile transaction marker exists.
    Clean,
    /// Only the referenced profile is isolated from remote-AI use.
    RecoveryRequired(PendingProfileTxn),
    /// The marker exists but is malformed or contains an unsupported value;
    /// all remote AI profile resolution is disabled until explicit repair.
    InvalidMarker,
}

/// Guard held while profile metadata and its transaction marker are mutated.
/// Implementations keep the lock alive for the guard's lifetime.
pub trait ProfileFileLock: Send {}

/// Platform/file-system port for profile lock acquisition and atomic publish.
///
/// The AI crate owns serialization and durable temporary-file writes.  The
/// platform port owns the final replacement primitive, allowing Windows to use
/// `ReplaceFileW`/`MoveFileExW` without making Core or the AI crate depend on a
/// platform API.
pub trait ProfileFilePort: Send + Sync {
    /// Acquires the exclusive `<data_dir>/ai-profiles.lock` guard.
    fn acquire_exclusive(&self, lock_path: &Path) -> Result<Box<dyn ProfileFileLock>, String>;
    /// Publishes a fully flushed sibling temporary file over `target`.
    fn atomic_replace(&self, temporary: &Path, target: &Path) -> Result<(), String>;
}

/// Backward-compatible descriptive alias for callers that name this an AI
/// profile atomic-file port.
pub use ProfileFilePort as AiProfileFilePort;

/// Expands one strict `String`-backed token newtype (no path / secret content).
macro_rules! define_ai_token {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[derive(Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps the token text.
            #[must_use]
            pub const fn new(value: String) -> Self {
                Self(value)
            }

            /// Returns the token text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

define_ai_token! {
    /// Opaque per-request local alias for one scan item. Minted randomly per
    /// prepared batch in `devresidue-ai`; **never** reuses `ScanItemId` so the
    /// remote cannot correlate requests or infer id numbering.
    AiEntryToken
}

define_ai_token! {
    /// Correlates one prepared/analysed batch within a process (in-memory).
    AiBatchId
}

// ---- fixed enums ------------------------------------------------------------

/// Provenance origin of a rule written from an AiAdvisor review. Only
/// `ai-advisor` exists in this release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleOrigin {
    /// Rule produced by the optional remote AiAdvisor (local evidence, local
    /// draft builder, local validator — the model never authors rules).
    AiAdvisor,
}

/// Fixed, non-sensitive zone label for a sanitised entry (never the raw path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiZone {
    /// Under the user profile (HOME) zone.
    Home,
    /// Under `%APPDATA%` (Roaming).
    AppData,
    /// Under `%LOCALAPPDATA%` (Local).
    LocalAppData,
    /// A project/workspace directory.
    Workspace,
    /// Anything that maps to no registered zone.
    Other,
}

/// Coarse size bucket sent to the remote (never exact sizes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SizeBucket {
    /// `< 1 MiB`
    Under1MiB,
    /// `< 64 MiB`
    Under64MiB,
    /// `< 1 GiB`
    Under1GiB,
    /// `>= 1 GiB`
    AtLeast1GiB,
}

/// Coarse age bucket sent to the remote (never an exact timestamp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgeBucket {
    /// `< 1 day`
    Under1Day,
    /// `< 7 days`
    Under7Days,
    /// `< 30 days`
    Under30Days,
    /// `< 180 days`
    Under180Days,
    /// `>= 180 days`
    AtLeast180Days,
    /// Age could not be established.
    Unknown,
}

/// How the OpenAI-compatible endpoint is asked to produce structured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StructuredOutputMode {
    /// Let the client pick (strict `json_schema`, fall back to `json_object`).
    Auto,
    /// `response_format.json_schema` strict mode first.
    JsonSchema,
    /// `response_format.json_object` + strict local validation.
    JsonObject,
}

// ---- pure data DTOs ---------------------------------------------------------

/// Non-sensitive AI profile configuration (the API key value never lives in a
/// profile: only the generated environment-variable *name* is stored).
///
/// Every field is **private**. The only constructor is [`AiProfile::new`],
/// which never accepts an `api_key_env`: it always derives the exclusive
/// variable name from the profile's own id via [`derived_api_key_env`]. Both
/// [`Serialize`] and [`Deserialize`] likewise derive the name from the stored
/// id, so a profile whose environment-variable name belongs to a *different*
/// id is unrepresentable through any public path.
///
/// ```compile_fail
/// use devresidue_core::ai::{derived_api_key_env, AiProfile, AiProfileId, StructuredOutputMode};
/// let id_a = AiProfileId::parse("018f7e21-9d15-7b17-a5fd-4f0f2bcadc72").unwrap();
/// let id_b = AiProfileId::parse("1f2e3d4c-5b6a-4e8f-9a7b-0c1d2e3f4a5b").unwrap();
/// // External code cannot assemble a profile whose api_key_env was derived from
/// // a different id: the fields are private and construction only happens
/// // through AiProfile::new, which always derives from the profile's own id.
/// let _mismatched = AiProfile {
///     id: id_a,
///     name: String::from("n"),
///     base_url: String::from("https://example.invalid/v1"),
///     model: String::from("m"),
///     api_key_env: derived_api_key_env(&id_b),
///     structured_output_mode: StructuredOutputMode::Auto,
///     timeout_secs: 120,
///     enabled: true,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiProfile {
    id: AiProfileId,
    name: String,
    base_url: String,
    model: String,
    api_key_env: AiKeyEnvName,
    structured_output_mode: StructuredOutputMode,
    timeout_secs: u64,
    enabled: bool,
}

impl AiProfile {
    /// Constructs a profile from its non-secret settings. `api_key_env` is not
    /// an argument: the exclusive environment-variable name is always derived
    /// from `id`, so an id/name mismatch is unrepresentable.
    #[must_use]
    pub fn new(
        id: AiProfileId,
        name: String,
        base_url: String,
        model: String,
        structured_output_mode: StructuredOutputMode,
        timeout_secs: u64,
        enabled: bool,
    ) -> Self {
        let api_key_env = derived_api_key_env(&id);
        Self {
            id,
            name,
            base_url,
            model,
            api_key_env,
            structured_output_mode,
            timeout_secs,
            enabled,
        }
    }

    /// Profile id (canonical UUID).
    #[must_use]
    pub fn id(&self) -> &AiProfileId {
        &self.id
    }

    /// User-facing profile name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Base URL of the OpenAI-compatible endpoint.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Model name.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// App-generated exclusive environment-variable name — always the exact
    /// derivation for this profile's own id (never a key value).
    #[must_use]
    pub fn api_key_env(&self) -> &AiKeyEnvName {
        &self.api_key_env
    }

    /// Preferred structured-output mode.
    #[must_use]
    pub const fn structured_output_mode(&self) -> StructuredOutputMode {
        self.structured_output_mode
    }

    /// Whole-batch timeout in seconds.
    #[must_use]
    pub const fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    /// Whether this profile may be used.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
}

impl Serialize for AiProfile {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialise the name derived from this profile's own id, never a
        // possibly-mutated field value: output always matches the id.
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("AiProfile", 8)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("base_url", &self.base_url)?;
        state.serialize_field("model", &self.model)?;
        state.serialize_field("api_key_env", &derived_api_key_env(&self.id))?;
        state.serialize_field("structured_output_mode", &self.structured_output_mode)?;
        state.serialize_field("timeout_secs", &self.timeout_secs)?;
        state.serialize_field("enabled", &self.enabled)?;
        state.end()
    }
}

/// Raw, field-for-field parse view of [`AiProfile`] keeping `deny_unknown_fields`.
/// `api_key_env` is read as text here only so it can be cross-checked against
/// the exact derivation; it is never used to build the profile's field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AiProfileRaw {
    id: AiProfileId,
    name: String,
    base_url: String,
    model: String,
    api_key_env: String,
    structured_output_mode: StructuredOutputMode,
    timeout_secs: u64,
    enabled: bool,
}

impl<'de> Deserialize<'de> for AiProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = AiProfileRaw::deserialize(deserializer)?;
        // Cross-check the supplied name against the exact derivation for this
        // profile id, then build through `new` (which re-derives it), never
        // from the raw text.
        let expected = derived_api_key_env(&raw.id);
        if raw.api_key_env != expected.as_str() {
            return Err(serde::de::Error::custom(format!(
                "api_key_env must equal the exact derived name `{}` for this profile id",
                expected.as_str()
            )));
        }
        Ok(AiProfile::new(
            raw.id,
            raw.name,
            raw.base_url,
            raw.model,
            raw.structured_output_mode,
            raw.timeout_secs,
            raw.enabled,
        ))
    }
}

/// One fully sanitised entry prepared for remote review. Contains **only**
/// these whitelisted fields — never a `PathBuf`, raw evidence text, raw
/// request text or a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizedEntry {
    /// Request-local random alias.
    pub id: AiEntryToken,
    /// Coarse zone label.
    pub zone: AiZone,
    /// Depth below the zone boundary (capped).
    pub relative_depth: u8,
    /// Sanitised display name (sensitive words replaced by placeholders).
    pub display_name: String,
    /// Fixed kebab label of the existing source kind.
    pub source_kind: String,
    /// Fixed kebab label of the existing residue category.
    pub category_hint: String,
    /// Existing product label, filtered and truncated (or `None`).
    pub product_hint: Option<String>,
    /// Coarse size bucket.
    pub size_bucket: SizeBucket,
    /// Coarse age bucket.
    pub age_bucket: AgeBucket,
    /// Allow-listed structural signal labels only.
    pub signals: Vec<String>,
}

/// A fully prepared batch of sanitised entries, ready for user consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedBatch {
    /// In-memory batch correlation id.
    pub id: AiBatchId,
    /// Profile the batch is destined for.
    pub profile_id: AiProfileId,
    /// Scan generation this batch was prepared from.
    pub scan_generation: u64,
    /// Sanitised entries (≤ hard batch cap).
    pub entries: Vec<SanitizedEntry>,
}

/// Optional, non-sensitive provenance attached to a user rule written from an
/// AiAdvisor review (see [`RuleProvenance`] module docs for the whitelist).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleProvenance {
    /// Origin — only `ai-advisor` in this release.
    pub origin: RuleOrigin,
    /// Non-sensitive profile id the suggestion came from.
    pub profile_id: AiProfileId,
    /// Scan generation the suggestion was based on.
    pub scan_generation: u64,
    /// Canonical UUID correlation id of the accepted suggestion. Typed, never
    /// free text, so a path / secret / raw response string cannot be persisted.
    pub suggestion_id: AiSuggestionId,
    /// The risk the user finally chose (recorded verbatim).
    pub user_final_risk: RiskLevel,
    /// Unix epoch seconds when the rule was created.
    pub created_at_epoch_secs: i64,
}

/// One in-memory suggestion returned by the remote and validated locally.
/// Never persisted; tied to the current scan generation + profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiSuggestion {
    /// Request-local entry alias echoed by the model.
    pub token: AiEntryToken,
    /// Suggested risk (closed RiskLevel set).
    pub suggested_risk: RiskLevel,
    /// Model confidence in `[0, 1]`.
    pub confidence: f32,
    /// One-line sanitised reason (display only — never a rule field source).
    pub reason: String,
    /// Optional product guess (display only; never a rule field source).
    pub product_guess: Option<String>,
    /// Profile that produced the suggestion.
    pub profile_id: AiProfileId,
    /// Scan generation the suggestion belongs to.
    pub scan_generation: u64,
}

// ---- ports (implemented outside Core) ----------------------------------------

/// Port for managing the app-generated user-level environment variable that
/// holds one profile's API key. Implemented by platform crates (Windows HKCU
/// registry, future macOS `env.sh`); Core only defines the contract.
pub trait EnvKeyStore: Send + Sync {
    /// Sets `name` to `value` in the user-level environment.
    fn set(&self, name: &AiKeyEnvName, value: &str) -> Result<(), String>;
    /// Reads `name` from the user-level environment.
    fn get(&self, name: &AiKeyEnvName) -> Result<Option<String>, String>;
    /// Removes `name` from the user-level environment.
    fn remove(&self, name: &AiKeyEnvName) -> Result<(), String>;
    /// Reads the value visible to the current process after a write/delete.
    ///
    /// Platform adapters should override this with a process-environment read;
    /// the default keeps simple adapters source-compatible by using `get`.
    fn get_process(&self, name: &AiKeyEnvName) -> Result<Option<String>, String> {
        self.get(name)
    }
}

/// Marker for an exclusive lock held over the user rules file for the duration
/// of one AI rule batch transaction. Implemented by the platform port.
pub trait UserRuleTransactionGuard: Send {}

/// One prepared user-rule transaction: the file handles the port stages and
/// the Core transaction drives through [`UserRuleTransactionPort::replace`],
/// [`UserRuleTransactionPort::rollback`] and
/// [`UserRuleTransactionPort::commit_verified`].
///
/// All paths live in the same directory as the live file:
///
/// ```text
/// live       user-dispositions.yaml      the authoritative rule file
/// marker     user-dispositions.yaml.txn  exactly ACTIVE/PRESENT,
///                                        ACTIVE/ABSENT,
///                                        ROLLED_BACK/PRESENT,
///                                        ROLLED_BACK/ABSENT or COMMITTED
///                                        (the single persistent marker)
/// backup     user-dispositions.yaml.txn.original   pre-replace live bytes
/// candidate  user-dispositions.yaml.txn.candidate  staged replacement bytes
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRuleTransaction {
    /// The live user rules file.
    pub live_path: PathBuf,
    /// The persistent transaction marker (`.txn`).
    pub marker_path: PathBuf,
    /// The durable pre-replace backup (`.txn.original`).
    pub backup_path: PathBuf,
    /// The staged candidate (`.txn.candidate`).
    pub candidate_path: PathBuf,
}

/// Outcome of [`UserRuleTransactionPort::recover_if_needed`] — what a crashed
/// transaction left behind, resolved strictly by the persistent marker state
/// (never guessed from live parseability).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionRecovery {
    /// No interrupted transaction: the live file is authoritative (any stray
    /// candidate/backup was already removed).
    Clean,
    /// An interrupted transaction was restored to its pre-transaction state,
    /// but its recovery material is still pending cleanup. The marker remains
    /// `ROLLED_BACK/PRESENT` or `ROLLED_BACK/ABSENT`; the Core caller must
    /// validate the live state and then call
    /// [`UserRuleTransactionPort::cleanup_rolled_back`].
    RolledBackPendingCleanup(PreparedRuleTransaction),
    /// An interrupted transaction reached `COMMITTED` (the commit point) but
    /// crashed before its candidate/backup/marker were cleaned up. The
    /// committed live rules are authoritative and **must not be rolled back**;
    /// the caller re-validates the live file and then calls
    /// [`UserRuleTransactionPort::cleanup_committed`].
    CommittedPendingCleanup(PreparedRuleTransaction),
}

/// File-system transaction port for atomic AI user-rule batch commits.
/// Implemented by platform crates (Windows in this release); Core orchestrates
/// against this contract and never promises `std::fs::rename` atomicity.
///
/// # Single persistent marker state machine
///
/// Transaction state is **never inferred** from whether the live YAML parses or
/// from whether a backup happens to exist. [`Self::prepare`] publishes a
/// candidate, a durable backup (only when the original live file exists) and
/// then — and only then — a single persistent marker whose contents are exactly
/// one of:
///
/// ```text
/// ACTIVE/PRESENT   original live file existed; .txn.original holds its bytes
/// ACTIVE/ABSENT    original live file did not exist (first creation)
/// ROLLED_BACK/PRESENT
///                  original bytes were restored and verified; cleanup pending
/// ROLLED_BACK/ABSENT
///                  original absence was restored and verified; cleanup pending
/// COMMITTED        the replacement has been fully verified; commit point
/// ```
///
/// The marker is **never** published through a remove-then-rename sequence: a
/// state change rewrites the fixed-length token in place and flushes, so there
/// is no instant at which the marker is absent. An interrupted write can leave
/// a truncated/invalid token (fail-closed on the next read), never a missing
/// marker.
///
/// [`Self::replace`] swaps the live file but never removes the marker/backup/
/// candidate. After the replacement has been re-read, re-validated and
/// re-verified, [`Self::commit_verified`] transitions the marker to `COMMITTED`
/// — the **sole commit point**. The caller then calls [`Self::cleanup_committed`]
/// to best-effort remove the candidate/backup/marker; a cleanup failure never
/// turns a committed transaction into an uncommitted report. Any failure before
/// `commit_verified` succeeds calls [`Self::rollback`], which recovers strictly
/// from the `ACTIVE` marker state — `ACTIVE/PRESENT` restores the backup bytes,
/// `ACTIVE/ABSENT` removes the live file — verifies that restoration, publishes
/// the corresponding `ROLLED_BACK/*` marker, and returns the transaction for
/// stateful cleanup. It must not perform unclassified cleanup while the marker
/// is still `ACTIVE`.
///
/// [`Self::recover_if_needed`] (crash recovery at the start of the next
/// transaction) follows the same state machine and reports its outcome via
/// [`TransactionRecovery`]. `ACTIVE/*` is restored and published as
/// `ROLLED_BACK/*`; an existing `ROLLED_BACK/*` marker is returned directly
/// without guessing or deleting. `COMMITTED` transactions are never rolled
/// back; the port keeps their material and returns `CommittedPendingCleanup` so
/// the Core caller can semantically validate the committed live file before
/// cleanup.
pub trait UserRuleTransactionPort: Send + Sync {
    /// Acquires the exclusive transaction lock for `rule_file`.
    fn acquire_exclusive(
        &self,
        rule_file: &Path,
    ) -> Result<Box<dyn UserRuleTransactionGuard>, String>;
    /// Recovers any interrupted transaction left by a crashed process, strictly
    /// by the marker state (see the trait docs), and reports what was done.
    /// Called before a new transaction starts. Invalid or contradictory states
    /// refuse and preserve all evidence.
    fn recover_if_needed(&self, rule_file: &Path) -> Result<TransactionRecovery, String>;
    /// Reads the current live bytes of `rule_file` (or `None` if absent).
    fn read_current(&self, rule_file: &Path) -> Result<Option<Vec<u8>>, String>;
    /// Stages `bytes` as the transaction candidate for `rule_file`: atomically
    /// publishes the candidate, a durable backup of the pre-replace live file
    /// (only when one exists; for `ABSENT` first creation any stale backup must
    /// be removed first), and finally the persistent `ACTIVE/PRESENT` or
    /// `ACTIVE/ABSENT` marker. The live file is **not** replaced here.
    fn prepare(&self, rule_file: &Path, bytes: &[u8]) -> Result<PreparedRuleTransaction, String>;
    /// Atomically replaces the live file with the staged candidate. The marker,
    /// backup and candidate are left untouched.
    fn replace(&self, tx: &PreparedRuleTransaction) -> Result<(), String>;
    /// Rolls back a not-yet-committed transaction by its `ACTIVE` marker:
    /// `ACTIVE/PRESENT` restores the backup bytes over the live file,
    /// `ACTIVE/ABSENT` removes the live file to restore the original absence.
    /// After byte/absence verification it must first publish the corresponding
    /// `ROLLED_BACK/*` marker and return the transaction. It must not remove
    /// candidate/marker/backup while the state is still `ACTIVE`. A
    /// `COMMITTED` or already `ROLLED_BACK` transaction is never rolled back.
    fn rollback(&self, tx: &PreparedRuleTransaction) -> Result<PreparedRuleTransaction, String>;
    /// Cleans material for a verified `ROLLED_BACK/*` transaction. For
    /// `ROLLED_BACK/PRESENT`, remove candidate → backup → marker. For
    /// `ROLLED_BACK/ABSENT`, reject any backup as a contradiction and remove
    /// candidate → marker. Any failure must leave the ROLLED_BACK marker in
    /// place; in particular, backup-removal failure must never make the backup
    /// look like an unclassified stray on the next recovery.
    fn cleanup_rolled_back(&self, tx: &PreparedRuleTransaction) -> Result<(), String>;
    /// Atomically transitions the marker to `COMMITTED` — the sole commit
    /// point — using an in-place fixed-length marker rewrite (no no-marker
    /// window). An `Err` means the commit point was not reached (caller must
    /// roll back). This does **not** remove the candidate/backup/marker.
    fn commit_verified(&self, tx: &PreparedRuleTransaction) -> Result<(), String>;
    /// Best-effort removal of a committed transaction's material. Called only
    /// after the marker reached `COMMITTED` and the caller validated the
    /// committed live file. An `Err` means some material remains (caller must
    /// not roll back — the rules are committed) and a new transaction should be
    /// refused until cleanup succeeds.
    fn cleanup_committed(&self, tx: &PreparedRuleTransaction) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CANON: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";
    const SUGGESTION_CANON: &str = "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f";

    fn sample_provenance() -> RuleProvenance {
        RuleProvenance {
            origin: RuleOrigin::AiAdvisor,
            profile_id: AiProfileId::parse(CANON).unwrap(),
            scan_generation: 7,
            suggestion_id: AiSuggestionId::parse(SUGGESTION_CANON).unwrap(),
            user_final_risk: crate::RiskLevel::RegenerableLocal,
            created_at_epoch_secs: 1_760_000_000,
        }
    }

    #[test]
    fn ai_profile_id_rejects_non_uuid_and_never_contains_a_key() {
        assert!(AiProfileId::parse("not-a-uuid").is_err());
        assert!(AiProfileId::parse("sk-test-secret-value-abcdef0123456789").is_err());
        let id = AiProfileId::parse(CANON).unwrap();
        assert_eq!(id.as_str(), CANON);
    }

    #[test]
    fn ai_profile_id_rejects_bad_lengths_hyphen_layout_and_non_hex() {
        assert!(AiProfileId::parse(&CANON[..35]).is_err());
        assert!(AiProfileId::parse(&format!("{CANON}0")).is_err());
        // Hyphen in the wrong place.
        assert!(AiProfileId::parse("018f7e219-d15-7b17-a5fd-4f0f2bcadc72").is_err());
        // Non-hex character in the last group.
        assert!(AiProfileId::parse("018f7e21-9d15-7b17-a5fd-4f0f2bcadc7g").is_err());
        // Empty string and whitespace.
        assert!(AiProfileId::parse("").is_err());
        assert!(AiProfileId::parse("  ").is_err());
    }

    #[test]
    fn ai_profile_id_requires_hyphens_at_separator_positions_only() {
        // Fix-round 1 regression: the old byte check treated the hex-digit
        // clause as an independent `||`, so a hex digit sitting *on* a
        // separator position (index 8/13/18/23) was wrongly accepted. All
        // four `-` replaced by `0` must be rejected, and so must a single hex
        // digit on one separator position.
        let hyphens_to_zero = "018f7e2109d1507b170a5fd04f0f2bcadc72";
        assert_eq!(hyphens_to_zero.len(), 36);
        assert!(AiProfileId::parse(hyphens_to_zero).is_err());

        let mut chars: Vec<char> = CANON.chars().collect();
        chars[8] = '0';
        let one_bad_separator: String = chars.into_iter().collect();
        assert!(AiProfileId::parse(&one_bad_separator).is_err());
    }

    #[test]
    fn ai_profile_id_normalises_to_lowercase_canonical_display() {
        let upper = "018F7E21-9D15-7B17-A5FD-4F0F2BCADC72";
        let id = AiProfileId::parse(upper).unwrap();
        assert_eq!(id.as_str(), CANON);
        assert_eq!(id.to_string(), CANON);
    }

    #[test]
    fn ai_profile_id_serde_validates_at_the_parse_layer() {
        assert!(serde_json::from_str::<AiProfileId>(&format!("\"{CANON}\"")).is_ok());
        assert!(serde_json::from_str::<AiProfileId>("\"not-a-uuid\"").is_err());
        let id: AiProfileId = serde_json::from_str(&format!("\"{CANON}\"")).unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{CANON}\""));
    }

    #[test]
    fn ai_suggestion_id_is_a_canonical_uuid_not_free_text() {
        assert!(AiSuggestionId::parse(SUGGESTION_CANON).is_ok());
        assert_eq!(
            AiSuggestionId::parse(SUGGESTION_CANON).unwrap().as_str(),
            SUGGESTION_CANON
        );
        // Same parse guarantees as AiProfileId: no free text / secrets / paths.
        for bad in [
            r"C:\Users\alice\.ssh",
            "sk-secret-value",
            "not-a-uuid",
            "",
            "sug-42",
        ] {
            assert!(
                AiSuggestionId::parse(bad).is_err(),
                "suggestion id must be a canonical UUID, got {bad:?}"
            );
        }
        // Serde round-trip preserves the canonical text.
        let id = AiSuggestionId::parse(SUGGESTION_CANON).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{SUGGESTION_CANON}\""));
        assert!(serde_json::from_str::<AiSuggestionId>(&format!("\"{SUGGESTION_CANON}\"")).is_ok());
        assert!(
            serde_json::from_str::<AiSuggestionId>("\"C:\\\\Users\\\\alice\\\\.ssh\"").is_err()
        );
    }

    #[test]
    fn derived_api_key_env_is_exact_uppercase_derivation() {
        let id = AiProfileId::parse(CANON).unwrap();
        let derived: AiKeyEnvName = derived_api_key_env(&id);
        assert_eq!(
            derived.as_str(),
            format!("DEVRESIDUE_AI_KEY_{}", CANON.to_ascii_uppercase())
        );
        // Lower-case canonical input still derives the upper-case name.
        let upper_id = AiProfileId::parse(&CANON.to_ascii_uppercase()).unwrap();
        assert_eq!(derived_api_key_env(&upper_id).as_str(), derived.as_str());
    }

    #[test]
    fn api_key_env_serialises_only_the_exact_derived_name() {
        // Fix-round 2: AiKeyEnvName is a strong type whose only public origin is
        // derived_api_key_env(&AiProfileId). Its serialised value is exactly
        // DEVRESIDUE_AI_KEY_<UPPERCASE_UUID> — never arbitrary text.
        let id = AiProfileId::parse(CANON).unwrap();
        let name: AiKeyEnvName = derived_api_key_env(&id);
        assert_eq!(
            serde_json::to_string(&name).unwrap(),
            format!("\"DEVRESIDUE_AI_KEY_{}\"", CANON.to_ascii_uppercase())
        );
        assert_eq!(
            name.as_str().strip_prefix(AI_KEY_ENV_PREFIX),
            Some(CANON.to_ascii_uppercase().as_str())
        );
    }

    #[test]
    fn api_key_env_rejects_every_non_derived_value_through_any_public_path() {
        // Fix-round 2: no public API can persist a PATH, a secret value, a
        // lower-case derived name or another profile's derived name. The only
        // construction surface is derived_api_key_env(&id), which always yields
        // the exact upper-case derivation for *that* id, so the JSON/YAML parse
        // cross-check is the last line of defence.
        let other_uuid = "1f2e3d4c-5b6a-4e8f-9a7b-0c1d2e3f4a5b";
        for bad in [
            r"C:\Users\alice\.ssh",
            "sk-secret-value",
            "DEVRESIDUE_AI_KEY_not-a-uuid",
            &format!("DEVRESIDUE_AI_KEY_{CANON}"), // lower-case suffix
            &format!("DEVRESIDUE_AI_KEY_{}", other_uuid.to_ascii_uppercase()),
        ] {
            let json = json!({
                "id": CANON,
                "name": "x",
                "base_url": "https://example.invalid/v1",
                "model": "m",
                "api_key_env": bad,
                "structured_output_mode": "auto",
                "timeout_secs": 120,
                "enabled": true,
            });
            assert!(
                serde_json::from_value::<AiProfile>(json.clone()).is_err(),
                "json persistence of {bad:?} must fail"
            );
            let yaml = serde_yaml_ng::to_string(
                &serde_json::from_value::<serde_json::Value>(json).unwrap(),
            )
            .unwrap();
            assert!(
                serde_yaml_ng::from_str::<AiProfile>(&yaml).is_err(),
                "yaml persistence of {bad:?} must fail"
            );
        }

        // The exact derived name still round-trips through both formats.
        let good = format!("DEVRESIDUE_AI_KEY_{}", CANON.to_ascii_uppercase());
        let profile_json = json!({
            "id": CANON,
            "name": "x",
            "base_url": "https://example.invalid/v1",
            "model": "m",
            "api_key_env": good,
            "structured_output_mode": "auto",
            "timeout_secs": 120,
            "enabled": true,
        });
        let parsed: AiProfile = serde_json::from_value(profile_json.clone()).unwrap();
        assert_eq!(parsed.api_key_env().as_str(), good);
        let yaml = serde_yaml_ng::to_string(&profile_json).unwrap();
        let parsed_yaml: AiProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed_yaml.api_key_env().as_str(), good);
    }

    #[test]
    fn rule_provenance_serialises_only_whitelisted_fields() {
        let value = serde_json::to_value(sample_provenance()).unwrap();
        let obj = value.as_object().expect("provenance object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = vec![
            "created_at_epoch_secs",
            "origin",
            "profile_id",
            "scan_generation",
            "suggestion_id",
            "user_final_risk",
        ];
        expected.sort_unstable();
        assert_eq!(keys, expected);
        for forbidden in ["path", "reason", "model", "key", "request", "response"] {
            assert!(
                !keys.contains(&forbidden),
                "`{forbidden}` must never leak into provenance"
            );
        }
    }

    #[test]
    fn rule_provenance_round_trips_through_json() {
        let original = sample_provenance();
        let json = serde_json::to_string(&original).unwrap();
        let back: RuleProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn rule_origin_serialises_as_ai_advisor_token() {
        assert_eq!(
            serde_json::to_string(&RuleOrigin::AiAdvisor).unwrap(),
            "\"ai-advisor\""
        );
    }

    #[test]
    fn rule_provenance_rejects_free_text_suggestion_id_at_parse() {
        // Fix-round 1: suggestion_id must be a canonical UUID, never free text
        // (a path, a key-looking value or a multi-line response string).
        for bad in [
            r"C:\Users\alice\.ssh",
            "sk-secret-value",
            "{\n  \"role\": \"system\",\n  \"content\": \"multi-line\"\n}",
            "",
        ] {
            let value = json!({
                "origin": "ai-advisor",
                "profile_id": CANON,
                "scan_generation": 7,
                "suggestion_id": bad,
                "user_final_risk": "safe",
                "created_at_epoch_secs": 1_760_000_000,
            });
            assert!(
                serde_json::from_value::<RuleProvenance>(value).is_err(),
                "free-text suggestion_id must be rejected at parse, got {bad:?}"
            );
        }
    }

    #[test]
    fn ai_profile_rejects_invalid_api_key_env_at_parse() {
        // Fix-round 1: api_key_env must be the *exact* derived variable name
        // `DEVRESIDUE_AI_KEY_<UPPERCASE_CANONICAL_UUID>` for this profile id.
        // Paths, secret-looking values and non-derived names are rejected at
        // the serde parse layer so they can never be persisted.
        let other_uuid = "1f2e3d4c-5b6a-4e8f-9a7b-0c1d2e3f4a5b";
        for bad in [
            r"C:\Users\alice\.ssh",
            "sk-secret-value",
            "DEVRESIDUE_AI_KEY_not-a-uuid",
            // Lower-case suffix: not the exact derived (uppercase) form.
            &format!("DEVRESIDUE_AI_KEY_{CANON}"),
            // A different (but well-formed) profile's variable name.
            &format!("DEVRESIDUE_AI_KEY_{}", other_uuid.to_ascii_uppercase()),
        ] {
            let value = json!({
                "id": CANON,
                "name": "x",
                "base_url": "https://example.invalid/v1",
                "model": "m",
                "api_key_env": bad,
                "structured_output_mode": "auto",
                "timeout_secs": 120,
                "enabled": true,
            });
            assert!(
                serde_json::from_value::<AiProfile>(value).is_err(),
                "api_key_env must equal the exact derived name for this profile, got {bad}"
            );
        }

        // The correctly derived uppercase name still parses.
        let good = format!("DEVRESIDUE_AI_KEY_{}", CANON.to_ascii_uppercase());
        let value = json!({
            "id": CANON,
            "name": "x",
            "base_url": "https://example.invalid/v1",
            "model": "m",
            "api_key_env": good,
            "structured_output_mode": "auto",
            "timeout_secs": 120,
            "enabled": true,
        });
        assert!(serde_json::from_value::<AiProfile>(value).is_ok());
    }

    #[test]
    fn ai_profile_json_has_no_secret_value_field() {
        let id = AiProfileId::parse(CANON).unwrap();
        let env = derived_api_key_env(&id);
        let profile = AiProfile::new(
            id,
            "local gateway".to_string(),
            "https://example.invalid/v1".to_string(),
            "model-x".to_string(),
            StructuredOutputMode::JsonSchema,
            120,
            true,
        );
        let value = serde_json::to_value(&profile).unwrap();
        let obj = value.as_object().expect("profile object");
        for forbidden in ["apiKey", "api_key", "secret", "keyValue"] {
            assert!(
                !obj.contains_key(forbidden),
                "profile must not serialise a secret field `{forbidden}`"
            );
        }
        // The only key-adjacent field is the generated environment-variable
        // *name* (non-sensitive), never a value.
        assert_eq!(obj["api_key_env"], json!(profile.api_key_env().as_str()));
        // Getters expose every non-secret field.
        assert_eq!(profile.id().as_str(), CANON);
        assert_eq!(profile.name(), "local gateway");
        assert_eq!(profile.base_url(), "https://example.invalid/v1");
        assert_eq!(profile.model(), "model-x");
        assert_eq!(profile.api_key_env().as_str(), env.as_str());
        assert_eq!(
            profile.structured_output_mode(),
            StructuredOutputMode::JsonSchema
        );
        assert_eq!(profile.timeout_secs(), 120);
        assert!(profile.enabled());
    }

    #[test]
    fn serialized_env_name_always_matches_the_profiles_own_id() {
        // Fix-round 3: even if the private field were corrupted in-module to
        // another id's derived name, serialization derives the emitted
        // api_key_env from this profile's own id — a mismatched name can never
        // be serialized. (External code cannot perform this mutation at all:
        // the fields are private.)
        let id_a = AiProfileId::parse(CANON).unwrap();
        let id_b = AiProfileId::parse("1f2e3d4c-5b6a-4e8f-9a7b-0c1d2e3f4a5b").unwrap();
        let env_a = derived_api_key_env(&id_a);
        let env_b = derived_api_key_env(&id_b);
        let mut profile = AiProfile::new(
            id_a,
            "n".to_string(),
            "https://example.invalid/v1".to_string(),
            "m".to_string(),
            StructuredOutputMode::Auto,
            120,
            true,
        );
        // Mutate the private field (only reachable inside this module) to a
        // name derived from a *different* valid id.
        profile.api_key_env = env_b.clone();
        let value = serde_json::to_value(&profile).unwrap();
        let emitted = value["api_key_env"].as_str().unwrap();
        assert_eq!(
            emitted,
            env_a.as_str(),
            "serialized env name must be derived from the profile's own id, \
             never from a mismatched field value"
        );
        assert_ne!(emitted, env_b.as_str());
        // The stored getter reflects the (corrupted) field, but the wire output
        // does not — proving serialization has no externally injectable point.
        assert_eq!(profile.api_key_env().as_str(), env_b.as_str());
    }

    #[test]
    fn sanitized_entry_fields_are_stable_and_path_free() {
        // Guards the no-secret DTO surface against accidental schema drift
        // (adding PathBuf / raw text / arbitrary JSON later would trip this).
        let entry = SanitizedEntry {
            id: AiEntryToken::from("tok-1".to_string()),
            zone: AiZone::LocalAppData,
            relative_depth: 2,
            display_name: "<sensitive-metadata>".to_string(),
            source_kind: "dev-cache".to_string(),
            category_hint: "developer-cache".to_string(),
            product_hint: Some("some-tool".to_string()),
            size_bucket: SizeBucket::Under64MiB,
            age_bucket: AgeBucket::Under7Days,
            signals: vec!["layout:cache".to_string()],
        };
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 10);
        let json = serde_json::to_string(&value).unwrap();
        assert!(!json.contains("PathBuf"));
        assert!(!json.contains("C:\\"));
    }
}
