//! Windows implementation of Core's crash-safe user-rule transaction port.
//!
//! The port owns only the sibling transaction files for one live user-rule
//! file.  It never walks a directory, deletes a scan target, parses rules or
//! invokes the cleanup authority.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use devresidue_core::ai::{
    PreparedRuleTransaction, TransactionRecovery, UserRuleTransactionGuard, UserRuleTransactionPort,
};
#[cfg(test)]
use uuid::Uuid;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE};
use windows::Win32::Storage::FileSystem::{
    DeleteFileW, LockFileEx, MoveFileExW, ReplaceFileW, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, MOVEFILE_WRITE_THROUGH, MOVE_FILE_FLAGS, REPLACEFILE_WRITE_THROUGH,
    REPLACE_FILE_FLAGS,
};
use windows::Win32::System::IO::OVERLAPPED;

const ACTIVE_PRESENT: &str = "ACTIVE/PRESENT";
const ACTIVE_ABSENT: &str = "ACTIVE/ABSENT";
const ROLLED_BACK_PRESENT: &str = "ROLLED_BACK/PRESENT";
const ROLLED_BACK_ABSENT: &str = "ROLLED_BACK/ABSENT";
const COMMITTED: &str = "COMMITTED";

/// Stateless Windows transaction port.  All mutable state is on disk and is
/// recovered from the marker, so a fresh value can recover a prior process.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsUserRuleTransactionPort;

