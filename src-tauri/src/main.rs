//! DevResidue — Tauri v2 shell entry point.
//!
//! This crate is the *shell*: it wires the real Windows adapters (probes,
//! delete port, tool shell) into the core planner/engine and exposes an
//! ID-based command layer to the React frontend. The dependency direction is
//! strictly `src-tauri -> crates/*`; core never sees a Tauri type.
//!
//! Command surface (SPEC §20 — every request is an opaque id or a structured
//! scope; there is deliberately **no** `delete_path` / `run_command` /
//! `remove_directory`):
//!
//! ```text
//! scan(scope)                    -> ScanHandleDto         (events: scan://*)
//! cancel_scan(scan_id)           -> bool
//! get_scan_results()             -> ScanSnapshot
//! create_cleanup_plan(item_ids, policy) -> CleanupPlanDto
//! execute_cleanup_plan(plan_id, policy, dry_run) -> CleanupSessionDto
//!                                                       (events: cleanup://item)
//! get_journal(last_n?)           -> Vec<JournalEntryDto>
//! set_disposition(item_id, disposition) -> DispositionResultDto  (Phase 13)
//! open_folder(item_id)           -> ()                              (Phase 13)
//! get_settings()                 -> SettingsDto                     (Phase 14)
//! set_analyzer_enabled(bool)     -> ()
//! analyze_item(item_id)          -> SuggestionDto
//! create_rule_from_suggestion(item_id, suggested_risk) -> DispositionResultDto
//! get_rules()                   -> Vec<RuleDto>                    (R11)
//! validate_rules()              -> RulesValidationDto              (R11)
//! ```
//!
//! # IPC contract notes (frontend ↔ backend)
//!
//! - **Command names equal the function identifiers** (Tauri v2 `#[command]`
//!   registers `stringify!(fn)`), so the functions below are intentionally
//!   **prefix-free** (`scan`, not `cmd_scan`). Renaming a command means
//!   renaming the function here and in its module.
//! - **Argument names are camelCase on the wire** (Tauri v2 defaults to
//!   `ArgumentCase::Camel`): a Rust `item_id` parameter is invoked by the
//!   frontend as `{ itemId }`. No `rename_all` attributes are needed.
//! - Every mutation command takes an opaque item/plan id — never a path
//!   (SPEC §20 / INV-013).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analyzer;
mod contract;
mod disposition;
mod ipc_contract;
mod journal;
mod plan;
mod rules;
mod scan;
mod state;
mod support;

use state::AppState;

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            // Keep in sync with ipc_contract::REGISTERED_COMMANDS (the tests
            // pin the wire names).
            scan::scan,
            scan::cancel_scan,
            scan::get_scan_results,
            plan::create_cleanup_plan,
            plan::execute_cleanup_plan,
            journal::get_journal,
            journal::clear_all_data,
            disposition::set_disposition,
            disposition::open_folder,
            analyzer::get_settings,
            analyzer::set_analyzer_enabled,
            analyzer::analyze_item,
            analyzer::create_rule_from_suggestion,
            rules::get_rules,
            rules::validate_rules
        ])
        .run(tauri::generate_context!())
        .expect("error while running DevResidue");
}
