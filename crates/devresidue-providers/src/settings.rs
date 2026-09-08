//! User settings — `settings.json` under the DevResidue data directory
//! (honouring the `DEVRESIDUE_DATA_DIR` override used by tests).
//!
//! Current keys:
//!
//! ```json
//! { "analyzer_enabled": false }
//! ```
//!
//! The analyzer (SPEC §26) is **opt-in**: it stays off until the user enables
//! it through the UI (`set_analyzer_enabled`) — "AI 完全关闭时程序功能不受
//! 影响" (PLAN Phase 14 acceptance). Missing file = all defaults (off).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `settings.json` under a data root.
#[must_use]
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// The persisted settings (defaults = off).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// Whether the directory analyzer may run (SPEC §26, default off).
    #[serde(default)]
    pub analyzer_enabled: bool,
}

/// Loads the settings. A missing file yields the defaults; a corrupt file is a
/// hard error (fail closed — never silently treat corrupt settings as off if
/// the user enabled them).
pub fn load_settings(data_dir: &Path) -> Result<Settings, String> {
    let path = settings_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => Ok(Settings::default()),
        Ok(text) => {
            serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Persists the analyzer-enabled flag (creating the data directory as needed).
pub fn set_analyzer_enabled(data_dir: &Path, enabled: bool) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("create {}: {e}", data_dir.display()))?;
    let settings = Settings {
        analyzer_enabled: enabled,
    };
    let json =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialise settings: {e}"))?;
    std::fs::write(settings_path(data_dir), json)
        .map_err(|e| format!("write {}: {e}", settings_path(data_dir).display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_off_and_round_trip() {
        let dir = std::env::temp_dir().join(format!("dr-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let defaults = load_settings(&dir).unwrap();
        assert!(!defaults.analyzer_enabled);

        set_analyzer_enabled(&dir, true).unwrap();
        assert!(load_settings(&dir).unwrap().analyzer_enabled);

        // Corrupt file fails closed.
        std::fs::write(settings_path(&dir), b"{not json").unwrap();
        assert!(load_settings(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
