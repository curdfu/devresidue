//! `set_disposition` / `open_folder` commands (SPEC §25 actions).
//!
//! Both are strictly ID-based: the item id is resolved against the most recent
//! scan and only that registered path is touched — there is no path parameter
//! (INV-013). `open_folder` opens the item's directory in Explorer (read-only,
//! "Open Folder" action); `set_disposition` writes a user rule (Ignore →
//! path disappears from future scans, Protect → surfaces as Protected).

use devresidue_core::rules::{upsert_disposition, user_rules_dir, DispositionKind};
use tauri::State;

use crate::contract::{CommandError, DispositionArg, DispositionResultDto, ErrorCode};
use crate::state::AppState;

/// `set_disposition` command: apply Ignore / Protect to one scan item.
#[tauri::command]
pub fn set_disposition(
    state: State<'_, AppState>,
    item_id: u64,
    disposition: DispositionArg,
) -> Result<DispositionResultDto, CommandError> {
    let (data_dir, item) = {
        let model = state.model.lock().unwrap();
        let item = model
            .latest()
            .and_then(|snap| snap.items.iter().find(|i| i.id.raw() == item_id).cloned());
        let data_dir = model.data_dir().to_path_buf();
        let item = item.ok_or_else(|| {
            CommandError::new(
                ErrorCode::InvalidItem,
                format!(
                    "item id {item_id} does not belong to the latest scan — re-scan then \
                     retry"
                ),
            )
        })?;
        (data_dir, item)
    };

    let kind = match disposition {
        DispositionArg::Ignore => DispositionKind::Ignore,
        DispositionArg::Protect => DispositionKind::Protect,
    };
    let user_dir = user_rules_dir(&data_dir);
    std::fs::create_dir_all(&user_dir).map_err(|e| {
        CommandError::new(
            ErrorCode::Engine,
            format!("create {}: {e}", user_dir.display()),
        )
    })?;
    let rule_id = upsert_disposition(
        &user_dir,
        &item.path,
        item.product.as_deref(),
        item.category,
        kind,
    )
    .map_err(|e| CommandError::new(ErrorCode::Engine, e))?;

    Ok(DispositionResultDto {
        item_id,
        rule_id,
        path: item.path.display().to_string(),
        effect: match kind {
            DispositionKind::Ignore => "ignored; will not appear in future scans".into(),
            DispositionKind::Protect => "protected; future scans list it as Protected".into(),
        },
    })
}

/// `open_folder` command: open one scan item's directory in Explorer.
///
/// The path comes exclusively from the registered scan item (never from the
/// request), so this is the SPEC §25 "Open Folder" action with no arbitrary
/// path surface.
#[tauri::command]
pub fn open_folder(state: State<'_, AppState>, item_id: u64) -> Result<(), CommandError> {
    let item = {
        let model = state.model.lock().unwrap();
        model
            .latest()
            .and_then(|snap| snap.items.iter().find(|i| i.id.raw() == item_id).cloned())
    };
    let item = item.ok_or_else(|| {
        CommandError::new(
            ErrorCode::InvalidItem,
            format!("item id {item_id} does not belong to the latest scan — re-scan then retry"),
        )
    })?;
    let path = &item.path;
    if !path.is_dir() {
        return Err(CommandError::new(
            ErrorCode::Engine,
            format!("cannot open {}: not a directory", path.display()),
        ));
    }
    // Explorer is the Windows file manager; opening a folder is read-only.
    // Path comes from a trusted scan item, never from a command argument.
    let explorer = std::process::Command::new("explorer")
        .arg(path)
        .spawn()
        .map_err(|e| {
            CommandError::new(
                ErrorCode::Engine,
                format!("failed to start Explorer for {}: {e}", path.display()),
            )
        })?;
    // Detach: Explorer keeps running after we return.
    drop(explorer);
    Ok(())
}
