//! pip cache provider.
//!
//! Tool-reported path: `pip cache dir` (5 s timeout). Default (Mole Windows
//! reference): `%LOCALAPPDATA%\pip\cache`. Cleanup is tool-native
//! `pip cache purge`.

use std::path::PathBuf;

use devresidue_core::{RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

use super::common::{emit, query_verified, CacheCandidate};

pub const PROVIDER: &str = "pip";

/// Default cache directory (Mole Windows reference).
fn default_dir(ctx: &ScanContext) -> Option<PathBuf> {
    ctx.env("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("pip").join("cache"))
}

/// Scans for the pip cache.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let Some(default) = default_dir(ctx) else {
        return Vec::new();
    };
    let mut items = Vec::new();

    match query_verified(ctx, "pip", &["cache", "dir"], "pip") {
        Err(reason) => {
            ctx.warn(format!("pip provider skipped: {reason}"));
            return items;
        }
        Ok(Some(path)) => {
            items.extend(emit(
                ctx,
                CacheCandidate {
                    provider: PROVIDER,
                    path: path.clone(),
                    title: "pip cache",
                    product: "pip",
                    risk: RiskLevel::RegenerableDownload,
                    explanation: "pip wheel download cache; deleting triggers re-downloads \
                                  (regenerable-download). Cleaned via pip cache purge."
                        .into(),
                    // R06: `pip cache purge` takes no path; frozen scope only.
                    action: super::common::tool_cache_command(
                        "pip",
                        vec!["cache".into(), "purge".into()],
                        "pip",
                        vec!["cache".into(), "dir".into()],
                        "pip",
                        &path,
                        None,
                        300,
                    ),
                    source: SourceKind::PackageManager,
                    notes: vec!["tool-reported path (pip cache dir)".into()],
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
            title: "pip cache",
            product: "pip",
            risk: RiskLevel::RegenerableDownload,
            explanation: "pip wheel download cache (known default location); deleting triggers \
                          re-downloads (regenerable-download)."
                .into(),
            // Tool-missing fallback (fallback_cache_action): a plain deletable
            // item when pip itself is absent, otherwise the scoped command.
            action: super::common::fallback_cache_action(
                "pip",
                vec!["cache".into(), "purge".into()],
                vec!["cache".into(), "dir".into()],
                &default,
                None,
                300,
            ),
            source: SourceKind::DeveloperCacheProvider,
            notes: vec!["known default location (%LOCALAPPDATA%\\pip\\cache)".into()],
        },
    ));
    items
}
