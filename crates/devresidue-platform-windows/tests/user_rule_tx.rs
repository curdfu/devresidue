use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use devresidue_core::ai::{TransactionRecovery, UserRuleTransactionPort};
use devresidue_platform_windows::user_rule_tx::WindowsUserRuleTransactionPort;
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "devresidue-task4-tx-{}-{}",
            std::process::id(),
            Uuid::new_v4()
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

fn rule_file(dir: &TempDir) -> PathBuf {
    dir.path().join("user-dispositions.yaml")
}

#[test]
fn existing_live_replace_is_atomic_and_does_not_touch_neighbor() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let neighbor = dir.path().join("neighbor.yaml");
    fs::write(&live, b"original").unwrap();
    fs::write(&neighbor, b"neighbor").unwrap();
    let port = WindowsUserRuleTransactionPort::new();

    let guard = port.acquire_exclusive(&live).unwrap();
    assert!(matches!(
        port.recover_if_needed(&live).unwrap(),
        TransactionRecovery::Clean
    ));
    let tx = port.prepare(&live, b"candidate").unwrap();
    port.replace(&tx).unwrap();
    assert_eq!(fs::read(&live).unwrap(), b"candidate");
    assert_eq!(fs::read(&neighbor).unwrap(), b"neighbor");
    assert_eq!(
        fs::read_to_string(&tx.marker_path).unwrap(),
        "ACTIVE/PRESENT"
    );
    assert_eq!(fs::read(&tx.backup_path).unwrap(), b"original");
    assert_eq!(fs::read(&tx.candidate_path).unwrap(), b"candidate");
    port.commit_verified(&tx).unwrap();
    assert_eq!(fs::read_to_string(&tx.marker_path).unwrap(), "COMMITTED");
    assert!(tx.backup_path.exists());
    assert!(tx.candidate_path.exists());
    port.cleanup_committed(&tx).unwrap();
    drop(guard);
    assert_eq!(fs::read(&live).unwrap(), b"candidate");
    assert!(!tx.marker_path.exists());
    assert!(!tx.backup_path.exists());
    assert!(!tx.candidate_path.exists());
}

#[test]
fn absent_original_can_reach_committed_and_cleanup_without_creating_backup() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();

    let tx = port.prepare(&live, b"new").unwrap();
    port.replace(&tx).unwrap();
    port.commit_verified(&tx).unwrap();
    assert_eq!(fs::read_to_string(&tx.marker_path).unwrap(), "COMMITTED");
    assert!(!tx.backup_path.exists());
    assert_eq!(fs::read(&live).unwrap(), b"new");
    port.cleanup_committed(&tx).unwrap();
    assert!(!tx.marker_path.exists());
    assert!(!tx.candidate_path.exists());
}

#[test]
fn absent_original_rolls_back_to_absence_and_present_original_restores_bytes() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();

    let absent = port.prepare(&live, b"new").unwrap();
    assert!(!absent.backup_path.exists());
    port.replace(&absent).unwrap();
    let restored = port.rollback(&absent).unwrap();
    assert!(!live.exists());
    assert_eq!(
        fs::read_to_string(&restored.marker_path).unwrap(),
        "ROLLED_BACK/ABSENT"
    );
    port.cleanup_rolled_back(&restored).unwrap();
    assert!(!restored.marker_path.exists());

    fs::write(&live, b"old").unwrap();
    let present = port.prepare(&live, b"new").unwrap();
    port.replace(&present).unwrap();
    let restored = port.rollback(&present).unwrap();
    assert_eq!(fs::read(&live).unwrap(), b"old");
    assert_eq!(
        fs::read_to_string(&restored.marker_path).unwrap(),
        "ROLLED_BACK/PRESENT"
    );
    port.cleanup_rolled_back(&restored).unwrap();
}

