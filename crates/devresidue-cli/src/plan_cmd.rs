//! `devresidue plan` — build a cleanup plan from the most recent scan (the
//! persisted `last-scan.json`), persist it and print a summary (SPEC §16 /
//! §19 / §23).

use std::path::Path;

use devresidue_core::cleanup::planner::{ConfirmPolicy, PlannerOutput};
use devresidue_core::cleanup::CleanupPlanner;
use devresidue_core::domain::plan::ConfirmRequirement;
use devresidue_core::{ScanItem, ScanItemId};
use devresidue_providers::scan_store;

use super::support;
use crate::scan::human_bytes;
use crate::PlanOptions;

/// Runs the plan subcommand against the most recent scan.
pub fn run(opts: PlanOptions) -> Result<(), String> {
    let base = support::data_dir()?;
    let _operation_lock = support::acquire_app_operation_lock(&base)?;
    let snapshot = scan_store::load(&base).map_err(|e| {
        format!(
            "cannot read the most recent scan ({e}); run `devresidue scan` first \
             (plan ids refer to the latest scan only)"
        )
    })?;
    reject_partial_scan(snapshot.cancelled, opts.allow_partial)?;
    // R3-G04: capture the scan-persisted workspace roots before the items are
    // moved out (the roots are part of the snapshot's protection context).
    let workspace_roots = support::workspace_protection_roots(&base, Some(&snapshot));
    let items = snapshot.items;

    // R4-H04: a bare `--items` selection MUST pin the scan generation the
    // ids came from. Without the pin, ids from an older scan (read from an
    // old terminal, a script, or muscle memory) would silently resolve
    // against the newest snapshot's objects after a cross-process id
    // renumber — the R3-G03 gate only compared when the flag was supplied,
    // so omitting it bypassed the protection entirely. `--safe` selects
    // "the current scan's cleanable items" by construction (no stale ids
    // involved) and stays pin-free.
    if opts.items.is_some() {
        match opts.scan_generation {
            Some(user_generation) => {
                if user_generation != snapshot.generation {
                    return Err(format!(
                        "selection-generation-mismatch: the selection was made against scan \
                         generation {user_generation} but the latest scan is generation \
                         {}; re-run `devresidue scan` and plan from its ids",
                        snapshot.generation
                    ));
                }
            }
            None => {
                return Err(
                    "plan --items requires --scan-generation <gen> (the generation printed \
                     by the scan that listed the ids); a bare id list cannot prove which \
                     scan it selected from"
                        .to_string(),
                );
            }
        }
    }

    let (selection, policy) = selection_and_policy(&opts, &items)?;

    // R3-G04: the plan-time validator carries the workspace protection roots
    // (scan-persisted ∪ current env) so a workspace root can never be planned
    // as a cleanup target (SPEC §11 / INV-011).
    let validator = support::build_validator_with_roots(workspace_roots, Vec::new())?;
    // R03/R08 rule gate at plan time (same merged set the scan used): the
    // planner revalidates protected descendants and rule discovery sources
    // before anything is persisted.
    let scan_rules = support::load_scan_rules(&base)?;
    let mut store = support::open_store_at(&base)?;
    let planner = CleanupPlanner::new_with_rules(validator, policy, scan_rules.rules)
        // R2-F03: bind every plan to the generation it was built against.
        .with_generation(snapshot.generation)
        // R2-F05: real scans require scan-time snapshots on every item.
        .with_real_scan(true);

    let (id, output) = planner
        .build_and_store(&items, &selection, &mut store)
        .map_err(|e| match e {
            devresidue_core::cleanup::PlannerError::UnknownItem(id) => format!(
                "item id {id} does not belong to the most recent scan — a newer scan \
                 replaced it; re-run `devresidue scan` then plan again"
            ),
            other => other.to_string(),
        })?;
    println!(
        "Plan {} saved to {}",
        id.raw(),
        store.dir().join(format!("{}.json", id.raw())).display()
    );
    print_plan_summary(&output);
    // Tell the user *before* they hit the clean confirmation gate which flag
    // unlocks the rest (previously only the engine error revealed it).
    let (redownload, review) = confirmation_requirements(&output);
    for line in confirmation_hint_lines(id.raw(), redownload, review) {
        println!("{line}");
    }
    Ok(())
}

/// Error text returned when planning against a cancelled (partial) scan.
pub const PARTIAL_SCAN_ERROR: &str =
    "last scan was cancelled and results are partial; re-scan, or pass --allow-partial";

