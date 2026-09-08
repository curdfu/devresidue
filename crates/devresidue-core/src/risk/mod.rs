//! Risk classification services — **skeleton (Phase 5)**.
//!
//! The classification *data* already lives in the domain model
//! ([`crate::domain::risk_level::RiskLevel`]). This module will host the
//! `RiskClassifier` that maps evidence/rules/providers to a `RiskLevel`, and
//! the risk→cleanup policy table from SPEC §19 (Protected/Unknown → Deny,
//! Review → explicit confirm, ...).
//!
//! Until Phase 5 it intentionally defines nothing; rules and providers assign
//! risks directly through the domain types.

// No items yet by design.
