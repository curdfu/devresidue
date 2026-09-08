//! uv cache provider.
//!
//! Tool-reported path: `uv cache dir` (5 s timeout). Default (Mole Windows
//! reference): `%LOCALAPPDATA%\uv\cache`. Cleanup is tool-native
//! `uv cache clean`.

use std::path::PathBuf;

use devresidue_core::{RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

use super::common::{emit, query_verified, CacheCandidate};

pub const PROVIDER: &str = "uv";

/// Default cache directory (Mole Windows reference).
fn default_dir(ctx: &ScanContext) -> Option<PathBuf> {
    ctx.env("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("uv").join("cache"))
}

/// Scans for the uv cache.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let Some(default) = default_dir(ctx) else {
        return Vec::new();
    };
    let mut items = Vec::new();

    match query_verified(ctx, "uv", &["cache", "dir"], "uv") {
        Err(reason) => {
            ctx.warn(format!("uv provider skipped: {reason}"));
            return items;
        }
        Ok(Some(path)) => {
            items.extend(emit(
                ctx,
                CacheCandidate {
                    provider: PROVIDER,
                    path: path.clone(),
                    title: "uv cache",
                    product: "uv",
                    risk: RiskLevel::RegenerableDownload,
                    explanation: "uv (python package manager) download cache; deleting triggers \
                                  re-downloads (regenerable-download). Cleaned via uv cache clean."
                        .into(),
                    // R06 + F08: the verified cache root is frozen into the
                    // scope binding AND passed to uv through its documented
                    // cache-directory option. `uv cache clean` takes *package*
                    // names as positional arguments — an absolute Windows path
                    // there would be parsed as a package name and silently do
                    // nothing (F08, verified against the official CLI docs).
                    action: super::common::tool_cache_command(
                        "uv",
                        vec![
                            "cache".into(),
                            "clean".into(),
                            "--cache-dir".into(),
                            path.display().to_string(),
                        ],
                        "uv",
                        vec!["cache".into(), "dir".into()],
                        "uv",
                        &path,
                        None,
                        300,
                    ),
                    source: SourceKind::PackageManager,
                    notes: vec!["tool-reported path (uv cache dir)".into()],
                },
            ));
            return items;
        }
        Ok(None) => {}
    }

    // Tool-missing fallback (`fallback_cache_action`): when uv itself is
    // absent, a scoped `uv cache clean` could never run and the execution-time
    // re-query would hard-refuse (tool-scope-drift) — a plain deletable item
    // keeps the leftover cache cleanable through the verified delete.
    let action = super::common::fallback_cache_action(
        "uv",
        vec![
            "cache".into(),
            "clean".into(),
            "--cache-dir".into(),
            default.display().to_string(),
        ],
        vec!["cache".into(), "dir".into()],
        &default,
        None,
        300,
    );
    items.extend(emit(
        ctx,
        CacheCandidate {
            provider: PROVIDER,
            path: default.clone(),
            title: "uv cache",
            product: "uv",
            risk: RiskLevel::RegenerableDownload,
            explanation: "uv download cache (known default location); deleting triggers re-downloads (regenerable-download).".into(),
            action,
            source: SourceKind::DeveloperCacheProvider,
            notes: vec![
                "known default location (%LOCALAPPDATA%\\uv\\cache)".into(),
            ],
        },
    ));
    items
}
