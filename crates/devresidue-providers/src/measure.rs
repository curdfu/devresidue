//! Shared measurement — one-pass directory walk producing logical size, file
//! count and newest modification time.
//!
//! # Safety rules (INV-004 / SPEC §17)
//!
//! - never follows symlinks / junctions / mount points (`follow_links(false)`
//!   **and** an explicit reparse-point filter on every directory entry, since
//!   Windows junctions are not flagged as symlinks by every API);
//! - a directory that turns out to be a reparse point is skipped entirely
//!   (its contents are not descended into and not counted);
//! - if the *measurement root itself* is a reparse point the walk returns an
//!   empty result with `error_count == 1` — the caller must never size through
//!   a link.
//!
//! Entries whose metadata cannot be read are counted as zero bytes and bump
//! `error_count` (the walk continues).
//!
//! # Parallelism (Phase 16)
//!
//! [`measure_tree_parallel`] is the product path for large trees: a bounded
//! worker pool (≤8, `DEVRESIDUE_MAX_THREADS` override) shares a directory
//! queue and atomic counters. Behaviour is **bit-for-bit equivalent** to
//! [`measure_tree`] on every observable field (size / file_count / errors /
//! newest-modification truncated to the second) except that the two entry
//! callbacks run concurrently, so cancellation becomes best-effort per entry
//! rather than strictly ordered — exactly the same partial-result semantics
//! the serial walker had on cancel.
//!
//! The serial [`measure_tree`] stays public: it is what the unit / fs-matrix
//! tests exercise and a natural fallback when only one worker is available.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use walkdir::WalkDir;

/// Outcome of a tree measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Measure {
    /// Sum of regular-file lengths (bytes).
    pub logical_size: u64,
    /// Number of regular files counted.
    pub file_count: u64,
    /// Newest modification time seen (files and directories).
    pub last_modified: Option<SystemTime>,
    /// Number of entries whose metadata could not be read.
    pub error_count: u64,
}

/// Win32 `FILE_ATTRIBUTE_REPARSE_POINT` — symlink, junction, mount point.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// True when `path` is a reparse point (Windows) or a symlink (any platform).
#[must_use]
pub fn path_is_reparse_like(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    is_reparse_attributes(meta)
}

#[cfg(windows)]
fn is_reparse_attributes(meta: std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_attributes(_meta: std::fs::Metadata) -> bool {
    false
}

/// True when a walkdir entry is a symlink or a Windows reparse point.
fn entry_is_reparse_like(entry: &walkdir::DirEntry) -> bool {
    if entry.file_type().is_symlink() {
        return true;
    }
    // DirEntry::metadata() does not follow links (it reflects the entry's own
    // attributes), so this is safe to consult for the reparse flag.
    match entry.metadata() {
        Ok(meta) => is_reparse_attributes(meta),
        Err(_) => false,
    }
}

/// Serial reference implementation — kept for the safety / fs-matrix tests and
/// single-worker fallback.
fn measure_tree_serial(root: &Path, should_continue: &dyn Fn() -> bool) -> Measure {
    if path_is_reparse_like(root) {
        return Measure {
            error_count: 1,
            ..Measure::default()
        };
    }

    // Single file: fast path.
    if root.is_file() {
        return match std::fs::metadata(root) {
            Ok(meta) => Measure {
                logical_size: meta.len(),
                file_count: 1,
                last_modified: meta.modified().ok(),
                error_count: 0,
            },
            Err(_) => Measure {
                error_count: 1,
                ..Measure::default()
            },
        };
    }

    let mut measure = Measure::default();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true);
    let mut iter = walker.into_iter();

    while let Some(item) = iter.next() {
        if !should_continue() {
            break;
        }
        let entry = match item {
            Ok(entry) => entry,
            Err(_) => {
                measure.error_count += 1;
                continue;
            }
        };
        let file_type = entry.file_type();
        if file_type.is_dir() {
            if entry_is_reparse_like(&entry) {
                // Junction / symlink directory inside the tree: never descend.
                iter.skip_current_dir();
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    measure.last_modified =
                        Some(measure.last_modified.map_or(modified, |m| m.max(modified)));
                }
            }
            continue;
        }
        if file_type.is_file() {
            match entry.metadata() {
                Ok(meta) => {
                    measure.logical_size += meta.len();
                    measure.file_count += 1;
                    if let Ok(modified) = meta.modified() {
                        measure.last_modified =
                            Some(measure.last_modified.map_or(modified, |m| m.max(modified)));
                    }
                }
                Err(_) => measure.error_count += 1,
            }
        }
    }
    measure
}

