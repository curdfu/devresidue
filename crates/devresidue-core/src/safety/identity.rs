//! Identity snapshot capture and fingerprint comparison (SPEC §16, INV-005).
//!
//! [`capture`] records the live state of a target into a
//! [`crate::TargetSnapshot`] at plan time. [`check_fingerprint`] re-reads the
//! live identity right before deletion and compares it against the snapshot:
//! the NTFS `(volume_serial, file_index)` pair detects rename/replace/re-create
//! between scan and cleanup; `last_write` detects plain in-place modification.
//!
//! All probing goes through the [`PathProbe`] port, so every scenario in this
//! module's tests is exercised against a programmable fake.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::domain::ids::{ProviderId, RuleId};
use crate::domain::risk_level::RiskLevel;
use crate::domain::snapshot::TargetSnapshot;

use super::canonical;
use super::probe::{FileIdentity, PathProbe};

/// Why a snapshot could not be captured.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("target does not exist at capture time: {path}")]
    NotFound { path: PathBuf },
    #[error("capture probe failed on {path}: {detail}")]
    Unverifiable { path: PathBuf, detail: String },
    #[error("capture requires an absolute path, got: {path}")]
    NotAbsolute { path: PathBuf },
}

/// Result of comparing the recorded fingerprint against a live one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintCheck {
    /// Identity + last-write match the snapshot.
    Match,
    /// No identity was recorded in the snapshot (legacy). Fail closed —
    /// rename/replace between scan and delete is then undetectable.
    NoRecordedIdentity,
    /// `(volume_serial, file_index)` changed → the object was deleted and
    /// re-created, or replaced by a different object (TOCTOU).
    IndexMismatch {
        expected: FileIdentity,
        live: FileIdentity,
    },
    /// Content modification time changed while the object identity stayed the
    /// same → the file was written after the scan. Stale snapshot.
    LastWriteMismatch {
        expected: Option<std::time::SystemTime>,
        live: Option<std::time::SystemTime>,
    },
}

/// Stateless snapshotting + fingerprint helper.
pub struct IdentitySnapshot;

impl IdentitySnapshot {
    /// Captures the full live state of `path` into a [`TargetSnapshot`].
    ///
    /// The recorded snapshot keeps the *scan-observed* identity: the probe
    /// must open the object without following reparse points so a junction /
    /// symlink is fingerprinted as the entry itself.
    ///
    /// # Errors
    /// Fails when the path is not absolute (INV-013 style guard), when it does
    /// not exist (the plan should never be built over a ghost), or when any
    /// probe cannot inspect it — the safety layer never proceeds on partial
    /// information.
    pub fn capture(
        probe: &dyn PathProbe,
        path: &Path,
        rule_id: Option<RuleId>,
        provider_id: Option<ProviderId>,
        risk: RiskLevel,
    ) -> Result<TargetSnapshot, CaptureError> {
        if !canonical::is_absolute(path) {
            return Err(CaptureError::NotAbsolute {
                path: path.to_path_buf(),
            });
        }

        let attributes = probe
            .attributes(path)
            .map_err(|e| probe_to_capture_error(path, e))?;
        let reparse_info = probe
            .reparse_info(path)
            .map_err(|e| probe_to_capture_error(path, e))?;
        let identity = probe
            .file_identity(path)
            .map_err(|e| probe_to_capture_error(path, e))?;

        Ok(TargetSnapshot {
            path: path.to_path_buf(),
            normalized_path: Some(canonical::normalize(path)),
            attributes: Some(attributes),
            reparse_state: reparse_info.kind,
            last_write_time: identity.last_write,
            file_identity: Some(identity),
            rule_id,
            provider_id,
            risk,
        })
    }

    /// Compares the recorded snapshot fingerprint against a freshly probed
    /// identity of the same target.
    pub fn check_fingerprint(snapshot: &TargetSnapshot, live: &FileIdentity) -> FingerprintCheck {
        let Some(expected) = &snapshot.file_identity else {
            return FingerprintCheck::NoRecordedIdentity;
        };
        if expected.volume_serial != live.volume_serial || expected.file_index != live.file_index {
            return FingerprintCheck::IndexMismatch {
                expected: expected.clone(),
                live: live.clone(),
            };
        }
        // The authoritative recorded last-write is the dedicated snapshot
        // field; legacy snapshots fall back to the identity record. The live
        // time is never used to fill a gap on the expected side — that would
        // manufacture a match out of missing data.
        //
        // Comparison is aligned to whole seconds because plan persistence
        // quantises `SystemTime` to epoch seconds: a recorded sub-second
        // component cannot survive a plan round trip. Quantising both sides
        // keeps persisted-plan revalidation stable while still detecting any
        // real modification newer than one second.
        let expected_time = snapshot.last_write_time.or(expected.last_write);
        let live_time = live.last_write;
        match (
            expected_time.map(quantise_secs),
            live_time.map(quantise_secs),
        ) {
            // Both known and equal (to the second): content untouched.
            (Some(e), Some(l)) if e == l => FingerprintCheck::Match,
            // Fail closed on any ambiguity (missing side or real change).
            _ => FingerprintCheck::LastWriteMismatch {
                expected: expected_time,
                live: live_time,
            },
        }
    }
}

