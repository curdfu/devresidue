//! Persistent cleanup journal — append-only JSONL under
//! `<exe dir>\Data\journal\devresidue-YYYYMMDD.jsonl` (portable-first; SPEC §24).
//!
//! # Format & fields (whitelist)
//!
//! Each line is one [`JournalRecord`] with **exactly** the SPEC §24 fields:
//! `session_id, time, product, rule, provider, path, action, estimated_size,
//! result, error`. The struct itself is the whitelist (`deny_unknown_fields`):
//! tokens, credentials, secrets, private keys and file contents are never
//! journal fields — nothing else can be smuggled in because serde refuses any
//! JSON that carries additional keys.
//!
//! # Rolling
//!
//! A single file rolls over once it exceeds [`MAX_FILE_BYTES`] (10 MiB): the
//! writer switches to `devresidue-YYYYMMDD.1.jsonl`, `.2.jsonl`, … so every
//! shard is addressable and the reader can merge them back in order.
//!
//! # Failure semantics
//!
//! `append` returns `Err` on write failure; the CleanupEngine treats that as
//! an audit degradation (session `journal_degraded`) and never rolls back a
//! deletion that already happened.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::domain::action::CleanupMode;
use crate::domain::serde_time::system_time;

/// Roll-over threshold for one journal shard.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Upper bound for any single journal string field (F17: an over-long error /
/// product / result must not bloat the audit trail unboundedly).
pub const MAX_FIELD_CHARS: usize = 4096;

/// Sanitises a journal string field (F17): truncates past
/// [`MAX_FIELD_CHARS`] and strips control characters (a raw `\n` or `\r`
/// would let a crafted field forge journal lines / fake records). `\t` is
/// kept for readability; all other C0 controls are removed.
fn sanitize_field(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_FIELD_CHARS));
    for ch in text.chars().take(MAX_FIELD_CHARS) {
        if ch.is_control() && ch != '\t' {
            continue;
        }
        out.push(ch);
    }
    out
}

/// `%LOCALAPPDATA%\DevResidue`, falling back to the user profile / temp.
pub fn default_data_dir() -> Result<PathBuf, String> {
    // Portable-first (user decision 2026-09-08): all data lives NEXT TO the
    // running executable (`<exe dir>/Data`) so a portable distribution keeps
    // its scans/plans/journal/rules with it and leaves %LOCALAPPDATA% alone.
    // `DEVRESIDUE_DATA_DIR` still overrides everything (checked by callers).
    // Fallbacks only when the exe path cannot be determined (embedded/odd
    // hosts): LOCALAPPDATA → profile → temp.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // Only claim the portable location when the exe dir is a real,
            // writable-looking directory (guard against exotic hosts where
            // current_exe points somewhere unwritable).
            return Ok(exe_dir.join("Data"));
        }
    }
    if let Ok(root) = std::env::var("LOCALAPPDATA") {
        return Ok(PathBuf::from(root).join("DevResidue"));
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return Ok(PathBuf::from(profile).join(".devresidue"));
    }
    Ok(std::env::temp_dir().join("DevResidue"))
}

/// When a journal write fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("journal failure: {0}")]
pub struct JournalError(pub String);

/// attempt | result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JournalPhase {
    /// Logged right before the engine acts on an item.
    Attempt,
    /// Logged after the engine knows the outcome of an item.
    Result,
}

/// One journal line. This exact field list is the SPEC §24 whitelist; serde
/// refuses any extra key (`deny_unknown_fields`), so sensitive data cannot be
/// written into the audit trail by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRecord {
    /// Execution/session key (`CleanupPlanId` in the current 1:1 model).
    pub session_id: u64,
    /// Wall-clock time of the event.
    #[serde(with = "system_time")]
    pub time: SystemTime,
    pub phase: JournalPhase,
    /// Owning product/tool (e.g. `"npm"`).
    pub product: Option<String>,
    /// Classifying rule id, when applicable.
    pub rule: Option<u64>,
    /// Detecting provider id, when applicable.
    pub provider: Option<u64>,
    /// Target path (never user-supplied; always from the plan).
    pub path: Option<PathBuf>,
    /// Cleanup mode attempted.
    pub action: Option<CleanupMode>,
    /// Estimated reclaim in bytes.
    pub estimated_size: u64,
    /// Outcome token: success / skipped / failed / dry-run / (None=attempt).
    pub result: Option<String>,
    /// Human error or skip reason.
    pub error: Option<String>,
}

