//! IPC contract guard — pins the frontend↔backend wire contract so the two
//! sides cannot silently drift again.
//!
//! # What is pinned here
//!
//! 1. **Command names.** Tauri v2 registers a command under `stringify!(fn
//!    ident)` (verified in `tauri-macros` `wrapper.rs`: the default is
//!    `RenamePolicy::Keep` + `stringify!(#ident)`), so the command name *is*
//!    the function name. [`REGISTERED_COMMANDS`] is compared **against the
//!    real `generate_handler!` source in `main.rs`** (parsed via
//!    `include_str!`), so adding a command without updating this list — or
//!    renaming a handler without updating the list — fails the build's tests.
//! 2. **Wire argument names.** Tauri v2 defaults to `ArgumentCase::Camel`
//!    (same `wrapper.rs`: `argument_case: ArgumentCase::Camel`), i.e. a Rust
//!    `item_id` parameter is invoked from JS as `{ itemId }`. The tests below
//!    model the exact per-command argument struct the macro generates and
//!    deserialise the *frontend JSON* through it — the same serde path the IPC
//!    layer uses.
//! 3. **Response DTO key sets (F-M4-2).** Every DTO the commands return is
//!    serialised and its exact wire key set asserted. This is the fix for the
//!    `risk` vs `suggestedRisk` drift: a response-field rename now fails a
//!    test instead of silently breaking the frontend.
//!
//! # Response-shape maintenance contract
//!
//! **Adding or renaming a field on any command-result DTO requires updating
//! BOTH this file's response-key tests AND the frontend types**
//! (`src/types/index.ts` in the React app). The tests below are the backend
//! half of that contract; they fail loudly when the backend shape changes so
//! the frontend cannot be left behind silently.
//!
//! # Method note
//!
//! Tauri's mock runtime (`tauri::test::mock_app`) does not expose a public
//! high-level "invoke a command" entry point (IPC goes through a webview
//! channel), and the argument structs the `#[command]` macro generates are
//! private. The strongest practical pin is therefore: compile-time existence
//! of the command functions (they are referenced by `generate_handler!`) +
//! serde round-trip of the frontend wire JSON against the argument shapes +
//! exact serialised key-set assertions on every response DTO.

#[cfg(test)]
use serde::Deserialize;

#[cfg(test)]
use crate::contract::{
    AiConfirmResultDto, AiPreparedBatchDto, AiPreparedEntryDto, AiProfileDto, AiProfileStateDto,
    AiSuggestionDto, ConfirmPolicyArg, DispositionArg, RiskLevelArg, ScanScope,
};

/// Every registered command, in the same order as `main.rs`'s
/// `generate_handler!`. **Keep in sync** — `command_names_match_contract`
/// asserts the exact set so an unregistered rename surfaces as a test failure.
#[cfg(test)]
pub const REGISTERED_COMMANDS: &[&str] = &[
    "scan",
    "cancel_scan",
    "get_scan_results",
    "create_cleanup_plan",
    "execute_cleanup_plan",
    "get_journal",
    "clear_all_data",
    "set_disposition",
    "open_folder",
    "get_rules",
    "validate_rules",
    "delete_user_rule",
    "ai_list_profiles",
    "ai_upsert_profile",
    "ai_delete_profile",
    "ai_set_active_profile",
    "ai_set_master_enabled",
    "ai_test_connection",
    "ai_list_models",
    "ai_prepare_batch",
    "ai_analyze",
    "ai_confirm",
    "ai_cancel",
    "ai_discard_batch",
];

