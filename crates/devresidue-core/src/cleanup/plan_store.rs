//! Plan persistence — `%LOCALAPPDATA%\DevResidue\plans\<id>.json` (SPEC §16
//! "plan must survive the process that created it").
//!
//! - ids are allocated from a persisted high-water counter (`_next_id`), so a
//!   restart never re-uses an id of an older plan;
//! - writes are serialised through the domain [`CleanupPlan`] DTO only;
//! - a plan that cannot be persisted is never handed out in-memory (fail
//!   closed): the planner refuses instead of degrading.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::domain::ids::CleanupPlanId;
use crate::domain::plan::CleanupPlan;
use crate::journal::default_data_dir;

/// Storage errors (message-only: surfaced to CLI users as plain text).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plan store failure: {0}")]
pub struct PlanStoreError(pub String);

/// How long a stale lock file may live before it is assumed dead (a crashed
/// writer leaves the file behind; after this window we break it).
const STALE_LOCK_SECS: u64 = 60;
/// Busy-wait budget for acquiring the inter-process lock (generous so a
/// heavily loaded CI box does not spuriously fail legitimate contention).
const LOCK_ACQUIRE_ATTEMPTS: u32 = 2000;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);

/// The plans directory under the data root.
pub fn plans_dir(base: &Path) -> PathBuf {
    base.join("plans")
}

/// Opens the plans store rooted at `<base>/plans`, creating the directory.
pub struct PlanStore {
    dir: PathBuf,
}