impl JournalRecord {
    /// Builds an attempt or result line for one planned item.
    #[allow(clippy::too_many_arguments)]
    pub fn for_item(
        session_id: u64,
        phase: JournalPhase,
        product: Option<String>,
        rule: Option<u64>,
        provider: Option<u64>,
        path: PathBuf,
        action: CleanupMode,
        estimated_size: u64,
        result: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            session_id,
            time: SystemTime::now(),
            phase,
            product,
            rule,
            provider,
            path: Some(path),
            action: Some(action),
            estimated_size,
            result,
            error,
        }
    }
}

fn today_stamp() -> String {
    // YYYYMMDD from the local clock.
    let now = time_now_secs();
    let days = now / 86_400;
    // Civil-from-days (Howard Hinnant algorithm) for a stable date stamp.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

/// Seconds since the Unix epoch (local-wall-clock approximation is fine for
/// file names; timestamps inside records remain full `SystemTime`s).
fn time_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Append-only JSONL journal rooted at `<base>/journal`.
#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
    /// Current shard name (e.g. `devresidue-20260904.jsonl`).
    current: PathBuf,
    max_bytes: u64,
    /// Whether appends are disabled (after a permanent error) — engine still
    /// degrades gracefully.
    broken: bool,
}

impl Journal {
    /// Opens (creating) the journal at the default data directory.
    pub fn open_default() -> Result<Self, JournalError> {
        let base =
            default_data_dir().map_err(|e| JournalError(format!("locate data directory: {e}")))?;
        Self::open_at(&base)
    }

    /// Opens the journal rooted under `base_dir/journal`.
    pub fn open_at(base_dir: &Path) -> Result<Self, JournalError> {
        Self::with_max_bytes_at(base_dir, MAX_FILE_BYTES)
    }

    /// Test hook: small roll threshold.
    pub fn with_max_bytes_at(base_dir: &Path, max_bytes: u64) -> Result<Self, JournalError> {
        let dir = base_dir.join("journal");
        fs::create_dir_all(&dir)
            .map_err(|e| JournalError(format!("create {}: {e}", dir.display())))?;
        let current = Self::first_shard(&dir);
        Ok(Self {
            dir,
            current,
            max_bytes,
            broken: false,
        })
    }

    /// The directory holding journal shards.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one record as a JSON line.
    ///
    /// String fields are sanitised first (F17): control characters are
    /// stripped and over-long values truncated, so a crafted `error`/`result`
    /// can never inject a fake journal line or balloon the file.
    pub fn append(&mut self, record: &JournalRecord) -> Result<(), JournalError> {
        if self.broken {
            return Err(JournalError(
                "journal is broken (previous write failed)".into(),
            ));
        }
        let line = serde_json::to_string(&sanitize_record(record))
            .map_err(|e| JournalError(format!("serialise record: {e}")))?;
        self.ensure_shard_ready(line.len() as u64 + 1)?;

        let path = self.dir.join(&self.current);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| JournalError(format!("open {}: {e}", path.display())))?;
        if let Err(e) = writeln!(file, "{line}") {
            self.broken = true;
            return Err(JournalError(format!("append {}: {e}", path.display())));
        }
        if let Err(e) = file.flush() {
            self.broken = true;
            return Err(JournalError(format!("flush {}: {e}", path.display())));
        }
        Ok(())
    }

    /// Picks the first shard whose size is below the roll threshold.
    fn ensure_shard_ready(&mut self, incoming: u64) -> Result<(), JournalError> {
        let current = self.dir.join(&self.current);
        let size = current.metadata().map(|m| m.len()).unwrap_or(0);
        if size + incoming <= self.max_bytes {
            return Ok(());
        }
        // Roll to the next free suffix (devresidue-YYYYMMDD.N.jsonl). The date
        // is derived from the current name so suffixes never stack.
        let date = self.date_part();
        for i in 1..10_000_u32 {
            let candidate = PathBuf::from(format!("devresidue-{date}.{i}.jsonl"));
            if !self.dir.join(&candidate).exists() {
                self.current = candidate;
                return Ok(());
            }
        }
        Err(JournalError(
            "too many journal shards for one day".to_string(),
        ))
    }

    /// The `YYYYMMDD` part of the current shard name.
    fn date_part(&self) -> String {
        let name = self.current.to_string_lossy();
        let Some(stripped) = name.strip_prefix("devresidue-") else {
            return today_stamp();
        };
        stripped.chars().take(8).collect()
    }

    fn first_shard(dir: &Path) -> PathBuf {
        let stamp = today_stamp();
        let base = dir.join(format!("devresidue-{stamp}.jsonl"));
        if !base.exists() {
            return base.file_name().unwrap_or_default().into();
        }
        // Pick the numerically largest existing shard for today so appends
        // continue the correct file.
        let mut chosen = stamp.clone();
        let mut idx = 0_u32;
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd {
                let Ok(entry) = entry else { continue };
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                let Some(stripped) = name.strip_prefix("devresidue-") else {
                    continue;
                };
                let Some(stripped) = stripped.strip_suffix(".jsonl") else {
                    continue;
                };
                let Some((date, suffix)) = stripped.split_once('.') else {
                    continue;
                };
                if date != stamp {
                    continue;
                }
                if let Ok(n) = suffix.parse::<u32>() {
                    if n > idx {
                        idx = n;
                    }
                }
            }
        }
        if idx > 0 {
            chosen = format!("{stamp}.{idx}");
        }
        PathBuf::from(format!("devresidue-{chosen}.jsonl"))
    }
}

