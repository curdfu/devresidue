//! The `SafetyValidator` — the pre-delete revalidation authority (SPEC §15,
//! §16, PLAN Phase 5). Highest-priority module in the product.
//!
//! # Orchestration order
//!
//! 1. **protected-root registry** — the target itself or an ancestor of it
//!    would delete a protected root (SPEC §18 / INV-006);
//! 2. **risk** — `Protected` / `Unknown` snapshots are denied even if the
//!    registry passed (INV-001 / INV-002, defence in depth — the planner
//!    should already have filtered them);
//! 3. **lexical path** — absolute target, canonical form matches the
//!    snapshot;
//! 4. **existence & attributes** — target present, no new read-only flag;
//! 5. **reparse guard** — no reparse ancestor (traversal escape) and the
//!    target itself is not a reparse point (INV-004);
//! 6. **fingerprint** — NTFS identity, last-write and recorded reparse state
//!    still match the snapshot (TOCTOU, INV-005);
//! 7. **process guard** — watch-list not running (Defer), and process state
//!    known (INV-012);
//! 8. **Allow** with a freshly refreshed snapshot for the engine/journal.
//!
//! Everything is programmed against the [`PathProbe`] / [`ProcessProbe`]
//! ports; the Windows adapters live in the platform crate.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::domain::ids::{ProviderId, RuleId};
use crate::domain::risk_level::RiskLevel;
use crate::domain::snapshot::TargetSnapshot;

use super::identity::{CaptureError, FingerprintCheck, IdentitySnapshot};
use super::path::PathCheck;
use super::path::PathValidator;
use super::process::{GuardOutcome, ProcessGuard};
use super::protected::{ProtectedRootRegistry, RootHitKind};
use super::reparse::{ReparseCheck, ReparseGuard, ReparseIssue};
use super::{
    canonical,
    probe::{FileIdentity, PathProbe, ProcessProbe, ReparseKind},
};

/// A named reason a cleanup was refused. Each variant is machine-comparable;
/// the human-readable text lives in the [`SafetyVerdict::Deny`] fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    /// The target is a protected root.
    ProtectedRootExact { root: PathBuf },
    /// The target is an ancestor of a protected root (deleting it removes the
    /// protected root).
    ProtectedRootAncestor { root: PathBuf },
    /// Snapshot risk is `Protected` (INV-002).
    ProtectedRisk,
    /// Snapshot risk is `Unknown` (INV-001).
    UnknownRisk,
    /// The requested target is not an absolute Windows path.
    NotAbsolute { path: PathBuf },
    /// The target's canonical form no longer matches the snapshot's.
    PathMismatch { recorded: PathBuf, live: PathBuf },
    /// The target does not exist at validation time.
    TargetMissing { path: PathBuf },
    /// A probe could not inspect the target (permission/lock/other).
    Unverifiable { path: PathBuf, detail: String },
    /// A path component between the root and the target is a reparse point.
    ReparseCrossing {
        component: PathBuf,
        kind: ReparseKind,
    },
    /// The target itself is a reparse point (INV-004 / SPEC §17).
    ReparseTarget { path: PathBuf, kind: ReparseKind },
    /// The recorded reparse state differs from the live one (link swap).
    ReparseSubstitution {
        recorded: Option<ReparseKind>,
        live: Option<ReparseKind>,
    },
    /// NTFS identity changed between scan and delete (replace / re-create).
    IdentityChanged {
        expected: FileIdentity,
        live: FileIdentity,
    },
    /// The snapshot carries no identity record; substitution is undetectable.
    IdentityNotRecorded,
    /// Last-write time changed while identity stayed put (stale snapshot).
    StaleSnapshot {
        recorded: Option<SystemTime>,
        live: Option<SystemTime>,
    },
    /// The target became read-only since the snapshot.
    ReadOnlyChanged { path: PathBuf },
    /// Process state could not be determined (INV-012 — unknown ≠ not
    /// running).
    ProcessUnknown { names: Vec<String> },
}

