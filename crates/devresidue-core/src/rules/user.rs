//! User disposition persistence — `Ignore` / `Protect` commands write one rule
//! per target into `<data_dir>/rules/user/user-dispositions.yaml`.
//!
//! # Disposition → rule mapping (Phase 13)
//!
//! - **Ignore**  → a `user-ignore/<n>` declaration (`source: user`). These are
//!   **not compiled** into the resolve set (see `loader::USER_IGNORE_ID_PREFIX`):
//!   the scan layer pre-seeds their anchor paths into the context seen-set, so
//!   the path never appears in any scan again.
//! - **Protect** → a `source: user-protected` rule with `risk: protected`.
//!   Loaded by the user-directory collector and resolved at scan time, where
//!   the F-2-1 rule application forces the item to `Protected` (never planned,
//!   INV-002).
//!
//! Both kinds are plain `RuleDoc`s inside a normal `RuleFile`, so the ordinary
//! loader validation (dangerous-root rejection, protected-source rules, ...)
//! applies on the next load. A disposition on the same target path **replaces**
//! any earlier user disposition rule (idempotent overwrite, keep the file
//! small).

use std::path::{Path, PathBuf};

use crate::rules::loader::USER_IGNORE_ID_PREFIX;
use crate::rules::priority::RuleSource;
use crate::rules::schema::{MatchSpec, RuleDoc, RuleFile};
use crate::rules::validator::validate_rule_file;
use crate::{ResidueCategory, RiskLevel};

/// The disposition a user applies to one scan item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispositionKind {
    /// Do not report this path again (not a classification).
    Ignore,
    /// Report it as Protected from now on.
    Protect,
}

impl DispositionKind {
    /// Short label used in ids / messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            DispositionKind::Ignore => "ignore",
            DispositionKind::Protect => "protect",
        }
    }
}

/// `<data_dir>/rules/user` — the user rules directory (created on demand).
#[must_use]
pub fn user_rules_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("rules").join("user")
}

/// `<user_rules_dir>/user-dispositions.yaml`.
#[must_use]
pub fn dispositions_path(user_dir: &Path) -> PathBuf {
    user_dir.join("user-dispositions.yaml")
}

