//! `get_journal` command — read back recent cleanup journal entries (SPEC §24)
//! as plain DTOs for the UI's history view.

use std::time::UNIX_EPOCH;

use tauri::State;

use crate::contract::{CommandError, ErrorCode, JournalEntryDto};
use crate::state::AppState;

/// Default number of journal entries returned when the UI omits `last_n`.
pub const DEFAULT_LAST_N: usize = 50;

/// `get_journal` command.
#[tauri::command]
pub fn get_journal(
    state: State<'_, AppState>,
    last_n: Option<usize>,
) -> Result<Vec<JournalEntryDto>, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();
    let limit = last_n.unwrap_or(DEFAULT_LAST_N);
    let records = devresidue_core::journal::read_last(&data_dir, limit)
        .map_err(|e| CommandError::new(ErrorCode::Engine, e.to_string()))?;
    Ok(records.iter().map(entry_dto).collect())
}

/// `clear_all_data` command (the log page's 清空 button): wipes every journal
/// shard, the persisted scan snapshot (`last-scan.json`), the plan store and
/// the unknown-data dispositions' derived state, then resets the in-memory
/// authoritative model — the whole app returns to its first-run state.
///
/// Safety: only DevResidue-owned state files under the data dir are touched;
/// nothing on the user's real file system (caches, projects) is modified.
/// The scan-store `scan-store.lock` / `integrity.key` are kept (they are
/// plumbing, not history).
#[tauri::command]
pub fn clear_all_data(state: State<'_, AppState>) -> Result<usize, CommandError> {
    let data_dir = state.model.lock().unwrap().data_dir().to_path_buf();

    // 1. Journal shards.
    let cleared = devresidue_core::journal::clear_all(&data_dir)
        .map_err(|e| CommandError::new(ErrorCode::Engine, e.to_string()))?;

    // 2. Persisted scan snapshot.
    let snapshot = devresidue_providers::scan_store::snapshot_path(&data_dir);
    if snapshot.is_file() {
        std::fs::remove_file(&snapshot)
            .map_err(|e| CommandError::new(ErrorCode::Engine, format!("remove snapshot: {e}")))?;
    }

    // 3. Persisted plans.
    let plans = data_dir.join("plans");
    if plans.is_dir() {
        std::fs::remove_dir_all(&plans)
            .map_err(|e| CommandError::new(ErrorCode::Engine, format!("remove plans: {e}")))?;
    }

    // 4. Reset the in-memory authoritative model (the AppState's "latest
    // scan" the dashboard reads on startup).
    state.model.lock().unwrap().reset();

    Ok(cleared)
}

/// Maps one journal record onto the UI DTO (epoch seconds for the timestamp;
/// action as the recycle/delete/execute label).
fn entry_dto(record: &devresidue_core::journal::JournalRecord) -> JournalEntryDto {
    let time_secs = record
        .time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    JournalEntryDto {
        session_id: record.session_id,
        time_secs,
        phase: match record.phase {
            devresidue_core::journal::JournalPhase::Attempt => "attempt".into(),
            devresidue_core::journal::JournalPhase::Result => "result".into(),
        },
        product: record.product.clone(),
        rule: record.rule,
        provider: record.provider,
        path: record.path.as_ref().map(|p| p.display().to_string()),
        action: record
            .action
            .map(crate::contract::mode_label)
            .map(String::from),
        estimated_size: record.estimated_size,
        result: record.result.clone(),
        error: record.error.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devresidue_core::domain::action::CleanupMode;
    use devresidue_core::journal::{Journal, JournalPhase, JournalRecord};

    fn tmp_base() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dr-tauri-journal-{}", std::process::id()))
    }

    fn record(seed: u64) -> JournalRecord {
        JournalRecord::for_item(
            seed,
            JournalPhase::Result,
            Some("npm".into()),
            Some(2),
            Some(3),
            r"C:\Users\demo\AppData\Local\npm-cache".into(),
            CleanupMode::ExternalCommand,
            1024,
            Some("success".into()),
            None,
        )
    }

    #[test]
    fn journal_reads_and_maps_entries() {
        let base = tmp_base();
        let _ = std::fs::remove_dir_all(&base);
        let mut journal = Journal::open_at(&base).expect("open");
        journal.append(&record(7)).expect("append");

        let records = devresidue_core::journal::read_last(&base, 10).expect("read");
        assert_eq!(records.len(), 1);
        let dto = entry_dto(&records[0]);
        assert_eq!(dto.session_id, 7);
        assert_eq!(dto.phase, "result");
        assert_eq!(dto.action.as_deref(), Some("execute"));
        assert_eq!(
            dto.path.as_deref(),
            Some(r"C:\Users\demo\AppData\Local\npm-cache")
        );
        assert!(dto.time_secs > 0);
        assert_eq!(dto.estimated_size, 1024);
        let _ = std::fs::remove_dir_all(&base);
    }
}
