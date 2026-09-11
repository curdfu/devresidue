//! Privacy-bounded optional AI review for DevResidue.
//!
//! This crate owns metadata sanitization, the OpenAI-compatible transport,
//! response validation, transient review state and bounded audit records. It
//! depends on Core contracts but not on Tauri, React, Providers, snapshot
//! storage or cleanup APIs. Callers validate the current snapshot locally and
//! pass already-registered [`devresidue_core::ScanItem`] values plus a scan
//! generation; only an explicit user confirmation can invoke Core's atomic
//! local rule transaction.

#![forbid(unsafe_code)]

pub mod audit;
pub mod confirm;
pub mod profile;
pub mod sanitize;
pub mod service;
pub mod transport;
pub mod validate;

pub use audit::{AiAuditEvent, AiAuditLog, AiAuditRecord, AiAuditResult, MAX_AUDIT_BYTES};
pub use confirm::{
    AiAuditWarning, AiConfirmationConfig, AiConfirmationResult, AiConfirmationSelection,
};
pub use devresidue_core::ai::{PendingProfileTxn, RecoveryStatus};
pub use profile::{AiProfileInput, AiProfileStore, StoredProfiles};
pub use sanitize::MetadataSanitizer;
pub use service::{AiAdvisorService, AiReviewBatch};
pub use transport::{
    AiCancellationToken, AiServiceError, AiServiceErrorKind, AiTransport, OpenAiCompatibleTransport,
};
pub use validate::ResponseValidator;
