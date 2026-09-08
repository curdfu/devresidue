//! Path semantics helpers for rule matching: env expansion, path
//! normalisation and the per-rule matcher.
//!
//! Matching is **case-insensitive** and treats `\` and `/` as equal
//! (Windows semantics, but kept platform-neutral in core). Every candidate and
//! every compiled pattern is normalised to `/` before comparison / globbing.

use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};

use super::loader::CompiledRule;

/// Error produced while expanding `%VAR%` / `${VAR}` in a rule pattern.
///
/// Expansion is fail-closed: an unknown variable makes the whole load fail
/// rather than silently producing a bogus path (a rule that could not be
/// anchored must never be half-loaded).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    /// `%` or `${` opened but never closed.
    #[error("unterminated environment reference at byte {offset} in `{pattern}`")]
    Unterminated {
        /// The offending pattern.
        pattern: String,
        /// Byte offset of the opening marker.
        offset: usize,
    },
    /// Empty variable name (`%%` / `${}`).
    #[error("empty environment variable name in `{pattern}`")]
    EmptyName {
        /// The offending pattern.
        pattern: String,
    },
    /// Referenced environment variable is not defined (fail-closed).
    #[error("unknown environment variable `{name}` referenced by rule path (fail-closed: not loading rule)")]
    UnknownVariable {
        /// Variable name, uppercased.
        name: String,
    },
}

/// Normalises a Windows/Unix path string to `/`-separated form for comparison
/// and globbing.
#[must_use]
pub fn normalize_slashes(text: &str) -> String {
    text.replace('\\', "/")
}

/// Case-insensitive path equality on slash-normalised strings.
#[must_use]
pub fn path_eq(a: &str, b: &str) -> bool {
    normalize_slashes(a).eq_ignore_ascii_case(&normalize_slashes(b))
}

/// Returns the relative remainder of a slash-normalised `candidate` under a
/// slash-normalised `root`, or `None` when the candidate is not inside `root`.
/// Equality yields `Some("")`. Comparison is case-insensitive.
///
/// Both arguments are expected to be already slash-normalised (see
/// [`normalize_slashes`]); this keeps the function allocation-free and lets it
/// return a slice of `candidate`.
#[must_use]
pub fn rel_under_root<'a>(candidate: &'a str, root: &str) -> Option<&'a str> {
    if candidate.eq_ignore_ascii_case(root) {
        return Some("");
    }
    let rl = root.len();
    if candidate.len() > rl
        && candidate.is_char_boundary(rl)
        && candidate[..rl].eq_ignore_ascii_case(root)
        && candidate.as_bytes()[rl] == b'/'
    {
        return Some(&candidate[rl + 1..]);
    }
    None
}

