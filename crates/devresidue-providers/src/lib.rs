//! DevResidue discovery-only providers.
//!
//! Providers detect, measure and explain residue data. They **never** delete
//! anything (INV-009) — the maximum they may express is the
//! [`CleanupAction`] *intention* stored on a [`ScanItem`], which only the
//! CleanupEngine may act on.
//!
//! ```text
//! project/    project/build-artifact discovery via kondo-lib (Phase 7, read-only, INV-008)
//! dev_cache/  developer cache providers first batch (Phase 8: npm/bun/pip/uv/cargo/nuget)
//! agents/     AI agent data providers        (Phase 9)
//! unknown/    unknown developer data provider (Phase 13, SPEC §25)
//! package/    reserved: package-store discovery (future)
//! fixtures.rs built-in fixture provider used by CLI acceptance/demo
//! measure.rs  shared safe tree measurement (no symlink/junction descent)
//! scan_ctx.rs injected scan context (env/tool/cancellation/dedupe/id allocation/rules)
//! registry.rs stable provider slug ↔ id table
//! scan_store.rs last-scan.json persistence for the plan command
//! ```

pub mod agents;
pub mod classify;
pub mod dev_cache;
pub mod fixtures;
pub mod measure;
pub mod package;
pub mod project;
pub mod registry;
pub mod scan_ctx;
pub mod scan_store;
pub mod unknown;

use devresidue_core::ScanItem;

use scan_ctx::ScanContext;

/// Common contract of every legacy fixture-style provider.
///
/// Real providers (dev-cache, project) expose `scan(ctx: &ScanContext)`
/// functions instead — see [`scan_real`].
pub trait ResidueProvider {
    /// Stable machine-readable provider name (e.g. `"fixture"`, `"npm"`).
    fn name(&self) -> &'static str;

    /// Runs a discovery scan and returns the found items.
    ///
    /// The contract is discovery + explanation only. No provider may delete,
    /// move or mutate residue data from here.
    fn scan(&self) -> Vec<ScanItem>;
}

/// Runs every real provider (dev caches + kondo projects) over one context.
pub fn scan_real(ctx: &ScanContext) -> Vec<ScanItem> {
    let mut items = Vec::new();
    items.extend(dev_cache::scan(ctx));
    items.extend(project::scan(ctx));
    items
}
