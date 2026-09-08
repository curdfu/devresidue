//! Lexical (never-touching-disk) Windows path semantics living **in core**.
//!
//! The platform crate owns the full-fidelity UTF-16 implementation
//! (`crates/devresidue-platform-windows/src/path`); core must not depend on
//! it, so the exact lexical rules the safety layer relies on are mirrored
//! here as platform-neutral text functions. The test matrix below is aligned
//! with the platform crate's `path` tests so both stay in lockstep.
//!
//! Guarantees provided:
//!
//! - separator normalisation (`/` → `\`) and run collapsing;
//! - lexical `.` / `..` resolution that never climbs above the drive / UNC
//!   share / volume anchor;
//! - case **preserved** in [`normalize`] output, case **folded** (simple
//!   upper) in comparison keys;
//! - `\\?\`, `\\?\UNC\` and `\\?\Volume{...}` extended prefixes understood and
//!   round-tripped;
//! - component-boundary containment ([`is_within`]) that is case-insensitive,
//!   treats extended and plain spellings of the same root as equal and rejects
//!   sibling prefixes (`C:\foo-bar` is not inside `C:\foo`).
//!
//! Scope note: text-level only. Non-UTF-8 names are lossy-transcribed here
//! (the platform layer keeps them verbatim as UTF-16 when actually touching
//! disk). Physical canonicalisation (hard links, 8.3 aliases, mount points) is
//! out of scope — deletion-time *identity* checks are what close those gaps.

use std::path::{Path, PathBuf};

/// Simple one-to-one upper-case fold (approximates Windows ordinal-ignore-case
/// for comparison purposes; multi-char expansions have no simple mapping and
/// stay as authored, matching the platform layer).
fn fold_char(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(folded), None) => folded,
        _ => c,
    }
}