/// A held inter-process lock (removed on drop). `create_new(true)` guarantees
/// at-most-one holder per directory across threads *and* processes (F16).
struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl PlanStore {
    /// Opens the store at the default DevResidue data directory.
    pub fn open_default() -> Result<Self, PlanStoreError> {
        let base = default_data_dir()
            .map_err(|e| PlanStoreError(format!("locate data directory: {e}")))?;
        Self::open_at(&base)
    }

    /// Opens the store under `base_dir/plans` (idempotent mkdir).
    pub fn open_at(base_dir: &Path) -> Result<Self, PlanStoreError> {
        let dir = plans_dir(base_dir);
        fs::create_dir_all(&dir)
            .map_err(|e| PlanStoreError(format!("create {}: {e}", dir.display())))?;
        Ok(Self { dir })
    }

    /// Tests: point straight at a directory.
    pub fn open_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The directory holding the plan JSON files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn counter_path(&self) -> PathBuf {
        self.dir.join("_next_id")
    }

    /// Acquires the inter-process store lock (busy-waits on contention, breaks
    /// a stale lock older than [`STALE_LOCK_SECS`]).
    ///
    /// On Windows a concurrent `create_new` can surface as `AccessDenied`
    /// (os error 5) instead of `AlreadyExists` when the winner is mid-hold or
    /// mid-release; both are treated as contention and retried.
    fn lock(&self) -> Result<StoreLock, PlanStoreError> {
        let path = self.dir.join(".lock");
        for attempt in 0..LOCK_ACQUIRE_ATTEMPTS {
            let contention = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(StoreLock { path }),
                Err(err) => {
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::AlreadyExists
                            | std::io::ErrorKind::PermissionDenied
                            | std::io::ErrorKind::WouldBlock
                    ) {
                        true
                    } else {
                        return Err(PlanStoreError(format!(
                            "acquire lock {}: {err}",
                            path.display()
                        )));
                    }
                }
            };
            if contention {
                if is_stale(&path) {
                    let _ = fs::remove_file(&path);
                }
                if attempt + 1 == LOCK_ACQUIRE_ATTEMPTS {
                    return Err(PlanStoreError(format!(
                        "plans store is busy: {}",
                        path.display()
                    )));
                }
                thread::sleep(LOCK_RETRY_DELAY);
            }
        }
        Err(PlanStoreError(format!(
            "plans store is busy: {}",
            path.display()
        )))
    }

    /// Allocates a fresh plan id (persisted high-water counter). Must be
    /// called while holding the store lock.
    fn allocate_id(&self) -> Result<CleanupPlanId, PlanStoreError> {
        let next: u64 = match fs::read_to_string(self.counter_path()) {
            Ok(text) => text.trim().parse().unwrap_or(1),
            Err(_) => 1,
        };
        let id = CleanupPlanId::from_raw(next);
        fs::write(self.counter_path(), (next + 1).to_string().as_bytes())
            .map_err(|e| PlanStoreError(format!("write {}: {e}", self.counter_path().display())))?;
        Ok(id)
    }

    /// Assigns an id to `plan` and writes `<id>.json`.
    ///
    /// Id allocation and the plan-file write happen under one lock so
    /// concurrent writers (threads or processes sharing the directory) never
    /// hand out the same id twice (F16).
    pub fn save(&self, plan: &mut CleanupPlan) -> Result<CleanupPlanId, PlanStoreError> {
        let _guard = self.lock()?;
        let id = self.allocate_id()?;
        plan.id = id;
        let path = self.dir.join(format!("{}.json", id.raw()));
        let json =
            serde_json::to_vec(plan).map_err(|e| PlanStoreError(format!("serialise plan: {e}")))?;
        let mut file = fs::File::create(&path)
            .map_err(|e| PlanStoreError(format!("create {}: {e}", path.display())))?;
        file.write_all(&json)
            .and_then(|()| file.flush())
            .map_err(|e| PlanStoreError(format!("write {}: {e}", path.display())))?;
        Ok(id)
    }

    /// Loads a persisted plan by id.
    pub fn load(&self, id: CleanupPlanId) -> Result<CleanupPlan, PlanStoreError> {
        let path = self.dir.join(format!("{}.json", id.raw()));
        let bytes =
            fs::read(&path).map_err(|e| PlanStoreError(format!("read {}: {e}", path.display())))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| PlanStoreError(format!("parse {}: {e}", path.display())))
    }

    /// Removes only persisted plan files owned by this store. The counter and
    /// lock are intentionally retained so a reset cannot re-use plan IDs.
    pub fn clear_plans(&self) -> Result<usize, PlanStoreError> {
        let _guard = self.lock()?;
        let entries = fs::read_dir(&self.dir)
            .map_err(|e| PlanStoreError(format!("read {}: {e}", self.dir.display())))?;
        let mut removed = 0;
        for entry in entries {
            let entry = entry
                .map_err(|e| PlanStoreError(format!("plan dir entry: {e}")))?;
            let file_type = entry
                .file_type()
                .map_err(|e| PlanStoreError(format!("inspect {}: {e}", entry.path().display())))?;
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".json") else { continue };
            if stem.is_empty() || !stem.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            fs::remove_file(entry.path()).map_err(|e| {
                PlanStoreError(format!(
                    "remove plan {} after {removed} files: {e}",
                    entry.path().display()
                ))
            })?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// Whether a lock file is older than the stale window (a crash orphan).
fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age.as_secs() > STALE_LOCK_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("dr-planlock-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        base
    }

    fn sample_plan(id: CleanupPlanId) -> CleanupPlan {
        CleanupPlan {
            id,
            created_at: SystemTime::now(),
            dry_run: None,
            scan_generation: 0,
            items: vec![],
        }
    }

    #[test]
    fn ten_concurrent_saves_produce_ten_distinct_ids() {
        // F16: concurrent writers (each with its own PlanStore handle over the
        // same directory) must never receive the same plan id.
        let base = tmp_base("concurrent");
        let store = PlanStore::open_at(&base).expect("open");
        // Seed the counter at a known value.
        fs::write(store.counter_path(), b"1").unwrap();

        let dir = store.dir().to_path_buf();
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let dir = dir.clone();
                thread::spawn(move || {
                    let store = PlanStore::open_dir(dir);
                    let mut plan = sample_plan(CleanupPlanId::from_raw(0));
                    store.save(&mut plan).expect("save")
                })
            })
            .collect();

        let mut ids: Vec<u64> = Vec::new();
        for handle in handles {
            ids.push(handle.join().expect("thread").raw());
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 10, "ids must be distinct: {ids:?}");
        assert_eq!(ids[0], 1);
        assert_eq!(ids[9], 10);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn save_releases_the_lock_file() {
        // After a successful save the lock file must be gone (guards clean up
        // on drop), so later writers are not blocked by a ghost lock.
        let base = tmp_base("lock-release");
        let store = PlanStore::open_at(&base).expect("open");
        let mut plan = sample_plan(CleanupPlanId::from_raw(0));
        let id = store.save(&mut plan).expect("save");
        assert_eq!(id.raw(), 1);
        assert!(
            !store.dir().join(".lock").exists(),
            "lock file must be removed after the guarded write"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn stale_lock_predicate_detects_old_files() {
        // The predicate used to break crashed writers: an mtime far in the
        // past reads as stale, a fresh file does not.
        let base = tmp_base("stale-pred");
        std::fs::create_dir_all(&base).unwrap();
        let fresh = base.join("fresh.lock");
        fs::write(&fresh, b"").unwrap();
        assert!(!is_stale(&fresh), "a fresh lock is not stale");

        // std cannot backdate mtimes portably; exercise the "broken file"
        // branch instead: a lock whose metadata is unreadable counts as
        // non-stale (safe default — we never break an ambiguous lock).
        assert!(!is_stale(&base.join("missing.lock")));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn clear_plans_removes_only_numeric_json_and_keeps_counter() {
        let base = tmp_base("clear-plans");
        let store = PlanStore::open_at(&base).expect("open");
        let mut plan = sample_plan(CleanupPlanId::from_raw(0));
        store.save(&mut plan).expect("save");
        fs::write(store.dir().join("notes.json"), b"keep").unwrap();
        fs::write(store.dir().join("2.tmp"), b"keep").unwrap();
        let removed = store.clear_plans().expect("clear");
        assert_eq!(removed, 1);
        assert!(store.dir().join("_next_id").is_file());
        assert!(store.dir().join("notes.json").is_file());
        assert!(store.dir().join("2.tmp").is_file());
        let mut next = sample_plan(CleanupPlanId::from_raw(0));
        let id = store.save(&mut next).expect("save after clear");
        assert_eq!(id.raw(), 2);
        let _ = fs::remove_dir_all(&base);
    }
}
