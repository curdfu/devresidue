//! Local-only inputs and configuration for committing a reviewed AI batch.
//!
//! The selection surface contains only a trusted scan-item id and the user's
//! final risk. Paths, model text, cleanup intents and rule grammar are never
//! accepted here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use devresidue_core::ai::UserRuleTransactionPort;
use devresidue_core::rules::{
    compile_rule_docs, load_rules, resolve, AiRuleConfirmation, CandidateRuleVerifier,
    LocalRuleDraftBuilder, RuleFile,
};
use devresidue_core::{ResidueCategory, RiskLevel, RuleId, ScanItemId};

use crate::AiAuditLog;

/// Platform-injected destination for one atomic user-rule confirmation.
#[derive(Clone)]
pub struct AiConfirmationConfig {
    pub(crate) user_rules_dir: PathBuf,
    pub(crate) builtin_rules_dir: PathBuf,
    pub(crate) transaction_port: Arc<dyn UserRuleTransactionPort>,
    pub(crate) audit_log: AiAuditLog,
}

impl AiConfirmationConfig {
    /// Builds a configuration rooted under the application's data directory.
    ///
    /// The injected port owns the filesystem transaction mechanics; this crate
    /// remains independent of Windows APIs and of every cleanup capability.
    #[must_use]
    pub fn new(
        data_dir: impl AsRef<Path>,
        transaction_port: Arc<dyn UserRuleTransactionPort>,
        builtin_rules_dir: impl AsRef<Path>,
    ) -> Self {
        let data_dir = data_dir.as_ref();
        Self::with_audit_log(
            data_dir,
            transaction_port,
            builtin_rules_dir,
            AiAuditLog::open(data_dir),
        )
    }

    /// Builds a confirmation configuration with an explicit bounded audit log.
    /// This is primarily useful for exercising audit failure behavior without
    /// weakening the rule transaction's success semantics.
    #[must_use]
    pub fn with_audit_log(
        data_dir: impl AsRef<Path>,
        transaction_port: Arc<dyn UserRuleTransactionPort>,
        builtin_rules_dir: impl AsRef<Path>,
        audit_log: AiAuditLog,
    ) -> Self {
        Self {
            user_rules_dir: data_dir.as_ref().join("rules").join("user"),
            builtin_rules_dir: builtin_rules_dir.as_ref().to_path_buf(),
            transaction_port,
            audit_log,
        }
    }
}

/// One explicit user decision for a suggestion in the current batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiConfirmationSelection {
    item_id: ScanItemId,
    final_risk: RiskLevel,
    final_category: ResidueCategory,
}

impl AiConfirmationSelection {
    #[must_use]
    pub const fn new(
        item_id: ScanItemId,
        final_risk: RiskLevel,
        final_category: ResidueCategory,
    ) -> Self {
        Self {
            item_id,
            final_risk,
            final_category,
        }
    }

    #[must_use]
    pub const fn item_id(self) -> ScanItemId {
        self.item_id
    }

    #[must_use]
    pub const fn final_risk(self) -> RiskLevel {
        self.final_risk
    }

    #[must_use]
    pub const fn final_category(self) -> ResidueCategory {
        self.final_category
    }
}

/// Result of one all-or-nothing local rule confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConfirmationResult {
    pub written_rule_ids: Vec<RuleId>,
    pub audit_warning: Option<AiAuditWarning>,
}

/// Non-secret best-effort audit outcome after an already committed rule batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAuditWarning {
    WriteFailed,
}

/// Reloads the product registry and resolves it together with the staged user
/// file before Core publishes an AI-confirmed rule batch. This prevents an AI
/// draft from silently shadowing a higher-priority built-in protection rule.
pub(crate) struct FullRuleRematchVerifier {
    builtin_rules_dir: PathBuf,
}

impl FullRuleRematchVerifier {
    pub(crate) fn new(builtin_rules_dir: impl AsRef<Path>) -> Self {
        Self {
            builtin_rules_dir: builtin_rules_dir.as_ref().to_path_buf(),
        }
    }
}

impl CandidateRuleVerifier for FullRuleRematchVerifier {
    fn verify(
        &self,
        candidate_user_file: &RuleFile,
        confirmations: &[AiRuleConfirmation],
    ) -> Result<(), String> {
        let lookup = |name: &str| std::env::var(name).ok();
        let mut combined = load_rules(&self.builtin_rules_dir, &lookup);
        if !combined.is_clean() {
            return Err("full rematch: built-in registry is not clean".to_string());
        }
        let staged_user = compile_rule_docs(&candidate_user_file.rules, &lookup)
            .map_err(|_| "full rematch: staged user rules did not compile".to_string())?;
        combined.merge(staged_user);

        for confirmation in confirmations {
            let draft = LocalRuleDraftBuilder
                .build(confirmation)
                .map_err(|_| "full rematch: could not rebuild local draft".to_string())?;
            let resolved = resolve(&confirmation.item.path, &combined.rules)
                .ok_or_else(|| "full rematch: confirmed item did not resolve".to_string())?;
            if resolved.id != draft.id
                || resolved.risk != confirmation.final_risk
                || resolved.category != confirmation.final_category
            {
                return Err("full rematch: final rule resolution changed".to_string());
            }
        }
        Ok(())
    }
}
