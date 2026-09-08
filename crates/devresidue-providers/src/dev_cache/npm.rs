//! npm cache provider (SPEC §10 first batch).
//!
//! Tool-reported path: `npm config get cache` (5 s timeout). Default (Mole
//! Windows reference): `%LOCALAPPDATA%\npm-cache`. Cleanup is tool-native
//! `npm cache clean --force` (SPEC §10 preferred strategy).

use std::path::PathBuf;

use devresidue_core::{RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

use super::common::{emit, query_verified, CacheCandidate};

pub const PROVIDER: &str = "npm";

/// Default cache directory (Mole Windows reference).
fn default_dir(ctx: &ScanContext) -> Option<PathBuf> {
    ctx.env("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("npm-cache"))
}

/// Scans for the npm cache.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let Some(default) = default_dir(ctx) else {
        return Vec::new();
    };

    let mut items = Vec::new();

    // Tool-reported first (INV-011 verified).
    match query_verified(ctx, "npm", &["config", "get", "cache"], "npm") {
        Err(reason) => {
            ctx.warn(format!("npm provider skipped: {reason}"));
            return items;
        }
        Ok(Some(path)) => {
            items.extend(emit(
                ctx,
                CacheCandidate {
                    provider: PROVIDER,
                    path: path.clone(),
                    title: "npm cache",
                    product: "npm",
                    risk: RiskLevel::RegenerableDownload,
                    explanation: "npm download cache; deleting triggers re-downloads for future \
                                  installs (regenerable-download, SPEC §8). Cleaned via the \
                                  tool-native command."
                        .into(),
                    // R06: `npm cache clean --force` takes no cache path, so the
                    // verified root is frozen as a re-verified scope instead.
                    action: super::common::tool_cache_command(
                        "npm",
                        vec!["cache".into(), "clean".into(), "--force".into()],
                        "npm",
                        vec!["config".into(), "get".into(), "cache".into()],
                        "npm",
                        &path,
                        None,
                        300,
                    ),
                    source: SourceKind::PackageManager,
                    notes: vec!["tool-reported path (npm config get cache)".into()],
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
            title: "npm cache",
            product: "npm",
            risk: RiskLevel::RegenerableDownload,
            explanation: "npm download cache (known default location); deleting triggers \
                          re-downloads for future installs (regenerable-download)."
                .into(),
            // Tool-missing fallback (fallback_cache_action): a plain deletable
            // item when npm itself is absent, otherwise the scoped command.
            action: super::common::fallback_cache_action(
                "npm",
                vec!["cache".into(), "clean".into(), "--force".into()],
                vec!["config".into(), "get".into(), "cache".into()],
                &default,
                None,
                300,
            ),
            source: SourceKind::DeveloperCacheProvider,
            notes: vec!["known default location (LOCALAPPDATA\\npm-cache)".into()],
        },
    ));
    items
}
