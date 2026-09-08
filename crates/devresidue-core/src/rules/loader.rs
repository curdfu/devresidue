//! Rule loader: reads `resources/rules/**/*.{yaml,yml}`, validates every file
//! and compiles the survivors into ready-to-resolve [`CompiledRule`]s.
//!
//! # Directory -> source mapping
//!
//! ```text
//! resources/rules/protected/   -> RuleSource::BuiltinProtected
//! resources/rules/agents/      -> RuleSource::BuiltinDetection
//! resources/rules/ide/         -> RuleSource::BuiltinDetection
//! resources/rules/dev_cache/   -> RuleSource::BuiltinDetection
//! resources/rules/packages/    -> RuleSource::BuiltinDetection
//! <data>/rules/user/           -> RuleSource::User | UserProtected  (Phase 13)
//! ```
//!
//! The `user/` source directory is wired in Phase 13: it may mix `User`
//! (detection + `user-ignore/` declarations) and `UserProtected` rules, split
//! per rule by the collector so the single-source validator contract holds.
//! Community / AI-suggestion directories are still **not loaded**; a YAML file
//! in an unexpected directory is a hard error (fail-closed: rules must never
//! be silently ignored).
//!
//! # Load result
//!
//! Loading never panics and never stops on the first bad file: every finding
//! is collected into [`RuleSet::issues`] and only rules without a blocking
//! `Error` issue are compiled and exposed in [`RuleSet::rules`].

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use globset::GlobMatcher;

use crate::rules::matcher::{compile_glob, expand_env, first_meta_offset, normalize_slashes};
use crate::rules::priority::RuleSource;
use crate::rules::schema::{RuleDoc, RuleFile};
use crate::rules::validator::{validate_rule_file, RuleIssue, Severity};
use crate::{ResidueCategory, RiskLevel, RuleId};

/// Mapping of a supported rules sub-directory to the source of the rules it
/// holds. Directories not in this table are not loaded (Phase 3 scope).
///
/// `user/` holds **both** `User` (detection / ignore declarations) and
/// `UserProtected` rules (written by the disposition commands, see
/// `rules::user`); the collector splits the file by per-rule source so the
/// single-source validator contract is preserved.
pub fn expected_source_for_dir(dir_name: &str) -> Option<RuleSource> {
    match dir_name {
        "protected" => Some(RuleSource::BuiltinProtected),
        "agents" | "ide" | "dev_cache" | "packages" => Some(RuleSource::BuiltinDetection),
        "user" => Some(RuleSource::User),
        _ => None,
    }
}

/// Rule ids with this prefix in the `user/` directory declare a *user ignore*
/// disposition: the anchor path is excluded from every future scan (pre-seeded
/// into the scan context's seen-set). They are never compiled into the
/// [`RuleSet`] — an ignore is "do not report", not a classification.
pub const USER_IGNORE_ID_PREFIX: &str = "user-ignore/";

/// A fully validated and compiled rule ready for resolution.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    /// Rule slug (globally unique, e.g. `builtin-protected/ssh`).
    pub id: String,
    /// Registry-assigned numeric rule id.
    pub numeric_id: RuleId,
    /// Source (drives priority).
    pub source: RuleSource,
    /// Risk assigned to hits.
    pub risk: RiskLevel,
    /// Category assigned to hits.
    pub category: ResidueCategory,
    /// Owning product, when declared.
    pub product: Option<String>,
    /// Description (explainability).
    pub description: String,
    /// Compiled anchor pattern.
    pub pattern: CompiledPattern,
    /// `parent_marker` from the `match` block.
    pub parent_marker: Option<String>,
    /// `exists: true` from the `match` block.
    pub requires_existing: bool,
    /// Raw `include` patterns (kept for listings/DTOs).
    pub include_raw: Vec<String>,
    /// Raw `exclude` patterns.
    pub exclude_raw: Vec<String>,
    /// Compiled relative include/exclude matchers.
    pub(crate) include_matchers: Vec<GlobMatcher>,
    pub(crate) exclude_matchers: Vec<GlobMatcher>,
    /// Declaration order across the whole load (tie-break).
    pub(crate) order: usize,
}

impl CompiledRule {
    /// Human-oriented one-line summary used by the CLI listing.
    #[must_use]
    pub fn describe_anchor(&self) -> String {
        match &self.pattern {
            CompiledPattern::Exact { target } => format!("exact {target}"),
            CompiledPattern::Glob { pattern_norm, .. } => format!("glob {pattern_norm}"),
        }
    }
}