/// Safety gate shared by `plan` and `clean --safe` (F-6-1 / R09): a cancelled
/// scan holds partial results, so planning from it is refused by default.
/// `--allow-partial` is the explicit escape hatch for users who accept an
/// incomplete basis. Rendered messages refer to the current command generically.
pub(crate) fn reject_partial_scan(cancelled: bool, allow_partial: bool) -> Result<(), String> {
    if !cancelled {
        return Ok(());
    }
    if allow_partial {
        println!(
            "warning: working from a cancelled scan — the results are partial; \
             proceeding because --allow-partial was passed"
        );
        return Ok(());
    }
    Err(PARTIAL_SCAN_ERROR.to_string())
}

/// Whether the plan needs extra confirmation flags at clean time: any planned
/// item whose confirmation exceeds the current policy, plus anything skipped
/// purely because of an unmet confirmation level.
pub fn confirmation_requirements(output: &PlannerOutput) -> (bool, bool) {
    let mut redownload = false;
    let mut review = false;
    for item in &output.plan.items {
        match item.confirmation {
            ConfirmRequirement::None => {}
            ConfirmRequirement::Redownload => redownload = true,
            ConfirmRequirement::Review => review = true,
        }
    }
    for skip in &output.skipped {
        if let devresidue_core::cleanup::SkipReason::ConfirmationRequired { required } =
            &skip.reason
        {
            match required {
                ConfirmRequirement::None => {}
                ConfirmRequirement::Redownload => redownload = true,
                ConfirmRequirement::Review => review = true,
            }
        }
    }
    (redownload, review)
}

/// Human lines describing which clean flags unlock the full plan.
pub fn confirmation_hint_lines(plan_id: u64, redownload: bool, review: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if review {
        lines.push(format!(
            "confirmation needed to run: clean --plan {plan_id} --confirm-review"
        ));
    } else if redownload {
        lines.push(format!(
            "confirmation needed to run: clean --plan {plan_id} --confirm-redownload"
        ));
    }
    lines
}

/// Resolves the `--safe` / `--items` selection against the scanned items plus
/// the confirmation policy derived from the confirm flags.
pub fn selection_and_policy(
    opts: &PlanOptions,
    items: &[ScanItem],
) -> Result<(Vec<ScanItemId>, ConfirmPolicy), String> {
    let policy = support::policy_from(opts.confirm_redownload, opts.confirm_review, opts.yes);

    let ids: Vec<ScanItemId> = match (opts.safe, opts.items.as_deref()) {
        (true, None) => items.iter().map(|i| i.id).collect(),
        (false, Some(text)) => support::parse_ids(text)?,
        (true, Some(_)) => {
            return Err("choose either --safe or --items, not both".to_string());
        }
        (false, None) => {
            return Err("plan requires a selection (--safe or --items <ids>)".to_string());
        }
    };
    Ok((ids, policy))
}

/// Prints the plan summary: per-item action / confirmation / size plus the
/// intentionally skipped selections.
pub fn print_plan_summary(output: &PlannerOutput) {
    println!("\nPlanned items ({}):", output.plan.items.len());
    if output.plan.items.is_empty() {
        println!("  (none)");
    }
    for item in &output.plan.items {
        let action = match item.mode {
            // RecycleBin executes as a verified permanent deletion (engine
            // routing); label the plan summary honestly.
            devresidue_core::CleanupMode::RecycleBin => "delete",
            devresidue_core::CleanupMode::DirectDelete => "delete",
            devresidue_core::CleanupMode::ExternalCommand => "execute",
        };
        println!(
            "  #{}  {action:<7}  size={:>10}  confirm={:<10}  {}",
            item.scan_item_id.raw(),
            human_bytes(item.estimated_size),
            confirm_label(item.confirmation),
            item.snapshot.path.display()
        );
    }
    if !output.skipped.is_empty() {
        println!("\nSkipped ({}):", output.skipped.len());
        for skip in &output.skipped {
            println!(
                "  #{}  {:<38}  {}",
                skip.scan_item_id.raw(),
                skip_reason_label(&skip.reason),
                skip.path.display()
            );
        }
    }
}

pub(crate) fn confirm_label(req: ConfirmRequirement) -> &'static str {
    match req {
        ConfirmRequirement::None => "none",
        ConfirmRequirement::Redownload => "redownload",
        ConfirmRequirement::Review => "review",
    }
}

