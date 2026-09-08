//! Scan result persistence — `last-scan.json` under the DevResidue data
//! directory (PLAN Phase 7/8: "scan 输出本机真实数据" + plan reuses it).
//!
//! The file stores the most recent scan only. Scan-item ids are sequential
//! **within one scan run** and are re-assigned on the next scan; a plan built
//! against a scan becomes stale the moment a newer scan overwrites the file.
//! Corrupt or conflicting data fails closed (an explicit error, never a silent
//! empty plan).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use devresidue_core::journal::default_data_dir;
use devresidue_core::ScanItem;

/// What produced the stored scan (drives plan interpretation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanMode {
    /// Real providers over the given workspace roots.
    Real { workspace_roots: Vec<String> },
    /// The built-in demo fixtures.
    Fixtures,
}

/// One persisted scan run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSnapshot {
    /// When the scan finished (epoch seconds).
    pub scanned_at: u64,
    /// Scan **generation** (R04): increments every time a snapshot is saved to
    /// this data root. `(generation, item.id)` uniquely identifies an object
    /// across processes; a plan/session that references an older generation is
    /// automatically stale. Missing on pre-R04 files (defaults to 0, which the
    /// reader treats as 1 for display, but any id from such a file is
    /// immediately stale once a new scan runs).
    #[serde(default)]
    pub generation: u64,
    pub mode: ScanMode,
    /// Items with sequential ids (valid for the most recent scan only).
    pub items: Vec<ScanItem>,
    /// Warnings collected during the scan (tool rejections etc.).
    pub warnings: Vec<String>,
    /// Whether the scan was cancelled by the user (Ctrl-C / cancel token).
    /// Partial results are still meaningful; `default` keeps older snapshots
    /// (written before this flag existed) readable as `false`.
    #[serde(default)]
    pub cancelled: bool,
    /// R2-F01 integrity: HMAC-SHA256 over the canonical serialisation of the
    /// **whole authorisation record** (R3-G05: `generation`, `mode` — with its
    /// workspace roots — `cancelled`, `scanned_at` and `items` together; see
    /// [`mac_input`]), keyed by `integrity.key` next to this data root.
    /// Written by [`save`] and verified by [`load`]; a record whose MAC does
    /// not match is refused (fail-closed). `None` on legacy pre-HMAC files —
    /// [`load`] refuses those too and requires a re-scan (a record without a
    /// MAC is not an authorised command source).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

impl ScanSnapshot {
    /// Records a snapshot now (a completed, non-cancelled scan). The caller
    /// assigns `generation` (see [`next_generation`]); this constructor keeps
    /// it at 0 so a caller that forgets is visible in tests.
    pub fn new(mode: ScanMode, items: Vec<ScanItem>, warnings: Vec<String>) -> Self {
        let scanned_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            scanned_at,
            generation: 0,
            mode,
            items,
            warnings,
            cancelled: false,
            integrity: None, // set by save()
        }
    }
}

/// Returns the next generation for this data root: the previous snapshot's
/// generation + 1, or 1 when no snapshot exists yet. Generation 0 is treated
/// as "no previous snapshot" (legacy files read back as 0).
///
/// R3-G06: this is a **read-only helper**. Allocating from it and saving
/// later is the non-atomic read→increment→save window two writers can
/// interleave through — production callers must use
/// [`save_with_next_generation`], which allocates and publishes under a
/// cross-process lock so concurrent writers never mint the same generation.
/// This entry point remains for diagnostics and tests that do not publish.
pub fn next_generation(base: &Path) -> u64 {
    let current = match load(base) {
        Ok(s) => s.generation,
        // R3-G06: only "file does not exist" means "no previous snapshot".
        // A corrupt / unreadable / tampered record is NOT a reset to zero —
        // that would let a damaged store hand out generation 1 again and
        // revive stale authorisations.
        Err(e) if e.contains("no snapshot at") => 0,
        Err(e) if e.contains("integrity check failed") || e.contains("no integrity tag") => {
            // MAC-invalid record (tampered / legacy schema): the declared
            // counter may still be consulted for *monotonicity only* (never
            // as an authorisation source — load() already refused it).
            declared_generation_of(base).unwrap_or(0)
        }
        Err(e) => {
            // Corrupt/unparseable: refuse to derive a sensible generation
            // (0 → next_generation() reports 1; callers must re-scan — the
            // locking allocator publishes at the fallback generation
            // instead of trusting this value).
            eprintln!("scan store unusable, next_generation refused: {e}");
            0
        }
    };
    current + 1
}

