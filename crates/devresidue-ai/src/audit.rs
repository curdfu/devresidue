//! Bounded, sensitive-data-free AI lifecycle audit.
//!
//! This log is intentionally separate from the cleanup journal. Its record
//! shape is closed: there is no slot for a path, model text, HTTP payload,
//! header, API key or arbitrary detail string.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use devresidue_core::ai::AiProfileId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum retained UTF-8 bytes for the complete audit file.
pub const MAX_AUDIT_BYTES: usize = 64 * 1024;

/// Closed lifecycle event vocabulary for the AI audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiAuditEvent {
    ConfirmWritten,
    ConfirmRolledBack,
}

/// Closed outcome vocabulary for the AI audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiAuditResult {
    Ok,
    Failed,
    RolledBack,
}

/// The complete white-listed, serializable audit record shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiAuditRecord {
    pub ts: i64,
    pub event: AiAuditEvent,
    pub profile_id: AiProfileId,
    pub count: u32,
    pub result: AiAuditResult,
    pub elapsed_ms: u64,
    pub request_id: String,
}

/// File-backed, bounded writer for [`AiAuditRecord`] JSON lines.
#[derive(Clone)]
pub struct AiAuditLog {
    data_dir: PathBuf,
    path: PathBuf,
}

impl AiAuditLog {
    #[must_use]
    pub fn open(data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref().to_path_buf();
        Self {
            path: data_dir.join("ai-audit.jsonl"),
            data_dir,
        }
    }

    /// Appends one closed-shape record, retaining only the newest complete
    /// JSONL lines within [`MAX_AUDIT_BYTES`].
    pub fn record(
        &self,
        event: AiAuditEvent,
        profile_id: AiProfileId,
        count: u32,
        result: AiAuditResult,
        elapsed_ms: u64,
    ) -> Result<(), String> {
        let record = AiAuditRecord {
            ts: epoch_seconds(),
            event,
            profile_id,
            count,
            result,
            elapsed_ms,
            request_id: Uuid::new_v4().to_string(),
        };
        let line = serde_json::to_string(&record)
            .map_err(|_| "unable to serialize AI audit record".to_string())?;
        if line.len() + 1 > MAX_AUDIT_BYTES {
            return Err("AI audit record exceeds the bounded audit capacity".to_string());
        }
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| "unable to create AI audit directory".to_string())?;
        let mut content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(_) => return Err("unable to read AI audit log".to_string()),
        };
        content.push_str(&line);
        content.push('\n');
        let retained = retain_newest_complete_lines(content);
        fs::write(&self.path, retained).map_err(|_| "unable to write AI audit log".to_string())
    }
}

fn epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn retain_newest_complete_lines(content: String) -> String {
    if content.len() <= MAX_AUDIT_BYTES {
        return content;
    }
    let excess = content.len() - MAX_AUDIT_BYTES;
    let start = content[excess..]
        .find('\n')
        .map(|offset| excess + offset + 1)
        .unwrap_or(content.len());
    content[start..].to_string()
}
