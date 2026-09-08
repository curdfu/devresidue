//! Provider registry — stable slugs bound to fixed numeric [`ProviderId`]s.
//!
//! The numeric ids are part of the persisted/journal data format (plans,
//! evidence, journal `provider` field): **the order below is frozen**.
//! Never renumber, never reuse a freed id — append new providers at the end.
//!
//! `Evidence.source` carries the slug (e.g. `"provider:kondo"`) so a human
//! reading a scan result can trace which producer emitted an item.

use devresidue_core::ProviderId;

/// Fixture provider (Phase 2 demo data).
pub const FIXTURE_ID: ProviderId = ProviderId::from_raw(1);
/// Kondo project/artifact discovery (INV-008: discovery only).
pub const KONDO_ID: ProviderId = ProviderId::from_raw(2);
pub const NPM_ID: ProviderId = ProviderId::from_raw(3);
pub const BUN_ID: ProviderId = ProviderId::from_raw(4);
pub const PIP_ID: ProviderId = ProviderId::from_raw(5);
pub const UV_ID: ProviderId = ProviderId::from_raw(6);
pub const CARGO_ID: ProviderId = ProviderId::from_raw(7);
pub const NUGET_ID: ProviderId = ProviderId::from_raw(8);
/// AI agent providers (SPEC §9 first batch, Phase 9).
pub const CODEX_ID: ProviderId = ProviderId::from_raw(9);
pub const CLAUDE_ID: ProviderId = ProviderId::from_raw(10);
pub const OPENCODE_ID: ProviderId = ProviderId::from_raw(11);
pub const OMO_ID: ProviderId = ProviderId::from_raw(12);
pub const CURSOR_ID: ProviderId = ProviderId::from_raw(13);
pub const WINDSURF_ID: ProviderId = ProviderId::from_raw(14);
/// Unknown developer data provider (Phase 13, heuristic discovery).
pub const UNKNOWN_ID: ProviderId = ProviderId::from_raw(15);

/// All registered providers (id, slug). Stable order — never reorder.
pub const ALL: &[(ProviderId, &str)] = &[
    (FIXTURE_ID, "fixture"),
    (KONDO_ID, "kondo"),
    (NPM_ID, "npm"),
    (BUN_ID, "bun"),
    (PIP_ID, "pip"),
    (UV_ID, "uv"),
    (CARGO_ID, "cargo"),
    (NUGET_ID, "nuget"),
    (CODEX_ID, "codex"),
    (CLAUDE_ID, "claude"),
    (OPENCODE_ID, "opencode"),
    (OMO_ID, "omo"),
    (CURSOR_ID, "cursor"),
    (WINDSURF_ID, "windsurf"),
    (UNKNOWN_ID, "unknown"),
];

/// Resolves the numeric id for a slug.
#[must_use]
pub fn id_of(slug: &str) -> Option<ProviderId> {
    ALL.iter().find(|(_, s)| *s == slug).map(|(id, _)| *id)
}

/// Resolves the slug for a numeric id.
#[must_use]
pub fn slug_of(id: ProviderId) -> Option<&'static str> {
    ALL.iter().find(|(pid, _)| *pid == id).map(|(_, s)| *s)
}

/// Evidence source tag for a provider slug (`provider:kondo`).
#[must_use]
pub fn evidence_tag(slug: &str) -> String {
    format!("provider:{slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_slugs_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for (id, slug) in ALL {
            assert!(seen.insert((id.raw(), *slug)), "duplicate {id:?}/{slug}");
            assert_eq!(id_of(slug), Some(*id));
            assert_eq!(slug_of(*id), Some(*slug));
        }
    }

    #[test]
    fn evidence_tag_format() {
        assert_eq!(evidence_tag("kondo"), "provider:kondo");
    }
}
