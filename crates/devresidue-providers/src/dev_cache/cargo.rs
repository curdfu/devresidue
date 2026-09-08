//! cargo cache provider — **split into sub-directories**; the `.cargo` root is
//! never emitted whole (SPEC §10: never "discover ~/.xxx → delete").
//!
//! - `registry/cache` — `.crate` download cache → `RegenerableDownload`;
//! - `registry/src` — extracted sources → `RegenerableDownload`;
//! - `git/checkouts` — git-dependency checkouts with no registry reference to
//!   restore them from → `Review`.
//!
//! `.cargo/bin` and `.cargo/config*` are never emitted (Protected at the rule
//! layer; providers simply have no deletion surface for them).
//!
//! There is no official `cargo cache clean` command, so the conservative
//! `RecycleBin` strategy is used (recoverable).

use std::path::{Path, PathBuf};

use devresidue_core::{CleanupAction, RiskLevel, ScanItem, SourceKind};

use crate::scan_ctx::ScanContext;

use super::common::{emit, CacheCandidate};

pub const PROVIDER: &str = "cargo";
const PRODUCT: &str = "cargo";

/// `.cargo` home: `$CARGO_HOME` or `%USERPROFILE%\.cargo`.
fn cargo_home(ctx: &ScanContext) -> Option<PathBuf> {
    if let Some(home) = ctx.env("CARGO_HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    ctx.env("USERPROFILE")
        .map(|root| PathBuf::from(root).join(".cargo"))
}

fn split(ctx: &ScanContext, items: &mut Vec<ScanItem>, cargo_home: &Path) {
    let base = |sub: &str| cargo_home.join(sub);
    let mk = |items: &mut Vec<ScanItem>,
              path: PathBuf,
              title: &'static str,
              risk: RiskLevel,
              explanation: String,
              note: String| {
        items.extend(emit(
            ctx,
            CacheCandidate {
                provider: PROVIDER,
                path,
                title,
                product: PRODUCT,
                risk,
                explanation,
                action: CleanupAction::RecycleBin,
                source: SourceKind::DeveloperCacheProvider,
                notes: vec![note],
            },
        ));
    };

    let cache = base("registry").join("cache");
    if cache.exists() {
        mk(
            items,
            cache.clone(),
            "cargo registry cache",
            RiskLevel::RegenerableDownload,
            "cargo .crate download cache; re-fetchable from the registry \
             (regenerable-download). Recycle-bin strategy (no official cargo \
             cache-clean command)."
                .into(),
            format!(
                "sub-item of {} (never the .cargo root)",
                cargo_home.display()
            ),
        );
    }

    let src = base("registry").join("src");
    if src.exists() {
        mk(
            items,
            src.clone(),
            "cargo registry sources",
            RiskLevel::RegenerableDownload,
            "extracted cargo registry sources; re-extractable from the download cache \
             or re-fetched (regenerable-download). Recycle-bin strategy."
                .into(),
            format!("sub-item of {}", cargo_home.display()),
        );
    }

    let checkouts = base("git").join("checkouts");
    if checkouts.exists() {
        mk(
            items,
            checkouts,
            "cargo git-dependency checkouts",
            RiskLevel::Review,
            "cargo git-dependency checkouts (no registry reference to restore them); \
             removing may break offline builds and lose un-referenced git state. \
             Review before recycle."
                .into(),
            format!("sub-item of {}", cargo_home.display()),
        );
    }
}

/// Scans cargo cache sub-directories (no tool query; CARGO_HOME / default).
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    let mut items = Vec::new();
    let Some(home) = cargo_home(ctx) else {
        return items;
    };
    if !home.is_dir() {
        return items;
    }
    split(ctx, &mut items, &home);
    items
}
