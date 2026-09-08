//! serde helpers for `std::time::SystemTime`.
//!
//! `SystemTime` does not implement serde by itself. All domain structs that
//! carry timestamps serialise them as **Unix epoch seconds** (`u64`) so the
//! JSON/on-disk DTO shape stays stable, small and diff-friendly.

use serde::{Deserialize, Deserializer, Serializer};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Epoch seconds for `t`; pre-epoch instants clamp to 0 (never expected).
fn epoch_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// Serialise/deserialise a non-optional `SystemTime` as epoch seconds.
pub(crate) mod system_time {
    use super::*;

    pub fn serialize<S: Serializer>(value: &SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(epoch_secs(*value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SystemTime, D::Error> {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

/// Serialise/deserialise `Option<SystemTime>` as `null` or epoch seconds.
pub(crate) mod opt_system_time {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(t) => serializer.serialize_some(&epoch_secs(*t)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        let secs = Option::<u64>::deserialize(deserializer)?;
        Ok(secs.map(|s| UNIX_EPOCH + Duration::from_secs(s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        #[serde(with = "crate::domain::serde_time::opt_system_time")]
        at: Option<SystemTime>,
        #[serde(with = "crate::domain::serde_time::system_time")]
        fixed: SystemTime,
    }

    #[test]
    fn round_trips_epoch_seconds() {
        // Serialisation is second-granular: a round trip floors sub-second
        // precision, so compare against the quantised instant.
        let now = SystemTime::now();
        let floored = UNIX_EPOCH + Duration::from_secs(epoch_secs(now));
        let s = Sample {
            at: Some(now),
            fixed: now,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Sample = serde_json::from_str(&json).unwrap();
        assert_eq!(back.at, Some(floored));
        assert_eq!(back.fixed, floored);
    }

    #[test]
    fn none_serialises_as_null() {
        let fixed = UNIX_EPOCH + Duration::from_secs(42);
        let s = Sample { at: None, fixed };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"at\":null"));
        assert!(json.contains("\"fixed\":42"));
    }
}
