//! Rule Engine (Phase 3).
//!
//! Loads declarative YAML rules from `resources/rules/`, validates them
//! against the release-blocking safety checks (SPEC §31/§32), compiles the
//! survivors and resolves the best rule for a candidate path.
//!
//! ```text
//! schema.rs    rule file schema (deny unknown fields — no shell strings)
//! loader.rs    directory -> source mapping, load & compile pipeline
//! validator.rs INV-006 root protection, SPEC §9 agent-root guard, ...
//! matcher.rs   env expansion, path semantics, per-rule matching
//! priority.rs  RuleSource ranking (SPEC §14) + resolve() tie-breaks
//! user.rs      user disposition persistence (Ignore / Protect, Phase 13)
//! ```
//!
//! User / Community sources exist in the type system; the `user/` directory
//! is wired to the loader in Phase 13 (Community remains unloaded by design).

pub mod draft;
pub mod loader;
pub mod matcher;
pub mod priority;
pub mod schema;
pub mod transaction;
pub mod user;
pub mod validator;

// ---- Re-exports -----------------------------------------------------------

pub use loader::{
    compile_rule_docs, load_rules, load_user_ignore_paths, CompiledPattern, CompiledRule, RuleSet,
    USER_IGNORE_ID_PREFIX,
};
pub use matcher::{rule_matches, ExpandError};
pub use priority::{resolve, ResolvedRule, RuleSource};
pub use schema::{MatchSpec, RuleDoc, RuleFile};
pub use user::{
    dispositions_path, remove_user_rule, upsert_detection_rule, upsert_disposition, user_rules_dir,
    DispositionKind,
};
pub use validator::{validate_rule_file, RuleIssue, Severity};

// ---- AI-rule drafting + atomic transaction (Task 2 / AiAdvisor Phase A) ----

pub use draft::{AiRuleConfirmation, CandidateRuleVerifier, LocalRuleDraftBuilder};
pub use transaction::{commit_ai_rule_batch, CandidateRematchVerifier};