pub(crate) fn skip_reason_label(reason: &devresidue_core::cleanup::SkipReason) -> String {
    use devresidue_core::cleanup::SkipReason as R;
    match reason {
        R::ProtectedRisk => "skip: protected-risk".into(),
        R::UnknownRisk => "skip: unknown-risk".into(),
        R::ActionNone => "skip: action none".into(),
        R::ActionDeferred { reason } => format!("skip: deferred ({reason})"),
        R::ConfirmationRequired { required } => {
            format!("skip: requires {} confirmation", confirm_label(*required))
        }
        R::Unverifiable { detail } => format!("skip: unverifiable ({detail})"),
        R::UnknownSelection => "skip: unknown selection".into(),
        R::ProtectedDescendant { area } => {
            format!("skip: protected area inside target ({})", area.display())
        }
        R::DiscoverySourceInvalidated { rule_id } => {
            format!("skip: discovery source invalidated (rule {rule_id:?})")
        }
        R::MissingScanSnapshot => "skip: item lacks a scan-time snapshot (re-scan required)".into(),
        R::CurrentProtectedRoot { root } => format!(
            "skip: target hits the current protected-root set ({}) — protected at plan time",
            root.display()
        ),
    }
}

/// Path display guard (absolute paths from plans only).
#[allow(dead_code)]
pub(crate) fn display(path: &Path) -> &Path {
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_lines_reveal_the_missing_clean_flag() {
        assert_eq!(
            confirmation_hint_lines(7, false, false),
            Vec::<String>::new()
        );
        assert_eq!(
            confirmation_hint_lines(7, true, false),
            vec!["confirmation needed to run: clean --plan 7 --confirm-redownload"]
        );
        // Review subsumes redownload at clean time (Review policy permits
        // RegenerableDownload items), so only the strictest flag is hinted.
        assert_eq!(
            confirmation_hint_lines(7, true, true),
            vec!["confirmation needed to run: clean --plan 7 --confirm-review"]
        );
    }

    #[test]
    fn cancelled_scan_refuses_plan_unless_allow_partial() {
        // F-6-1: default = refuse partial results; --allow-partial opens it.
        assert_eq!(
            reject_partial_scan(true, false),
            Err(PARTIAL_SCAN_ERROR.to_string())
        );
        assert_eq!(reject_partial_scan(true, true), Ok(()));
        // Completed scans (and snapshots written before the flag existed) pass.
        assert_eq!(reject_partial_scan(false, false), Ok(()));
        assert_eq!(reject_partial_scan(false, true), Ok(()));
    }

    // ---- R3-G03 / R4-H04: a bare --items selection must be pinned ---------

    /// Builds the plan command's snapshot-dependent early gate in isolation:
    /// mirrors `run`'s order (load → partial gate → generation gate) without
    /// touching the real data dir.
    fn generation_gate(
        user_generation: Option<u64>,
        snapshot_generation: u64,
    ) -> Result<(), String> {
        // Same check `run` performs for a `--items` selection.
        match user_generation {
            Some(user) if user != snapshot_generation => Err(format!(
                "selection-generation-mismatch: the selection was made against scan \
                 generation {user} but the latest scan is generation \
                 {}; re-run `devresidue scan` and plan from its ids",
                snapshot_generation
            )),
            // R4-H04: no pin at all is now a hard error — a bare id list
            // cannot prove which scan it selected from.
            None => Err(
                "plan --items requires --scan-generation <gen> (the generation printed \
                 by the scan that listed the ids); a bare id list cannot prove which \
                 scan it selected from"
                    .to_string(),
            ),
            Some(_) => Ok(()),
        }
    }

    #[test]
    fn r3g03_selection_from_an_older_generation_is_refused() {
        // The user selected ids during scan A (generation 7); the store now
        // holds scan B (generation 8) — the selection must be refused, not
        // re-resolved against B's ids (which may alias different objects
        // after a cross-process renumber).
        let err = generation_gate(Some(7), 8).expect_err("stale selection refused");
        assert!(
            err.starts_with("selection-generation-mismatch"),
            "got: {err}"
        );
        // A matching generation passes.
        assert!(generation_gate(Some(8), 8).is_ok());
    }

    #[test]
    fn h04_bare_items_selection_without_a_generation_pin_is_refused() {
        // R4-H04: `plan --items 1` with no `--scan-generation` used to plan
        // against the NEWEST snapshot's objects (the exact production
        // incident: gen 7 selection planned fine against gen 8). The pin is
        // now mandatory for --items.
        let err = generation_gate(None, 8).expect_err("unpinned --items must be refused");
        assert!(
            err.contains("requires --scan-generation"),
            "the refusal explains the required pin, got: {err}"
        );
    }
}