/// Creates the user rules directory and returns its path.
pub fn ensure_user_rules_dir(data_dir: &Path) -> Result<PathBuf, String> {
    let dir = user_rules_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Inserts (or replaces) a disposition rule for `target`.
///
/// Returns the new rule id. Idempotent: any earlier user rule anchoring the
/// same exact path is dropped first, so toggling Ignore→Protect (or back)
/// leaves exactly one disposition per path.
pub fn upsert_disposition(
    user_dir: &Path,
    target: &Path,
    product: Option<&str>,
    category: ResidueCategory,
    kind: DispositionKind,
) -> Result<String, String> {
    let path = dispositions_path(user_dir);
    let mut file = read_or_empty(&path)?;

    let target_text = target.to_string_lossy().into_owned();
    // Drop every earlier user disposition anchored on the same path.
    file.rules.retain(|rule| {
        let is_user_disposition = rule.source == RuleSource::User
            || rule.source == RuleSource::UserProtected
            || rule.id.starts_with(USER_IGNORE_ID_PREFIX);
        let anchors_here = rule.match_spec.exact.as_deref() == Some(target_text.as_str());
        !(is_user_disposition && anchors_here)
    });

    let seq = file.rules.len() + 1;
    let new_rule = match kind {
        DispositionKind::Ignore => RuleDoc {
            id: format!("{USER_IGNORE_ID_PREFIX}{seq}"),
            description: format!("user ignore: {} ({})", target.display(), target_text),
            product: product.map(str::to_string),
            category,
            risk: RiskLevel::Protected, // never deletable even if re-evaluated elsewhere
            source: RuleSource::User,
            match_spec: MatchSpec {
                exact: Some(target_text),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: vec![],
            exclude: vec![],
        },
        DispositionKind::Protect => RuleDoc {
            id: format!("user-protected/{seq}"),
            description: format!("user protected: {} ({})", target.display(), target_text),
            product: product.map(str::to_string),
            category,
            risk: RiskLevel::Protected,
            source: RuleSource::UserProtected,
            match_spec: MatchSpec {
                exact: Some(target_text),
                glob: None,
                parent_marker: None,
                exists: false,
            },
            include: vec![],
            exclude: vec![],
        },
    };
    let id = new_rule.id.clone();
    file.rules.push(new_rule);

    // F-M4-1: refuse the write when the rule would not survive the loader's
    // own validation (dangerous roots, non-absolute anchors, ...). A bad
    // disposition errors now instead of silently failing closed on the next
    // scan.
    validate_before_write(&file)?;

    let text = serde_yaml_ng::to_string(&file)
        .map_err(|e| format!("serialise {}: {e}", path.display()))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(id)
}

/// Inserts (or replaces) a *detection* rule for `target` with the given
/// classification — used by the analyzer's "create rule" action (SPEC §26:
/// the AI / heuristic may only *suggest*; turning a suggestion into a rule
/// goes through this ordinary user-rule write, and the next scan's F-2-1 rule
/// gate applies it like any user rule).
///
/// The rule is `source: user` with `risk` / `category` from the suggestion.
/// Returns the new rule id.
pub fn upsert_detection_rule(
    user_dir: &Path,
    target: &Path,
    product: Option<&str>,
    category: ResidueCategory,
    risk: RiskLevel,
) -> Result<String, String> {
    let path = dispositions_path(user_dir);
    let mut file = read_or_empty(&path)?;
    let target_text = target.to_string_lossy().into_owned();

    // Drop earlier *detection* dispositions on the same path (source User and
    // not an ignore declaration). Protect dispositions stay — a user Protect
    // outranks any detection rule by source priority anyway.
    file.rules.retain(|rule| {
        let is_detection =
            rule.source == RuleSource::User && !rule.id.starts_with(USER_IGNORE_ID_PREFIX);
        let anchors_here = rule.match_spec.exact.as_deref() == Some(target_text.as_str());
        !(is_detection && anchors_here)
    });

    let seq = file.rules.len() + 1;
    let rule = RuleDoc {
        id: format!("user-analysis/{seq}"),
        description: format!("user classification (from analyzer): {}", target.display()),
        product: product.map(str::to_string),
        category,
        risk,
        source: RuleSource::User,
        match_spec: MatchSpec {
            exact: Some(target_text),
            glob: None,
            parent_marker: None,
            exists: false,
        },
        include: vec![],
        exclude: vec![],
    };
    let id = rule.id.clone();
    file.rules.push(rule);

    // F-M4-1: same write-time validation as dispositions.
    validate_before_write(&file)?;

    let text = serde_yaml_ng::to_string(&file)
        .map_err(|e| format!("serialise {}: {e}", path.display()))?;
    std::fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(id)
}

/// Runs the loader's semantic validation on a file about to be written.
///
/// A disposition file may mix `User` and `UserProtected` rules, so each rule
/// is validated against its own expected source (the loader groups the same
/// way). Returns the first blocking issue; the write is refused (F-M4-1).
fn validate_before_write(file: &RuleFile) -> Result<(), String> {
    let lookup = |name: &str| std::env::var(name).ok();
    for rule in &file.rules {
        let expected = match rule.source {
            RuleSource::UserProtected => RuleSource::UserProtected,
            _ => RuleSource::User,
        };
        let single = RuleFile {
            rules: vec![rule.clone()],
        };
        let issues = validate_rule_file(&single, expected, &lookup);
        if let Some(issue) = issues.iter().find(|i| i.is_error()) {
            return Err(format!(
                "user rule rejected before write ({}): {}",
                rule.id, issue.message
            ));
        }
    }
    Ok(())
}

/// Reads `user-dispositions.yaml`, treating a missing file as an empty rule
/// file (the directory may not exist yet on a fresh install). A corrupt file
/// is a hard error (never silently overwrite user rules).
fn read_or_empty(path: &Path) -> Result<RuleFile, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(RuleFile { rules: vec![] }),
        Ok(text) => serde_yaml_ng::from_str(&text)
            .map_err(|e| format!("parse {} (refusing to overwrite): {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RuleFile { rules: vec![] }),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::loader::{load_rules, load_user_ignore_paths};
    use std::fs;

    fn fake_env() -> impl Fn(&str) -> Option<String> {
        |name: &str| {
            let base = r"C:\Users\demo";
            match name.to_ascii_uppercase().as_str() {
                "USERPROFILE" => Some(base.to_string()),
                "LOCALAPPDATA" => Some(format!(r"{base}\AppData\Local")),
                "APPDATA" => Some(format!(r"{base}\AppData\Roaming")),
                _ => None,
            }
        }
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dr-user-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ignore_then_protect_replaces_the_same_path_entry() {
        let dir = tmp("rt");
        let user = ensure_user_rules_dir(&dir).unwrap();
        let target = Path::new(r"C:\Users\demo\.odd-tool-data");

        let id_ignore = upsert_disposition(
            &user,
            target,
            Some("some-tool"),
            ResidueCategory::Unknown,
            DispositionKind::Ignore,
        )
        .unwrap();
        assert!(id_ignore.starts_with(USER_IGNORE_ID_PREFIX));

        // Protect the same path → the ignore declaration is replaced by one
        // user-protected rule; the file holds exactly one disposition rule.
        let id_protect = upsert_disposition(
            &user,
            target,
            Some("some-tool"),
            ResidueCategory::DeveloperCache,
            DispositionKind::Protect,
        )
        .unwrap();
        assert!(id_protect.starts_with("user-protected/"));
        let text = fs::read_to_string(dispositions_path(&user)).unwrap();
        assert_eq!(text.matches("match:").count(), 1, "{text}");

        // The user rules *container* (data_dir/rules, holding the user/
        // source directory) loads cleanly — single-source grouping — and the
        // protect rule resolves.
        let container = dir.join("rules");
        let env = fake_env();
        let set = load_rules(&container, &env);
        assert!(set.is_clean(), "issues: {:?}", set.issues);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].source, RuleSource::UserProtected);
        let hit = crate::rules::priority::resolve(target, &set.rules).unwrap();
        assert_eq!(hit.risk, RiskLevel::Protected);

        // Ignore again → protect rule gone, ignore path parseable.
        upsert_disposition(
            &user,
            target,
            None,
            ResidueCategory::Unknown,
            DispositionKind::Ignore,
        )
        .unwrap();
        let ignores = load_user_ignore_paths(&user, &env);
        assert_eq!(
            ignores,
            vec![PathBuf::from(r"C:\Users\demo\.odd-tool-data")]
        );
        let set = load_rules(&container, &env);
        assert!(set.is_clean(), "issues: {:?}", set.issues);
        assert!(set.rules.is_empty(), "ignores never compile");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mixed_user_and_user_protected_file_loads_without_source_errors() {
        let dir = tmp("mixed");
        let user = ensure_user_rules_dir(&dir).unwrap();
        upsert_disposition(
            &user,
            Path::new(r"C:\Users\demo\.a"),
            None,
            ResidueCategory::Unknown,
            DispositionKind::Ignore,
        )
        .unwrap();
        upsert_disposition(
            &user,
            Path::new(r"C:\Users\demo\.b"),
            None,
            ResidueCategory::Unknown,
            DispositionKind::Protect,
        )
        .unwrap();

        let container = dir.join("rules");
        let set = load_rules(&container, &fake_env());
        assert!(set.is_clean(), "issues: {:?}", set.issues);
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].id, "user-protected/2");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_disposition_file_is_not_overwritten() {
        let dir = tmp("corrupt");
        let user = ensure_user_rules_dir(&dir).unwrap();
        fs::write(dispositions_path(&user), b"{broken yaml: [").unwrap();
        let err = upsert_disposition(
            &user,
            Path::new(r"C:\Users\demo\.x"),
            None,
            ResidueCategory::Unknown,
            DispositionKind::Ignore,
        );
        assert!(err.is_err(), "corrupt file must fail closed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dangerous_root_disposition_is_refused_at_write_time() {
        // F-M4-1: a disposition targeting a drive root errors immediately —
        // not merely on the next scan's fail-closed load.
        let dir = tmp("root-reject");
        let user = ensure_user_rules_dir(&dir).unwrap();
        let err = upsert_disposition(
            &user,
            Path::new(r"C:\"),
            None,
            ResidueCategory::Unknown,
            DispositionKind::Ignore,
        );
        assert!(err.is_err(), "C:\\ must be rejected");
        assert!(err.unwrap_err().contains("drive root"));

        // Nothing was written (the file does not exist).
        assert!(!dispositions_path(&user).exists());

        // The same guard applies to the analyzer rule path.
        let err = upsert_detection_rule(
            &user,
            Path::new(r"C:\"),
            None,
            ResidueCategory::Unknown,
            RiskLevel::Safe,
        );
        assert!(err.is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