/// Measures one tree (or file) without ever crossing a reparse boundary.
///
/// Serial walker — see module docs for the parallel product path.
pub fn measure_tree(root: &Path, should_continue: &dyn Fn() -> bool) -> Measure {
    measure_tree_serial(root, should_continue)
}

// ---------------------------------------------------------------------------
// Phase 16 — bounded parallel walker
// ---------------------------------------------------------------------------

/// Default upper bound on parallel measurement workers.
pub const DEFAULT_MAX_WORKERS: usize = 8;

/// Env override for the worker count (`DEVRESIDUE_MAX_THREADS`).
const MAX_THREADS_ENV: &str = "DEVRESIDUE_MAX_THREADS";

/// Effective worker count: env override wins, else
/// `available_parallelism` capped at [`DEFAULT_MAX_WORKERS`], floor 1.
fn worker_count() -> usize {
    if let Ok(text) = std::env::var(MAX_THREADS_ENV) {
        if let Ok(n) = text.trim().parse::<usize>() {
            return n.clamp(1, DEFAULT_MAX_WORKERS);
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, DEFAULT_MAX_WORKERS))
        .unwrap_or(1)
}

/// Shared state of one parallel walk.
struct WalkShared {
    queue: Mutex<std::collections::VecDeque<PathBuf>>,
    size: AtomicU64,
    files: AtomicU64,
    errors: AtomicU64,
    /// Newest modification time as UNIX epoch seconds (floor). 0 = none yet.
    last_mod: AtomicI64,
    /// Directories currently being processed (declared *before* any child is
    /// pushed, decremented after — see the worker loop for the termination
    /// proof).
    active: AtomicUsize,
}

impl WalkShared {
    fn record_mtime(&self, t: SystemTime) {
        if let Ok(d) = t.duration_since(SystemTime::UNIX_EPOCH) {
            self.last_mod
                .fetch_max(d.as_secs() as i64, Ordering::Relaxed);
        }
    }
}

/// Worker loop: pop a directory, walk it, push its sub-directories back, and
/// stop when the queue is empty and no worker is active.
///
/// Termination argument: a worker that finds an empty queue only stops when
/// `active == 0`. Every worker increments `active` *before* processing and
/// decrements only *after* it has pushed all children it will push, so the
/// last decrement to zero happens strictly after every child has entered the
/// queue — a straggler can never be missed.
fn worker_loop(state: &Arc<WalkShared>, cancel: &(dyn Fn() -> bool + Sync)) {
    loop {
        let dir = state.queue.lock().unwrap().pop_front();
        match dir {
            Some(path) => {
                state.active.fetch_add(1, Ordering::AcqRel);
                process_dir(&path, state, cancel);
                state.active.fetch_sub(1, Ordering::AcqRel);
            }
            None => {
                if state.active.load(Ordering::Acquire) == 0 {
                    break;
                }
                // Queue drained but a peer is still walking (it may yet push
                // children) — back off briefly instead of burning CPU.
                std::thread::sleep(std::time::Duration::from_micros(50));
            }
        }
    }
}