/// Copies a record with every string field sanitised (F17): length-capped and
/// control-character-free. `path` is left untouched (Windows path components
/// cannot contain control characters, and paths are plan-derived).
fn sanitize_record(record: &JournalRecord) -> JournalRecord {
    JournalRecord {
        session_id: record.session_id,
        time: record.time,
        phase: record.phase,
        product: record.product.as_deref().map(sanitize_field),
        rule: record.rule,
        provider: record.provider,
        path: record.path.clone(),
        action: record.action,
        estimated_size: record.estimated_size,
        result: record.result.as_deref().map(sanitize_field),
        error: record.error.as_deref().map(sanitize_field),
    }
}

/// Deletes every journal shard under `base_dir/journal` (the user-initiated
/// "清空" action on the log page). Returns the number of shard files removed.
/// A missing directory is not an error (nothing to clear).
pub fn clear_all(base_dir: &Path) -> Result<usize, JournalError> {
    let dir = base_dir.join("journal");
    let mut removed = 0;
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(JournalError(format!(
                "read journal dir {}: {e}",
                dir.display()
            )))
        }
    };
    for entry in rd {
        let entry = entry.map_err(|e| JournalError(format!("journal dir entry: {e}")))?;
        let path = entry.path();
        let is_owned_shard = entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
            && is_journal_shard_name(&entry.file_name());
        if is_owned_shard {
            fs::remove_file(&path)
                .map_err(|e| JournalError(format!("remove {}: {e}", path.display())))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Returns whether a directory entry is one of the journal shard names owned
/// by DevResidue. Unknown files, directories and reparse/symlink entries are
/// deliberately left untouched during maintenance.
fn is_journal_shard_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else { return false };
    let Some(rest) = name.strip_prefix("devresidue-") else { return false };
    let Some(stem) = rest.strip_suffix(".jsonl") else { return false };
    let mut parts = stem.split('.');
    let Some(date) = parts.next() else { return false };
    if date.len() != 8 || !date.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(suffix) => {
            !suffix.is_empty()
                && suffix.chars().all(|ch| ch.is_ascii_digit())
                && parts.next().is_none()
        }
    }
}

/// Reads journal shards newest-first and returns the last `limit` records in
/// chronological order.
pub fn read_last(base_dir: &Path, limit: usize) -> Result<Vec<JournalRecord>, JournalError> {
    let dir = base_dir.join("journal");
    let mut shards: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for entry in rd {
            let Ok(entry) = entry else { continue };
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                shards.push(p);
            }
        }
    }
    // Sort so multi-shard days merge oldest-shard-first. The base shard for a
    // date ("devresidue-YYYYMMDD.jsonl") sorts *before* its suffixed rolls
    // ("devresidue-YYYYMMDD.N.jsonl") under a numeric-aware comparison.
    let mut ordered: Vec<(String, u64, PathBuf)> = Vec::new();
    for shard in shards {
        let name = shard
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (date, num) = shard_key(&name);
        ordered.push((date, num, shard));
    }
    ordered.sort_by(|a, b| (a.0.as_str(), a.1).cmp(&(b.0.as_str(), b.1)));

    let mut lines: Vec<JournalRecord> = Vec::new();
    for (_, _, shard) in ordered {
        let file = fs::File::open(&shard)
            .map_err(|e| JournalError(format!("open {}: {e}", shard.display())))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| JournalError(format!("read {}: {e}", shard.display())))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalRecord>(&line) {
                Ok(record) => lines.push(record),
                Err(e) => {
                    return Err(JournalError(format!(
                        "corrupt journal line in {}: {e}",
                        shard.display()
                    )))
                }
            }
        }
    }
    if lines.len() <= limit {
        Ok(lines)
    } else {
        Ok(lines[lines.len() - limit..].to_vec())
    }
}