#[test]
fn active_crash_recovery_always_restores_original_and_returns_pending_cleanup() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"old").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    let tx = port.prepare(&live, b"parseable-candidate").unwrap();
    port.replace(&tx).unwrap();
    drop(_guard);

    let next = WindowsUserRuleTransactionPort::new();
    let _guard = next.acquire_exclusive(&live).unwrap();
    let recovery = next.recover_if_needed(&live).unwrap();
    let TransactionRecovery::RolledBackPendingCleanup(pending) = recovery else {
        panic!("expected rolled-back pending cleanup");
    };
    assert_eq!(fs::read(&live).unwrap(), b"old");
    assert_eq!(
        fs::read_to_string(&pending.marker_path).unwrap(),
        "ROLLED_BACK/PRESENT"
    );
    next.cleanup_rolled_back(&pending).unwrap();
}

#[test]
fn replace_failure_cleans_replacement_temp_and_preserves_active_marker() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"old").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    let tx = port.prepare(&live, b"candidate").unwrap();

    fs::remove_file(&live).unwrap();
    fs::create_dir(&live).unwrap();

    assert!(port.replace(&tx).is_err());
    assert_eq!(
        fs::read_to_string(&tx.marker_path).unwrap(),
        "ACTIVE/PRESENT"
    );
    assert!(live.is_dir());
    assert!(!PathBuf::from(format!("{}.txn.replacement", live.display())).exists());
}

#[test]
fn crash_after_prepare_restores_original_present_and_absent_states() {
    for original in [Some(b"old".as_slice()), None] {
        let dir = TempDir::new();
        let live = rule_file(&dir);
        if let Some(bytes) = original {
            fs::write(&live, bytes).unwrap();
        }
        let port = WindowsUserRuleTransactionPort::new();
        let guard = port.acquire_exclusive(&live).unwrap();
        let tx = port.prepare(&live, b"candidate").unwrap();
        assert!(tx.marker_path.exists());
        drop(guard);

        let next = WindowsUserRuleTransactionPort::new();
        let guard = next.acquire_exclusive(&live).unwrap();
        let recovery = next.recover_if_needed(&live).unwrap();
        let TransactionRecovery::RolledBackPendingCleanup(pending) = recovery else {
            panic!("expected rolled-back pending cleanup");
        };
        match original {
            Some(bytes) => assert_eq!(fs::read(&live).unwrap(), bytes),
            None => assert!(!live.exists()),
        }
        assert!(pending.marker_path.exists());
        next.cleanup_rolled_back(&pending).unwrap();
        drop(guard);
    }
}

#[test]
fn rollback_marker_is_recovered_as_pending_cleanup_after_restart() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"old").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let guard = port.acquire_exclusive(&live).unwrap();
    let tx = port.prepare(&live, b"candidate").unwrap();
    port.replace(&tx).unwrap();
    let pending = port.rollback(&tx).unwrap();
    drop(guard);

    let next = WindowsUserRuleTransactionPort::new();
    let guard = next.acquire_exclusive(&live).unwrap();
    let recovery = next.recover_if_needed(&live).unwrap();
    let TransactionRecovery::RolledBackPendingCleanup(recovered) = recovery else {
        panic!("expected rolled-back pending cleanup");
    };
    assert_eq!(recovered, pending);
    assert_eq!(fs::read(&live).unwrap(), b"old");
    next.cleanup_rolled_back(&recovered).unwrap();
    drop(guard);
}

#[test]
fn committed_recovery_keeps_live_and_returns_pending_cleanup() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"old").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    let tx = port.prepare(&live, b"committed").unwrap();
    port.replace(&tx).unwrap();
    port.commit_verified(&tx).unwrap();
    drop(_guard);

    let next = WindowsUserRuleTransactionPort::new();
    let _guard = next.acquire_exclusive(&live).unwrap();
    let recovery = next.recover_if_needed(&live).unwrap();
    let TransactionRecovery::CommittedPendingCleanup(pending) = recovery else {
        panic!("expected committed pending cleanup");
    };
    assert_eq!(fs::read(&live).unwrap(), b"committed");
    next.cleanup_committed(&pending).unwrap();
}