impl WindowsUserRuleTransactionPort {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl UserRuleTransactionPort for WindowsUserRuleTransactionPort {
    fn acquire_exclusive(
        &self,
        rule_file: &Path,
    ) -> Result<Box<dyn UserRuleTransactionGuard>, String> {
        let lock_path = append_suffix(rule_file, ".lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| "unable to create the user rule lock directory".to_string())?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|_| "unable to open the user rule transaction lock".to_string())?;
        let mut overlapped = OVERLAPPED::default();
        let flags = LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY;
        // SAFETY: `file` is open for read/write and is moved into the returned
        // guard after this call; its raw handle remains valid for the call.
        // `overlapped` is a zeroed stack structure used only by this synchronous
        // byte-range lock. The requested range covers the whole lock file.
        #[allow(unsafe_code)]
        let result = unsafe {
            LockFileEx(
                HANDLE(file.as_raw_handle() as *mut _),
                flags,
                None,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        result.map_err(|_| {
            "user rule transaction lock is already held by another transaction".to_string()
        })?;
        Ok(Box::new(WindowsRuleTransactionGuard { _file: file }))
    }

    fn recover_if_needed(&self, rule_file: &Path) -> Result<TransactionRecovery, String> {
        let tx = transaction_paths(rule_file);
        let marker = read_marker(&tx.marker_path)?;
        match marker.as_deref() {
            None => {
                // Without a marker, live is authoritative.  Only known sibling
                // recovery files are removed, and a failed removal blocks a new
                // transaction rather than guessing what they mean.
                cleanup_unmarked_material(rule_file)?;
                Ok(TransactionRecovery::Clean)
            }
            Some(ACTIVE_PRESENT) => {
                remove_transient_material(&tx.live_path)?;
                require_exists(&tx.backup_path, "ACTIVE/PRESENT backup")?;
                let original = read_bytes(&tx.backup_path, "ACTIVE/PRESENT backup")?;
                restore_from_source(&tx.live_path, &tx.backup_path)?;
                verify_bytes(&tx.live_path, &original, "ACTIVE/PRESENT restoration")?;
                publish_marker(&tx.marker_path, ROLLED_BACK_PRESENT)?;
                Ok(TransactionRecovery::RolledBackPendingCleanup(tx))
            }
            Some(ACTIVE_ABSENT) => {
                remove_transient_material(&tx.live_path)?;
                if tx.backup_path.exists() {
                    return Err("ACTIVE/ABSENT transaction has an original backup; state contradiction (evidence preserved)".to_string());
                }
                remove_if_exists(&tx.live_path, "ACTIVE/ABSENT live restoration")?;
                if tx.live_path.exists() {
                    return Err("ACTIVE/ABSENT live file remains present after restoration (evidence preserved)".to_string());
                }
                publish_marker(&tx.marker_path, ROLLED_BACK_ABSENT)?;
                Ok(TransactionRecovery::RolledBackPendingCleanup(tx))
            }
            Some(ROLLED_BACK_PRESENT) => Ok(TransactionRecovery::RolledBackPendingCleanup(tx)),
            Some(ROLLED_BACK_ABSENT) => {
                if tx.backup_path.exists() {
                    return Err("ROLLED_BACK/ABSENT transaction has an original backup; state contradiction (evidence preserved)".to_string());
                }
                Ok(TransactionRecovery::RolledBackPendingCleanup(tx))
            }
            Some(COMMITTED) => {
                if !tx.live_path.exists() {
                    return Err(
                        "COMMITTED transaction has no live user rule file (evidence preserved)"
                            .to_string(),
                    );
                }
                Ok(TransactionRecovery::CommittedPendingCleanup(tx))
            }
            Some(_) => Err("invalid user rule transaction marker (evidence preserved)".to_string()),
        }
    }

    fn read_current(&self, rule_file: &Path) -> Result<Option<Vec<u8>>, String> {
        match fs::read(rule_file) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("unable to read the live user rule file".to_string()),
        }
    }

    fn prepare(&self, rule_file: &Path, bytes: &[u8]) -> Result<PreparedRuleTransaction, String> {
        let tx = transaction_paths(rule_file);
        if tx.marker_path.exists() || tx.candidate_path.exists() || tx.backup_path.exists() {
            return Err(
                "user rule transaction has leftover recovery material; recover it first"
                    .to_string(),
            );
        }
        if known_transient_material(rule_file)
            .iter()
            .any(|path| path.exists())
        {
            return Err(
                "user rule transaction has leftover transient recovery material; recover it first"
                    .to_string(),
            );
        }
        if let Some(parent) = rule_file.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| "unable to create the user rule directory".to_string())?;
        }
        let original_present = rule_file.exists();
        if let Err(error) = (|| {
            publish_new_bytes(&tx.candidate_path, bytes, "candidate")?;
            if original_present {
                let original = read_bytes(rule_file, "original user rule file")?;
                publish_new_bytes(&tx.backup_path, &original, "original")?;
            }
            Ok::<(), String>(())
        })() {
            return Err(cleanup_prepare_error(error, rule_file));
        }
        let marker = if original_present {
            ACTIVE_PRESENT
        } else {
            ACTIVE_ABSENT
        };
        if let Err(error) = publish_new_marker(&tx.marker_path, marker) {
            // No marker was published, so these named siblings are the only
            // possible recovery material.  Do not hide cleanup failures: a
            // caller must know if no-marker recovery material remains.
            return Err(combine_cleanup_error(
                error,
                cleanup_unmarked_material(rule_file),
            ));
        }
        Ok(tx)
    }

    fn replace(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
        ensure_marker_active(&tx.marker_path)?;
        require_exists(&tx.candidate_path, "transaction candidate")?;
        let marker = read_marker(&tx.marker_path)?;
        let original_present = marker.as_deref() == Some(ACTIVE_PRESENT);
        if original_present && !tx.live_path.exists() {
            return Err("ACTIVE/PRESENT live file disappeared before replacement".to_string());
        }
        if !original_present && tx.live_path.exists() {
            return Err("ACTIVE/ABSENT live file appeared before replacement".to_string());
        }
        replace_from_source(&tx.live_path, &tx.candidate_path, original_present)
    }