/// Parses a shard file name into `(date, roll_index)` for ordering
/// (`devresidue-YYYYMMDD.jsonl` → index 0, `.N.jsonl` → N).
fn shard_key(name: &str) -> (String, u64) {
    let Some(stripped) = name.strip_prefix("devresidue-") else {
        return (String::new(), 0);
    };
    let Some(stripped) = stripped.strip_suffix(".jsonl") else {
        return (String::new(), 0);
    };
    match stripped.split_once('.') {
        Some((date, num)) => (date.to_string(), num.parse().unwrap_or(0)),
        None => (stripped.to_string(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::action::CleanupMode;
    use std::path::PathBuf;

    fn tmp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("devresidue-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        base
    }

    fn sample_record(seed: u64) -> JournalRecord {
        JournalRecord::for_item(
            seed,
            JournalPhase::Result,
            Some("npm".into()),
            Some(2),
            Some(1),
            PathBuf::from(r"C:\Users\demo\AppData\Local\npm-cache"),
            CleanupMode::ExternalCommand,
            1024,
            Some("success".into()),
            None,
        )
    }

    #[test]
    fn journal_fields_are_the_whitelist_and_secrets_are_rejected() {
        let record = sample_record(7);
        let value: serde_json::Value = serde_json::to_value(&record).expect("serialise");
        let map = value.as_object().expect("object");
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "action",
                "error",
                "estimated_size",
                "path",
                "phase",
                "product",
                "provider",
                "result",
                "rule",
                "session_id",
                "time",
            ]
        );

        // A JSON line that smuggles a secret field must NOT parse: the record
        // struct is deny_unknown_fields, i.e. the field list is the whitelist.
        let mut line = serde_json::to_string(&record).unwrap();
        line = line.trim_end_matches('}').to_string();
        line.push_str(",\"token\":\"SECRET\"}");
        assert!(
            serde_json::from_str::<JournalRecord>(&line).is_err(),
            "extra (secret) field must be refused"
        );
    }

    #[test]
    fn append_then_read_last_round_trips_records() {
        let base = tmp_base("journal-rt");
        let mut journal = Journal::open_at(&base).expect("open");
        for i in 0..5 {
            journal.append(&sample_record(i)).expect("append");
        }
        let records = read_last(&base, 3).expect("read");
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].session_id, 2);
        assert_eq!(records[2].session_id, 4);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn oversized_shard_rolls_to_a_suffixed_file() {
        let base = tmp_base("journal-roll");
        let mut journal = Journal::with_max_bytes_at(&base, 200).expect("open");
        // Each serialised record is well above 200 bytes, so every append must
        // roll to a fresh shard.
        for i in 0..5u64 {
            journal.append(&sample_record(i)).expect("append");
        }
        let files = fs::read_dir(base.join("journal")).expect("dir").count();
        assert!(
            files >= 2,
            "expected multiple journal shards after roll-over, got {files}"
        );
        // All records are readable across shards, oldest first.
        let records = read_last(&base, 100).expect("read");
        assert_eq!(records.len(), 5);
        let ids: Vec<u64> = records.iter().map(|r| r.session_id).collect();
        assert_eq!(ids, (0..5).collect::<Vec<u64>>());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn default_dir_resolves_from_env_without_error() {
        // Guard: whatever the machine layout, resolution must not panic.
        let _ = default_data_dir();
    }

    #[test]
    fn fields_are_truncated_and_control_chars_stripped_on_append() {
        // F17: a crafted error field cannot inject new journal lines or grow
        // the record without bound.
        let base = tmp_base("journal-sanitize");
        let mut journal = Journal::open_at(&base).expect("open");

        let mut record = sample_record(1);
        record.error = Some(format!(
            "bad\x00payload\nforged-line\r{}\n",
            "x".repeat(MAX_FIELD_CHARS * 2)
        ));
        record.result = Some("ok\x1b[31mred".into());
        journal.append(&record).expect("append");

        // Exactly one physical line on disk (no injection).
        let shard = std::fs::read_dir(base.join("journal"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(&shard).unwrap();
        assert_eq!(text.lines().count(), 1, "no forged line may appear: {text}");

        let back = read_last(&base, 1).expect("read");
        assert_eq!(back.len(), 1);
        let err = back[0].error.as_deref().unwrap();
        assert!(
            err.chars().count() <= MAX_FIELD_CHARS,
            "error must be length-capped, got {} chars",
            err.chars().count()
        );
        assert!(
            !err.contains('\n') && !err.contains('\r') && !err.contains('\x00'),
            "control characters must be stripped: {err:?}"
        );
        assert_eq!(
            back[0].result.as_deref(),
            Some("ok[31mred"),
            "control char stripped, remaining printable text kept"
        );
        let _ = fs::remove_dir_all(&base);
    }
}