/// Compiled anchor of a rule.
#[derive(Debug, Clone)]
pub enum CompiledPattern {
    /// `match.exact` — the normalized literal target.
    Exact {
        /// Slash-normalized absolute target.
        target: String,
    },
    /// `match.glob` — globset matcher plus its literal root (used for
    /// include/exclude refinement).
    Glob {
        /// Case-insensitive slash-separated glob matcher.
        matcher: GlobMatcher,
        /// Slash-normalized full pattern (used for tie-break/specificity).
        pattern_norm: String,
        /// Literal prefix of the pattern (up to the first glob metacharacter),
        /// slash-normalized with no trailing slash — the refinement root.
        root: String,
    },
}

/// Outcome of loading a rules directory.
#[derive(Debug, Default)]
pub struct RuleSet {
    /// Rules that passed every validation step, in deterministic load order.
    pub rules: Vec<CompiledRule>,
    /// All findings (parse errors, validation errors, warnings).
    pub issues: Vec<RuleIssue>,
}

impl RuleSet {
    /// Number of blocking issues.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.issues.iter().filter(|i| i.is_error()).count()
    }

    /// True when loading produced no blocking issue.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.error_count() == 0
    }

    /// Appends another rule set (built-in resources first, user rules second
    /// — the F-2-1 scan assembly) and re-numbers every rule deterministically
    /// so numeric ids stay globally unique across the merged set.
    ///
    /// Issues are concatenated; a caller that wants fail-closed behaviour
    /// checks [`RuleSet::error_count`] after merging.
    pub fn merge(&mut self, other: RuleSet) {
        self.rules.extend(other.rules);
        self.issues.extend(other.issues);
        for (index, rule) in self.rules.iter_mut().enumerate() {
            rule.order = index;
            rule.numeric_id = RuleId::from_raw(index as u64 + 1);
        }
    }

    /// **Protected areas** (R03): the conservative on-disk regions that may
    /// never be deleted. For every rule declaring `risk: Protected`, this is:
    ///
    /// - an `exact` anchor → the anchor path itself;
    /// - a `glob` anchor → its literal prefix root (the directory the glob
    ///   covers); this is a conservative **superset** — the rule's
    ///   include/exclude refinement may narrow the real match, and treating
    ///   the whole root as protected only over-protects.
    ///
    /// A caller deleting target `T` must refuse when *any* protected area `A`
    /// satisfies `is_within(A, T)` — i.e. a protected path sits inside the
    /// tree about to be deleted (deleting `T` would take `A` with it) — or
    /// when `T` itself is inside `A` (already handled by the resolve gate).
    ///
    /// The registry's fixed protected roots are *not* included (they are
    /// checked separately); this covers user/built-in protected *rules*.
    #[must_use]
    pub fn protected_areas(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for rule in &self.rules {
            if rule.risk != crate::RiskLevel::Protected {
                continue;
            }
            match &rule.pattern {
                CompiledPattern::Exact { target } => out.push(PathBuf::from(target)),
                CompiledPattern::Glob { root, .. } if !root.is_empty() => {
                    out.push(PathBuf::from(root))
                }
                CompiledPattern::Glob { .. } => {}
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Loads and validates every YAML rule file under `rules_dir`.
///
/// `env` is the environment lookup used for `%VAR%`/`${VAR}` expansion
/// (case-insensitive). Loading is deterministic: directories and files are
/// visited in sorted order and rules keep their in-file declaration order.
pub fn load_rules(rules_dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> RuleSet {
    let mut issues = Vec::new();
    let mut entries: Vec<(PathBuf, RuleSource, RuleFile)> = Vec::new();
    collect_entries(rules_dir, &mut entries, &mut issues);

    // Deterministic order across the whole load.
    entries.sort_by(|a, b| (a.0.to_string_lossy()).cmp(&b.0.to_string_lossy()));

    // Semantic validation per file.
    for (path, expected, file) in &entries {
        let mut file_issues = validate_rule_file(file, *expected, env);
        for issue in &mut file_issues {
            issue.file = Some(path.clone());
        }
        issues.extend(file_issues);
    }

    // Globally unique ids (first declaration wins, later duplicates error).
    let mut first_index: HashMap<&str, usize> = HashMap::new();
    let mut doc_locations: Vec<(&str, &PathBuf)> = Vec::new();
    for (path, _, file) in &entries {
        for rule in &file.rules {
            doc_locations.push((rule.id.as_str(), path));
        }
    }
    for (index, (id, path)) in doc_locations.iter().enumerate() {
        if let Some(first) = first_index.get(*id) {
            issues.push(RuleIssue {
                file: Some((*path).clone()),
                rule_id: Some((*id).to_string()),
                field: Some("id".to_string()),
                severity: Severity::Error,
                message: format!(
                    "duplicate rule id `{id}` (first declared at index {first}); ids must be \
                     globally unique"
                ),
            });
        } else {
            first_index.insert(*id, index);
        }
    }

    // Compile surviving rules. `user-ignore/` declarations are not rules —
    // they declare an excluded path (consumed by `load_user_ignore_paths`) —
    // so they never enter the resolve set.
    let mut rules = Vec::new();
    for (path, _, file) in &entries {
        for doc in &file.rules {
            if doc.id.starts_with(USER_IGNORE_ID_PREFIX) {
                continue;
            }
            let has_error = issues.iter().any(|i| {
                i.is_error()
                    && i.file.as_deref() == Some(path.as_path())
                    && i.rule_id.as_deref() == Some(doc.id.as_str())
            });
            if has_error {
                continue;
            }
            match compile_rule(doc, env) {
                Ok(rule) => rules.push(rule),
                Err(message) => issues.push(RuleIssue {
                    file: Some(path.clone()),
                    rule_id: Some(doc.id.clone()),
                    field: None,
                    severity: Severity::Error,
                    message,
                }),
            }
        }
    }

    // Stable numeric ids follow the deterministic rule order.
    for (index, rule) in rules.iter_mut().enumerate() {
        rule.order = index;
        rule.numeric_id = RuleId::from_raw(index as u64 + 1);
    }

    RuleSet { rules, issues }
}

/// Parses every `user/` rule file and returns the **expanded absolute anchor
/// paths** of `user-ignore/<n>` declarations (see [`USER_IGNORE_ID_PREFIX`]).
///
/// The scan layer pre-seeds these paths into its context's seen-set so the
/// declared path never appears again in any scan. Only `match.exact` anchors
/// are honoured (the disposition writer emits exact rules); a declaration that
/// fails env expansion is skipped.
pub fn load_user_ignore_paths(
    user_dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(user_dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> = read_dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|x| x.to_str()).is_some_and(|ext| {
                ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
            })
        })
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(parsed) = serde_yaml_ng::from_str::<RuleFile>(&text) else {
            continue;
        };
        for rule in parsed.rules {
            if !rule.id.starts_with(USER_IGNORE_ID_PREFIX) {
                continue;
            }
            let Some(anchor) = rule.match_spec.exact else {
                continue;
            };
            if let Ok(expanded) = expand_env(&anchor, env) {
                out.push(PathBuf::from(expanded));
            }
        }
    }
    out
}

/// Walks `rules_dir` (one level) and collects parseable YAML entries.
fn collect_entries(
    rules_dir: &Path,
    entries: &mut Vec<(PathBuf, RuleSource, RuleFile)>,
    issues: &mut Vec<RuleIssue>,
) {
    let Ok(read_dir) = std::fs::read_dir(rules_dir) else {
        issues.push(RuleIssue {
            file: Some(rules_dir.to_path_buf()),
            rule_id: None,
            field: None,
            severity: Severity::Error,
            message: format!("cannot read rules directory {}", rules_dir.display()),
        });
        return;
    };

    let mut dirs: Vec<PathBuf> = read_dir
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let Some(dir_name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(expected) = expected_source_for_dir(dir_name) else {
            // Fail closed: rule files in unexpected locations must never be
            // silently ignored. README-only directories are fine.
            let has_yaml = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten().any(|e| {
                        e.path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|ext| {
                                ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                            })
                    })
                })
                .unwrap_or(false);
            if has_yaml {
                issues.push(RuleIssue {
                    file: Some(dir.clone()),
                    rule_id: None,
                    field: None,
                    severity: Severity::Error,
                    message: format!(
                        "unsupported rules directory `{}`: rules are only loaded from \
                         protected/ agents/ ide/ dev_cache/ packages/ in this phase",
                        dir.display()
                    ),
                });
            }
            continue;
        };

        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().and_then(|x| x.to_str()).is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();

        for file in files {
            match std::fs::read_to_string(&file) {
                Ok(text) => match serde_yaml_ng::from_str::<RuleFile>(&text) {
                    Ok(parsed) => {
                        if dir_name == "user" {
                            // The user directory may mix `User` detection /
                            // ignore declarations with `UserProtected` rules
                            // written by disposition commands. Group by source
                            // so the per-file single-source validator contract
                            // holds. Any other source in a user file is a hard
                            // error (fail-closed: never silently ignore rules).
                            let mut groups: BTreeMap<RuleSource, Vec<RuleDoc>> = BTreeMap::new();
                            let mut rejected = false;
                            for rule in parsed.rules {
                                match rule.source {
                                    RuleSource::User | RuleSource::UserProtected => {
                                        groups.entry(rule.source).or_default().push(rule)
                                    }
                                    other => {
                                        rejected = true;
                                        issues.push(RuleIssue {
                                            file: Some(file.clone()),
                                            rule_id: Some(rule.id.clone()),
                                            field: Some("source".to_string()),
                                            severity: Severity::Error,
                                            message: format!(
                                                "user rules directory only accepts source \
                                                 `user` / `user-protected`, got `{other}`"
                                            ),
                                        });
                                    }
                                }
                            }
                            if rejected {
                                continue;
                            }
                            for (source, docs) in groups {
                                entries.push((file.clone(), source, RuleFile { rules: docs }));
                            }
                        } else {
                            entries.push((file, expected, parsed));
                        }
                    }
                    Err(err) => issues.push(RuleIssue {
                        file: Some(file),
                        rule_id: None,
                        field: None,
                        severity: Severity::Error,
                        message: format!("rule file failed to parse: {err}"),
                    }),
                },
                Err(err) => issues.push(RuleIssue {
                    file: Some(file),
                    rule_id: None,
                    field: None,
                    severity: Severity::Error,
                    message: format!("cannot read rule file: {err}"),
                }),
            }
        }
    }
}

