//! NuGet packages cache provider.
//!
//! No official `nuget cache clean` equivalent for the global packages folder,
//! so the conservative `RecycleBin` strategy is used (SPEC §10 fallback).
//! Default (Mole Windows reference): `%USERPROFILE%\.nuget\packages`.

use std::path::PathBuf;

use devresidue_core::{CleanupAction, RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

use super::common::{emit, CacheCandidate};

pub const PROVIDER: &str = "nuget";

/// Default packages directory (Mole Windows reference).
fn default_dir(ctx: &ScanContext) -> Option<PathBuf> {
    ctx.env("USERPROFILE")
        .map(|root| PathBuf::from(root).join(".nuget").join("packages"))
}

/// Scans for the NuGet global packages cache.
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let Some(default) = default_dir(ctx) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    items.extend(emit(
        ctx,
        CacheCandidate {
            provider: PROVIDER,
            path: default,
            title: "NuGet global packages",
            product: "NuGet",
            risk: RiskLevel::RegenerableDownload,
            explanation: "NuGet global package store; packages are re-downloadable on demand \
                          (regenerable-download). No tool-native clean command for this \
                          folder — conservative RecycleBin strategy."
                .into(),
            action: CleanupAction::RecycleBin,
            source: SourceKind::DeveloperCacheProvider,
            notes: vec!["known default location (%USERPROFILE%\\.nuget\\packages)".into()],
        },
    ));
    items
}