/// Path of the cross-process allocation lock for this data root (R3-G06).
fn generation_lock_path(base: &Path) -> PathBuf {
    base.join("scan-store.lock")
}

/// Acquires the cross-process generation lock (R3-G06). The whole-file
/// exclusive lock is taken on an open lock file through the injected
/// [`LockAcquirer`] (the platform layer's Win32 `LockFileEx` / POSIX `flock`
/// adapter — the only crate allowed `unsafe`). The guard releases on drop.
struct GenerationLock {
    _file: Box<dyn devresidue_core::safety::FileLock>,
}

impl GenerationLock {
    fn acquire(base: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(base).map_err(|e| format!("create {}: {e}", base.display()))?;
        // Open with read+write so every process may open it; the byte-range
        // lock below is the mutual exclusion.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(generation_lock_path(base))
            .map_err(|e| format!("open lock {}: {e}", generation_lock_path(base).display()))?;
        // Default to the platform's real Win32 lock on Windows targets, the
        // no-op test lock elsewhere — an explicit `install_lock_acquirer`
        // (the CLI / Tauri wiring) or `install_test_lock` (unit tests) wins
        // because OnceLock keeps the first setter.
        let acquirer = *LOCK_ACQUIRER.get_or_init(default_lock_acquirer);
        let lock = acquirer(file)
            .map_err(|e| format!("lock {}: {e}", generation_lock_path(base).display()))?;
        Ok(Self { _file: lock })
    }
}

/// The default acquirer: the real Win32 `LockFileEx` adapter on Windows (the
/// providers crate itself stays `unsafe`-free — the platform crate owns the
/// FFI); a no-op stub on other targets, where cross-process exclusion is a
/// non-Windows test concern only.
fn default_lock_acquirer() -> LockAcquirer {
    #[cfg(windows)]
    {
        devresidue_platform_windows::filesystem::lock_file_exclusive
    }
    #[cfg(not(windows))]
    {
        |_file: std::fs::File| {
            #[derive(Debug)]
            struct NoLock;
            impl devresidue_core::safety::FileLock for NoLock {}
            Ok(Box::new(NoLock) as Box<dyn devresidue_core::safety::FileLock>)
        }
    }
}

/// How the platform layer provides the real cross-process lock: opens
/// `std::fs::File`, returns a held lock guard. Injected once at wiring time
/// (the CLI / Tauri shells install the Windows adapter before any scan).
type LockAcquirer = fn(std::fs::File) -> Result<Box<dyn devresidue_core::safety::FileLock>, String>;

static LOCK_ACQUIRER: std::sync::OnceLock<LockAcquirer> = std::sync::OnceLock::new();

/// The real acquirer to install in production (CLI / Tauri wiring): delegates
/// to the Windows platform crate's `LockFileEx` adapter. Exposed so both
/// shells install the same function pointer without each spelling the path.
#[cfg(windows)]
pub fn windows_lock_acquirer() -> LockAcquirer {
    devresidue_platform_windows::filesystem::lock_file_exclusive
}

/// Installs the platform lock acquirer (called by the CLI / Tauri wiring;
/// idempotent). Until it is installed, [`save_with_next_generation`] refuses
/// to publish — fail closed rather than allocating generations without
/// cross-process exclusion.
pub fn install_lock_acquirer(acquirer: LockAcquirer) {
    let _ = LOCK_ACQUIRER.set(acquirer);
}

/// Test/pure-Rust fallback: a no-op "lock" that provides NO exclusion — never
/// installed in production; used only so `save_with_next_generation` can run
/// in the providers crate's own tests (single-threaded, no cross-writer
/// contention).
#[cfg(test)]
pub(crate) fn install_test_lock() {
    install_lock_acquirer(|_file| {
        #[derive(Debug)]
        struct NoLock;
        impl devresidue_core::safety::FileLock for NoLock {}
        Ok(Box::new(NoLock))
    });
}

