//! `devresidue unknown ignore|protect <item-id>` — user dispositions for the
//! unknown developer-data provider (SPEC §25 actions).
//!
//! Both commands are strictly ID-based (INV-013): the id is resolved against
//! the most recent scan, and the resulting path is written as a user rule (see
//! `devresidue_core::rules::user`). `Ignore` pre-seeds the path into the next
//! scan's seen-set so it never appears again; `Protect` makes it surface as
//! Protected via the F-2-1 rule gate.

use devresidue_core::rules::{upsert_disposition, user_rules_dir, DispositionKind};

use super::support;
use crate::UnknownCommand;

/// Runs one disposition subcommand.
pub fn run(cmd: UnknownCommand) -> Result<(), String> {
    let (kind, item_id) = match cmd {
        UnknownCommand::Ignore { item_id } => (DispositionKind::Ignore, item_id),
        UnknownCommand::Protect { item_id } => (DispositionKind::Protect, item_id),
    };

    let data_dir = support::data_dir()?;
    let user_dir = user_rules_dir(&data_dir);
    std::fs::create_dir_all(&user_dir)
        .map_err(|e| format!("create {}: {e}", user_dir.display()))?;

    let item = support::find_latest_item(item_id)?;
    let rule_id = upsert_disposition(
        &user_dir,
        &item.path,
        item.product.as_deref(),
        item.category,
        kind,
    )?;

    match kind {
        DispositionKind::Ignore => println!(
            "ignored {} (rule {rule_id}); it will not appear in future scans",
            item.path.display()
        ),
        DispositionKind::Protect => println!(
            "protected {} (rule {rule_id}); future scans will list it as Protected \
             (never auto-cleanable)",
            item.path.display()
        ),
    }
    println!(
        "rules written under {}; run `devresidue scan` to apply them",
        user_dir.display()
    );
    Ok(())
}