#[test]
fn committed_transaction_cannot_be_rolled_back_after_the_commit_point() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"old").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    let tx = port.prepare(&live, b"committed").unwrap();
    port.replace(&tx).unwrap();
    port.commit_verified(&tx).unwrap();

    assert!(port.rollback(&tx).is_err());
    assert_eq!(fs::read(&live).unwrap(), b"committed");
    assert_eq!(fs::read_to_string(&tx.marker_path).unwrap(), "COMMITTED");
}

#[test]
fn invalid_or_contradictory_marker_is_preserved_and_refused() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let marker = PathBuf::from(format!("{}.txn", live.display()));
    fs::write(&live, b"live").unwrap();
    fs::write(&marker, b"INVALID").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    assert!(port.recover_if_needed(&live).is_err());
    assert!(marker.exists());
    assert_eq!(fs::read(&live).unwrap(), b"live");
}

#[test]
fn cleanup_failures_keep_the_marker_and_lock_contention_is_refused() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"old").unwrap();
    let port = Arc::new(WindowsUserRuleTransactionPort::new());
    let first = port.acquire_exclusive(&live).unwrap();
    let second_port = Arc::clone(&port);
    let live_for_thread = live.clone();
    let joined = thread::spawn(move || second_port.acquire_exclusive(&live_for_thread).is_err())
        .join()
        .unwrap();
    assert!(joined);
    drop(first);
}

#[test]
fn no_marker_stray_cleanup_failure_preserves_live_and_evidence() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let candidate = PathBuf::from(format!("{}.txn.candidate", live.display()));
    fs::write(&live, b"authoritative").unwrap();
    fs::create_dir(&candidate).unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();

    assert!(port.recover_if_needed(&live).is_err());
    assert_eq!(fs::read(&live).unwrap(), b"authoritative");
    assert!(candidate.exists());
}

#[test]
fn active_present_without_backup_is_refused_without_touching_live() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let marker = PathBuf::from(format!("{}.txn", live.display()));
    fs::write(&live, b"candidate-looking-live").unwrap();
    fs::write(&marker, b"ACTIVE/PRESENT").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();

    assert!(port.recover_if_needed(&live).is_err());
    assert_eq!(fs::read(&live).unwrap(), b"candidate-looking-live");
    assert_eq!(fs::read(&marker).unwrap(), b"ACTIVE/PRESENT");
}

#[test]
fn active_absent_with_backup_is_refused_without_touching_live() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let marker = PathBuf::from(format!("{}.txn", live.display()));
    let backup = PathBuf::from(format!("{}.txn.original", live.display()));
    fs::write(&live, b"candidate").unwrap();
    fs::write(&backup, b"original").unwrap();
    fs::write(&marker, b"ACTIVE/ABSENT").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();

    assert!(port.recover_if_needed(&live).is_err());
    assert_eq!(fs::read(&live).unwrap(), b"candidate");
    assert!(backup.exists());
    assert_eq!(fs::read(&marker).unwrap(), b"ACTIVE/ABSENT");
}

#[test]
fn rolled_back_absent_with_backup_is_refused_and_committed_missing_live_is_refused() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    let marker = PathBuf::from(format!("{}.txn", live.display()));
    let backup = PathBuf::from(format!("{}.txn.original", live.display()));
    fs::write(&backup, b"contradictory").unwrap();
    fs::write(&marker, b"ROLLED_BACK/ABSENT").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    assert!(port.recover_if_needed(&live).is_err());
    assert!(marker.exists());
    assert!(backup.exists());

    fs::remove_file(&backup).unwrap();
    fs::write(&marker, b"COMMITTED").unwrap();
    assert!(port.recover_if_needed(&live).is_err());
    assert_eq!(fs::read(&marker).unwrap(), b"COMMITTED");
}