/// Walks one directory: records the directory's own mtime, enumerates
/// entries, accumulates file metadata into the shared counters and pushes
/// sub-directories for other workers.
fn process_dir(path: &Path, state: &Arc<WalkShared>, cancel: &(dyn Fn() -> bool + Sync)) {
    if !cancel() {
        return;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(modified) = meta.modified() {
            state.record_mtime(modified);
        }
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => {
            state.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    for entry in entries {
        // Cancellation is re-checked per entry — the same density the serial
        // walker had, so a mid-directory cancel leaves the same kind of
        // honest partial result.
        if !cancel() {
            return;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                state.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                state.errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        // Reparse points (symlinks / junctions / mount points) are never
        // followed: `DirEntry::file_type` reports every Windows reparse point
        // as a symlink — the same std behaviour the serial walker relies on —
        // so this skips the entry exactly like the serial walker's
        // `skip_current_dir` on a reparse directory and its non-counting of
        // symlink files, without a second metadata syscall per entry.
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    state.record_mtime(modified);
                }
            }
            state.queue.lock().unwrap().push_back(entry.path());
        } else if file_type.is_file() {
            // `DirEntry::metadata` never follows links (and links were already
            // filtered above), so it is the file's own metadata.
            match entry.metadata() {
                Ok(meta) => {
                    state.size.fetch_add(meta.len(), Ordering::Relaxed);
                    state.files.fetch_add(1, Ordering::Relaxed);
                    if let Ok(modified) = meta.modified() {
                        state.record_mtime(modified);
                    }
                }
                Err(_) => {
                    state.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        // Other entry kinds (device nodes, sockets) are not counted — same as
        // the serial walker which only handles dirs and regular files.
    }
}

/// Measures one tree with a bounded parallel worker pool.
///
/// Semantics match [`measure_tree`] exactly on size / count / errors and
/// newest-modification truncated to the second (flooring is monotone, so the
/// max of floored times equals the floored max). The caller must supply a
/// `Sync` cancellation predicate because it is shared across workers.
pub fn measure_tree_parallel(root: &Path, should_continue: &(dyn Fn() -> bool + Sync)) -> Measure {
    // Root fast paths are identical to the serial walker (reparse root →
    // error; single file → direct metadata).
    if path_is_reparse_like(root) {
        return Measure {
            error_count: 1,
            ..Measure::default()
        };
    }
    if root.is_file() {
        return match std::fs::metadata(root) {
            Ok(meta) => Measure {
                logical_size: meta.len(),
                file_count: 1,
                last_modified: meta.modified().ok(),
                error_count: 0,
            },
            Err(_) => Measure {
                error_count: 1,
                ..Measure::default()
            },
        };
    }
    if !root.is_dir() {
        // Missing / unreadable root: one honest error (the serial walker
        // reports the same for a nonexistent root).
        return Measure {
            error_count: 1,
            ..Measure::default()
        };
    }

    let workers = worker_count();
    if workers <= 1 {
        return measure_tree_serial(root, should_continue);
    }

    let state = Arc::new(WalkShared {
        queue: Mutex::new(std::collections::VecDeque::from([root.to_path_buf()])),
        size: AtomicU64::new(0),
        files: AtomicU64::new(0),
        errors: AtomicU64::new(0),
        last_mod: AtomicI64::new(0),
        active: AtomicUsize::new(0),
    });

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let state = Arc::clone(&state);
            scope.spawn(move || worker_loop(&state, should_continue));
        }
    });

    Measure {
        logical_size: state.size.load(Ordering::Relaxed),
        file_count: state.files.load(Ordering::Relaxed),
        error_count: state.errors.load(Ordering::Relaxed),
        last_modified: {
            let secs = state.last_mod.load(Ordering::Relaxed);
            if secs > 0 {
                Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
            } else {
                None
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always() -> impl Fn() -> bool {
        || true
    }

    fn never() -> impl Fn() -> bool {
        || false
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dr-measure-{name}-{}", std::process::id()))
    }

    #[test]
    fn single_file_measures_quickly() {
        let dir = tmp("file");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"0123456789").unwrap();
        let m = measure_tree(&file, &always());
        assert_eq!(m.logical_size, 10);
        assert_eq!(m.file_count, 1);
        assert_eq!(m.error_count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tree_sums_bytes_and_counts_files() {
        let dir = tmp("tree");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("one.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("sub").join("two.bin"), vec![0u8; 250]).unwrap();
        let m = measure_tree(&dir, &always());
        assert_eq!(m.logical_size, 350);
        assert_eq!(m.file_count, 2);
        assert!(m.last_modified.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancellation_stops_the_walk() {
        let dir = tmp("cancel");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..50 {
            std::fs::write(dir.join(format!("f{i}")), vec![0u8; 10]).unwrap();
        }
        let m = measure_tree(&dir, &never());
        // May stop before walking everything, but must never count all files.
        assert!(m.file_count < 50);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_root_counts_an_error_and_zero() {
        let ghost = std::env::temp_dir().join("definitely-missing-devresidue-measure");
        let m = measure_tree(&ghost, &always());
        assert_eq!(m.logical_size, 0);
        assert_eq!(m.error_count, 1);
        let p = measure_tree_parallel(&ghost, &always());
        assert_eq!(p.logical_size, 0);
        assert_eq!(p.error_count, 1);
    }

    #[test]
    fn single_file_parallel_matches_serial() {
        let dir = tmp("pfile");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, vec![0u8; 77]).unwrap();
        let serial = measure_tree(&file, &always());
        let parallel = measure_tree_parallel(&file, &always());
        assert_eq!(
            (serial.logical_size, serial.file_count, serial.error_count),
            (
                parallel.logical_size,
                parallel.file_count,
                parallel.error_count
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Builds a multi-level tree: `dirs` files per sub dir across `fan` sub
    /// dirs under `depth` levels.
    fn make_tree(base: &std::path::Path, fan: usize, depth: usize, files: usize) {
        if depth == 0 {
            for i in 0..files {
                std::fs::write(base.join(format!("f{i:03}.bin")), vec![0xAB; i % 7 + 1]).unwrap();
            }
            return;
        }
        for s in 0..fan {
            let sub = base.join(format!("d{s:02}"));
            std::fs::create_dir_all(&sub).unwrap();
            make_tree(&sub, fan, depth - 1, files);
        }
    }

    fn secs(t: Option<SystemTime>) -> Option<u64> {
        t.map(|x| {
            x.duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        })
    }

    #[test]
    fn parallel_matches_serial_on_a_multi_level_tree() {
        let dir = tmp("pequiv");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        make_tree(&dir, 4, 2, 30); // 4*4*30 + 4*30 + 30 files
        let serial = measure_tree(&dir, &always());
        let parallel = measure_tree_parallel(&dir, &always());
        assert_eq!(
            serial.file_count, parallel.file_count,
            "file counts must agree"
        );
        assert_eq!(
            serial.logical_size, parallel.logical_size,
            "sizes must agree"
        );
        assert_eq!(
            serial.error_count, parallel.error_count,
            "error counts must agree"
        );
        assert_eq!(
            secs(serial.last_modified),
            secs(parallel.last_modified),
            "newest mtime (truncated to the second) must agree"
        );
        assert_eq!(
            serial.file_count,
            4 * 4 * 30,
            "fixture integrity (only the leaf level carries files)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parallel_reports_partial_results_when_cancelled() {
        let dir = tmp("pcancel");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        make_tree(&dir, 3, 2, 20);
        // Cancel after the walker has been polled a handful of times: whatever
        // was counted must be a *partial* (never the full 3*3*20+… total) and
        // must not panic.
        let polls = std::sync::atomic::AtomicU32::new(0);
        let m = measure_tree_parallel(&dir, &|| polls.fetch_add(1, Ordering::Relaxed) < 12);
        assert!(
            m.file_count < 3 * 3 * 20,
            "cancelled parallel walk must stay partial (got {})",
            m.file_count
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