// ---- Argument shapes mirroring what the #[tauri::command] macro generates ---
// (snake_case Rust fields → camelCase wire keys, default ArgumentCase::Camel.)
// Only the tests below construct them, so the whole region is test-gated.

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanArgs {
    scope: ScanScope,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelScanArgs {
    scan_id: u64,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetDispositionArgs {
    item_id: u64,
    disposition: DispositionArg,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenFolderArgs {
    item_id: u64,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCleanupPlanArgs {
    item_ids: Vec<u64>,
    policy: ConfirmPolicyArg,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteCleanupPlanArgs {
    plan_id: u64,
    policy: ConfirmPolicyArg,
    dry_run: bool,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetJournalArgs {
    #[allow(dead_code)]
    last_n: Option<usize>,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiConfirmArgs {
    batch_id: String,
    scan_generation: u64,
    items: Vec<AiConfirmItemArgs>,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiConfirmItemArgs {
    item_id: u64,
    final_risk: RiskLevelArg,
    final_category: devresidue_core::ResidueCategory,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteUserRuleArgs {
    rule_id: String,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiPrepareArgs {
    profile_id: String,
    scan_generation: u64,
    item_ids: Vec<u64>,
    include_paths: bool,
}

#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AiDiscardBatchArgs {
    batch_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        CleanupPlanDto, CleanupSessionDto, CommandError, DispositionResultDto, ErrorCode,
        JournalEntryDto, PlannedItemDto, RuleDto, RuleIssueDto, RulesValidationDto,
        ScanDonePayload, ScanErrorPayload, ScanHandleDto, SessionItemDto, SessionTotalsDto,
        SkippedItemDto,
    };
    use devresidue_providers::scan_store::{ScanMode, ScanSnapshot};

    // ---- Response DTO key-set helpers --------------------------------------

    /// Sorted wire keys of a serialised value (fails on non-objects).
    fn wire_keys<T: serde::Serialize>(value: &T) -> Vec<String> {
        let mut keys: Vec<String> = serde_json::to_value(value)
            .expect("DTO must serialise")
            .as_object()
            .expect("DTO must serialise to an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// Exact key-set assertion: both more and fewer keys than expected fail.
    fn assert_keys<T: serde::Serialize>(value: &T, expected: &[&str]) {
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(
            wire_keys(value),
            expected,
            "response DTO wire keys drifted — update ipc_contract.rs tests AND \
             src/types/index.ts (frontend) together (F-M4-2)"
        );
    }

    // ---- Command-name drift guard (parses main.rs source) -------------------

    /// Extracts the command identifiers inside `generate_handler![...]` of
    /// `main.rs` (resolved via `include_str!`, relative to this file's
    /// `src/` directory). Naive textual parse is sufficient: the handler list
    /// contains one `module::command` per line and no nested brackets /
    /// brackets inside comments. The `// keep in sync` comment inside the
    /// list is skipped.
    fn commands_from_main_generate_handler() -> Vec<String> {
        let src = include_str!("main.rs");
        let marker = "generate_handler![";
        let start = src
            .find(marker)
            .unwrap_or_else(|| panic!("main.rs no longer contains `{marker}`"));
        let body_start = start + marker.len();
        let body_end = body_start
            + src[body_start..]
                .find(']')
                .unwrap_or_else(|| panic!("main.rs `generate_handler![` is never closed"));
        src[body_start..body_end]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .map(|line| line.trim_end_matches(',').trim())
            .filter(|line| !line.is_empty())
            .map(|line| {
                line.rsplit("::")
                    .next()
                    .expect("handler entry has a function name")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn registered_commands_exactly_match_generate_handler_in_main_rs() {
        // The ground truth is main.rs's handler list; REGISTERED_COMMANDS is
        // the test-side contract copy. Adding/renaming a command in main.rs
        // without updating REGISTERED_COMMANDS now fails here.
        let from_source = commands_from_main_generate_handler();
        let expected: Vec<String> = REGISTERED_COMMANDS.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            from_source, expected,
            "generate_handler! in main.rs drifted from REGISTERED_COMMANDS \
             (add the new command to both, or rename in both)"
        );
        // The source itself must contain no cmd_-prefixed handlers.
        assert!(
            !from_source.iter().any(|c| c.starts_with("cmd_")),
            "commands must be prefix-free: {from_source:?}"
        );
    }

    #[test]
    fn command_names_match_the_frontend_contract_exactly() {
        // The exact wire names the frontend invokes (prefix-free).
        assert_eq!(
            REGISTERED_COMMANDS,
            &[
                "scan",
                "cancel_scan",
                "get_scan_results",
                "create_cleanup_plan",
                "execute_cleanup_plan",
                "get_journal",
                "clear_all_data",
                "set_disposition",
                "open_folder",
                "get_rules",
                "validate_rules",
                "delete_user_rule",
                "ai_list_profiles",
                "ai_upsert_profile",
                "ai_delete_profile",
                "ai_set_active_profile",
                "ai_set_master_enabled",
                "ai_test_connection",
                "ai_list_models",
                "ai_prepare_batch",
                "ai_analyze",
                "ai_confirm",
                "ai_cancel",
                "ai_discard_batch",
            ]
        );
        // No duplicates (generate_handler! would reject them anyway).
        let mut sorted = REGISTERED_COMMANDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), REGISTERED_COMMANDS.len());
    }

    #[test]
    fn ai_profile_dto_has_no_api_key_or_environment_name_field() {
        let dto = AiProfileDto {
            id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
            name: "local test".into(),
            base_url: "https://example.invalid/v1".into(),
            model: "test-model".into(),
            enabled: true,
            is_active: true,
        };
        let value = serde_json::to_value(dto).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["baseUrl", "enabled", "id", "isActive", "model", "name"]
        );
        assert!(value.get("apiKey").is_none());
        assert!(value.get("apiKeyEnv").is_none());
    }

    #[test]
    fn ai_confirm_wire_argument_contains_only_batch_generation_ids_risks_and_categories() {
        let parsed: AiConfirmArgs = serde_json::from_str(
            r#"{
                "batchId": "batch-1",
                "scanGeneration": 7,
                "items": [{"itemId": 12, "finalRisk": "review", "finalCategory": "developer-cache"}]
            }"#,
        )
        .unwrap();
        assert_eq!(parsed.batch_id, "batch-1");
        assert_eq!(parsed.scan_generation, 7);
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].item_id, 12);
        assert_eq!(parsed.items[0].final_risk, RiskLevelArg::Review);
        assert_eq!(
            parsed.items[0].final_category,
            devresidue_core::ResidueCategory::DeveloperCache
        );
        assert!(serde_json::from_str::<AiConfirmArgs>(
            r#"{"batchId":"batch-1","scanGeneration":7,"items":[{"path":"C:\\\\x","finalRisk":"review","finalCategory":"developer-cache"}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<AiConfirmArgs>(
            r#"{"batchId":"batch-1","scanGeneration":7,"items":[{"itemId":12,"path":"C:\\\\x","finalRisk":"review","finalCategory":"developer-cache"}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<AiConfirmArgs>(
            r#"{"batchId":"batch-1","scanGeneration":7,"items":[{"itemId":12,"finalRisk":"unknown","finalCategory":"developer-cache"}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<AiConfirmArgs>(
            r#"{"batchId":"batch-1","scanGeneration":7,"items":[{"itemId":12,"finalRisk":"review","finalCategory":"unknown"}]}"#
        )
        .is_ok());
    }

    #[test]
    fn delete_user_rule_accepts_only_an_exact_rule_id() {
        let parsed: DeleteUserRuleArgs =
            serde_json::from_str(r#"{"ruleId":"user-ai/7f75448b-6d89-4d8e-a861-d66b6cd1ea72"}"#)
                .unwrap();
        assert!(parsed.rule_id.starts_with("user-ai/"));
        assert!(serde_json::from_str::<DeleteUserRuleArgs>(
            r#"{"ruleId":"user-ai/test","path":"C:\\\\Users\\\\demo"}"#
        )
        .is_err());
    }

    #[test]
    fn ai_prepare_accepts_only_ids_and_an_explicit_path_disclosure_flag() {
        let args: AiPrepareArgs = serde_json::from_str(
            r#"{
                "profileId": "profile-1",
                "scanGeneration": 7,
                "itemIds": [12, 13],
                "includePaths": true
            }"#,
        )
        .unwrap();
        assert_eq!(args.profile_id, "profile-1");
        assert_eq!(args.scan_generation, 7);
        assert_eq!(args.item_ids, vec![12, 13]);
        assert!(args.include_paths);
        assert!(serde_json::from_str::<AiPrepareArgs>(
            r#"{"profileId":"profile-1","scanGeneration":7,"itemIds":[12],"includePaths":true,"path":"C:\\\\private"}"#
        )
        .is_err());

        let discard: AiDiscardBatchArgs = serde_json::from_str(r#"{"batchId":"batch-1"}"#).unwrap();
        assert_eq!(discard.batch_id, "batch-1");
        assert!(serde_json::from_str::<AiDiscardBatchArgs>(
            r#"{"batchId":"batch-1","path":"C:\\\\private"}"#
        )
        .is_err());
    }

    #[test]
    fn remote_ai_response_dtos_have_exact_non_secret_non_path_key_sets() {
        let profile = AiProfileDto {
            id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
            name: "local test".into(),
            base_url: "https://example.invalid/v1".into(),
            model: "test-model".into(),
            enabled: true,
            is_active: true,
        };
        assert_keys(
            &profile,
            &["id", "name", "baseUrl", "model", "enabled", "isActive"],
        );
        let profiles = AiProfileStateDto {
            master_enabled: true,
            profiles: vec![profile],
        };
        assert_keys(&profiles, &["masterEnabled", "profiles"]);

        let entry = AiPreparedEntryDto {
            item_id: 12,
            zone: "local-app-data".into(),
            relative_depth: 2,
            display_name: "cache".into(),
            source_kind: "unknown-provider".into(),
            category_hint: "unknown".into(),
            product_hint: None,
            size_bucket: "under-64-mi-b".into(),
            age_bucket: "under-30-days".into(),
            signals: vec!["path-layout".into()],
        };
        assert_keys(
            &entry,
            &[
                "itemId",
                "zone",
                "relativeDepth",
                "displayName",
                "sourceKind",
                "categoryHint",
                "productHint",
                "sizeBucket",
                "ageBucket",
                "signals",
            ],
        );
        let batch = AiPreparedBatchDto {
            batch_id: "batch-1".into(),
            scan_generation: 7,
            profile_id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
            includes_paths: false,
            entries: vec![entry],
        };
        assert_keys(
            &batch,
            &[
                "batchId",
                "scanGeneration",
                "profileId",
                "includesPaths",
                "entries",
            ],
        );

        let suggestion = AiSuggestionDto {
            item_id: 12,
            suggested_risk: "review".into(),
            confidence: 0.8,
            reason: "sanitized reason".into(),
            product_guess: None,
            final_risk_options: vec![RiskLevelArg::Review],
        };
        assert_keys(
            &suggestion,
            &[
                "itemId",
                "suggestedRisk",
                "confidence",
                "reason",
                "productGuess",
                "finalRiskOptions",
            ],
        );
        let result = AiConfirmResultDto {
            confirmed_count: 1,
            audit_warning: false,
        };
        assert_keys(&result, &["confirmedCount", "auditWarning"]);
    }

    #[test]
    fn scan_wire_arg_deserialises_scope_enum() {
        // Frontend: invoke("scan", { scope: { kind: "dev-cache" } })
        let args: ScanArgs =
            serde_json::from_str(r#"{ "scope": { "kind": "dev-cache" } }"#).unwrap();
        assert!(matches!(args.scope, ScanScope::DevCache));

        let args: ScanArgs =
            serde_json::from_str(r#"{ "scope": { "kind": "projects", "roots": ["D:\\Code"] } }"#)
                .unwrap();
        match args.scope {
            ScanScope::Projects { roots } => assert_eq!(roots, vec!["D:\\Code"]),
            other => panic!("expected projects scope, got {other:?}"),
        }

        let args: ScanArgs = serde_json::from_str(r#"{ "scope": { "kind": "unknown" } }"#).unwrap();
        assert!(matches!(args.scope, ScanScope::Unknown));
        // Unknown kind fails closed.
        assert!(serde_json::from_str::<ScanArgs>(r#"{ "scope": { "kind": "bogus" } }"#).is_err());
    }

    #[test]
    fn set_disposition_wire_uses_flat_item_id_and_kebab_disposition() {
        // Frontend: invoke("set_disposition", { itemId: 7, disposition: "ignore" })
        let args: SetDispositionArgs =
            serde_json::from_str(r#"{ "itemId": 7, "disposition": "ignore" }"#).unwrap();
        assert_eq!(args.item_id, 7);
        assert_eq!(args.disposition, DispositionArg::Ignore);

        let args: SetDispositionArgs =
            serde_json::from_str(r#"{ "itemId": 3, "disposition": "protect" }"#).unwrap();
        assert_eq!(args.item_id, 3);
        assert_eq!(args.disposition, DispositionArg::Protect);

        // snake_case wire keys must fail (they are not the camelCase contract).
        assert!(serde_json::from_str::<SetDispositionArgs>(
            r#"{ "item_id": 7, "disposition": "ignore" }"#
        )
        .is_err());
    }

    #[test]
    fn plan_and_engine_wire_args() {
        let args: CreateCleanupPlanArgs =
            serde_json::from_str(r#"{ "itemIds": [1, 2], "policy": "redownload" }"#).unwrap();
        assert_eq!(args.item_ids, vec![1, 2]);
        assert_eq!(args.policy, ConfirmPolicyArg::Redownload);

        let args: ExecuteCleanupPlanArgs =
            serde_json::from_str(r#"{ "planId": 4, "policy": "default", "dryRun": true }"#)
                .unwrap();
        assert_eq!(args.plan_id, 4);
        assert_eq!(args.policy, ConfirmPolicyArg::Default);
        assert!(args.dry_run);

        let args: CancelScanArgs = serde_json::from_str(r#"{ "scanId": 2 }"#).unwrap();
        assert_eq!(args.scan_id, 2);
    }

    #[test]
    fn journal_and_folder_wire_args() {
        let args: GetJournalArgs = serde_json::from_str(r#"{ "lastN": 5 }"#).unwrap();
        assert_eq!(args.last_n, Some(5));
        let args: GetJournalArgs = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(args.last_n, None);

        let args: OpenFolderArgs = serde_json::from_str(r#"{ "itemId": 11 }"#).unwrap();
        assert_eq!(args.item_id, 11);
    }

    // ---- F-M4-2: response DTO wire key sets (exact) ------------------------
    //
    // Each of these asserts the precise serialised key set of a command
    // result. A field rename/removal (like the earlier `risk` ->
    // `suggestedRisk` drift) now fails here instead of breaking the frontend
    // silently. Keep the expected lists in sync with src/types/index.ts.

    #[test]
    fn disposition_result_dto_response_key_set_is_exact() {
        let dto = DispositionResultDto {
            item_id: 7,
            rule_id: "user-protected/1".into(),
            path: r"C:\x\.tiger".into(),
            effect: "protected".into(),
        };
        assert_keys(&dto, &["itemId", "ruleId", "path", "effect"]);
    }

    #[test]
    fn scan_handle_dto_response_key_set_is_exact() {
        let dto = ScanHandleDto { scan_id: 3 };
        assert_keys(&dto, &["scanId"]);
    }

    #[test]
    fn scan_snapshot_response_key_set_is_exact() {
        // `get_scan_results` returns the core/providers ScanSnapshot verbatim
        // (snake_case fields, no rename_all). This is the actual object shape
        // the frontend receives.
        let snapshot = ScanSnapshot::new(ScanMode::Fixtures, vec![], vec![]);
        assert_keys(
            &snapshot,
            &[
                "scanned_at",
                "generation",
                "mode",
                "items",
                "warnings",
                "cancelled",
            ],
        );
    }

    #[test]
    fn cleanup_plan_dto_response_key_set_is_exact() {
        let dto = CleanupPlanDto {
            plan_id: 1,
            dry_run: false,
            total_estimated_bytes: 0,
            items: vec![],
            skipped: vec![],
        };
        assert_keys(
            &dto,
            &[
                "planId",
                "dryRun",
                "totalEstimatedBytes",
                "items",
                "skipped",
            ],
        );

        // Nested item shapes (non-empty so the fields are exercised).
        let planned = PlannedItemDto {
            scan_item_id: 1,
            action: "recycle".into(),
            confirmation: "none".into(),
            estimated_size: 0,
            path: r"C:\x".into(),
        };
        assert_keys(
            &planned,
            &[
                "scanItemId",
                "action",
                "confirmation",
                "estimatedSize",
                "path",
            ],
        );
        let skipped = SkippedItemDto {
            scan_item_id: 2,
            path: r"C:\y".into(),
            reason: "skip: protected-risk".into(),
        };
        assert_keys(&skipped, &["scanItemId", "path", "reason"]);
    }

    #[test]
    fn cleanup_session_dto_response_key_set_is_exact() {
        let dto = CleanupSessionDto {
            session_id: 1,
            plan_id: 1,
            dry_run: true,
            totals: SessionTotalsDto {
                planned_bytes: 0,
                completed_bytes: 0,
                succeeded: 0,
                skipped: 0,
                failed: 0,
            },
            items: vec![],
            journal_degraded: false,
        };
        assert_keys(
            &dto,
            &[
                "sessionId",
                "planId",
                "dryRun",
                "totals",
                "items",
                "journalDegraded",
            ],
        );

        let totals = SessionTotalsDto {
            planned_bytes: 1,
            completed_bytes: 2,
            succeeded: 3,
            skipped: 4,
            failed: 5,
        };
        assert_keys(
            &totals,
            &[
                "plannedBytes",
                "completedBytes",
                "succeeded",
                "skipped",
                "failed",
            ],
        );

        let item = SessionItemDto {
            scan_item_id: 1,
            path: r"C:\x".into(),
            product: None,
            action: "recycle".into(),
            estimated_size: 0,
            status: "skipped".into(),
            detail: Some("deny:target-missing".into()),
        };
        assert_keys(
            &item,
            &[
                "scanItemId",
                "path",
                "product",
                "action",
                "estimatedSize",
                "status",
                "detail",
            ],
        );
    }

    #[test]
    fn journal_entry_dto_response_key_set_is_exact() {
        let dto = JournalEntryDto {
            session_id: 1,
            time_secs: 0,
            phase: "result".into(),
            product: None,
            rule: None,
            provider: None,
            path: None,
            action: None,
            estimated_size: 0,
            result: None,
            error: None,
        };
        assert_keys(
            &dto,
            &[
                "sessionId",
                "timeSecs",
                "phase",
                "product",
                "rule",
                "provider",
                "path",
                "action",
                "estimatedSize",
                "result",
                "error",
            ],
        );
    }

    #[test]
    fn command_error_response_keys_honour_the_optional_required_field() {
        // Plain error: exactly { code, message } — `required` is omitted.
        let plain = CommandError::new(ErrorCode::ScanNotFound, "nothing scanned");
        assert_keys(&plain, &["code", "message"]);

        // Confirmation-required error: { code, message, required }.
        let gate = CommandError::confirmation_required(
            devresidue_core::domain::plan::ConfirmRequirement::Review,
        );
        assert_keys(&gate, &["code", "message", "required"]);
        let json = serde_json::to_value(&gate).unwrap();
        assert_eq!(json["required"], "review");
    }

    #[test]
    fn rule_dto_response_key_set_is_exact() {
        let valid = RuleDto {
            rule_id: "builtin-protected/ssh".into(),
            source: Some("builtin-protected".into()),
            risk: Some("protected".into()),
            category: Some("credential".into()),
            description: Some("ssh keys".into()),
            valid: true,
            issues: vec![],
        };
        assert_keys(
            &valid,
            &[
                "ruleId",
                "source",
                "risk",
                "category",
                "description",
                "valid",
                "issues",
            ],
        );

        // An invalid (failed-to-load) rule serialises the same key set with
        // null metadata — the frontend can rely on the shape.
        let invalid = RuleDto {
            rule_id: "user-detection/broken".into(),
            source: None,
            risk: None,
            category: None,
            description: None,
            valid: false,
            issues: vec!["yaml parse error".into()],
        };
        let json = serde_json::to_value(&invalid).unwrap();
        assert_eq!(json["valid"], false);
        assert_eq!(json["issues"][0], "yaml parse error");
        assert!(json["source"].is_null(), "no metadata for invalid rules");
    }

    #[test]
    fn rules_validation_dto_and_issue_key_sets_are_exact() {
        let dto = RulesValidationDto {
            total: 3,
            errors: 1,
            warnings: 2,
            issues: vec![
                RuleIssueDto {
                    rule_id: Some("broken".into()),
                    message: "parse error".into(),
                    severity: "error".into(),
                },
                RuleIssueDto {
                    rule_id: None,
                    message: "file note".into(),
                    severity: "warning".into(),
                },
            ],
        };
        assert_keys(&dto, &["total", "errors", "warnings", "issues"]);
        for issue in &dto.issues {
            assert_keys(issue, &["ruleId", "message", "severity"]);
        }
        let json = serde_json::to_value(&dto.issues[1]).unwrap();
        assert!(json["ruleId"].is_null(), "file-level issue has null ruleId");
    }

    #[test]
    fn scan_done_payload_carries_generation_and_error_payload_is_distinct() {
        // R10: the done payload now carries the persisted snapshot's
        // generation so the frontend can reject stale finishes. Event payload
        // structs have no rename_all — the wire keys are the snake_case Rust
        // field names (scan_id, consistent with the other scan:// payloads).
        let done = ScanDonePayload {
            scan_id: 4,
            cancelled: false,
            total: 12,
            generation: 3,
        };
        let json = serde_json::to_value(&done).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["generation"], 3);
        assert!(obj.contains_key("total"));
        assert!(obj.contains_key("cancelled"));
        assert!(obj.contains_key("scan_id"));

        let err = ScanErrorPayload {
            scan_id: 4,
            message: "boom".into(),
        };
        let json = serde_json::to_value(&err).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj["message"], "boom");
        assert_eq!(obj["scan_id"], 4);
        assert!(!obj.contains_key("cancelled"), "error is not a done");
    }
}