    fn rollback(&self, tx: &PreparedRuleTransaction) -> Result<PreparedRuleTransaction, String> {
        let marker = read_marker(&tx.marker_path)?;
        match marker.as_deref() {
            Some(ACTIVE_PRESENT) => {
                remove_transient_material(&tx.live_path)?;
                require_exists(&tx.backup_path, "ACTIVE/PRESENT backup")?;
                let original = read_bytes(&tx.backup_path, "ACTIVE/PRESENT backup")?;
                restore_from_source(&tx.live_path, &tx.backup_path)?;
                verify_bytes(&tx.live_path, &original, "rollback restoration")?;
                publish_marker(&tx.marker_path, ROLLED_BACK_PRESENT)?;
                Ok(tx.clone())
            }
            Some(ACTIVE_ABSENT) => {
                remove_transient_material(&tx.live_path)?;
                if tx.backup_path.exists() {
                    return Err("ACTIVE/ABSENT transaction has an original backup; state contradiction (evidence preserved)".to_string());
                }
                remove_if_exists(&tx.live_path, "ACTIVE/ABSENT rollback")?;
                if tx.live_path.exists() {
                    return Err(
                        "ACTIVE/ABSENT rollback did not restore absence (evidence preserved)"
                            .to_string(),
                    );
                }
                publish_marker(&tx.marker_path, ROLLED_BACK_ABSENT)?;
                Ok(tx.clone())
            }
            Some(ROLLED_BACK_PRESENT | ROLLED_BACK_ABSENT) => {
                Err("user rule transaction is already rolled back".to_string())
            }
            Some(COMMITTED) => {
                Err("refusing to roll back a committed user rule transaction".to_string())
            }
            Some(_) => Err("invalid user rule transaction marker (evidence preserved)".to_string()),
            None => Err(
                "user rule transaction marker is missing; recovery state is unknown".to_string(),
            ),
        }
    }

    fn cleanup_rolled_back(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
        match read_marker(&tx.marker_path)?.as_deref() {
            Some(ROLLED_BACK_PRESENT) => {
                remove_if_exists(&tx.candidate_path, "rolled-back candidate")?;
                remove_if_exists(&tx.backup_path, "rolled-back original")?;
                remove_transient_material(&tx.live_path)?;
                remove_if_exists(&tx.marker_path, "rolled-back marker")
            }
            Some(ROLLED_BACK_ABSENT) => {
                if tx.backup_path.exists() {
                    return Err("ROLLED_BACK/ABSENT transaction has an original backup; state contradiction (evidence preserved)".to_string());
                }
                remove_if_exists(&tx.candidate_path, "rolled-back candidate")?;
                remove_transient_material(&tx.live_path)?;
                remove_if_exists(&tx.marker_path, "rolled-back marker")
            }
            Some(_) => Err(
                "user rule transaction is not in a rolled-back state; evidence preserved"
                    .to_string(),
            ),
            None => {
                Err("rolled-back transaction marker is missing; evidence preserved".to_string())
            }
        }
    }

    fn commit_verified(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
        ensure_marker_active(&tx.marker_path)?;
        publish_marker(&tx.marker_path, COMMITTED)
    }

    fn cleanup_committed(&self, tx: &PreparedRuleTransaction) -> Result<(), String> {
        match read_marker(&tx.marker_path)?.as_deref() {
            Some(COMMITTED) => {
                if !tx.live_path.exists() {
                    return Err(
                        "COMMITTED transaction live file is missing; evidence preserved"
                            .to_string(),
                    );
                }
                remove_if_exists(&tx.candidate_path, "committed candidate")?;
                remove_if_exists(&tx.backup_path, "committed original")?;
                remove_transient_material(&tx.live_path)?;
                remove_if_exists(&tx.marker_path, "committed marker")
            }
            Some(_) => {
                Err("user rule transaction is not committed; evidence preserved".to_string())
            }
            None => Err("committed transaction marker is missing; evidence preserved".to_string()),
        }
    }
}

struct WindowsRuleTransactionGuard {
    _file: File,
}

impl UserRuleTransactionGuard for WindowsRuleTransactionGuard {}

fn transaction_paths(rule_file: &Path) -> PreparedRuleTransaction {
    PreparedRuleTransaction {
        live_path: rule_file.to_path_buf(),
        marker_path: append_suffix(rule_file, ".txn"),
        backup_path: append_suffix(rule_file, ".txn.original"),
        candidate_path: append_suffix(rule_file, ".txn.candidate"),
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn read_marker(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(value) => {
            let value = value.to_string();
            if matches!(
                value.as_str(),
                ACTIVE_PRESENT
                    | ACTIVE_ABSENT
                    | ROLLED_BACK_PRESENT
                    | ROLLED_BACK_ABSENT
                    | COMMITTED
            ) {
                Ok(Some(value))
            } else {
                Err("invalid user rule transaction marker (evidence preserved)".to_string())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => {
            Err("unable to read user rule transaction marker (evidence preserved)".to_string())
        }
    }
}

fn ensure_marker_active(path: &Path) -> Result<(), String> {
    match read_marker(path)?.as_deref() {
        Some(ACTIVE_PRESENT | ACTIVE_ABSENT) => Ok(()),
        Some(_) => Err("user rule transaction is not active".to_string()),
        None => {
            Err("user rule transaction marker is missing; recovery state is unknown".to_string())
        }
    }
}

fn require_exists(path: &Path, what: &str) -> Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err(format!("{what} is missing; evidence preserved"))
    }
}

fn read_bytes(path: &Path, what: &str) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|_| format!("unable to read {what}; evidence preserved"))
}