/// A named reason a cleanup was deferred (not refused permanently).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferReason {
    /// A watch-list process is running (SPEC §12).
    ProcessRunning { names: Vec<String> },
}

/// The outcome of validating one plan item immediately before deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyVerdict {
    /// Safe to delete; carries a freshly refreshed snapshot for the journal.
    Allow { snapshot: TargetSnapshot },
    /// Do not delete now; retry later (process running).
    Defer {
        reason: DeferReason,
        explanation: String,
    },
    /// Do not delete, ever, on this snapshot. `next_step` says what a human
    /// must do to make progress.
    Deny {
        reason: DenyReason,
        explanation: String,
        next_step: String,
    },
}

impl SafetyVerdict {
    /// True when deletion is authorised.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, SafetyVerdict::Allow { .. })
    }

    /// The denial reason, when denied.
    #[must_use]
    pub fn deny_reason(&self) -> Option<&DenyReason> {
        match self {
            SafetyVerdict::Deny { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// Everything the validator needs to know about the *intended* deletion.
pub struct ValidationRequest<'a> {
    /// The path the engine intends to delete. For Phase 6 this is
    /// `snapshot.path` itself; never user-supplied (INV-013).
    pub path: &'a Path,
    /// Owning product/agent of the residue item, used to pick the process
    /// watch-list.
    pub product: Option<&'a str>,
    /// The scan-time snapshot this plan item was built on.
    pub snapshot: &'a TargetSnapshot,
}

/// Composes all guards into one fail-closed decision.
pub struct SafetyValidator {
    path_probe: Arc<dyn PathProbe + Send + Sync>,
    process_probe: Arc<dyn ProcessProbe + Send + Sync>,
    registry: Arc<ProtectedRootRegistry>,
    process_guard: ProcessGuard,
}

impl SafetyValidator {
    /// Creates the validator over injected probes, registry and guard policy.
    pub fn new(
        path_probe: Arc<dyn PathProbe + Send + Sync>,
        process_probe: Arc<dyn ProcessProbe + Send + Sync>,
        registry: ProtectedRootRegistry,
        process_guard: ProcessGuard,
    ) -> Self {
        Self {
            path_probe,
            process_probe,
            registry: Arc::new(registry),
            process_guard,
        }
    }

    /// The protected-root registry (for diagnostics / re-checking).
    #[must_use]
    pub fn registry(&self) -> &ProtectedRootRegistry {
        &self.registry
    }

    /// Captures a scan-time snapshot for one plan item.
    ///
    /// # Errors
    /// See [`CaptureError`] — notably the target must exist *and* be absolute.
    pub fn capture(
        &self,
        path: &Path,
        rule_id: Option<RuleId>,
        provider_id: Option<ProviderId>,
        risk: RiskLevel,
    ) -> Result<TargetSnapshot, CaptureError> {
        IdentitySnapshot::capture(self.path_probe.as_ref(), path, rule_id, provider_id, risk)
    }

    /// Revalidates a plan item against its snapshot immediately before
    /// deletion.
    pub fn validate(&self, request: &ValidationRequest<'_>) -> SafetyVerdict {
        let path = request.path;
        let snapshot = request.snapshot;

        // 1. Protected-root registry.
        if let Some(hit) = self.registry.protection_hit(path) {
            let root_display = hit.root.display().to_string();
            let (reason, next_step) = match hit.kind {
                RootHitKind::Equal => (
                    DenyReason::ProtectedRootExact {
                        root: hit.root.clone(),
                    },
                    "This path is a protected root and can never be a cleanup target.".to_string(),
                ),
                RootHitKind::ContainsProtectedRoot => (
                    DenyReason::ProtectedRootAncestor {
                        root: hit.root.clone(),
                    },
                    "Re-target the plan to a descendant of the protected root, never its ancestor."
                        .to_string(),
                ),
            };
            return deny(
                reason,
                format!(
                    "target '{}' is protected by root '{root_display}'",
                    path.display()
                ),
                next_step,
            );
        }

        // 2. Risk invariant (defence in depth).
        match snapshot.risk {
            RiskLevel::Protected => {
                return deny(
                    DenyReason::ProtectedRisk,
                    format!("item '{}' carries risk Protected (INV-002)", path.display()),
                    "Protected items never enter the cleanup queue.".to_string(),
                );
            }
            RiskLevel::Unknown => {
                return deny(
                    DenyReason::UnknownRisk,
                    format!("item '{}' carries risk Unknown (INV-001)", path.display()),
                    "Re-scan and classify the item before it can be cleaned.".to_string(),
                );
            }
            _ => {}
        }

        // 3. Lexical path: absolute + matches snapshot canonical form.
        let normalized =
            match PathValidator::check(path, snapshot.normalized_path.as_deref(), &snapshot.path) {
                PathCheck::Ok { normalized } => normalized,
                PathCheck::NotAbsolute { path } => {
                    return deny(
                        DenyReason::NotAbsolute { path },
                        "cleanup requires an absolute target path".to_string(),
                        "Plans must be built by the scanner with absolute paths.".to_string(),
                    );
                }
                PathCheck::NormalizationMismatch { recorded, live } => {
                    return deny(
                        DenyReason::PathMismatch {
                            recorded: recorded.clone(),
                            live: live.clone(),
                        },
                        format!(
                            "path '{}' canonicalises to '{}' but the snapshot recorded '{}'",
                            path.display(),
                            live.display(),
                            recorded.display()
                        ),
                        "Rebuild the plan from a fresh scan.".to_string(),
                    );
                }
            };

        // 4. Existence + attributes.
        let live_attrs = match self.path_probe.attributes(path) {
            Ok(a) => a,
            Err(e) => return self.deny_probe(path, e),
        };
        let recorded_read_only = snapshot.attributes.is_some_and(|a| a.read_only);
        if live_attrs.read_only && !recorded_read_only {
            return deny(
                DenyReason::ReadOnlyChanged {
                    path: path.to_path_buf(),
                },
                format!(
                    "target '{}' is read-only now but was not at snapshot time",
                    path.display()
                ),
                "Clear the read-only attribute and re-run the cleanup.".to_string(),
            );
        }

        // 5. Reparse guard: ancestors first, then the target itself.
        match ReparseGuard::check(&normalized, self.path_probe.as_ref()) {
            Ok(ReparseCheck::AncestorReparse { component, kind }) => {
                return deny(
                    DenyReason::ReparseCrossing {
                        component: component.clone(),
                        kind,
                    },
                    format!(
                        "ancestor '{}' of target is a {kind:?} reparse point — the plan may not \
                         traverse it",
                        component.display()
                    ),
                    "Keep the target under a plain directory tree; re-scan before cleanup."
                        .to_string(),
                );
            }
            Ok(ReparseCheck::TargetReparse { kind }) => {
                return deny(
                    DenyReason::ReparseTarget {
                        path: path.to_path_buf(),
                        kind,
                    },
                    format!(
                        "target '{}' is itself a {kind:?} reparse point (DO NOT FOLLOW, INV-004)",
                        path.display()
                    ),
                    "Deleting a symlink/junction entry is an explicit manual operation; product \
                     paths refuse it."
                        .to_string(),
                );
            }
            Ok(ReparseCheck::Ok) => {}
            Err(issue) => return self.deny_reparse_issue(path, issue),
        }

        // 6. Fingerprint: NTFS identity + last-write + reparse-state drift.
        let live_identity = match self.path_probe.file_identity(path) {
            Ok(id) => id,
            Err(e) => return self.deny_probe(path, e),
        };
        match IdentitySnapshot::check_fingerprint(snapshot, &live_identity) {
            FingerprintCheck::Match => {}
            FingerprintCheck::NoRecordedIdentity => {
                return deny(
                    DenyReason::IdentityNotRecorded,
                    format!(
                        "snapshot for '{}' carries no file identity; substitution cannot be \
                         detected",
                        path.display()
                    ),
                    "Rebuild the plan with a Phase 5 snapshot that records identities.".to_string(),
                );
            }
            FingerprintCheck::IndexMismatch { expected, live } => {
                return deny(
                    DenyReason::IdentityChanged {
                        expected: expected.clone(),
                        live: live.clone(),
                    },
                    format!(
                        "file identity of '{}' changed since the snapshot \
                         (expected {}/{} → live {}/{}) — the object was replaced or re-created",
                        path.display(),
                        expected.volume_serial,
                        expected.file_index,
                        live.volume_serial,
                        live.file_index
                    ),
                    "The item changed on disk after the scan; re-scan and rebuild the plan."
                        .to_string(),
                );
            }
            FingerprintCheck::LastWriteMismatch { expected, live } => {
                return deny(
                    DenyReason::StaleSnapshot {
                        recorded: expected,
                        live,
                    },
                    format!(
                        "last-write time of '{}' changed since the snapshot",
                        path.display()
                    ),
                    "Re-scan to obtain a fresh snapshot, then re-plan.".to_string(),
                );
            }
        }
        // Reparse-state drift (recorded link vanished / swapped to a real
        // object). Live reparse was already verified `None` by the guard.
        if snapshot.reparse_state.is_some() {
            return deny(
                DenyReason::ReparseSubstitution {
                    recorded: snapshot.reparse_state,
                    live: None,
                },
                format!(
                    "target '{}' was a reparse point at scan time but is a plain object now \
                     (substitution)",
                    path.display()
                ),
                "Re-scan and rebuild the plan; never delete across a changed reparse state."
                    .to_string(),
            );
        }

        // 7. Process guard.
        match self
            .process_guard
            .check(self.process_probe.as_ref(), request.product)
        {
            GuardOutcome::Running => {
                let names = self.process_guard.names_for(request.product).to_vec();
                return SafetyVerdict::Defer {
                    reason: DeferReason::ProcessRunning {
                        names: names.clone(),
                    },
                    explanation: format!(
                        "a related tool is running ({}) — deferring cleanup of '{}'",
                        names.join(", "),
                        path.display()
                    ),
                };
            }
            GuardOutcome::Unknown => {
                let names = self.process_guard.names_for(request.product).to_vec();
                return deny(
                    DenyReason::ProcessUnknown {
                        names: names.clone(),
                    },
                    format!(
                        "process state could not be determined for '{}' — unknown is not \
                         'not running' (INV-012)",
                        path.display()
                    ),
                    "Retry once process enumeration works; fail closed until then.".to_string(),
                );
            }
            GuardOutcome::NotRunning => {}
        }

        // 8. Allow: refresh the snapshot with the just-verified live state.
        let fresh = TargetSnapshot {
            path: snapshot.path.clone(),
            normalized_path: Some(normalized),
            attributes: Some(live_attrs),
            reparse_state: None,
            last_write_time: live_identity.last_write,
            file_identity: Some(live_identity),
            rule_id: snapshot.rule_id,
            provider_id: snapshot.provider_id,
            risk: snapshot.risk,
        };
        SafetyVerdict::Allow { snapshot: fresh }
    }

    fn deny_probe(&self, path: &Path, e: super::probe::ProbeError) -> SafetyVerdict {
        match e {
            super::probe::ProbeError::NotFound { path } => deny(
                DenyReason::TargetMissing { path: path.clone() },
                format!(
                    "target '{}' does not exist at validation time",
                    path.display()
                ),
                "The item was renamed or removed after the scan; re-scan and rebuild the plan."
                    .to_string(),
            ),
            other => deny(
                DenyReason::Unverifiable {
                    path: path.to_path_buf(),
                    detail: other.to_string(),
                },
                format!("cannot inspect '{}': {other}", path.display()),
                "Check permissions/locks on the path and re-run.".to_string(),
            ),
        }
    }

    fn deny_reparse_issue(&self, path: &Path, issue: ReparseIssue) -> SafetyVerdict {
        match issue {
            ReparseIssue::NotFound { component } => deny(
                DenyReason::TargetMissing {
                    path: component.clone(),
                },
                format!(
                    "path component '{}' of '{}' vanished during validation",
                    component.display(),
                    path.display()
                ),
                "Re-scan and rebuild the plan.".to_string(),
            ),
            ReparseIssue::Unverifiable { component, detail } => deny(
                DenyReason::Unverifiable {
                    path: component.clone(),
                    detail,
                },
                format!(
                    "reparse state of '{}' could not be determined",
                    component.display()
                ),
                "Check permissions/locks and re-run.".to_string(),
            ),
        }
    }
}

fn deny(reason: DenyReason, explanation: String, next_step: String) -> SafetyVerdict {
    SafetyVerdict::Deny {
        reason,
        explanation,
        next_step,
    }
}

/// Canonical-normalised copy of a path (test + diagnostics helper).
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    canonical::normalize(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::fake::FakeProbe;
    use crate::safety::probe::{ProcessState, ReparseKind};
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    /// Fake process probe with a fixed state.
    struct FixedProcess(ProcessState);

    impl ProcessProbe for FixedProcess {
        fn process_state(&self, _names: &[&str]) -> ProcessState {
            self.0
        }
    }

    fn fake_env() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("USERPROFILE".into(), r"C:\Users\alice".into());
        m.insert("SYSTEMROOT".into(), r"C:\Windows".into());
        m.insert("PROGRAMFILES".into(), r"C:\Program Files".into());
        m.insert("PROGRAMFILES(X86)".into(), r"C:\Program Files (x86)".into());
        m.insert("PROGRAMDATA".into(), r"C:\ProgramData".into());
        m
    }

    fn registry() -> ProtectedRootRegistry {
        let env = fake_env();
        ProtectedRootRegistry::build(&|k: &str| env.get(k).cloned(), vec![]).expect("fake env")
    }

    fn make_validator(probe: &Arc<FakeProbe>, process_state: ProcessState) -> SafetyValidator {
        let path_probe: Arc<dyn PathProbe + Send + Sync> = probe.clone();
        let process_probe: Arc<dyn ProcessProbe + Send + Sync> =
            Arc::new(FixedProcess(process_state));
        SafetyValidator::new(
            path_probe,
            process_probe,
            registry(),
            ProcessGuard::default_list(),
        )
    }

    fn snapshot_for(validator: &SafetyValidator, path: &str) -> TargetSnapshot {
        validator
            .capture(Path::new(path), None, None, RiskLevel::Safe)
            .expect("capture must succeed")
    }

    fn validate_path(
        validator: &SafetyValidator,
        path: &str,
        snapshot: &TargetSnapshot,
    ) -> SafetyVerdict {
        validator.validate(&ValidationRequest {
            path: Path::new(path),
            product: None,
            snapshot,
        })
    }

    fn expect_deny(verdict: SafetyVerdict, pred: impl FnOnce(&DenyReason) -> bool) {
        match verdict {
            SafetyVerdict::Deny { reason, .. } => {
                assert!(pred(&reason), "unexpected reason: {reason:?}")
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    fn expect_deny_reason(verdict: SafetyVerdict, want: DenyReason) {
        match verdict {
            SafetyVerdict::Deny { reason, .. } => {
                assert_eq!(reason, want, "reason mismatch")
            }
            other => panic!("expected Deny({want:?}), got {other:?}"),
        }
    }

    // ---- Protected-root & risk stage -------------------------------------

    #[test]
    fn drive_root_and_user_profile_ancestors_are_denied() {
        let probe = FakeProbe::new();
        let validator = make_validator(&probe, ProcessState::NotRunning);

        let drive = TargetSnapshot::new(
            PathBuf::from("C:\\"),
            Some(SystemTime::now()),
            None,
            None,
            RiskLevel::Safe,
        );
        expect_deny_reason(
            validate_path(&validator, "C:\\", &drive),
            DenyReason::ProtectedRootExact {
                root: PathBuf::from("C:\\"),
            },
        );

        // C:\Users is an ancestor of %USERPROFILE%.
        let users = TargetSnapshot::new(
            PathBuf::from("C:\\Users"),
            Some(SystemTime::now()),
            None,
            None,
            RiskLevel::Safe,
        );
        expect_deny_reason(
            validate_path(&validator, "C:\\Users", &users),
            DenyReason::ProtectedRootAncestor {
                root: PathBuf::from("C:\\Users\\alice"),
            },
        );
    }

    #[test]
    fn protected_and_unknown_risk_are_denied_even_when_path_is_fine() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_dir(Path::new("C:\\tmp\\cache"));
        }
        let validator = make_validator(&probe, ProcessState::Unknown);

        let snap_protected = TargetSnapshot::new(
            PathBuf::from("C:\\tmp\\cache"),
            Some(SystemTime::now()),
            None,
            None,
            RiskLevel::Protected,
        );
        expect_deny_reason(
            validate_path(&validator, "C:\\tmp\\cache", &snap_protected),
            DenyReason::ProtectedRisk,
        );

        let snap_unknown = TargetSnapshot::new(
            PathBuf::from("C:\\tmp\\cache"),
            Some(SystemTime::now()),
            None,
            None,
            RiskLevel::Unknown,
        );
        // Risk stage runs before the (Unknown) process stage → reason is the
        // risk, not the process.
        expect_deny_reason(
            validate_path(&validator, "C:\\tmp\\cache", &snap_unknown),
            DenyReason::UnknownRisk,
        );
    }

    #[test]
    fn sibling_prefix_not_confused_with_protected_root() {
        // %USERPROFILE% = C:\Users\alice. C:\User is a sibling (missing 's')
        // and must not be blocked by the registry; it also is not an ancestor
        // of the profile.
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_dir(Path::new("C:\\User\\cache"));
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\User\\cache");
        assert!(
            matches!(
                validate_path(&validator, "C:\\User\\cache", &snap),
                SafetyVerdict::Allow { .. }
            ),
            "sibling of a protected component must be cleanable"
        );

        // C:\foo-bar vs a protected C:\foo\...: create that protected profile
        // variant and make sure C:\foo-bar stays out of its scope.
        let probe2 = FakeProbe::new();
        {
            let mut env = fake_env();
            env.insert("USERPROFILE".into(), r"C:\foo\alice".into());
            let reg = ProtectedRootRegistry::build(&|k: &str| env.get(k).cloned(), vec![])
                .expect("fake env");
            let _ = reg;
            let mut fs = probe2.lock();
            fs.create_dir(Path::new("C:\\foo-bar\\cache"));
            fs.create_dir(Path::new("C:\\foo\\alice\\cache"));
        }
        let validator2 = {
            let env = fake_env_with_userprofile(r"C:\foo\alice");
            let path_probe: Arc<dyn PathProbe + Send + Sync> = probe2.clone();
            let process_probe: Arc<dyn ProcessProbe + Send + Sync> =
                Arc::new(FixedProcess(ProcessState::NotRunning));
            SafetyValidator::new(
                path_probe,
                process_probe,
                ProtectedRootRegistry::build(&|k: &str| env.get(k).cloned(), vec![]).expect("env"),
                ProcessGuard::default_list(),
            )
        };
        let snap_bar = snapshot_for(&validator2, "C:\\foo-bar\\cache");
        assert!(
            matches!(
                validate_path(&validator2, "C:\\foo-bar\\cache", &snap_bar),
                SafetyVerdict::Allow { .. }
            ),
            "C:\\foo-bar must not be denied because C:\\foo is protected"
        );
        // And the real protected descendant is still denied when targeted.
        let snap_real = snapshot_for(&validator2, "C:\\foo\\alice");
        expect_deny_reason(
            validate_path(&validator2, "C:\\foo\\alice", &snap_real),
            DenyReason::ProtectedRootExact {
                root: PathBuf::from("C:\\foo\\alice"),
            },
        );
    }

    fn fake_env_with_userprofile(profile: &str) -> HashMap<String, String> {
        let mut m = fake_env();
        m.insert("USERPROFILE".into(), profile.into());
        m
    }

    // ---- Lexical path stage -----------------------------------------------

    #[test]
    fn relative_target_is_denied_before_any_probe() {
        let probe = FakeProbe::new();
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = TargetSnapshot::new(
            PathBuf::from("foo\\cache"),
            Some(SystemTime::now()),
            None,
            None,
            RiskLevel::Safe,
        );
        expect_deny_reason(
            validate_path(&validator, "foo\\cache", &snap),
            DenyReason::NotAbsolute {
                path: PathBuf::from("foo\\cache"),
            },
        );
    }

    #[test]
    fn canonical_path_mismatch_with_snapshot_is_denied() {
        let probe = FakeProbe::new();
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = TargetSnapshot {
            path: PathBuf::from("C:\\Users\\alice\\cache"),
            normalized_path: Some(PathBuf::from("C:\\Users\\alice\\OTHER")),
            ..TargetSnapshot::new(
                PathBuf::from("C:\\Users\\alice\\cache"),
                Some(SystemTime::now()),
                None,
                None,
                RiskLevel::Safe,
            )
        };
        expect_deny(
            validate_path(&validator, "C:\\Users\\alice\\cache", &snap),
            |r| matches!(r, DenyReason::PathMismatch { .. }),
        );
    }

    // ---- Reparse guards ---------------------------------------------------

    #[test]
    fn reparse_ancestor_blocks_traversal_after_scan() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\link\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\link\\cache");

        // Attacker replaces C:\proj\link with a junction after the scan.
        {
            let mut fs = probe.lock();
            fs.set_reparse(Path::new("C:\\proj\\link"), ReparseKind::Junction);
        }
        expect_deny_reason(
            validate_path(&validator, "C:\\proj\\link\\cache", &snap),
            DenyReason::ReparseCrossing {
                component: PathBuf::from("C:\\proj\\link"),
                kind: ReparseKind::Junction,
            },
        );
    }

    #[test]
    fn target_turned_into_reparse_point_is_denied() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\cache");

        {
            let mut fs = probe.lock();
            fs.set_reparse(Path::new("C:\\proj\\cache"), ReparseKind::Symlink);
        }
        expect_deny(validate_path(&validator, "C:\\proj\\cache", &snap), |r| {
            matches!(
                r,
                DenyReason::ReparseTarget {
                    kind: ReparseKind::Symlink,
                    ..
                }
            )
        });
    }

    #[test]
    fn reparse_target_swap_to_plain_object_is_denied_as_substitution() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\real\\file.txt"), None);
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        // Scan sees C:\proj\cache as a junction.
        {
            let mut fs = probe.lock();
            fs.set_reparse(Path::new("C:\\proj\\cache"), ReparseKind::Junction);
        }
        let snap = snapshot_for(&validator, "C:\\proj\\cache");
        assert_eq!(
            snap.reparse_state,
            Some(ReparseKind::Junction),
            "capture must record the junction kind"
        );

        // Before delete the reparse point is cleared but the directory stays
        // in place (the same object can no longer be a link). The recorded
        // reparse state no longer matches the live one.
        {
            let mut fs = probe.lock();
            fs.clear_reparse(Path::new("C:\\proj\\cache"));
        }
        expect_deny(validate_path(&validator, "C:\\proj\\cache", &snap), |r| {
            matches!(r, DenyReason::ReparseSubstitution { .. })
        });
    }

    // ---- TOCTOU fingerprint stage ----------------------------------------

    #[test]
    fn stale_snapshot_last_write_change_is_denied() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\cache");

        {
            let mut fs = probe.lock();
            fs.bump_last_write(Path::new("C:\\proj\\cache"));
        }
        expect_deny_reason(
            validate_path(&validator, "C:\\proj\\cache", &snap),
            DenyReason::StaleSnapshot {
                recorded: Some(snap.last_write_time.unwrap()),
                live: Some(snap.last_write_time.unwrap() + Duration::from_secs(1)),
            },
        );
    }

    #[test]
    fn delete_and_recreate_changes_identity_and_is_denied() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\cache");
        let expected_index = snap.file_identity.as_ref().unwrap().file_index;

        {
            let mut fs = probe.lock();
            fs.delete_and_recreate(Path::new("C:\\proj\\cache"));
        }
        expect_deny(validate_path(&validator, "C:\\proj\\cache", &snap), |r| {
            matches!(
                r,
                DenyReason::IdentityChanged { expected, live }
                    if expected.file_index == expected_index
                        && live.file_index != expected_index
            )
        });
    }

    #[test]
    fn renamed_away_target_reports_missing() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\cache");

        {
            let mut fs = probe.lock();
            fs.remove(Path::new("C:\\proj\\cache"));
        }
        expect_deny(validate_path(&validator, "C:\\proj\\cache", &snap), |r| {
            matches!(r, DenyReason::TargetMissing { .. })
        });
    }

    #[test]
    fn plain_cache_without_races_is_allowed_and_refreshes_snapshot() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\cache");
        let verdict = validate_path(&validator, "C:\\proj\\cache", &snap);
        match verdict {
            SafetyVerdict::Allow { snapshot } => {
                assert_eq!(snapshot.path, snap.path);
                assert!(snapshot.file_identity.is_some());
                assert_eq!(snapshot.reparse_state, None);
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn target_became_read_only_is_denied_with_next_step() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }
        let validator = make_validator(&probe, ProcessState::NotRunning);
        let snap = snapshot_for(&validator, "C:\\proj\\cache");

        {
            let mut fs = probe.lock();
            fs.set_read_only(Path::new("C:\\proj\\cache"), true);
        }
        match validate_path(&validator, "C:\\proj\\cache", &snap) {
            SafetyVerdict::Deny {
                reason: DenyReason::ReadOnlyChanged { .. },
                next_step,
                ..
            } => assert!(!next_step.is_empty()),
            other => panic!("expected ReadOnlyChanged deny, got {other:?}"),
        }
    }

    // ---- Process guard ----------------------------------------------------

    #[test]
    fn running_process_defers_unknown_process_denies() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\file.txt"), None);
        }

        // Running → Defer.
        let validator_running = make_validator(&probe, ProcessState::Running);
        let snap = snapshot_for(&validator_running, "C:\\proj\\cache");
        match validate_path(&validator_running, "C:\\proj\\cache", &snap) {
            SafetyVerdict::Defer {
                reason: DeferReason::ProcessRunning { .. },
                ..
            } => {}
            other => panic!("expected Defer, got {other:?}"),
        }

        // Unknown → Deny (INV-012), even though the target itself is fine.
        let validator_unknown = make_validator(&probe, ProcessState::Unknown);
        expect_deny(
            validate_path(&validator_unknown, "C:\\proj\\cache", &snap),
            |r| matches!(r, DenyReason::ProcessUnknown { .. }),
        );
    }
}