fn fold(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

fn is_drive_prefix(comp: &str) -> bool {
    let bytes = comp.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Where a path is anchored.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Anchor {
    /// Bare relative path (no leading separator).
    Relative,
    /// Root-relative, e.g. `\foo` (relative to the current drive's root).
    RootedRelative,
    /// Drive-relative, e.g. `C:foo`.
    DriveRelative { letter: char },
    /// Absolute on a drive, e.g. `C:\foo`.
    DriveAbsolute { letter: char },
    /// UNC `\\server\share\...` (plain or `\\?\UNC\`).
    Unc { server: String, share: String },
    /// Verbatim non-drive, non-UNC object root, e.g. `\\?\Volume{GUID}\...`.
    Device { name: String },
}

/// Whether the anchor denotes an absolute namespace root.
fn is_absolute_anchor(a: &Anchor) -> bool {
    matches!(
        a,
        Anchor::DriveAbsolute { .. } | Anchor::Unc { .. } | Anchor::Device { .. }
    )
}

/// Whether `..` may climb above the anchor (drive/UNC/volume root = floor).
fn is_anchored(a: &Anchor) -> bool {
    matches!(
        a,
        Anchor::DriveAbsolute { .. } | Anchor::Unc { .. } | Anchor::Device { .. }
    )
}

/// Parsed, dot-resolved path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LexPath {
    anchor: Anchor,
    /// Normalised components (no `.`, `..`, empty entries). Original case.
    comps: Vec<String>,
    /// True when the input carried the extended `\\?\` prefix (affects
    /// rendering for `normalize` round-tripping).
    verbatim: bool,
}

fn parse(text: &str) -> LexPath {
    // Strip one extended prefix for parsing; remember it for rendering.
    let mut verbatim = false;
    let body: &str = if let Some(rest) = text.strip_prefix("\\\\?\\") {
        verbatim = true;
        rest
    } else {
        text
    };

    let raw_comps: Vec<&str> = body.split(is_sep).filter(|s| !s.is_empty()).collect();

    let anchor: Anchor;
    let mut comp_start: usize = 0;

    if verbatim {
        if let Some(first) = raw_comps.first() {
            if first.eq_ignore_ascii_case("UNC") {
                let server = raw_comps.get(1).unwrap_or(&"").to_string();
                let share = raw_comps.get(2).unwrap_or(&"").to_string();
                anchor = Anchor::Unc { server, share };
                comp_start = 3.min(raw_comps.len());
            } else if is_drive_prefix(first) {
                let letter = first.chars().next().expect("len==2");
                // Drive-rooted only when a separator immediately follows "X:".
                let after_colon_is_sep = body[2..].chars().next().is_some_and(is_sep);
                if after_colon_is_sep {
                    anchor = Anchor::DriveAbsolute { letter };
                } else {
                    // "\\?\C:foo" — not a plain drive path; treat generically.
                    anchor = Anchor::Device {
                        name: (*first).to_string(),
                    };
                }
                comp_start = 1;
            } else {
                // Generic verbatim object (volume GUID, device, ...).
                anchor = Anchor::Device {
                    name: (*first).to_string(),
                };
                comp_start = 1;
            }
        } else {
            anchor = Anchor::Relative;
        }
    } else if let Some(rest) = text.strip_prefix("\\\\") {
        // Plain UNC "\\server\share\..."
        let server = raw_comps.first().cloned().unwrap_or_default().to_string();
        let share = raw_comps.get(1).cloned().unwrap_or_default().to_string();
        anchor = Anchor::Unc { server, share };
        comp_start = 2.min(raw_comps.len());
        let _ = rest;
    } else if text.starts_with('\\') {
        // Root-relative "\foo" (single leading separator; UNC handled above).
        anchor = Anchor::RootedRelative;
        comp_start = 0;
    } else if let Some(first) = raw_comps.first() {
        if is_drive_prefix(first) {
            let letter = first.chars().next().expect("len==2");
            // Absolute only when a separator follows the colon.
            let colon_idx = body.find(':').expect("is_drive_prefix implies ':'");
            let absolute = body[colon_idx + 1..].chars().next().is_some_and(is_sep);
            anchor = if absolute {
                Anchor::DriveAbsolute { letter }
            } else {
                Anchor::DriveRelative { letter }
            };
            comp_start = 1;
        } else {
            anchor = Anchor::Relative;
            comp_start = 0;
        }
    } else {
        anchor = Anchor::Relative;
        comp_start = 0;
    }

    // If anchor parsing consumed components (UNC share parts, drive token),
    // those are already accounted for via comp_start.

    // Lexical resolution of "." and "..".
    let mut comps: Vec<String> = Vec::new();
    for comp in raw_comps[comp_start..].iter().copied() {
        if comp == "." {
            continue;
        }
        if comp == ".." {
            if !comps.is_empty() {
                comps.pop();
            } else if !is_anchored(&anchor) {
                // Leading ".." above an unanchored path: cannot resolve
                // without the working directory — drop it.
                continue;
            }
            continue;
        }
        comps.push((*comp).to_string());
    }

    LexPath {
        anchor,
        comps,
        verbatim,
    }
}

fn join_comps(comps: &[String], sep: &str) -> String {
    comps.join(sep)
}

/// Lexically normalises a Windows path (pure text, no disk access).
pub fn normalize(path: impl AsRef<Path>) -> PathBuf {
    let text = path.as_ref().to_string_lossy();
    let lex = parse(&text);
    PathBuf::from(render(&lex))
}

fn render(lex: &LexPath) -> String {
    let mut out = String::new();
    match &lex.anchor {
        Anchor::Relative => {
            out.push_str(&join_comps(&lex.comps, "\\"));
        }
        Anchor::RootedRelative => {
            out.push('\\');
            out.push_str(&join_comps(&lex.comps, "\\"));
        }
        Anchor::DriveRelative { letter } => {
            out.push(*letter);
            out.push(':');
            if !lex.comps.is_empty() {
                out.push_str(&join_comps(&lex.comps, "\\"));
            }
        }
        Anchor::DriveAbsolute { letter } => {
            if lex.verbatim {
                out.push_str("\\\\?\\");
            }
            out.push(*letter);
            out.push_str(":\\");
            out.push_str(&join_comps(&lex.comps, "\\"));
        }
        Anchor::Unc { server, share } => {
            if lex.verbatim {
                out.push_str("\\\\?\\UNC\\");
            } else {
                out.push_str("\\\\");
            }
            out.push_str(server);
            out.push('\\');
            out.push_str(share);
            if !lex.comps.is_empty() {
                out.push('\\');
                out.push_str(&join_comps(&lex.comps, "\\"));
            }
        }
        Anchor::Device { name } => {
            out.push_str("\\\\?\\");
            out.push_str(name);
            if !lex.comps.is_empty() {
                out.push('\\');
                out.push_str(&join_comps(&lex.comps, "\\"));
            }
        }
    }
    out
}

/// True when the path is absolute in Windows terms (drive root, UNC, or
/// verbatim drive/UNC/volume) — pure lexical.
pub fn is_absolute(path: impl AsRef<Path>) -> bool {
    let lex = parse(&path.as_ref().to_string_lossy());
    is_absolute_anchor(&lex.anchor)
}

/// If `path` names exactly a UNC **share root** (`\\server\share` with no
/// deeper component, including the extended `\\?\UNC\server\share` spelling),
/// returns its normalised form. A share root is undeletable by nature — no
/// enumeration of mounted shares is needed to recognise the shape (F13).
#[must_use]
pub fn unc_share_root(path: impl AsRef<Path>) -> Option<PathBuf> {
    let lex = parse(&path.as_ref().to_string_lossy());
    match &lex.anchor {
        Anchor::Unc { server, share }
            if !server.is_empty() && !share.is_empty() && lex.comps.is_empty() =>
        {
            Some(PathBuf::from(render(&lex)))
        }
        _ => None,
    }
}

/// Folded comparison key of a normalised path. Extended (`\\?\`) and plain
/// spellings of the same path yield the same key; the key is component
/// structured (single `\` separators, no trailing separator, drive roots keep
/// `X:\`).
fn key_of(path: impl AsRef<Path>) -> String {
    let lex = parse(&path.as_ref().to_string_lossy());
    let mut key = String::new();
    match &lex.anchor {
        Anchor::Relative | Anchor::RootedRelative => {}
        Anchor::DriveRelative { letter } | Anchor::DriveAbsolute { letter } => {
            key.push(fold_char(*letter));
            key.push(':');
            if matches!(lex.anchor, Anchor::DriveAbsolute { .. }) {
                key.push('\\');
            }
        }
        Anchor::Unc { server, share } => {
            key.push_str("\\\\");
            key.push_str(&fold(server));
            key.push('\\');
            key.push_str(&fold(share));
        }
        Anchor::Device { name } => {
            // Distinct namespace marker: a volume path never collides with a
            // drive path.
            key.push_str("\\\\?\\");
            key.push_str(&fold(name));
        }
    }
    for (i, comp) in lex.comps.iter().enumerate() {
        if i > 0 || matches!(lex.anchor, Anchor::Unc { .. } | Anchor::Device { .. }) {
            key.push('\\');
        }
        key.push_str(&fold(comp));
    }
    key
}

/// True when `child` names the same object as `ancestor` or something below
/// it, comparing **component boundaries** case-insensitively and treating
/// extended/plain spellings of the same root as equal.
///
/// Both must share the same anchor (drive letter, UNC server+share, or
/// verbatim device); drive-relative and bare-relative paths compare only
/// against themselves.
pub fn is_within(child: impl AsRef<Path>, ancestor: impl AsRef<Path>) -> bool {
    let child = parse(&child.as_ref().to_string_lossy());
    let ancestor = parse(&ancestor.as_ref().to_string_lossy());
    if !same_anchor(&child.anchor, &ancestor.anchor) {
        return false;
    }
    ancestor.comps.len() <= child.comps.len()
        && child
            .comps
            .iter()
            .zip(ancestor.comps.iter())
            .all(|(c, a)| fold(c) == fold(a))
}

fn same_anchor(a: &Anchor, b: &Anchor) -> bool {
    use Anchor::*;
    match (a, b) {
        (Relative, Relative) | (RootedRelative, RootedRelative) => true,
        // A rooted-relative path is anchored to a drive root; a bare relative
        // path to a working directory — not comparable.
        (Relative, _) | (RootedRelative, _) | (_, Relative) | (_, RootedRelative) => false,
        (DriveRelative { letter: l1 }, DriveRelative { letter: l2 })
        | (DriveAbsolute { letter: l1 }, DriveAbsolute { letter: l2 }) => {
            fold_char(*l1) == fold_char(*l2)
        }
        (DriveRelative { .. }, DriveAbsolute { .. })
        | (DriveAbsolute { .. }, DriveRelative { .. }) => false,
        (
            Unc {
                server: s1,
                share: h1,
            },
            Unc {
                server: s2,
                share: h2,
            },
        ) => fold(s1) == fold(s2) && fold(h1) == fold(h2),
        (Device { name: n1 }, Device { name: n2 }) => fold(n1) == fold(n2),
        _ => false,
    }
}

/// True when two paths denote the same lexical location under Windows
/// case-insensitive semantics (component-boundary, extended/plain aware).
#[must_use]
pub fn normalized_eq_path(a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
    key_of(a) == key_of(b)
}

/// All strict ancestor prefixes of `path` (root first, target's parent last).
///
/// Used by the reparse guard to check that no component *between* the root
/// and the target is a reparse point without the target itself being touched.
/// Empty for paths with no components (a bare root).
pub fn ancestor_prefixes(path: impl AsRef<Path>) -> Vec<PathBuf> {
    let text = path.as_ref().to_string_lossy().into_owned();
    let lex = parse(&text);
    let mut out = Vec::new();

    // Rebuild progressively from the rendered root + first k components.
    for k in 0..lex.comps.len() {
        let partial = LexPath {
            anchor: lex.anchor.clone(),
            comps: lex.comps[..k].to_vec(),
            verbatim: lex.verbatim,
        };
        // A bare relative/root-relative anchor with zero components is not a
        // meaningful "existing ancestor" to probe.
        if k == 0
            && matches!(
                partial.anchor,
                Anchor::Relative | Anchor::RootedRelative | Anchor::DriveRelative { .. }
            )
        {
            continue;
        }
        let rendered = render(&partial);
        if rendered.is_empty() {
            continue;
        }
        out.push(PathBuf::from(rendered));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn normalize_resolves_mixed_separators_and_dots() {
        assert_eq!(normalize("C:/Users//foo/../bar"), p("C:\\Users\\bar"));
        assert_eq!(normalize("C:\\Users\\.\\foo"), p("C:\\Users\\foo"));
        assert_eq!(normalize("C:\\foo\\.."), p("C:\\"));
        assert_eq!(normalize("C:\\..\\foo"), p("C:\\foo"));
        assert_eq!(normalize("C:\\"), p("C:\\"));
        assert_eq!(normalize("C:\\foo\\bar\\..\\"), p("C:\\foo"));
    }

    #[test]
    fn normalize_dotdot_never_escapes_drive_root() {
        assert_eq!(normalize("C:\\a\\b\\..\\..\\..\\..\\x"), p("C:\\x"));
    }

    #[test]
    fn normalize_handles_verbatim_prefixes_bidirectionally() {
        assert_eq!(normalize("\\\\?\\C:\\a\\..\\b"), p("\\\\?\\C:\\b"));
        assert_eq!(
            normalize("\\\\?\\UNC\\srv\\share\\a\\..\\b"),
            p("\\\\?\\UNC\\srv\\share\\b")
        );
        assert_eq!(normalize("\\\\?\\C:/a//b"), p("\\\\?\\C:\\a\\b"));
    }

    #[test]
    fn normalize_handles_unc_and_case_preservation() {
        assert_eq!(normalize("\\\\srv\\share\\a"), p("\\\\srv\\share\\a"));
        assert_eq!(normalize("C:\\Users\\Foo"), p("C:\\Users\\Foo"));
    }

    #[test]
    fn normalize_drive_relative_and_root_relative() {
        assert_eq!(normalize("C:foo\\bar"), p("C:foo\\bar"));
        assert_eq!(normalize("\\foo\\bar"), p("\\foo\\bar"));
    }

    #[test]
    fn is_absolute_detection() {
        assert!(is_absolute("C:\\foo"));
        assert!(is_absolute("\\\\srv\\share\\x"));
        assert!(is_absolute("\\\\?\\Volume{abc}\\x"));
        assert!(is_absolute("\\\\?\\C:\\x"));
        assert!(!is_absolute("C:foo"));
        assert!(!is_absolute("foo\\bar"));
        assert!(!is_absolute("\\foo"));
    }

    #[test]
    fn keys_fold_case_extended_and_trailing() {
        assert_eq!(key_of("C:\\Foo\\Bar"), key_of("c:\\foo\\bar"));
        assert_eq!(key_of("C:\\FOO\\BAR\\"), key_of("c:\\foo\\bar"));
        assert_eq!(key_of("\\\\?\\C:\\foo\\bar"), key_of("C:\\foo\\bar"));
        assert_eq!(
            key_of("\\\\?\\UNC\\srv\\share\\x"),
            key_of("\\\\srv\\share\\x")
        );
        // Drive root keeps its separator; a drive-relative path is distinct.
        assert_ne!(key_of("C:\\"), key_of("C:"));
    }

    #[test]
    fn is_within_basic_and_sibling_negative() {
        assert!(is_within("C:\\foo\\bar", "C:\\foo"));
        assert!(is_within("C:\\foo", "C:\\foo"));
        assert!(is_within("C:\\foo\\bar\\baz\\..", "C:\\foo"));
        assert!(!is_within("C:\\foo-bar", "C:\\foo"));
        assert!(!is_within("C:\\fo", "C:\\foo"));
        assert!(!is_within("D:\\foo", "C:\\foo"));
    }

    #[test]
    fn is_within_case_and_prefix_mix() {
        assert!(is_within("c:\\FOO\\Bar", "C:\\foo"));
        assert!(is_within("\\\\?\\C:\\foo\\bar", "C:\\foo"));
        assert!(is_within("C:\\foo\\bar", "\\\\?\\C:\\foo"));
        assert!(is_within("\\\\?\\UNC\\srv\\share\\a", "\\\\srv\\share"));
        assert!(is_within("C:\\Windows\\System32", "C:\\"));
        assert!(!is_within("C:\\Windows", "C:\\Windows\\System32"));
    }

    #[test]
    fn is_within_relative_best_effort() {
        assert!(is_within("a\\b\\c", "a\\b"));
        assert!(!is_within("a\\b", "a\\c"));
        assert!(!is_within("C:\\foo", "foo"));
        assert!(!is_within("foo", "C:\\foo"));
        assert!(is_within("C:foo\\bar", "C:foo"));
        assert!(!is_within("C:foo", "C:\\"));
    }

    #[test]
    fn ancestor_prefixes_walk_from_root() {
        let prefixes = ancestor_prefixes("C:\\Users\\alice\\dir\\target");
        let rendered: Vec<String> = prefixes
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "C:\\",
                "C:\\Users",
                "C:\\Users\\alice",
                "C:\\Users\\alice\\dir"
            ]
        );

        let unc = ancestor_prefixes("\\\\srv\\share\\a\\b");
        let rendered: Vec<String> = unc
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered, vec!["\\\\srv\\share", "\\\\srv\\share\\a"]);

        let verbatim = ancestor_prefixes("\\\\?\\Volume{abc}\\x");
        let rendered: Vec<String> = verbatim
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered, vec!["\\\\?\\Volume{abc}"]);

        assert!(ancestor_prefixes("C:\\").is_empty());
        assert!(ancestor_prefixes("file.txt").is_empty());
    }

    #[test]
    fn normalized_eq_path_compares_case_and_prefix_aware() {
        assert!(normalized_eq_path("C:\\Foo\\Bar", "c:/foo/bar"));
        assert!(normalized_eq_path("\\\\?\\C:\\foo\\bar", "C:\\foo\\bar"));
        assert!(normalized_eq_path(
            "\\\\?\\UNC\\srv\\share\\x",
            "\\\\srv\\share\\x"
        ));
        assert!(!normalized_eq_path("C:\\foo", "C:\\foo-bar"));
    }
}
