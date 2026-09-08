//! bun cache provider.
//!
//! Tool-reported path: `bun pm cache` (5 s timeout). Default (Mole Windows
//! reference): `%USERPROFILE%\.bun\install\cache`. Cleanup is tool-native
//! `bun pm cache rm`.

use std::path::PathBuf;

use devresidue_core::{RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

use super::common::{emit, query_verified, CacheCandidate};

pub const PROVIDER: &str = "bun";

/// Default cache directory (Mole Windows reference).
fn default_dir(ctx: &ScanContext) -> Option<PathBuf> {
    ctx.env("USERPROFILE").map(|root| {
        PathBuf::from(root)
            .join(".bun")
            .join("install")
            .join("cache")
    })
}

/// Scans for the bun cache.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let Some(default) = default_dir(ctx) else {
        return Vec::new();
    };
    let mut items = Vec::new();

    match query_verified(ctx, "bun", &["pm", "cache"], "bun") {
        Err(reason) => {
            ctx.warn(format!("bun provider skipped: {reason}"));
            return items;
        }
        Ok(Some(path)) => {
            items.extend(emit(
                ctx,
                CacheCandidate {
                    provider: PROVIDER,
                    path: path.clone(),
                    title: "bun install cache",
                    product: "bun",
                    risk: RiskLevel::RegenerableDownload,
                    explanation: "bun package download cache; deleting triggers re-downloads \
                                  (regenerable-download). Cleaned via bun pm cache rm."
                        .into(),
                    // R06: `bun pm cache rm` takes no path; frozen scope only.
                    action: super::common::tool_cache_command(
                        "bun",
                        vec!["pm".into(), "cache".into(), "rm".into()],
                        "bun",
                        vec!["pm".into(), "cache".into()],
                        "bun",
                        &path,
                        None,
                        120,
                    ),
                    source: SourceKind::PackageManager,
                    notes: vec!["tool-reported path (bun pm cache)".into()],
                },
            ));
            return items;
        }
        Ok(None) => {}
    }

    items.extend(emit(
        ctx,
        CacheCandidate {
            provider: PROVIDER,
            path: default.clone(),
            title: "bun install cache",
            product: "bun",
            risk: RiskLevel::RegenerableDownload,
            explanation: "bun package download cache (known default location); deleting \
                          triggers re-downloads (regenerable-download)."
                .into(),
            // Tool-missing fallback (fallback_cache_action): a plain deletable
            // item when bun itself is absent, otherwise the scoped command.
            action: super::common::fallback_cache_action(
                "bun",
                vec!["pm".into(), "cache".into(), "rm".into()],
                vec!["pm".into(), "cache".into()],
                &default,
                None,
                120,
            ),
            source: SourceKind::DeveloperCacheProvider,
            notes: vec!["known default location (%USERPROFILE%\\.bun\\install\\cache)".into()],
        },
    ));
    items
}
