//! DevResidue platform-neutral core.
//!
//! This crate owns the domain model and the safety contracts every other layer
//! (providers, platform, CLI, future Tauri UI) must honour. It **never**
//! depends on Tauri, React, WebView or any presentation concern
//! (SPEC §4: "Core 不依赖 Tauri、React 或 WebView").
//!
//! # Safety responsibilities
//!
//! The types re-exported here enforce the non-negotiable constraints from
//! SPEC §31 and PLAN §26:
//!
//! - `ScanItemId` / `CleanupPlanId` are strong types that can **never** be
//!   constructed from a `Path` (INV-013, PLAN §4).
//! - `RiskLevel::Protected` / `RiskLevel::Unknown` items never enter the
//!   normal cleanup queue (INV-001 / INV-002).
//! - The only deletion authority is the future `CleanupEngine`
//!   (`crate::cleanup`, implemented in Phase 6). No provider, rule engine,
//!   AI module or presentation layer may delete.
//!
//! # Module layout
//!
//! ```text
//! domain/   the domain model itself (implemented)
//! rules/    rule engine skeleton          (Phase 3)
//! risk/     risk classifier skeleton      (Phase 5)
//! cleanup/  cleanup planner/engine        (Phase 6)
//! safety/   safety validator              (Phase 5)
//! journal/  cleanup journal               (Phase 6)
//! ```

pub mod cleanup;
pub mod domain;
pub mod integrity;
pub mod journal;
pub mod risk;
pub mod rules;
pub mod safety;

// ---- Public domain model re-exports -------------------------------------

pub use domain::action::{CleanupAction, CleanupMode, ExternalCommandSpec};
pub use domain::category::ResidueCategory;
pub use domain::evidence::Evidence;
pub use domain::ids::{CleanupPlanId, IdParseError, ProviderId, RuleId, ScanItemId};
pub use domain::plan::{CleanupPlan, CleanupPlanItem};
pub use domain::risk_level::RiskLevel;
pub use domain::scan_item::ScanItem;
pub use domain::session::{CleanupResult, CleanupSession, CleanupStatus, SessionTotals};
pub use domain::snapshot::TargetSnapshot;
pub use domain::source::SourceKind;