/// Floors a `SystemTime` to whole seconds (matching serde persistence).
fn quantise_secs(t: SystemTime) -> SystemTime {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

fn probe_to_capture_error(path: &Path, e: crate::safety::probe::ProbeError) -> CaptureError {
    match e {
        crate::safety::probe::ProbeError::NotFound { path } => CaptureError::NotFound { path },
        other => CaptureError::Unverifiable {
            path: path.to_path_buf(),
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safety::fake::FakeProbe;
    use crate::safety::probe::{AttrFlags, ReparseKind};
    use crate::safety::probe::{ProcessProbe, ProcessState};
    use std::path::Path;

    #[test]
    fn capture_records_full_live_state() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\x.txt"), None);
        }
        let snap = IdentitySnapshot::capture(
            probe.as_ref(),
            Path::new("C:\\proj\\cache"),
            None,
            None,
            crate::domain::risk_level::RiskLevel::Safe,
        )
        .expect("capture");

        assert_eq!(
            snap.normalized_path.as_deref(),
            Some(Path::new("C:\\proj\\cache"))
        );
        let attrs = snap.attributes.expect("attributes recorded");
        assert!(attrs.directory);
        assert!(!attrs.reparse);
        assert_eq!(snap.reparse_state, None);
        assert!(snap.file_identity.is_some());
        assert!(snap.last_write_time.is_some());
    }

    #[test]
    fn capture_records_reparse_kind_and_capture_fails_when_missing() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\x.txt"), None);
            fs.set_reparse(Path::new("C:\\proj\\cache"), ReparseKind::Junction);
        }
        let snap = IdentitySnapshot::capture(
            probe.as_ref(),
            Path::new("C:\\proj\\cache"),
            None,
            None,
            crate::domain::risk_level::RiskLevel::Safe,
        )
        .expect("capture junction");
        assert_eq!(snap.reparse_state, Some(ReparseKind::Junction));

        let ghost = IdentitySnapshot::capture(
            probe.as_ref(),
            Path::new("C:\\proj\\ghost"),
            None,
            None,
            crate::domain::risk_level::RiskLevel::Safe,
        );
        assert!(matches!(ghost, Err(CaptureError::NotFound { .. })));
    }

    #[test]
    fn capture_rejects_relative_paths() {
        let probe = FakeProbe::new();
        let err = IdentitySnapshot::capture(
            probe.as_ref(),
            Path::new("cache"),
            None,
            None,
            crate::domain::risk_level::RiskLevel::Safe,
        )
        .expect_err("relative must fail");
        assert!(matches!(err, CaptureError::NotAbsolute { .. }));
    }

    #[test]
    fn fingerprint_match_and_detection() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\cache\\x.txt"), None);
        }
        let snap = IdentitySnapshot::capture(
            probe.as_ref(),
            Path::new("C:\\proj\\cache"),
            None,
            None,
            crate::domain::risk_level::RiskLevel::Safe,
        )
        .unwrap();
        let live = probe.file_identity(Path::new("C:\\proj\\cache")).unwrap();

        // Untouched → match.
        assert_eq!(
            IdentitySnapshot::check_fingerprint(&snap, &live),
            FingerprintCheck::Match
        );

        // Last-write bumped → staleness.
        {
            let mut fs = probe.lock();
            fs.bump_last_write(Path::new("C:\\proj\\cache"));
        }
        let live = probe.file_identity(Path::new("C:\\proj\\cache")).unwrap();
        assert!(matches!(
            IdentitySnapshot::check_fingerprint(&snap, &live),
            FingerprintCheck::LastWriteMismatch { .. }
        ));

        // No identity in snapshot → fail closed.
        let legacy = crate::domain::snapshot::TargetSnapshot::new(
            PathBuf::from("C:\\proj\\cache"),
            None,
            None,
            None,
            crate::domain::risk_level::RiskLevel::Safe,
        );
        assert_eq!(
            IdentitySnapshot::check_fingerprint(&legacy, &live),
            FingerprintCheck::NoRecordedIdentity
        );
    }

    #[test]
    fn attribute_bits_translate_losslessly() {
        let probe = FakeProbe::new();
        {
            let mut fs = probe.lock();
            fs.create_file(Path::new("C:\\proj\\file.bin"), None);
            fs.set_read_only(Path::new("C:\\proj\\file.bin"), true);
        }
        let attrs: AttrFlags = probe.attributes(Path::new("C:\\proj\\file.bin")).unwrap();
        assert!(attrs.read_only);
        assert!(!attrs.directory);

        // (Compile-time guard: traits remain dyn-usable as expected.)
        let _dyn: &dyn crate::safety::probe::PathProbe = probe.as_ref();
        let _p: &dyn ProcessProbe = &DummyProcess;
    }

    struct DummyProcess;
    impl ProcessProbe for DummyProcess {
        fn process_state(&self, _names: &[&str]) -> ProcessState {
            ProcessState::NotRunning
        }
    }
}
