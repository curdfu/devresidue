//! Project/build-artifact discovery — kondo-lib wrapper (Phase 7).
//!
//! **Hard constraint (INV-008):** kondo-lib may only discover. Its delete
//! capability (`Project::clean` / top-level `clean`) is never called anywhere
//! in this crate.

pub mod kondo;

use devresidue_core::ScanItem;

use crate::scan_ctx::ScanContext;

/// Runs project/artifact discovery over the context's workspace roots.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    kondo::scan(ctx)
}