/// Expands `%VAR%` and `${VAR}` references in `text`.
///
/// Variable lookup is case-insensitive (Windows convention): the name is
/// uppercased before being passed to `lookup`. Any unknown variable is an
/// error (see [`ExpandError`]).
pub fn expand_env(
    text: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, ExpandError> {
    let mut out = String::with_capacity(text.len() + 16);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = text[i..]
            .chars()
            .next()
            .expect("byte index on char boundary");
        match ch {
            '%' => {
                let rest = &text[i + 1..];
                match rest.find('%') {
                    None => {
                        return Err(ExpandError::Unterminated {
                            pattern: text.into(),
                            offset: i,
                        })
                    }
                    Some(j) => {
                        let name = &rest[..j];
                        if name.is_empty() {
                            return Err(ExpandError::EmptyName {
                                pattern: text.into(),
                            });
                        }
                        push_var(&mut out, name, lookup)?;
                        i += 1 + j + 1;
                    }
                }
            }
            '$' => {
                if text[i + 1..].starts_with('{') {
                    let rest = &text[i + 2..];
                    match rest.find('}') {
                        None => {
                            return Err(ExpandError::Unterminated {
                                pattern: text.into(),
                                offset: i,
                            })
                        }
                        Some(j) => {
                            let name = &rest[..j];
                            if name.is_empty() {
                                return Err(ExpandError::EmptyName {
                                    pattern: text.into(),
                                });
                            }
                            push_var(&mut out, name, lookup)?;
                            i += 2 + j + 1;
                        }
                    }
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Ok(out)
}

fn push_var(
    out: &mut String,
    name: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), ExpandError> {
    let key = name.to_ascii_uppercase();
    match lookup(&key) {
        Some(value) => {
            out.push_str(&value);
            Ok(())
        }
        None => Err(ExpandError::UnknownVariable { name: key }),
    }
}

/// Evaluates a rule against one candidate path (a potential `ScanItem.path`).
///
/// All conditions are ANDed:
///
/// 1. anchor: exact equality or glob match on the slash-normalised candidate;
/// 2. `include`/`exclude` refinement on the candidate's relative path under
///    the anchor root (empty relative path == the root itself, which only
///    matches when `include` is empty);
/// 3. filesystem conditions: `exists: true` and `parent_marker`.
#[must_use]
pub fn rule_matches(rule: &CompiledRule, candidate: &Path) -> bool {
    let cand_norm = normalize_slashes(&candidate.to_string_lossy());

    // 1. Anchor.
    let anchor_rel: Option<&str> = match &rule.pattern {
        super::loader::CompiledPattern::Exact { target } => {
            // Case-insensitive via the canonical comparison key (simple
            // Unicode case fold — covers non-ASCII profile names such as
            // `José`, unlike a plain ASCII-only fold). Extended (`\\?\`)
            // spellings of the same path compare equal too.
            if crate::safety::canonical::normalized_eq_path(candidate, Path::new(target)) {
                Some("")
            } else {
                None
            }
        }
        super::loader::CompiledPattern::Glob { matcher, root, .. } => {
            if !matcher.is_match(&cand_norm) {
                return false;
            }
            rel_under_root(&cand_norm, root)
        }
    };
    let Some(rel) = anchor_rel else { return false };

    // 2. include / exclude refinement.
    if rel.is_empty() {
        if !rule.include_raw.is_empty() {
            return false;
        }
    } else {
        if !rule.include_raw.is_empty() && !rule.include_matchers.iter().any(|m| m.is_match(rel)) {
            return false;
        }
        if rule.exclude_matchers.iter().any(|m| m.is_match(rel)) {
            return false;
        }
    }

    // 3. Filesystem conditions.
    if rule.requires_existing && !path_exists(candidate) {
        return false;
    }
    if let Some(marker) = &rule.parent_marker {
        let exists = candidate
            .parent()
            .is_some_and(|p| path_exists(&p.join(marker)));
        if !exists {
            return false;
        }
    }
    true
}

/// `symlink_metadata`-based existence check (never follows links: an
/// existence claim on a dangling reparse point must not pass).
fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Byte offset of the first glob metacharacter (`*`, `?`, `[`, `{`) in a
/// slash-normalised pattern, if any.
#[must_use]
pub fn first_meta_offset(pattern: &str) -> Option<usize> {
    pattern
        .bytes()
        .position(|b| matches!(b, b'*' | b'?' | b'[' | b'{'))
}

/// Compiles a glob pattern (case-insensitive, `/`-separated) or returns the
/// globset error text.
///
/// `literal_separator(true)` keeps `*`/`?` from crossing `/` (standard glob
/// semantics); `**` still matches across directories.
pub fn compile_glob(pattern: &str) -> Result<GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_uppercase(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(&name.to_ascii_uppercase()).cloned()
    }

    #[test]
    fn expand_supports_percent_and_braces_mixed_case() {
        let env = fake_env(&[
            ("USERPROFILE", r"C:\Users\demo"),
            ("LocalAppData", r"C:\Users\demo\AppData\Local"),
        ]);
        assert_eq!(
            expand_env("%USERPROFILE%/.ssh", &env).unwrap(),
            r"C:\Users\demo/.ssh"
        );
        assert_eq!(
            expand_env("${localappdata}/npm-cache", &env).unwrap(),
            r"C:\Users\demo\AppData\Local/npm-cache"
        );
        assert_eq!(
            expand_env("a%USERPROFILE%b%USERPROFILE%", &env).unwrap(),
            "aC:\\Users\\demobC:\\Users\\demo"
        );
        // Literal text without variables is returned as-is.
        assert_eq!(expand_env("C:/plain/path", &env).unwrap(), "C:/plain/path");
    }

    #[test]
    fn expand_fails_closed_on_unknown_variable() {
        let env = fake_env(&[]);
        match expand_env("%NOPE%/x", &env) {
            Err(ExpandError::UnknownVariable { name }) => assert_eq!(name, "NOPE"),
            other => panic!("expected UnknownVariable, got {other:?}"),
        }
        assert!(matches!(
            expand_env("${UNCLOSED", &env),
            Err(ExpandError::Unterminated { .. })
        ));
        assert!(matches!(
            expand_env("%%", &env),
            Err(ExpandError::EmptyName { .. })
        ));
    }

    #[test]
    fn path_comparison_is_case_and_separator_insensitive() {
        assert!(path_eq(r"C:\Users\demo\.SSH", "c:/users/DEMO/.ssh"));
        assert!(!path_eq(r"C:\Users\demo", r"C:\Users\demo2"));
        // rel_under_root expects slash-normalised inputs.
        assert_eq!(
            rel_under_root("c:/Users/demo/.claude/x", "C:/Users/DEMO/.claude"),
            Some("x")
        );
        assert_eq!(
            rel_under_root("c:/Users/demo/.claude", "c:/Users/demo/.claude"),
            Some("")
        );
        assert_eq!(
            rel_under_root("c:/Users/demo/x", "c:/Users/demo/.claude"),
            None
        );
        // Sibling prefix must not be treated as containment.
        assert_eq!(rel_under_root("c:/a/bc", "c:/a/b"), None);
    }

    #[test]
    fn glob_meta_detection() {
        assert_eq!(first_meta_offset("C:/Users/*"), Some(9));
        assert_eq!(first_meta_offset("C:/Users/demo"), None);
        assert_eq!(first_meta_offset("**/x"), Some(0));
    }

    #[test]
    fn exact_matching_folds_unicode_case() {
        // Oracle F4: exact rules must catch case variants of non-ASCII user
        // names (`José` / `JOSÉ`), not just ASCII.
        use crate::rules::loader::{CompiledPattern, CompiledRule};
        use crate::rules::priority::RuleSource;
        use crate::{ResidueCategory, RiskLevel, RuleId};
        use std::path::PathBuf;

        let rule = CompiledRule {
            id: "builtin-protected/test-ssh".into(),
            numeric_id: RuleId::from_raw(1),
            source: RuleSource::BuiltinProtected,
            risk: RiskLevel::Protected,
            category: ResidueCategory::Credential,
            product: None,
            description: "ssh for José".into(),
            pattern: CompiledPattern::Exact {
                target: "c:/users/josé/.ssh".into(),
            },
            parent_marker: None,
            requires_existing: false,
            include_raw: Vec::new(),
            exclude_raw: Vec::new(),
            include_matchers: Vec::new(),
            exclude_matchers: Vec::new(),
            order: 0,
        };
        assert!(rule_matches(&rule, &PathBuf::from(r"C:\Users\José\.ssh")));
        assert!(rule_matches(&rule, &PathBuf::from("c:/USERS/JOSÉ/.ssh")));
        // Different path must still miss.
        assert!(!rule_matches(
            &rule,
            &PathBuf::from(r"C:\Users\José\.gnupg")
        ));
    }
}