#[test]
fn rolled_back_cleanup_failure_at_each_step_retains_marker() {
    for failure in ["candidate", "backup", "marker"] {
        let dir = TempDir::new();
        let live = rule_file(&dir);
        let port = WindowsUserRuleTransactionPort::new();
        let _guard = port.acquire_exclusive(&live).unwrap();
        fs::write(&live, b"original").unwrap();
        let tx = port.prepare(&live, b"candidate").unwrap();
        port.replace(&tx).unwrap();
        let pending = port.rollback(&tx).unwrap();

        let failed_path = match failure {
            "candidate" => &pending.candidate_path,
            "backup" => &pending.backup_path,
            "marker" => &pending.marker_path,
            _ => unreachable!(),
        };
        fs::remove_file(failed_path).unwrap();
        fs::create_dir(failed_path).unwrap();

        assert!(
            port.cleanup_rolled_back(&pending).is_err(),
            "failure={failure}"
        );
        assert!(pending.marker_path.exists(), "failure={failure}");
    }
}

#[test]
fn committed_cleanup_failure_at_each_step_retains_committed_marker_and_live() {
    for failure in ["candidate", "backup", "marker"] {
        let dir = TempDir::new();
        let live = rule_file(&dir);
        let port = WindowsUserRuleTransactionPort::new();
        let _guard = port.acquire_exclusive(&live).unwrap();
        fs::write(&live, b"original").unwrap();
        let tx = port.prepare(&live, b"committed").unwrap();
        port.replace(&tx).unwrap();
        port.commit_verified(&tx).unwrap();

        let failed_path = match failure {
            "candidate" => &tx.candidate_path,
            "backup" => &tx.backup_path,
            "marker" => &tx.marker_path,
            _ => unreachable!(),
        };
        fs::remove_file(failed_path).unwrap();
        fs::create_dir(failed_path).unwrap();

        assert!(port.cleanup_committed(&tx).is_err(), "failure={failure}");
        assert!(tx.marker_path.exists(), "failure={failure}");
        assert_eq!(fs::read(&live).unwrap(), b"committed");
    }
}

#[test]
fn successful_state_machine_leaves_no_known_transient_replacement_material() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"original").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();
    let tx = port.prepare(&live, b"candidate").unwrap();
    port.replace(&tx).unwrap();
    port.commit_verified(&tx).unwrap();
    port.cleanup_committed(&tx).unwrap();

    let leftovers = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.ends_with(".tmp")
                || name.ends_with(".state.tmp")
                || name.ends_with(".txn.replacement")
        })
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "transient material remains: {leftovers:?}"
    );
}

#[test]
fn active_recovery_cleans_fixed_replacement_material_before_restoring_backup() {
    let dir = TempDir::new();
    let live = rule_file(&dir);
    fs::write(&live, b"original").unwrap();
    let marker = PathBuf::from(format!("{}.txn", live.display()));
    let backup = PathBuf::from(format!("{}.txn.original", live.display()));
    let candidate = PathBuf::from(format!("{}.txn.candidate", live.display()));
    let replacement = PathBuf::from(format!("{}.txn.replacement", live.display()));
    fs::write(&marker, b"ACTIVE/PRESENT").unwrap();
    fs::write(&backup, b"original").unwrap();
    fs::write(&candidate, b"candidate").unwrap();
    fs::write(&replacement, b"candidate").unwrap();
    let port = WindowsUserRuleTransactionPort::new();
    let _guard = port.acquire_exclusive(&live).unwrap();

    let TransactionRecovery::RolledBackPendingCleanup(tx) = port.recover_if_needed(&live).unwrap()
    else {
        panic!("expected rolled-back recovery");
    };
    assert_eq!(fs::read(&live).unwrap(), b"original");
    assert!(!replacement.exists());
    port.cleanup_rolled_back(&tx).unwrap();
}