fn verify_bytes(path: &Path, expected: &[u8], what: &str) -> Result<(), String> {
    let actual = read_bytes(path, what)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{what} byte verification failed; evidence preserved"
        ))
    }
}

fn publish_new_bytes(target: &Path, bytes: &[u8], _label: &str) -> Result<(), String> {
    let temp = append_suffix(target, ".tmp");
    write_durable(&temp, bytes)?;
    if let Err(error) = move_new(&temp, target) {
        return Err(cleanup_after_publish_error(
            error,
            &temp,
            "temporary transaction file",
        ));
    }
    Ok(())
}

fn write_durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| "unable to create transaction temporary file".to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "unable to flush transaction temporary file".to_string())
    })();
    if let Err(error) = result {
        return Err(cleanup_after_publish_error(
            error,
            path,
            "failed transaction temporary file",
        ));
    }
    Ok(())
}

fn publish_new_marker(target: &Path, marker: &str) -> Result<(), String> {
    publish_new_bytes(target, marker.as_bytes(), "marker")
}

fn publish_marker(target: &Path, marker: &str) -> Result<(), String> {
    let temp = append_suffix(target, ".state.tmp");
    write_durable(&temp, marker.as_bytes())?;
    let result = replace_existing(target, &temp);
    if let Err(error) = result {
        return Err(cleanup_after_publish_error(
            error,
            &temp,
            "temporary marker file",
        ));
    }
    Ok(())
}

fn restore_from_source(live: &Path, source: &Path) -> Result<(), String> {
    replace_from_source(live, source, live.exists())
}

fn replace_from_source(live: &Path, source: &Path, live_present: bool) -> Result<(), String> {
    let replacement = append_suffix(live, ".txn.replacement");
    let bytes = read_bytes(source, "transaction source")?;
    write_durable(&replacement, &bytes)?;
    let result = if live_present {
        replace_existing(live, &replacement)
    } else {
        move_new(&replacement, live)
    };
    if let Err(error) = result {
        return Err(cleanup_after_publish_error(
            error,
            &replacement,
            "temporary replacement file",
        ));
    }
    Ok(())
}

fn cleanup_after_publish_error(error: String, temporary: &Path, label: &str) -> String {
    match remove_if_exists(temporary, label) {
        Ok(()) => error,
        Err(cleanup_error) => format!("{error}; {cleanup_error}"),
    }
}

fn known_transient_material(rule_file: &Path) -> Vec<PathBuf> {
    let tx = transaction_paths(rule_file);
    vec![
        append_suffix(&tx.candidate_path, ".tmp"),
        append_suffix(&tx.backup_path, ".tmp"),
        append_suffix(&tx.marker_path, ".tmp"),
        append_suffix(&tx.marker_path, ".state.tmp"),
        append_suffix(&tx.live_path, ".txn.replacement"),
    ]
}