/// Compiles one validated rule document (env expansion already known good by
/// the validator; still fail-closed here on any inconsistency).
fn compile_rule(
    doc: &RuleDoc,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<CompiledRule, String> {
    let anchor = doc
        .match_spec
        .exact
        .as_deref()
        .or(doc.match_spec.glob.as_deref())
        .ok_or_else(|| "rule has no match.exact or match.glob anchor".to_string())?;
    let expanded = expand_env(anchor, env).map_err(|e| e.to_string())?;
    let pattern_norm = normalize_slashes(&expanded);

    let pattern = if doc.match_spec.exact.is_some() {
        CompiledPattern::Exact {
            target: pattern_norm,
        }
    } else {
        let matcher = compile_glob(&pattern_norm)
            .map_err(|e| format!("invalid match.glob `{anchor}`: {e}"))?;
        let root = literal_root(&pattern_norm).to_string();
        CompiledPattern::Glob {
            matcher,
            pattern_norm,
            root,
        }
    };

    let mut include_matchers = Vec::with_capacity(doc.include.len());
    for pat in &doc.include {
        include_matchers
            .push(compile_glob(pat).map_err(|e| format!("invalid include glob `{pat}`: {e}"))?);
    }
    let mut exclude_matchers = Vec::with_capacity(doc.exclude.len());
    for pat in &doc.exclude {
        exclude_matchers
            .push(compile_glob(pat).map_err(|e| format!("invalid exclude glob `{pat}`: {e}"))?);
    }

    Ok(CompiledRule {
        id: doc.id.clone(),
        numeric_id: RuleId::from_raw(0), // assigned later by the loader
        source: doc.source,
        risk: doc.risk,
        category: doc.category,
        product: doc.product.clone(),
        description: doc.description.clone(),
        pattern,
        parent_marker: doc.match_spec.parent_marker.clone(),
        requires_existing: doc.match_spec.exists,
        include_raw: doc.include.clone(),
        exclude_raw: doc.exclude.clone(),
        include_matchers,
        exclude_matchers,
        order: 0,
    })
}

/// Literal path prefix of a slash-normalized glob pattern (up to the first
/// metacharacter), with the trailing slash removed.
fn literal_root(pattern_norm: &str) -> &str {
    match first_meta_offset(pattern_norm) {
        None => pattern_norm.trim_end_matches('/'),
        Some(mi) => pattern_norm[..mi].trim_end_matches('/'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::matcher::rule_matches;
    use std::collections::BTreeMap;
    use std::fs;

    /// Test-only alias for the environment lookup box.
    type EnvFn = Box<dyn Fn(&str) -> Option<String>>;

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_uppercase(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(&name.to_ascii_uppercase()).cloned()
    }

    struct TempRules {
        dir: PathBuf,
        env: EnvFn,
    }

    impl Drop for TempRules {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn write_yaml(dir: &Path, sub: &str, name: &str, content: &str) {
        let subdir = dir.join(sub);
        fs::create_dir_all(&subdir).unwrap();
        fs::write(subdir.join(name), content).unwrap();
    }

    fn temp_rules(files: &[(&str, &str, &str)]) -> TempRules {
        let dir = std::env::temp_dir().join(format!(
            "devresidue-rules-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (sub, name, content) in files {
            write_yaml(&dir, sub, name, content);
        }
        let env: EnvFn = Box::new(fake_env(&[
            ("USERPROFILE", r"C:\Users\demo"),
            ("LOCALAPPDATA", r"C:\Users\demo\AppData\Local"),
        ]));
        TempRules { dir, env }
    }

    const PROTECTED_YAML: &str = r#"
rules:
  - id: builtin-protected/ssh
    description: SSH keys & config
    product: OpenSSH
    category: credential
    risk: protected
    source: builtin-protected
    match:
      exact: "%USERPROFILE%/.ssh"
"#;

    const AGENT_YAML: &str = r#"
rules:
  - id: builtin-detection/claude-cache-shell-snapshots
    description: Claude Code shell snapshots
    product: Claude Code
    category: ai-agent
    risk: safe
    source: builtin-detection
    match:
      exact: "%USERPROFILE%/.claude/shell-snapshots"
  - id: builtin-detection/claude-session-history
    description: Claude session transcripts
    product: Claude Code
    category: session
    risk: review
    source: builtin-detection
    match:
      glob: "%USERPROFILE%/.claude/projects/*"
    exclude:
      - "**/.trash/**"
"#;

    #[test]
    fn loads_clean_directory_without_issues() {
        let t = temp_rules(&[
            ("protected", "builtin-protected.yaml", PROTECTED_YAML),
            ("agents", "claude.yaml", AGENT_YAML),
        ]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(set.is_clean(), "issues: {:?}", set.issues);
        assert_eq!(set.rules.len(), 3);
        // Deterministic load order: files are visited in sorted directory
        // order (agents/ before protected/), so agents rules get ids 1-2 and
        // the protected rule gets id 3.
        assert_eq!(set.rules[0].source, RuleSource::BuiltinDetection);
        assert_eq!(set.rules[0].numeric_id.raw(), 1);
        assert_eq!(set.rules[1].source, RuleSource::BuiltinDetection);
        assert_eq!(set.rules[2].source, RuleSource::BuiltinProtected);
        assert_eq!(set.rules[2].numeric_id.raw(), 3);
    }

    #[test]
    fn rule_matching_honours_exact_and_glob_and_priority() {
        let t = temp_rules(&[
            ("protected", "p.yaml", PROTECTED_YAML),
            ("agents", "claude.yaml", AGENT_YAML),
        ]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(set.is_clean());

        // Exact protected rule resolves for the .ssh path.
        let hit = crate::rules::priority::resolve(Path::new(r"C:\Users\demo\.ssh"), &set.rules)
            .expect("ssh must resolve");
        assert_eq!(hit.risk, RiskLevel::Protected);
        assert_eq!(hit.category, crate::ResidueCategory::Credential);

        // Unprotected exact beats nothing here; session glob resolves.
        let hit = crate::rules::priority::resolve(
            Path::new(r"C:\Users\demo\.claude\projects\app"),
            &set.rules,
        )
        .expect("session must resolve");
        assert_eq!(hit.risk, RiskLevel::Review);

        // Exclusion: a .trash child of a project does NOT match the session rule.
        let excluded = Path::new(r"C:\Users\demo\.claude\projects\app\.trash");
        assert!(!crate::rules::priority::resolve(excluded, &set.rules)
            .is_some_and(|r| r.id == "builtin-detection/claude-session-history"));
    }

    #[test]
    fn duplicate_ids_are_errors_and_second_rule_is_skipped() {
        // Duplicate the whole file: every duplicated id in b must error and
        // only the first declarations (file a) survive.
        let t = temp_rules(&[
            ("agents", "a.yaml", AGENT_YAML),
            ("agents", "b.yaml", AGENT_YAML),
        ]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(set
            .issues
            .iter()
            .any(|i| i.message.contains("duplicate rule id")));
        assert_eq!(set.rules.len(), 2);
    }

    #[test]
    fn broken_file_reports_issue_and_does_not_load() {
        let t = temp_rules(&[("agents", "bad.yaml", "rules:\n  - id: x\n")]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(!set.is_clean());
        assert!(set.rules.is_empty());
        assert!(set
            .issues
            .iter()
            .any(|i| i.message.contains("failed to parse")));
    }

    #[test]
    fn unknown_directory_with_yaml_is_rejected() {
        // A source directory that is not wired to the loader stays rejected.
        // (`user/` IS wired since Phase 13 — see rules::user tests.)
        let t = temp_rules(&[("community", "c.yaml", AGENT_YAML)]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(set
            .issues
            .iter()
            .any(|i| i.message.contains("unsupported rules directory")));
    }

    #[test]
    fn include_and_exclude_refine_glob_matches() {
        let yaml = r#"
rules:
  - id: builtin-detection/tool-root
    description: only selected subdirs
    category: temporary
    risk: safe
    source: builtin-detection
    match:
      glob: "%LOCALAPPDATA%/**"
    include:
      - "cache-a"
      - "cache-a/**"
      - "cache-b/**"
    exclude:
      - "**/internal/**"
"#;
        let t = temp_rules(&[("dev_cache", "tool.yaml", yaml)]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(set.is_clean(), "issues {:?}", set.issues);
        let rule = &set.rules[0];

        // Included sub-paths match.
        assert!(rule_matches(
            rule,
            Path::new(r"C:\Users\demo\AppData\Local\cache-a")
        ));
        assert!(rule_matches(
            rule,
            Path::new(r"C:\Users\demo\AppData\Local\cache-b\v1")
        ));
        // Excluded sub-path inside an included one does not match.
        assert!(!rule_matches(
            rule,
            Path::new(r"C:\Users\demo\AppData\Local\cache-a\internal\deep")
        ));
        // Neither included nor excluded sibling does not match (include narrows).
        assert!(!rule_matches(
            rule,
            Path::new(r"C:\Users\demo\AppData\Local\other")
        ));
        // The root itself is not a sub-path => no match under include.
        assert!(!rule_matches(
            rule,
            Path::new(r"C:\Users\demo\AppData\Local")
        ));
    }

    #[test]
    fn parent_marker_and_exists_use_the_filesystem() {
        let root = std::env::temp_dir().join(format!(
            "devresidue-marker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        // project layout: proj-a/package.json + node_modules; proj-b/node_modules only
        fs::create_dir_all(root.join("proj-a/node_modules")).unwrap();
        fs::create_dir_all(root.join("proj-b/node_modules")).unwrap();
        fs::write(root.join("proj-a/package.json"), "{}").unwrap();

        let pattern = format!(
            "{}/*/node_modules",
            root.to_string_lossy().replace('\\', "/")
        );
        let yaml = format!(
            r#"
rules:
  - id: builtin-detection/node-deps
    description: node deps behind a package.json parent
    category: dependency
    risk: regenerable-download
    source: builtin-detection
    match:
      glob: "{pattern}"
      parent_marker: package.json
      exists: true
"#
        );
        let t = temp_rules(&[("packages", "node.yaml", &yaml)]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(set.is_clean(), "issues {:?}", set.issues);
        let rule = &set.rules[0];

        // Candidate whose parent holds package.json -> match.
        let hit = root.join("proj-a/node_modules");
        assert!(rule_matches(rule, &hit));
        // Same pattern without the marker/exists file -> no match.
        let miss = root.join("proj-b/node_modules");
        assert!(!rule_matches(rule, &miss));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn include_narrowing_keeps_agent_root_rule_legal_but_open_sweep_is_rejected() {
        let yaml_narrow = r#"
rules:
  - id: builtin-detection/claude-cache-narrow
    description: narrowed cache only
    category: temporary
    risk: safe
    source: builtin-detection
    match:
      glob: "%USERPROFILE%/.claude/*"
    include:
      - "shell-snapshots/**"
"#;
        let t = temp_rules(&[("agents", "narrow.yaml", yaml_narrow)]);
        let set = load_rules(&t.dir, &*t.env);
        assert!(
            set.is_clean(),
            "expected narrow include to pass, got {:?}",
            set.issues
        );
        assert_eq!(set.rules.len(), 1);
    }
}
