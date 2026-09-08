//! `devresidue journal` — read back recent cleanup journal entries
//! (SPEC §24: an operator can answer "what happened, when, how much space").

use devresidue_core::journal;

use crate::support;

/// Runs the journal subcommand.
pub fn run(last: usize) -> Result<(), String> {
    let base = support::data_dir()?;
    let records = journal::read_last(&base, last).map_err(|e| e.to_string())?;
    if records.is_empty() {
        println!(
            "No journal entries yet under {}",
            base.join("journal").display()
        );
        return Ok(());
    }
    println!(
        "Recent journal entries (showing {} of {}):",
        records.len(),
        records.len()
    );
    for record in records {
        let action = record
            .action
            .map(|a| format!("{a:?}"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  [{}] session={:<4} {:<8} {:<9} {} {}",
            format_time(record.time),
            record.session_id,
            phase_label(record.phase),
            action,
            record.product.as_deref().unwrap_or("-"),
            record
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        if let Some(result) = &record.result {
            println!("        result={result}");
        }
        if let Some(error) = &record.error {
            println!("        error={error}");
        }
    }
    Ok(())
}

fn format_time(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Local wall clock from the epoch offset (best-effort for CLI display).
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    format!("{days}d {h:02}:{m:02}:{s:02}")
}

fn phase_label(p: journal::JournalPhase) -> &'static str {
    match p {
        journal::JournalPhase::Attempt => "attempt",
        journal::JournalPhase::Result => "result",
    }
}