fn cleanup_unmarked_material(rule_file: &Path) -> Result<(), String> {
    let tx = transaction_paths(rule_file);
    let mut errors = Vec::new();
    for (path, label) in [
        (&tx.candidate_path, "stray candidate"),
        (&tx.backup_path, "stray original"),
    ] {
        if let Err(error) = remove_if_exists(path, label) {
            errors.push(error);
        }
    }
    for path in known_transient_material(rule_file) {
        if let Err(error) = remove_if_exists(&path, "stray transaction temporary file") {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn remove_transient_material(live_path: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    for path in [
        append_suffix(live_path, ".txn.replacement"),
        append_suffix(&append_suffix(live_path, ".txn"), ".state.tmp"),
        append_suffix(&append_suffix(live_path, ".txn"), ".tmp"),
        append_suffix(&append_suffix(live_path, ".txn.original"), ".tmp"),
        append_suffix(&append_suffix(live_path, ".txn.candidate"), ".tmp"),
    ] {
        if let Err(error) = remove_if_exists(&path, "transaction temporary file") {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn cleanup_prepare_error(error: String, rule_file: &Path) -> String {
    combine_cleanup_error(error, cleanup_unmarked_material(rule_file))
}

fn combine_cleanup_error(error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => error,
        Err(cleanup_error) => format!("{error}; {cleanup_error}"),
    }
}

fn remove_if_exists(path: &Path, what: &str) -> Result<(), String> {
    let wide = path_wide(path)?;
    let result = {
        // SAFETY: `wide` is an owned, NUL-terminated UTF-16 path buffer that
        // remains alive for the synchronous DeleteFileW call. The port only
        // passes its own transaction sibling paths to this helper.
        #[allow(unsafe_code)]
        unsafe {
            DeleteFileW(PCWSTR(wide.as_ptr()))
        }
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let code = error.code().0 as u32 & 0xFFFF;
            if code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0 {
                Ok(())
            } else {
                Err(format!("unable to remove {what}; evidence preserved"))
            }
        }
    }
}

fn move_new(source: &Path, target: &Path) -> Result<(), String> {
    let source_wide = path_wide(source)?;
    let target_wide = path_wide(target)?;
    let flags = MOVE_FILE_FLAGS(MOVEFILE_WRITE_THROUGH.0);
    let result = {
        // SAFETY: both path buffers are owned, NUL-terminated UTF-16 values
        // alive for the synchronous call; no pointer escapes the call. The
        // WRITE_THROUGH makes the new sibling durable before this call returns;
        // the target is intentionally not overwritten in this helper.
        #[allow(unsafe_code)]
        unsafe {
            MoveFileExW(
                PCWSTR(source_wide.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                flags,
            )
        }
    };
    result.map_err(|_| "unable to atomically publish transaction file".to_string())
}

fn replace_existing(target: &Path, replacement: &Path) -> Result<(), String> {
    let target_wide = path_wide(target)?;
    let replacement_wide = path_wide(replacement)?;
    let result = {
        // SAFETY: the target and replacement are sibling paths represented by
        // owned, NUL-terminated UTF-16 buffers alive for the call. The
        // replacement is a fully flushed file and ReplaceFileW consumes only
        // that temporary name; no backup path is requested because the port's
        // durable .txn.original is managed separately.
        #[allow(unsafe_code)]
        unsafe {
            ReplaceFileW(
                PCWSTR(target_wide.as_ptr()),
                PCWSTR(replacement_wide.as_ptr()),
                None,
                REPLACE_FILE_FLAGS(REPLACEFILE_WRITE_THROUGH.0),
                None,
                None,
            )
        }
    };
    result.map_err(|_| "unable to atomically replace the live user rule file".to_string())
}

fn path_wide(path: &Path) -> Result<Vec<u16>, String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if wide[..wide.len().saturating_sub(1)].contains(&0) {
        return Err("transaction path contains an embedded NUL".to_string());
    }
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_through_flags_are_required_for_durable_publication() {
        assert_ne!(MOVEFILE_WRITE_THROUGH.0, 0);
        assert_ne!(REPLACEFILE_WRITE_THROUGH.0, 0);
    }

    #[test]
    fn replacement_cleanup_failure_is_reported_with_primary_error() {
        let root = std::env::temp_dir().join(format!(
            "devresidue-task4-tx-helper-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let temporary = root.join("replacement.tmp");
        fs::create_dir(&temporary).unwrap();
        let error = cleanup_after_publish_error(
            "primary replacement failure".to_string(),
            &temporary,
            "temporary replacement file",
        );
        assert!(error.contains("primary replacement failure"));
        assert!(error.contains("temporary replacement file"));
        assert!(temporary.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
