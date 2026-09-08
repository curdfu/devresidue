//! Cleanup planner & engine — the **only deletion path** (INV-010, PLAN
//! Phase 6).
//!
//! ```text
//! port.rs       DeletePort trait — the sole seam through which deletion may
//!               happen (implemented by the Windows platform adapter)
//! planner.rs    CleanupPlanner: selection + SPEC §19 risk gate + snapshot
//! plan_store.rs Plan persistence (JSON under %LOCALAPPDATA%\DevResidue\plans)
//! engine.rs     CleanupEngine: per-item revalidation → execute / skip /
//!               fail; dry-run shares the identical pipeline (SPEC §32)
//! ```
//!
//! Invariants honoured here:
//!
//! - the engine is the **single deletion authority**: it is the only component
//!   that calls [`DeletePort`]. No provider, rule engine, CLI or UI command
//!   accepts an arbitrary path for deletion (INV-013).
//! - `Protected` / `Unknown` items never enter a plan (INV-001 / INV-002).
//! - every item is revalidated against its scan-time snapshot right before
//!   execution (INV-005, SPEC §16).
//! - dry-run and real execution consume the same plan and the same validation
//!   outcomes (SPEC §32 Dry-Run Consistency).

pub mod engine;
pub mod plan_store;
pub mod planner;
pub mod port;

pub use engine::{CleanupEngine, EngineError, EngineOptions, EngineRun};
pub use plan_store::{PlanStore, PlanStoreError};
pub use planner::{
    CleanupPlanner, ConfirmPolicy, PlannerError, PlannerOutput, SkipReason, SkipRecord,
};
pub use port::{DeleteError, DeletePort};
