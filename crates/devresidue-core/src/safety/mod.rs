//! Safety validation — the pre-delete revalidation layer (SPEC §15/§16/§17/
//! §18/§31, PLAN Phase 5). Highest-priority module in the product.
//!
//! ```text
//! probe.rs      ports (PathProbe / ProcessProbe) + platform-neutral DTOs
//! canonical.rs  pure-lexical Windows path normalisation (core-owned mirror)
//! path.rs       lexical acceptance of a cleanup target vs its snapshot
//! reparse.rs    DO-NOT-FOLLOW guard: no reparse ancestors, no reparse target
//! identity.rs   scan-time snapshot capture + NTFS fingerprint comparison
//! process.rs    process guard (SPEC §12): Defer on running, Deny on unknown
//! protected.rs  protected-root registry (SPEC §18, INV-006/007)
//! validator.rs  SafetyValidator orchestration + SafetyVerdict/DenyReason
//! ```
//!
//! Core depends on the platform crate **never**; the dependency points the
//! other way. This module programs against the probe traits, and the Windows
//! adapters (in `devresidue-platform-windows/src/safety`) implement them.

pub mod canonical;
pub mod identity;
pub mod path;
pub mod probe;
pub mod process;
pub mod protected;
pub mod reparse;
pub mod validator;

#[cfg(test)]
pub(crate) mod fake;

pub use probe::{
    AttrFlags, FileIdentity, FileLock, PathProbe, ProbeError, ProcessProbe, ProcessState,
    ReparseInfo, ReparseKind,
};
pub use protected::{ProtectedRootRegistry, RegistryInitError, RootHit, RootHitKind};
pub use validator::{DeferReason, DenyReason, SafetyValidator, SafetyVerdict, ValidationRequest};
