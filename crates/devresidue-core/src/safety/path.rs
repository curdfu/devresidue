//! Path validator — lexical acceptance of a cleanup target (SPEC §15).
//!
//! Operates **without touching the disk**:
//!
//! - the target must be absolute (a bare relative path is never acceptable
//!   for deletion, INV-013); drive-relative paths (`C:foo`) are also
//!   rejected;
//! - the live target's canonical form must equal the form recorded in the
//!   plan snapshot. The snapshot stores its canonical form once captured;
//!   snapshots produced before normalisation existed fall back to their raw
//!   path and are canonicalised here on the fly.
//!
//! Deletion-time *existence* checks and reparse probing are the reparse /
//! identity guards' job; this module is purely lexical.

use std::path::{Path, PathBuf};

use super::canonical;

/// Why the lexical path check failed (or that it passed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathCheck {
    /// Path is absolute and equals the snapshot's canonical form.
    Ok {
        /// Canonical (normalised) form of the target for later stages.
        normalized: PathBuf,
    },
    /// Target is not an absolute Windows path.
    NotAbsolute { path: PathBuf },
    /// The canonical form differs from what the snapshot recorded.
    NormalizationMismatch { recorded: PathBuf, live: PathBuf },
}

/// Stateless lexical gate.
pub struct PathValidator;

impl PathValidator {
    /// Canonicalises `target` and compares it with the plan snapshot's
    /// recorded canonical path.
    ///
    /// `recorded_normalized` is `snapshot.normalized_path`; when it is `None`
    /// the snapshot's raw `recorded_raw` is canonicalised on the fly (legacy
    /// snapshots). `target` is the path the engine is about to delete — for
    /// the Phase 6 engine this is `snapshot.path` itself.
    #[must_use]
    pub fn check(
        target: &Path,
        recorded_normalized: Option<&Path>,
        recorded_raw: &Path,
    ) -> PathCheck {
        if !canonical::is_absolute(target) {
            return PathCheck::NotAbsolute {
                path: target.to_path_buf(),
            };
        }
        let live = canonical::normalize(target);
        let recorded = recorded_normalized
            .map(canonical::normalize)
            .unwrap_or_else(|| canonical::normalize(recorded_raw));
        if canonical::normalized_eq_path(&live, &recorded) {
            PathCheck::Ok { normalized: live }
        } else {
            PathCheck::NormalizationMismatch { recorded, live }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_and_drive_relative_targets_are_rejected() {
        assert_eq!(
            PathValidator::check(Path::new("foo\\bar"), None, Path::new("foo\\bar")),
            PathCheck::NotAbsolute {
                path: PathBuf::from("foo\\bar")
            }
        );
        assert_eq!(
            PathValidator::check(Path::new("C:foo"), None, Path::new("C:foo")),
            PathCheck::NotAbsolute {
                path: PathBuf::from("C:foo")
            }
        );
        assert_eq!(
            PathValidator::check(Path::new("\\foo"), None, Path::new("\\foo")),
            PathCheck::NotAbsolute {
                path: PathBuf::from("\\foo")
            }
        );
    }

    #[test]
    fn absolute_path_against_recorded_normalized() {
        // Case/separator variation of the recorded path is fine.
        let ok = PathValidator::check(
            Path::new("C:\\Users\\alice\\cache"),
            Some(Path::new("c:/users/alice/cache")),
            Path::new("C:\\Users\\alice\\cache"),
        );
        assert!(matches!(ok, PathCheck::Ok { .. }));

        // Real mismatch is caught.
        let mismatch = PathValidator::check(
            Path::new("C:\\Users\\alice\\cache2"),
            Some(Path::new("c:/users/alice/cache")),
            Path::new("C:\\Users\\alice\\cache"),
        );
        assert!(matches!(mismatch, PathCheck::NormalizationMismatch { .. }));
    }

    #[test]
    fn legacy_snapshot_without_normalization_is_reconciled() {
        let ok = PathValidator::check(
            Path::new("C:\\Users\\alice\\cache"),
            None,
            Path::new("c:/users/alice/cache"),
        );
        assert!(matches!(ok, PathCheck::Ok { .. }));
    }
}
