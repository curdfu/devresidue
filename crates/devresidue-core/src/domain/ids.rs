//! Strongly typed identifiers.
//!
//! # Design rationale — `u64`, not UUID, not `String`, never `Path`
//!
//! Every id in the system is a **newtype over `u64`**:
//!
//! 1. **No extra dependency.** The `uuid` crate is not needed; `u64` keeps the
//!    core dependency set at `thiserror + serde + serde_json` (PLAN Phase 1
//!    dependency minimalism).
//! 2. **Compact and stable.** Ids serialise as plain numbers, compare quickly
//!    and are trivial to store in the Phase 6 cleanup journal.
//! 3. **Process-local scope is enough today.** `ScanItemId`s are minted by a
//!    scan registry and only live while a scan result is in memory.
//!    `CleanupPlanId`s must outlive the process that created them once Phase 6
//!    persists plans for `devresidue clean --plan <id>`; a monotonic counter
//!    with a persisted high-water mark (or a later switch to UUIDv7 inside
//!    `ids.rs` only) covers that. See "遗留问题" in the Phase 0-2 report.
//!
//! # Never a `Path`
//!
//! These types **cannot** be constructed from a path — there is intentionally
//! no `From<PathBuf>`/`From<Path>` implementation (INV-013). UI/CLI layers may
//! only reference residues and cleanup plans through these opaque ids; paths
//! travel inside `ScanItem`/plan records produced by trusted core providers.

use std::{fmt, num::ParseIntError, str::FromStr};

use serde::{Deserialize, Serialize};

/// Error returned when a strongly typed id cannot be parsed from text.
///
/// Ids are decimal `u64` values; anything else (or a non-numeric input) fails
/// with this error carrying the offending id type name for good messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {id_type} (expected a decimal u64), got: {source}")]
pub struct IdParseError {
    /// Human-readable name of the id type that failed to parse.
    pub id_type: &'static str,
    /// The underlying integer parse failure.
    #[source]
    pub source: ParseIntError,
}

/// Generates one newtype id backed by `u64`.
macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Creates an id from a raw 64-bit value.
            ///
            /// Allowed: ids are minted by scan registries/planners from a
            /// counter. What is *forbidden* is deriving an id from a path.
            #[must_use]
            pub const fn from_raw(value: u64) -> Self {
                Self(value)
            }

            /// Returns the raw 64-bit value backing this id.
            #[must_use]
            pub const fn raw(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let value = s.parse::<u64>().map_err(|source| IdParseError {
                    id_type: stringify!($name),
                    source,
                })?;
                Ok(Self(value))
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            fn from(id: $name) -> u64 {
                id.0
            }
        }
    };
}

define_id! {
    /// Uniquely identifies one discovered residue item **within a scan
    /// session**. A `ScanItemId` is minted when a provider emits the item and
    /// is what the UI/CLI submit when building a cleanup plan (SPEC §20,
    /// INV-013). It says nothing about the underlying path.
    ScanItemId
}

define_id! {
    /// Identifies a `CleanupPlan` (SPEC §16/§20). Plans are the only handle
    /// through which any future deletion is requested
    /// (`devresidue clean --plan <id>`); a plan id is never a path.
    CleanupPlanId
}

define_id! {
    /// Identifies a rule inside the rule registry (Phase 3). Rules are
    /// registered by the rule loader with a stable slug plus an assigned
    /// numeric id; `ScanItem.evidence[].rule_id` and snapshots reference the
    /// numeric form. Rule ids are never paths.
    RuleId
}

define_id! {
    /// Identifies a registered provider instance inside a session registry.
    /// Providers additionally expose a stable string name
    /// (`ResidueProvider::name`) for display; the numeric id is what snapshots
    /// and the journal record for compact, joinable references.
    ProviderId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_from_str_round_trip_per_type() {
        let scan = ScanItemId::from_raw(7);
        assert_eq!(scan.to_string(), "7");
        assert_eq!(scan.to_string().parse::<ScanItemId>().unwrap(), scan);

        let plan = CleanupPlanId::from_raw(42);
        assert_eq!(plan.to_string(), "42");
        assert_eq!(plan.to_string().parse::<CleanupPlanId>().unwrap(), plan);

        let rule = RuleId::from_raw(9001);
        assert_eq!(rule.to_string(), "9001");
        assert_eq!(rule.to_string().parse::<RuleId>().unwrap(), rule);

        let provider = ProviderId::from_raw(3);
        assert_eq!(provider.to_string(), "3");
        assert_eq!(
            provider.to_string().parse::<ProviderId>().unwrap(),
            provider
        );
    }

    #[test]
    fn ids_are_distinct_newtypes() {
        // Each id kind is its own type: the compiler refuses to compare or mix
        // them (assert_ne!(ScanItemId, CleanupPlanId) does not even compile),
        // which is exactly the isolation requirement — an id can never be
        // substituted for another kind of id, let alone for a path.
        assert_eq!(ScanItemId::from_raw(5).raw(), 5);
        assert_eq!(CleanupPlanId::from_raw(5).raw(), 5);
        assert_eq!(RuleId::from_raw(5).raw(), 5);
        assert_eq!(ProviderId::from_raw(5).raw(), 5);
        // Same kind, same value: equal and orderable.
        assert_eq!(ScanItemId::from_raw(5), ScanItemId::from_raw(5));
        assert!(ScanItemId::from_raw(2) < ScanItemId::from_raw(3));
    }

    #[test]
    fn parse_errors_are_informative() {
        let err: IdParseError = "abc".parse::<ScanItemId>().unwrap_err();
        assert_eq!(err.id_type, "ScanItemId");
        let err: IdParseError = "-1".parse::<CleanupPlanId>().unwrap_err();
        assert_eq!(err.id_type, "CleanupPlanId");
        let err: IdParseError = "".parse::<RuleId>().unwrap_err();
        assert_eq!(err.id_type, "RuleId");
        let err: IdParseError = "9.5".parse::<ProviderId>().unwrap_err();
        assert_eq!(err.id_type, "ProviderId");
    }

    #[test]
    fn serde_round_trip_is_transparent_number() {
        let original = ScanItemId::from_raw(1234);
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, "1234");
        let back: ScanItemId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn no_conversion_from_path_exists() {
        // Compile-time guard: an id can never be obtained from a Path. We
        // cannot assert a negative in Rust, so this just documents intent and
        // reminds reviewers that the requirement lives in the type design
        // (there is no `From<PathBuf> for ScanItemId` anywhere).
        let raw = 1_u64;
        let id = ScanItemId::from_raw(raw);
        assert_eq!(id.raw(), raw);
    }
}