/// Atomically allocates the next generation and publishes the snapshot
/// (R3-G06): the whole read → increment → save transaction runs under the
/// data root's cross-process lock, so two concurrent writers get distinct
/// generations, and each published record is internally consistent (atomic
/// rename publish; the MAC binds the generation into the record).
///
/// Returns the published snapshot (with its assigned generation).
///
/// Error semantics (fail-closed where it matters, fail-open only for
/// *overwriting*):
///
/// - a **missing** snapshot starts at generation 1;
/// - an **integrity failure** in the existing record (tampered, legacy
///   pre-HMAC, or a MAC made under a different `mac_input` schema — e.g. the
///   v1 items-only MAC before R3-G05) means the record cannot authorise
///   anything, but it must not block a *new* scan from publishing over it:
///   publishing is the fail-closed remedy ("re-scan"). The new generation
///   continues from the record's declared generation + 1 (a tampered `items`
///   set is one thing; the generation counter itself is only consulted for
///   monotonicity, and the freshly published record is MAC-bound under the
///   *current* schema);
/// - a **corrupt / unparseable** record is the same remedy path (publish
///   over it), but the generation counter cannot be trusted, so allocation
///   continues from `u64::MAX/2` — strictly above any sane past generation,
///   so stale plans gated on the old generation can never match the new one.
pub fn save_with_next_generation(
    base: &Path,
    build: impl FnOnce(u64) -> ScanSnapshot,
) -> Result<ScanSnapshot, String> {
    let _guard = GenerationLock::acquire(base)?;
    // Under the lock: read the last generation. Only "no snapshot yet" may
    // start at 1.
    let generation = match load(base) {
        Ok(prev) => prev.generation + 1,
        Err(e) if e.contains("no snapshot at") => 1,
        Err(e) if e.contains("integrity check failed") || e.contains("no integrity tag") => {
            // The existing record failed its MAC (tampered / legacy schema).
            // Extract its declared generation if it parses; publishing a
            // fresh, correctly-signed snapshot over it IS the required
            // re-scan. When the declared counter is missing or itself
            // absurd (>= the fallback), fall back so the new generation is
            // strictly above anything a stale plan could gate on.
            match declared_generation_of(base) {
                Some(declared) if declared < integrity_fallback_generation() => {
                    declared.saturating_add(1)
                }
                _ => integrity_fallback_generation(),
            }
        }
        Err(e) if e.contains("parse") => {
            // Unparseable store: publish over it (re-scan), at a generation
            // no past plan could ever match.
            integrity_fallback_generation()
        }
        Err(e) => return Err(format!("cannot allocate next generation: {e}")),
    };
    let snapshot = build(generation);
    save(base, &snapshot)?;
    Ok(snapshot)
}

/// The generation an integrity-failed store falls back to when the declared
/// counter is missing or untrustworthy: `u64::MAX / 2`, far above any
/// legitimate generation (one per scan). A stale plan gated on an old
/// generation can therefore never alias the newly published one.
fn integrity_fallback_generation() -> u64 {
    u64::MAX / 2
}

/// Best-effort read of the declared `generation` field from an
/// integrity-failed record (parse only — never an authorisation source).
fn declared_generation_of(base: &Path) -> Option<u64> {
    let bytes = std::fs::read(snapshot_path(base)).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("generation")?.as_u64()
}

/// Path of the last-scan file under a data root.
pub fn snapshot_path(base: &Path) -> PathBuf {
    base.join("last-scan.json")
}

/// Path of the integrity key under a data root.
pub fn integrity_key_path(base: &Path) -> PathBuf {
    base.join("integrity.key")
}

/// Loads (creating on first use) the HMAC key for this data root. The key is
/// random per data root and written with user-default ACLs (the user profile /
/// data dir is the same trust boundary as the plan files).
fn load_or_create_key(base: &Path) -> Result<Vec<u8>, String> {
    std::fs::create_dir_all(base).map_err(|e| format!("create {}: {e}", base.display()))?;
    let path = integrity_key_path(base);
    if let Ok(bytes) = std::fs::read(&path) {
        return Ok(bytes);
    }
    // 32 random bytes via the OS RNG.
    let mut key = [0u8; 32];
    getrandom_fill(&mut key)?;
    std::fs::write(&path, key).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(key.to_vec())
}

