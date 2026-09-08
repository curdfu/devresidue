//! Reparse guard — enforces **DO NOT FOLLOW** (SPEC §17, INV-004, SPEC §32
//! Reparse Traversal / Junction Substitution / Symlink Substitution tests).
//!
//! Two distinct denials:
//!
//! - a path component *between* the namespace root and the target is a
//!   reparse point → deleting the target through that component would follow
//!   the link out of the scanned tree (**ReparseCrossing**);
//! - the target *itself* is a reparse point → deleting a symlink/junction
//!   entry is an explicit manual operation; product code paths refuse it
//!   outright (**DO NOT FOLLOW** on the entry).
//!
//! The guard probes ancestors root-first so it stops at the first crossing
//! (cheapest safe failure). All probing goes through the [`PathProbe`] port.

use std::path::{Path, PathBuf};

use super::canonical;
use super::probe::{PathProbe, ProbeError, ReparseKind};

/// Outcome of the reparse traversal check for a whole target path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReparseCheck {
    /// Neither the target nor any ancestor is a reparse point.
    Ok,
    /// An ancestor component is a reparse point — traversal would escape the
    /// scanned tree.
    AncestorReparse {
        component: PathBuf,
        kind: ReparseKind,
    },
    /// The target itself is a reparse point (INV-004).
    TargetReparse { kind: ReparseKind },
}

/// A probe could not establish the reparse state of a path component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReparseIssue {
    /// A probed component no longer exists (path dangling mid-tree).
    NotFound { component: PathBuf },
    /// The reparse state could not be determined.
    Unverifiable { component: PathBuf, detail: String },
}

/// Stateless reparse guard.
pub struct ReparseGuard;

impl ReparseGuard {
    /// Checks `target` (assumed absolute and normalised by the caller) and
    /// every ancestor prefix for reparse points.
    pub fn check(target: &Path, probe: &dyn PathProbe) -> Result<ReparseCheck, ReparseIssue> {
        for prefix in canonical::ancestor_prefixes(target) {
            match probe.reparse_info(&prefix) {
                Ok(info) => {
                    if let Some(kind) = info.kind {
                        return Ok(ReparseCheck::AncestorReparse {
                            component: prefix,
                            kind,
                        });
                    }
                }
                Err(e) => return Err(map_issue(prefix, e)),
            }
        }

        match probe.reparse_info(target) {
            Ok(info) => Ok(match info.kind {
                None => ReparseCheck::Ok,
                Some(kind) => ReparseCheck::TargetReparse { kind },
            }),
            Err(e) => Err(map_issue(target.to_path_buf(), e)),
        }
    }
}

fn map_issue(component: PathBuf, e: ProbeError) -> ReparseIssue {
    match e {
        ProbeError::NotFound { .. } => ReparseIssue::NotFound { component },
        other => ReparseIssue::Unverifiable {
            component,
            detail: other.to_string(),
        },
    }
}
