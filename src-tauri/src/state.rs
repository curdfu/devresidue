//! App state — the model behind every command.
//!
//! One process-wide [`AppModel`] is shared through a `tauri::State`
//! ([`AppState`]). It holds:
//!
//! - the DevResidue data directory (same override logic as the CLI,
//!   `DEVRESIDUE_DATA_DIR`, used by tests to point at a temp dir);
//! - the most recent [`ScanSnapshot`] (memory cache + the on-disk
//!   `last-scan.json` via `scan_store`);
//! - the *active* scan handle (id + cancellation flag). Scan runs never block
//!   a command: `scan` registers here, spawns a worker thread, and
//!   `cancel_scan` flips the flag the providers poll.
//!
//! Only ids cross this boundary (INV-013 / SPEC §20): scan ids minted here,
//! scan-item ids and plan ids owned by the core. No command ever accepts a
//! path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use devresidue_providers::scan_store::{self, ScanSnapshot};

use crate::support;

/// A running scan: its opaque handle and the cancellation flag providers poll.
#[derive(Debug)]
pub struct ActiveScan {
    pub scan_id: u64,
    pub cancel: Arc<AtomicBool>,
}

/// The whole application model (behind a `Mutex`).
#[derive(Debug)]
pub struct AppModel {
    pub data_dir: PathBuf,
    /// Latest finished scan (also persisted to `last-scan.json`).
    latest: Option<ScanSnapshot>,
    /// The currently running scan, if any (at most one at a time).
    active: Option<ActiveScan>,
    /// Monotonic scan-id allocator.
    next_scan_id: u64,
}

impl AppModel {
    /// Opens the model at the data directory, seeding the latest snapshot from
    /// disk when one exists (a plan/clean run from an earlier session stays
    /// usable).
    pub fn open() -> Self {
        let data_dir = support::data_dir().expect("resolve data directory");
        Self::open_at(data_dir)
    }

    /// Opens the model at an explicit data directory (tests).
    pub fn open_at(data_dir: PathBuf) -> Self {
        // A corrupt/missing last-scan file is not fatal at startup: `scan`
        // writes a fresh one; the UI surfaces "no scan yet" instead.
        let latest = scan_store::load(&data_dir).ok();
        Self {
            data_dir,
            latest,
            active: None,
            next_scan_id: 0,
        }
    }

    /// Registers a new scan and returns `(scan_id, cancel_flag)`.
    ///
    /// `Err` when another scan is still running (a single-writer model: the
    /// snapshot store holds one file).
    pub fn begin_scan(&mut self) -> Result<(u64, Arc<AtomicBool>), String> {
        if let Some(active) = &self.active {
            return Err(format!(
                "a scan is already running (scan id {})",
                active.scan_id
            ));
        }
        self.next_scan_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.active = Some(ActiveScan {
            scan_id: self.next_scan_id,
            cancel: Arc::clone(&cancel),
        });
        Ok((self.next_scan_id, cancel))
    }

    /// Attempts to cancel the scan with `scan_id`. Returns whether a matching
    /// scan was running (and is now flagged for cancellation).
    pub fn cancel_scan(&mut self, scan_id: u64) -> bool {
        match &self.active {
            Some(active) if active.scan_id == scan_id => {
                active.cancel.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    /// Whether the scan with `scan_id` is still the registered active one.
    #[cfg(test)]
    pub fn is_active(&self, scan_id: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.scan_id == scan_id)
    }

    /// Records a finished scan (partial results allowed) as the latest and
    /// clears the active handle (only if it still refers to `scan_id`).
    pub fn finish_scan(&mut self, scan_id: u64, snapshot: ScanSnapshot) {
        if self.active.as_ref().is_some_and(|a| a.scan_id == scan_id) {
            self.active = None;
        }
        self.latest = Some(snapshot);
    }

    /// Terminal state for a scan that failed before producing a snapshot
    /// (R10): clears the active handle so a later scan may start, but does
    /// **not** touch `latest` — a failed scan must never masquerade as the
    /// newest finished result.
    pub fn fail_scan(&mut self, scan_id: u64) {
        if self.active.as_ref().is_some_and(|a| a.scan_id == scan_id) {
            self.active = None;
        }
    }

    /// The most recent finished scan snapshot.
    pub fn latest(&self) -> Option<&ScanSnapshot> {
        self.latest.as_ref()
    }

    /// Drops the latest snapshot in memory (the `clear_all_data` command's
    /// final step — after the persisted state was wiped, the in-memory
    /// "latest" must not keep serving it). The active-scan handle is left
    /// alone: a wipe during a running scan keeps the scan's own lifecycle
    /// intact, and `finish_scan` will re-seed `latest` when it completes.
    pub fn reset(&mut self) {
        self.latest = None;
    }

    /// The data directory (plans/ + journal/ + last-scan.json).
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }
}

/// The managed Tauri state handle (cheap to clone: an `Arc` to the model).
#[derive(Debug, Clone)]
pub struct AppState {
    pub model: Arc<Mutex<AppModel>>,
}

impl AppState {
    /// Builds managed state over a freshly opened model.
    pub fn new() -> Self {
        Self {
            model: Arc::new(Mutex::new(AppModel::open())),
        }
    }

    /// Test hook: managed state over an explicit data directory.
    #[cfg(test)]
    pub fn at(data_dir: PathBuf) -> Self {
        Self {
            model: Arc::new(Mutex::new(AppModel::open_at(data_dir))),
        }
    }
}