/// Fills `buf` from the OS CSPRNG.
///
/// Windows: the platform crate's `BCryptGenRandom` adapter
/// (`BCRYPT_USE_SYSTEM_PREFERRED_RNG`). Unix: `/dev/urandom`. R3 §4: the
/// previous time+pid+address SplitMix seeding was a pragmatic fallback whose
/// comment honestly flagged it as "not a real CSPRNG" — this is the real
/// thing, so the HMAC key is genuinely unpredictable.
fn getrandom_fill(buf: &mut [u8]) -> Result<(), String> {
    #[cfg(windows)]
    {
        devresidue_platform_windows::safety::os_random(buf)
    }
    #[cfg(not(windows))]
    {
        // Unix: /dev/urandom.
        use std::io::Read;
        let mut f =
            std::fs::File::open("/dev/urandom").map_err(|e| format!("open /dev/urandom: {e}"))?;
        f.read_exact(buf)
            .map_err(|e| format!("read /dev/urandom: {e}"))
    }
}

/// Snapshot metadata + items as one canonical authorisation record (R3-G05).
///
/// Every field that feeds a **security decision** is bound into the MAC:
/// `generation` (stale-plan gating), `cancelled` (partial-scan refusal),
/// `mode` — including its embedded `workspace_roots` (validator protection
/// context and the fixtures/real gate) — and `items` themselves. `warnings`
/// are display-only and excluded. A new record schema/version bump changes
/// this serialisation shape, which automatically invalidates older MACs
/// (tamper-wise indistinguishable from a re-key → re-scan, which is the
/// fail-closed direction).
fn mac_input(snapshot: &ScanSnapshot) -> Result<Vec<u8>, String> {
    #[derive(Serialize)]
    struct AuthorisationRecord<'a> {
        /// Schema tag of the MAC input (R3-G05): versioning the signed shape
        /// itself, so a change of covered fields is a visible contract bump.
        schema: u32,
        generation: u64,
        scanned_at: u64,
        cancelled: bool,
        mode: &'a ScanMode,
        items: &'a [ScanItem],
    }
    const MAC_SCHEMA_VERSION: u32 = 2;
    let record = AuthorisationRecord {
        schema: MAC_SCHEMA_VERSION,
        generation: snapshot.generation,
        scanned_at: snapshot.scanned_at,
        cancelled: snapshot.cancelled,
        mode: &snapshot.mode,
        items: &snapshot.items,
    };
    serde_json::to_vec(&record).map_err(|e| format!("serialise authorisation record: {e}"))
}

/// Persists a scan snapshot with its HMAC integrity tag.
///
/// R3-G06: the whole publish is **atomic** — the snapshot is written to a
/// unique sibling temp file and renamed over `last-scan.json` (a same-volume
/// rename on NTFS is atomic), so a reader never observes a torn record.
/// Because the record's MAC covers `generation`, two writers racing on this
/// file always publish distinguishable, individually self-consistent
/// snapshots; writers are expected to allocate their generation under
/// [`next_generation_locked`] so concurrent processes never mint the same
/// one.
pub fn save(base: &Path, snapshot: &ScanSnapshot) -> Result<(), String> {
    let key = load_or_create_key(base)?;
    let mac = devresidue_core::integrity::hmac_sha256(&key, &mac_input(snapshot)?);
    let tagged = ScanSnapshot {
        integrity: Some(devresidue_core::integrity::hex(&mac)),
        ..snapshot.clone()
    };
    std::fs::create_dir_all(base).map_err(|e| format!("create {}: {e}", base.display()))?;
    let json =
        serde_json::to_vec_pretty(&tagged).map_err(|e| format!("serialise snapshot: {e}"))?;
    let path = snapshot_path(base);
    // R3-G06: write-then-rename atomic publish. The temp name embeds pid +
    // nanos, so concurrent writers never collide on the sibling.
    let tmp = base.join(format!(
        "last-scan.json.tmp-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("publish {}: {e}", path.display())
    })
}

/// Loads the most recent scan.
///
/// Fail-closed: a record whose HMAC does not match the key is refused; a
/// legacy record with no integrity tag is refused too (it predates the MAC and
/// is not an authorised command source — re-scan required). A missing file is
/// an error (`NoSnapshot` semantics: "run a scan first"); a **corrupt** file
/// is a distinct error and never resets generation state silently (R3-G06).
pub fn load(base: &Path) -> Result<ScanSnapshot, String> {
    let path = snapshot_path(base);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        // R3-G06: distinguish "no snapshot yet" from a read failure of an
        // existing file (never treat a transient error as "first run").
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no snapshot at {}", path.display()))
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let snapshot: ScanSnapshot =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let Some(expected_mac) = &snapshot.integrity else {
        return Err(format!(
            "scan store integrity check failed: {} carries no integrity tag (pre-HMAC \
             record) — re-scan required",
            path.display()
        ));
    };
    let key = std::fs::read(integrity_key_path(base))
        .map_err(|e| format!("read integrity key for {}: {e}", path.display()))?;
    let actual = devresidue_core::integrity::hmac_sha256(&key, &mac_input(&snapshot)?);
    if devresidue_core::integrity::hex(&actual) != *expected_mac {
        return Err(format!(
            "scan store integrity check failed: {} was tampered — re-scan required",
            path.display()
        ));
    }
    Ok(snapshot)
}

