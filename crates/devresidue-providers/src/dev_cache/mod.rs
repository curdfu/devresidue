//! Developer cache providers — first batch (SPEC §10).
//!
//! One module per package manager; each discovers its cache, prefers a
//! **tool-reported path** (verified through INV-011, never trusted blindly),
//! falls back to a known default from the Mole Windows reference, measures
//! with the shared walker and attaches a cleanup strategy:
//!
//! - tool-native command (`npm cache clean --force`, `pip cache purge`,
//!   `uv cache clean`, `bun pm cache rm`) — SPEC §10 preferred;
//! - `RecycleBin` where no official command exists (cargo, NuGet).
//!
//! Invariants honoured: never emit a whole home / drive root (INV-006);
//! never emit `.cargo/bin` or config; junction/symlink never descended.

pub mod bun;
pub mod cargo;
pub mod common;
pub mod npm;
pub mod nuget;
pub mod pip;
pub mod static_cache;
pub mod uv;

use devresidue_core::ScanItem;

use crate::scan_ctx::{ProgressEvent, ScanContext};

/// One registered dev-cache provider (registry slug + scan entry point).
struct CacheProvider {
    slug: &'static str,
    scan: fn(&ScanContext) -> Vec<ScanItem>,
}

/// The first batch in fixed registry order (SPEC §10).
const FIRST_BATCH: [CacheProvider; 7] = [
    CacheProvider {
        slug: npm::PROVIDER,
        scan: npm::scan,
    },
    CacheProvider {
        slug: bun::PROVIDER,
        scan: bun::scan,
    },
    CacheProvider {
        slug: pip::PROVIDER,
        scan: pip::scan,
    },
    CacheProvider {
        slug: uv::PROVIDER,
        scan: uv::scan,
    },
    CacheProvider {
        slug: cargo::PROVIDER,
        scan: cargo::scan,
    },
    CacheProvider {
        slug: nuget::PROVIDER,
        scan: nuget::scan,
    },
    // Rule-driven static cache locations (Mole-extracted knowledge): the
    // LAST dev-cache provider so the tool-query providers claim their
    // (verified) roots first; the static provider only emits rule locations
    // no earlier provider already reported (shared seen-set).
    CacheProvider {
        slug: static_cache::PROVIDER,
        scan: static_cache::scan,
    },
];

/// Runs every dev-cache provider in the first batch.
///
/// Coarse progress (one start/done pair per provider) is reported through the
/// context so the CLI can render a `Scanning: <slug>...` line per provider.
/// Cancellation takes effect between providers — and inside each provider's
/// own emit/measure boundaries — so an interrupted scan keeps partial results.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let mut items = Vec::new();
    for provider in FIRST_BATCH {
        if ctx.cancelled() {
            break;
        }
        ctx.progress(ProgressEvent::ProviderStart(provider.slug));
        items.extend((provider.scan)(ctx));
        if ctx.cancelled() {
            break;
        }
        ctx.progress(ProgressEvent::ProviderDone(provider.slug));
    }
    items
}