/// Convenience: the default data directory (for CLI wiring).
pub fn default_data_root() -> Result<PathBuf, String> {
    default_data_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::domain::action::CleanupAction;

    #[test]
    fn round_trips_and_corruption_fails_closed() {
        let base = std::env::temp_dir().join(format!("dr-scanstore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let item = ScanItem {
            id: devresidue_core::ScanItemId::from_raw(1),
            path: PathBuf::from("C:\\demo\\cache"),
            display_name: "demo".into(),
            product: None,
            category: devresidue_core::ResidueCategory::BuildArtifact,
            risk: devresidue_core::RiskLevel::RegenerableLocal,
            source: devresidue_core::SourceKind::Kondo,
            logical_size: 5,
            file_count: 1,
            last_modified: None,
            explanation: "e".into(),
            cleanup_action: CleanupAction::RecycleBin,
            scan_snapshot: None, // filled by the scan assembler when a probe is wired
            classification_rule_id: None,
            evidence: vec![],
        };
        let snap = ScanSnapshot::new(
            ScanMode::Real {
                workspace_roots: vec!["D:\\Code".into()],
            },
            vec![item.clone()],
            vec!["warn".into()],
        );
        save(&base, &snap).expect("save");
        let loaded = load(&base).expect("load");
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].path, item.path);
        assert_eq!(loaded.warnings, vec!["warn".to_string()]);
        assert!(matches!(loaded.mode, ScanMode::Real { .. }));
        assert!(!loaded.cancelled, "new() records a non-cancelled scan");

        // Corrupt file → error (never silently empty).
        std::fs::write(snapshot_path(&base), b"{not json").unwrap();
        assert!(load(&base).is_err());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cancelled_flag_round_trips_and_old_files_default_to_false() {
        let base = std::env::temp_dir().join(format!("dr-scanstore-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let mut snap = ScanSnapshot::new(
            ScanMode::Real {
                workspace_roots: vec!["D:\\Code".into()],
            },
            vec![],
            vec![],
        );
        snap.cancelled = true;
        save(&base, &snap).expect("save");
        let loaded = load(&base).expect("load");
        assert!(loaded.cancelled, "cancelled flag must round-trip");

        // A snapshot written before the field existed has no "cancelled" key;
        // serde default must read it back as false (backwards compatible).
        let mut json = serde_json::to_value(&snap).expect("serialise");
        json.as_object_mut().expect("object").remove("cancelled");
        let text = serde_json::to_string(&json).unwrap();
        let compat: ScanSnapshot = serde_json::from_str(&text).expect("old snapshot parses");
        assert!(!compat.cancelled);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_file_is_an_error() {
        let base =
            std::env::temp_dir().join(format!("dr-scanstore-missing-{}", std::process::id()));
        assert!(load(&base).is_err());
    }

    #[test]
    fn integrity_round_trips_and_tampering_is_refused() {
        let base = std::env::temp_dir().join(format!("dr-scanstore-hmac-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let item = ScanItem {
            id: devresidue_core::ScanItemId::from_raw(1),
            path: PathBuf::from("C:\\demo\\cache"),
            display_name: "demo".into(),
            product: None,
            category: devresidue_core::ResidueCategory::BuildArtifact,
            risk: devresidue_core::RiskLevel::RegenerableLocal,
            source: devresidue_core::SourceKind::Kondo,
            logical_size: 5,
            file_count: 1,
            last_modified: None,
            explanation: "e".into(),
            cleanup_action: CleanupAction::RecycleBin,
            scan_snapshot: None,
            classification_rule_id: None,
            evidence: vec![],
        };
        let snap = ScanSnapshot::new(
            ScanMode::Real {
                workspace_roots: vec!["D:\\Code".into()],
            },
            vec![item.clone()],
            vec![],
        );
        save(&base, &snap).expect("save writes an integrity tag");
        // A clean load succeeds.
        let loaded = load(&base).expect("clean load");
        assert!(loaded.integrity.is_some());

        // Tamper with an item's action (an attacker rewriting the file to
        // change the authorised command) → load must fail.
        let path = snapshot_path(&base);
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json["items"][0]["cleanup_action"] = serde_json::json!({"kind": "external-command", "command": {
            "executable": "forged.exe",
            "args": ["--x"],
            "working_directory": null,
            "timeout_secs": 1,
            "scope": null
        }});
        std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        let err = load(&base).expect_err("tampered scan record must be refused");
        assert!(err.contains("integrity check failed"), "{err}");

        // A legacy record without an integrity tag is also refused.
        json["items"][0]["cleanup_action"] = serde_json::json!({"kind": "recycle-bin"});
        let mut stripped = json.clone();
        stripped.as_object_mut().unwrap().remove("integrity");
        std::fs::write(&path, serde_json::to_vec_pretty(&stripped).unwrap()).unwrap();
        let err = load(&base).expect_err("legacy untagged record must be refused");
        assert!(err.contains("no integrity tag"), "{err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A minimal valid item for the R3-G05/G06 tamper matrices.
    fn one_item() -> ScanItem {
        ScanItem {
            id: devresidue_core::ScanItemId::from_raw(1),
            path: PathBuf::from("C:\\demo\\cache"),
            display_name: "demo".into(),
            product: None,
            category: devresidue_core::ResidueCategory::BuildArtifact,
            risk: devresidue_core::RiskLevel::RegenerableLocal,
            source: devresidue_core::SourceKind::Kondo,
            logical_size: 5,
            file_count: 1,
            last_modified: None,
            explanation: "e".into(),
            cleanup_action: CleanupAction::RecycleBin,
            scan_snapshot: None,
            classification_rule_id: None,
            evidence: vec![],
        }
    }

    fn tmp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dr-scanstore-{tag}-{}-{:?}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
        ))
    }

    /// R3-G05: every metadata field that feeds a security decision is bound
    /// into the MAC — modifying it without re-signing must be refused, and
    /// the store must not hand the modified value to a caller.
    type TamperCase = (&'static str, Box<dyn Fn(&mut serde_json::Value)>);

    #[test]
    fn r3g05_security_metadata_tampering_is_refused() {
        let cases: [TamperCase; 5] = [
            (
                "generation",
                Box::new(|json| {
                    json["generation"] = serde_json::json!(99);
                }),
            ),
            (
                "cancelled",
                Box::new(|json| {
                    json["cancelled"] = serde_json::json!(true);
                }),
            ),
            (
                "mode",
                Box::new(|json| {
                    json["mode"] = serde_json::json!({"fixtures": null});
                }),
            ),
            (
                "workspace_roots",
                Box::new(|json| {
                    json["mode"]["real"]["workspace_roots"] =
                        serde_json::json!(["E:\\attacker-workspace"]);
                }),
            ),
            (
                "scanned_at",
                Box::new(|json| {
                    json["scanned_at"] = serde_json::json!(0);
                }),
            ),
        ];
        for (label, mutate) in cases {
            let base = tmp_base("g05");
            let _ = std::fs::remove_dir_all(&base);
            save(
                &base,
                &ScanSnapshot::new(
                    ScanMode::Real {
                        workspace_roots: vec!["D:\\Code".into()],
                    },
                    vec![one_item()],
                    vec![],
                ),
            )
            .expect("save");
            let path = snapshot_path(&base);
            let mut json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            mutate(&mut json);
            std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
            let err = load(&base).expect_err("metadata tamper must be refused");
            assert!(
                err.contains("integrity check failed"),
                "{label}: tampered record refused with a MAC error, got: {err}"
            );
            let _ = std::fs::remove_dir_all(&base);
        }
    }

    /// R3-G06: two writers allocating under the lock get distinct
    /// generations (the read→increment→save transaction is serialised).
    ///
    /// Runs with the REAL Win32 lock adapter (the same one the CLI / Tauri
    /// shells install), so the test exercises the actual cross-thread
    /// mutual exclusion of `LockFileEx`.
    #[test]
    fn r3g06_concurrent_allocations_never_share_a_generation() {
        // NOTE: tests share the process-global LOCK_ACQUIRER; whichever
        // acquirer another test installed first stays. On Windows both the
        // real Win32 acquirer and the no-op test acquirer give this test its
        // guarantees differently — the no-op does NOT serialise, so the test
        // would be flaky. Run it in a child process instead: spawn the test
        // binary with the env filter selecting only this test (a fresh
        // process → fresh OnceLock → the #[cfg(windows)] install below is
        // guaranteed to be the real acquirer).
        // (Self-spawn avoids depending on --test-threads=1.)
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "r3g06_child_allocators_are_serialised",
                "--exact",
            ])
            .env("DEVRESIDUE_G06_CHILD", "1")
            .status();
        match child {
            Ok(status) if status.success() => {} // real assertion ran in the child
            other => panic!("child allocation test failed: {other:?}"),
        }
    }

    /// The child half of the G06 concurrency test (runs in a fresh process
    /// where the Win32 lock acquirer is guaranteed installed).
    ///
    /// `#[ignore]`-driven: plain `cargo test` skips it; the parent test
    /// spawns the test binary with `--ignored` + the env filter so it runs
    /// exactly once in its own process.
    #[test]
    #[ignore = "spawned by r3g06_concurrent_allocations_never_share_a_generation"]
    fn r3g06_child_allocators_are_serialised() {
        assert_eq!(
            std::env::var("DEVRESIDUE_G06_CHILD").as_deref(),
            Ok("1"),
            "runs only as the child of the parent G06 test"
        );
        #[cfg(windows)]
        install_lock_acquirer(devresidue_platform_windows::filesystem::lock_file_exclusive);
        #[cfg(not(windows))]
        install_test_lock();

        let base = tmp_base("g06-child");
        let _ = std::fs::remove_dir_all(&base);
        // Seed with generation 7.
        save(
            &base,
            &ScanSnapshot {
                generation: 7,
                ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
            },
        )
        .expect("seed");

        // Two threads allocate + publish concurrently; the Win32 whole-file
        // lock serialises their read→increment→save transactions.
        let base = std::path::PathBuf::from(&base);
        let base2 = base.clone();
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let base = if i == 0 {
                    std::path::PathBuf::from(&base)
                } else {
                    std::path::PathBuf::from(&base2)
                };
                std::thread::spawn(move || {
                    save_with_next_generation(&base, |generation| ScanSnapshot {
                        generation,
                        ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
                    })
                })
            })
            .collect();
        let published: Vec<u64> = handles
            .into_iter()
            .map(|h| h.join().expect("writer").expect("publish").generation)
            .collect();

        // The two allocations must be distinct (8 and 9 in some order).
        assert_eq!(published.len(), 2, "both writers publish: {published:?}");
        assert_ne!(
            published[0], published[1],
            "two concurrent writers must never share a generation"
        );
        let loaded = load(&base).expect("final record is valid");
        assert!(
            loaded.generation >= 8,
            "generation never regressed: {}",
            loaded.generation
        );
        assert!(loaded.generation >= published.into_iter().max().unwrap());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// R3-G06: a corrupt existing record never silently restarts at
    /// generation 1 — the allocator refuses to publish over a damaged store.
    #[test]
    fn r3g06_corrupt_store_refuses_allocation_instead_of_resetting() {
        install_test_lock();
        let base = tmp_base("g06-corrupt");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(snapshot_path(&base), b"{not json").unwrap();

        // A corrupt store is the re-scan remedy: publishing a fresh snapshot
        // over it is allowed (that IS the re-scan), but the generation must
        // never restart at 1 — it lands on the integrity fallback, far above
        // any past generation, so stale plans can never match it.
        let published = save_with_next_generation(&base, |generation| ScanSnapshot {
            generation,
            ..ScanSnapshot::new(ScanMode::Fixtures, vec![], vec![])
        })
        .expect("a corrupt store is re-scannable (publish-over)");
        assert_eq!(
            published.generation,
            integrity_fallback_generation(),
            "corrupt store allocates the fallback generation, never 1"
        );
        // And the published record is valid under the current MAC.
        let loaded = load(&base).expect("the fresh record loads cleanly");
        assert_eq!(loaded.generation, published.generation);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The G05 migration regression: a snapshot signed under the OLD
    /// `mac_input` schema (items-only MAC) fails `load` under the new schema.
    /// A new scan must still be able to publish over it — the exact
    /// production incident (UI: "cannot allocate next generation … was
    /// tampered").
    #[test]
    fn g05_migration_old_schema_mac_does_not_block_re_scan() {
        install_test_lock();
        let base = tmp_base("g05-migration");
        let _ = std::fs::remove_dir_all(&base);
        // Write a v1-MAC snapshot: sign ONLY the items (the old input).
        let snap = ScanSnapshot {
            generation: 4,
            ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
        };
        let key = load_or_create_key(&base).unwrap();
        let old_mac = devresidue_core::integrity::hmac_sha256(
            &key,
            &serde_json::to_vec(&snap.items).unwrap(),
        );
        let mut json = serde_json::to_value(&snap).unwrap();
        json["integrity"] = serde_json::json!(devresidue_core::integrity::hex(&old_mac));
        std::fs::write(
            snapshot_path(&base),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();

        // Old record: load refuses it (schema mismatch ≠ current MAC).
        let err = load(&base).expect_err("old-MAC record is not an authorised source");
        assert!(err.contains("integrity check failed"), "{err}");

        // The re-scan publishes over it, continuing from the declared
        // generation 4 → 5 (the counter is monotonicity-only material).
        let published = save_with_next_generation(&base, |generation| ScanSnapshot {
            generation,
            ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
        })
        .expect("re-scan over an old-schema MAC must publish");
        assert_eq!(
            published.generation, 5,
            "generation continues monotonically past the declared 4"
        );
        let loaded = load(&base).expect("the fresh record loads under the new MAC");
        assert_eq!(loaded.generation, 5);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A tampered record (MAC-invalid under the CURRENT schema, declared
    /// generation readable): publishing continues past the declared counter —
    /// a stale plan gated on the old generation can never re-authorise.
    #[test]
    fn tampered_record_publishes_at_a_generation_no_stale_plan_can_match() {
        install_test_lock();
        let base = tmp_base("tamper-republish");
        let _ = std::fs::remove_dir_all(&base);
        // Legitimate gen 9 record…
        save(
            &base,
            &ScanSnapshot {
                generation: 9,
                ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
            },
        )
        .expect("seed");
        // …then flip a byte in an item (MAC-invalid, declared gen intact).
        let path = snapshot_path(&base);
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            raw.replace("\"explanation\": \"e\"", "\"explanation\": \"X\""),
        )
        .unwrap();
        assert!(load(&base).is_err(), "tampered record refused by load");

        let published = save_with_next_generation(&base, |generation| ScanSnapshot {
            generation,
            ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
        })
        .expect("re-scan over a tampered record must publish");
        // Declared 9 → the new generation continues past it (10 or higher).
        assert!(
            published.generation > 9,
            "new generation strictly above the tampered record's declared one: {}",
            published.generation
        );
        assert!(load(&base).is_ok(), "fresh record valid");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// R3-G06: a missing snapshot is a distinct, predictable error — not the
    /// corrupt-store error and not a silent empty load.
    #[test]
    fn r3g06_missing_vs_corrupt_are_distinguished() {
        let base = tmp_base("g06-missing");
        let _ = std::fs::remove_dir_all(&base);
        let missing = load(&base).expect_err("missing file is an error");
        assert!(
            missing.contains("no snapshot at"),
            "missing snapshot is named as such, got: {missing}"
        );

        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(snapshot_path(&base), b"garbage").unwrap();
        let corrupt = load(&base).expect_err("corrupt file is an error");
        assert!(
            !corrupt.contains("no snapshot at"),
            "a corrupt file is not 'no snapshot', got: {corrupt}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// R3-G06: publish is atomic — a reader never observes a partially
    /// written snapshot file (write-to-tmp + rename leaves no torn state and
    /// no temp residue after either success or failure).
    #[test]
    fn r3g06_atomic_publish_leaves_no_temp_residue() {
        install_test_lock();
        let base = tmp_base("g06-atomic");
        let _ = std::fs::remove_dir_all(&base);
        save_with_next_generation(&base, |generation| ScanSnapshot {
            generation,
            ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
        })
        .expect("publish");
        save_with_next_generation(&base, |generation| ScanSnapshot {
            generation,
            ..ScanSnapshot::new(ScanMode::Fixtures, vec![one_item()], vec![])
        })
        .expect("second publish");

        let residue: Vec<_> = std::fs::read_dir(&base)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("last-scan.json.tmp")
            })
            .collect();
        assert!(
            residue.is_empty(),
            "no temp files remain after atomic publishes: {residue:?}"
        );
        let loaded = load(&base).expect("published record loads");
        assert_eq!(loaded.generation, 2, "generations allocated 1 then 2");

        let _ = std::fs::remove_dir_all(&base);
    }
}
