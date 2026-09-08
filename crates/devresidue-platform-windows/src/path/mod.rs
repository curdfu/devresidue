//! Windows path semantics — **pure lexical**, never touches the disk.
//!
//! Provides the primitive the Phase 5 SafetyValidator builds on:
//!
//! - [`normalize`] — separator normalisation (`/` → `\`), lexical `.` / `..`
//!   resolution, `\\?\` / `\\?\UNC\` extended-prefix round-tripping, original
//!   case preserved;
//! - [`is_within`] — component-boundary containment that is case-insensitive,
//!   treats extended (`\\?\`) and plain spellings of the same path as equal,
//!   and rejects sibling prefixes (`C:\foo-bar` is *not* inside `C:\foo`);
//! - [`as_key`] — a case-folded, single-separator, trailing-separator-stripped
//!   comparison form usable as a map key / JSON snapshot field;
//! - [`to_extended`] — renders an absolute path with the `\\?\` prefix so the
//!   Win32 handle-open helpers survive `MAX_PATH`.
//!
//! Semantics notes:
//!
//! - Everything operates on UTF-16 code units (via `OsStrExt::encode_wide`)
//!   so non-UTF-8 Windows names survive intact; case folding is a *simple*
//!   (one-to-one) invariant fold toward upper case, mirroring how
//!   `CompareStringOrdinal` behaves, not a full locale-aware fold.
//! - Verbatim (`\\?\`) paths are still lexically resolved for `.` / `..`.
//!   That deviates from strict Win32 verbatim behaviour but is what the
//!   normalisation contract of this layer requires; disk access always goes
//!   through the dedicated `filesystem` module afterwards.
//! - `..` may never climb above a namespace root: the drive letter / UNC
//!   share / volume anchor is a hard floor.
//! - Drive-relative paths (`C:foo`) and bare-relative paths are supported
//!   lexically but can never be said to sit inside an absolute path (that
//!   would require the process working directory — a disk/process fact).

use std::ffi::OsString;
use std::fmt;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Where a path is anchored (its "namespace root").
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// Bare relative path; `rooted` marks a leading separator (`\foo`).
    Relative { rooted: bool },
    /// Drive-relative (`C:foo`) — no separator follows the colon.
    DriveRelative { letter: u16 },
    /// Drive root `C:\`; `verbatim` renders it as `\\?\C:\`.
    DriveAbsolute { letter: u16, verbatim: bool },
    /// `\\server\share\...`; `verbatim` renders it as `\\?\UNC\server\share`.
    Unc {
        server: Vec<u16>,
        share: Vec<u16>,
        verbatim: bool,
    },
    /// Verbatim non-drive, non-UNC object, e.g. `\\?\Volume{GUID}\...`.
    Device { name: Vec<u16> },
}

impl Origin {
    /// True when this origin names a fixed absolute namespace root.
    fn is_absolute(&self) -> bool {
        matches!(
            self,
            Origin::DriveAbsolute { .. } | Origin::Unc { .. } | Origin::Device { .. }
        )
    }

    /// True when `..` has a hard floor (drive/UNC/volume root).
    fn is_anchored(&self) -> bool {
        matches!(
            self,
            Origin::DriveAbsolute { .. } | Origin::Unc { .. } | Origin::Device { .. }
        )
    }
}

/// A path parsed into an origin + normalised segments. Segment spelling is
/// preserved verbatim here (only anchors/parts are raw UTF-16); folding
/// happens lazily at comparison/key time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPath {
    origin: Origin,
    /// Normalised path components (no `.`, `..` or empty entries).
    segments: Vec<Vec<u16>>,
}

fn is_sep(u: u16) -> bool {
    u == b'\\' as u16 || u == b'/' as u16
}

fn is_ascii_letter(u: u16) -> bool {
    (0x41..=0x5A).contains(&u) || (0x61..=0x7A).contains(&u)
}

fn ascii_fold_lower(u: u16) -> u16 {
    if (0x61..=0x7A).contains(&u) {
        u - 0x20
    } else {
        u
    }
}

/// Simple one-to-one upper-case fold (approximates Windows ordinal-ignore-case).
fn simple_upper(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(folded), None) => folded,
        _ => c, // multi-char expansion (e.g. ß) has no simple mapping — keep as-is
    }
}

/// Case-folds a UTF-16 slice toward upper case. ASCII folds in place;
/// surrogate pairs and BMP characters go through [`simple_upper`]. Lone
/// surrogates are kept verbatim.
fn fold_units(units: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if u < 0x80 {
            out.push(ascii_fold_lower(u));
            i += 1;
            continue;
        } else if (0xD800..=0xDBFF).contains(&u) && i + 1 < units.len() {
            let next = units[i + 1];
            if (0xDC00..=0xDFFF).contains(&next) {
                let cp = 0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(next) - 0xDC00);
                let ch = char::from_u32(cp).map(simple_upper).unwrap_or('\u{FFFD}');
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    out.push(*unit);
                }
                i += 2;
                continue;
            }
        }
        let ch = char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}');
        let ch = simple_upper(ch);
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf) {
            out.push(*unit);
        }
        i += 1;
    }
    out
}

/// Splits `units[start..]` on separators, skipping empty runs. Never returns
/// empty components.
fn split_components(units: &[u16], start: usize) -> Vec<Vec<u16>> {
    let mut comps = Vec::new();
    let mut cur = Vec::new();
    for &u in &units[start..] {
        if is_sep(u) {
            if !cur.is_empty() {
                comps.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(u);
        }
    }
    if !cur.is_empty() {
        comps.push(cur);
    }
    comps
}

fn seg_eq(seg: &[u16], lit: &str) -> bool {
    seg.iter().copied().eq(lit.encode_utf16())
}

fn is_drive_prefix(seg: &[u16]) -> bool {
    seg.len() == 2 && is_ascii_letter(seg[0]) && seg[1] == b':' as u16
}

/// Parses and lexically resolves a path. Never touches the disk.
///
/// Returns `(origin, split_start, anchor_drop)`: components are read from
/// `split_start` and the first `anchor_drop` of them are consumed by the
/// origin itself (UNC server+share, drive letter token, ...).
fn determine(units: &[u16]) -> (Origin, usize, usize) {
    let n = units.len();

    // ---- Extended-length "\\?\" prefix ------------------------------------
    if n >= 4
        && units[0] == b'\\' as u16
        && units[1] == b'\\' as u16
        && units[2] == b'?' as u16
        && units[3] == b'\\' as u16
    {
        let comps = split_components(units, 4);
        let Some(first) = comps.first() else {
            return (Origin::Relative { rooted: false }, 4, 0);
        };
        if seg_eq(first, "UNC") {
            let server = comps.get(1).cloned().unwrap_or_default();
            let share = comps.get(2).cloned().unwrap_or_default();
            return (
                Origin::Unc {
                    server,
                    share,
                    verbatim: true,
                },
                4,
                3,
            );
        }
        if is_drive_prefix(first) {
            let letter = first[0];
            // Drive-rooted only when a separator immediately follows "X:".
            let colon_at = 4 + 1; // index of ':' inside `units`
            let rooted = units.get(colon_at + 1).is_some_and(|u| is_sep(*u));
            if rooted {
                return (
                    Origin::DriveAbsolute {
                        letter,
                        verbatim: true,
                    },
                    4,
                    1,
                );
            }
            // "\\?\C:foo" is not a plain drive path; treat generically.
            return (
                Origin::Device {
                    name: first.clone(),
                },
                4,
                1,
            );
        }
        // Generic verbatim object (volume GUID, device, ...).
        return (
            Origin::Device {
                name: first.clone(),
            },
            4,
            1,
        );
    }

    // ---- Plain UNC "\\server\share\..." -----------------------------------
    if n >= 2 && units[0] == b'\\' as u16 && units[1] == b'\\' as u16 {
        let comps = split_components(units, 0);
        let server = comps.first().cloned().unwrap_or_default();
        let share = comps.get(1).cloned().unwrap_or_default();
        return (
            Origin::Unc {
                server,
                share,
                verbatim: false,
            },
            0,
            2,
        );
    }

    // ---- Drive path "X:" / "X:\" ------------------------------------------
    if n >= 2 && is_ascii_letter(units[0]) && units[1] == b':' as u16 {
        let letter = units[0];
        if units.get(2).is_some_and(|u| is_sep(*u)) {
            return (
                Origin::DriveAbsolute {
                    letter,
                    verbatim: false,
                },
                3,
                0,
            );
        }
        return (Origin::DriveRelative { letter }, 2, 0);
    }

    // ---- Root-relative "\foo" or plain relative ----------------------------
    if n >= 1 && is_sep(units[0]) {
        (Origin::Relative { rooted: true }, 0, 0)
    } else {
        (Origin::Relative { rooted: false }, 0, 0)
    }
}

fn parse(path: &Path) -> ParsedPath {
    let units: Vec<u16> = path.as_os_str().encode_wide().collect();
    let (origin, split_start, anchor_drop) = determine(&units);
    let mut comps = split_components(&units, split_start);
    let keep = anchor_drop.min(comps.len());
    comps.drain(..keep);

    // Lexical resolution of "." and "..".
    let mut segments: Vec<Vec<u16>> = Vec::with_capacity(comps.len());
    for comp in comps {
        if seg_eq(&comp, ".") {
            continue;
        }
        if seg_eq(&comp, "..") {
            if !segments.is_empty() {
                segments.pop();
            } else if !origin.is_anchored() {
                // Relative path: leading ".." climb above the caller's own
                // root — cannot be resolved without the working directory.
                continue;
            }
            // Anchored root: clamped.
            continue;
        }
        segments.push(comp);
    }

    ParsedPath { origin, segments }
}

/// Builds the prefix text for an origin plus whether a separator must be
/// inserted before the first path segment.
fn render_parts(origin: &Origin) -> (Vec<u16>, bool) {
    let mut prefix: Vec<u16> = Vec::new();
    let needs_sep = match origin {
        Origin::Relative { rooted: false } => false,
        Origin::Relative { rooted: true } => {
            prefix.push(b'\\' as u16);
            false
        }
        Origin::DriveRelative { letter } => {
            prefix.push(*letter);
            prefix.push(b':' as u16);
            false // "C:" joins its first component directly ("C:foo")
        }
        Origin::DriveAbsolute { letter, verbatim } => {
            if *verbatim {
                prefix.extend_from_slice(&[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]);
            }
            prefix.push(*letter);
            prefix.push(b':' as u16);
            prefix.push(b'\\' as u16);
            false
        }
        Origin::Unc {
            server,
            share,
            verbatim,
        } => {
            if *verbatim {
                prefix.extend_from_slice(&[
                    b'\\' as u16,
                    b'\\' as u16,
                    b'?' as u16,
                    b'\\' as u16,
                    b'U' as u16,
                    b'N' as u16,
                    b'C' as u16,
                    b'\\' as u16,
                ]);
            } else {
                prefix.extend_from_slice(&[b'\\' as u16, b'\\' as u16]);
            }
            prefix.extend_from_slice(server);
            prefix.push(b'\\' as u16);
            prefix.extend_from_slice(share);
            true
        }
        Origin::Device { name } => {
            prefix.extend_from_slice(&[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]);
            prefix.extend_from_slice(name);
            true
        }
    };
    (prefix, needs_sep)
}

/// Renders origin + segments into an owned UTF-16 buffer.
fn render(parsed: &ParsedPath) -> Vec<u16> {
    let (mut prefix, needs_sep) = render_parts(&parsed.origin);
    for (i, seg) in parsed.segments.iter().enumerate() {
        if (i == 0 && needs_sep) || i > 0 {
            prefix.push(b'\\' as u16);
        }
        prefix.extend_from_slice(seg);
    }
    prefix
}

/// Normalises a Windows path lexically.
///
/// - `/` separators become `\`;
/// - runs of separators collapse;
/// - `.` components vanish, `..` pops the previous component but never climbs
///   above the drive / UNC share / volume anchor;
/// - `\\?\` and `\\?\UNC\` extended prefixes are preserved (canonical
///   spelling) when present on the input;
/// - original case is preserved.
///
/// Pure lexical: no disk access, no `MAX_PATH` handling.
pub fn normalize(raw: impl AsRef<Path>) -> PathBuf {
    let parsed = parse(raw.as_ref());
    PathBuf::from(OsString::from_wide(&render(&parsed)))
}

/// Renders an absolute path in extended-length (`\\?\`) form so Win32 handle
/// operations survive `MAX_PATH`. Relative and drive-relative inputs are
/// returned as their normalised plain form (no working directory available to
/// anchor them).
pub fn to_extended(raw: impl AsRef<Path>) -> PathBuf {
    let norm = normalize(raw);
    let parsed = parse(&norm);
    if !parsed.origin.is_absolute() {
        return norm;
    }
    let mut out: Vec<u16> = vec![b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    match &parsed.origin {
        Origin::DriveAbsolute { letter, .. } => {
            out.push(*letter);
            out.push(b':' as u16);
            out.push(b'\\' as u16);
        }
        Origin::Unc { server, share, .. } => {
            out.extend_from_slice(&[b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16]);
            out.extend_from_slice(server);
            out.push(b'\\' as u16);
            out.extend_from_slice(share);
        }
        Origin::Device { name } => {
            out.extend_from_slice(name);
        }
        _ => unreachable!("is_absolute checked above"),
    }
    // The extended drive prefix already ends with '\'; the UNC/device ones
    // need a separator before their first segment.
    let prefix_ends_sep = matches!(parsed.origin, Origin::DriveAbsolute { .. });
    for (i, seg) in parsed.segments.iter().enumerate() {
        if i > 0 || !prefix_ends_sep {
            out.push(b'\\' as u16);
        }
        out.extend_from_slice(seg);
    }
    PathBuf::from(OsString::from_wide(&out))
}

fn lossy_from_units(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// Canonical, case-folded comparison key for a path.
///
/// The key is built from the normalised path with:
///
/// - case folded to an invariant upper form (ASCII plus simple Unicode fold);
/// - single `\` separators;
/// - no trailing separator (the drive root keeps `C:\` as its key);
/// - extended (`\\?\`) and plain spellings mapping to the *same* key, and
///   `\\?\UNC\server\share` mapping onto `\\server\share`.
pub fn as_key(path: impl AsRef<Path>) -> PathKey {
    let parsed = parse(path.as_ref());
    PathKey(fold_path_to_key(&parsed))
}

fn push_folded(out: &mut String, units: &[u16]) {
    let folded = fold_units(units);
    out.push_str(&lossy_from_units(&folded));
}

fn fold_path_to_key(parsed: &ParsedPath) -> String {
    let mut key = String::new();
    match &parsed.origin {
        Origin::Relative { .. } => {}
        Origin::DriveRelative { letter } | Origin::DriveAbsolute { letter, .. } => {
            key.push(char::from_u32(u32::from(ascii_fold_lower(*letter))).unwrap_or('?'));
            key.push(':');
            if matches!(parsed.origin, Origin::DriveAbsolute { .. }) {
                key.push('\\');
            }
        }
        Origin::Unc { server, share, .. } => {
            key.push('\\');
            key.push('\\');
            push_folded(&mut key, server);
            key.push('\\');
            push_folded(&mut key, share);
        }
        Origin::Device { name } => {
            // Distinct namespace marker so a volume path never collides with
            // a drive path.
            key.push_str("\\\\?\\");
            push_folded(&mut key, name);
        }
    }
    for (i, seg) in parsed.segments.iter().enumerate() {
        if i > 0 || matches!(parsed.origin, Origin::Unc { .. } | Origin::Device { .. }) {
            key.push('\\');
        }
        push_folded(&mut key, seg);
    }
    key
}

/// True when `child` is `ancestor` itself or sits lexically inside it.
///
/// Rules:
///
/// - component-boundary compare (`C:\foo-bar` is NOT inside `C:\foo`);
/// - case-insensitive; extended vs plain spelling of the same root are equal;
/// - `child` must carry the *same* anchor (drive letter, UNC server+share, or
///   verbatim device) as `ancestor` — a UNC path is never "inside" a drive
///   path;
/// - drive-relative (`C:x`) / bare-relative paths are only comparable against
///   other relative paths (both lack a disk anchor);
/// - inclusive: `is_within(p, p) == true`.
pub fn is_within(child: impl AsRef<Path>, ancestor: impl AsRef<Path>) -> bool {
    let child = parse(child.as_ref());
    let ancestor = parse(ancestor.as_ref());

    if !same_anchor(&child.origin, &ancestor.origin) {
        return false;
    }
    ancestor.segments.len() <= child.segments.len()
        && child
            .segments
            .iter()
            .zip(ancestor.segments.iter())
            .all(|(c, a)| fold_units(c) == fold_units(a))
}

/// Whether two origins denote the same namespace anchor.
fn same_anchor(a: &Origin, b: &Origin) -> bool {
    use Origin::*;
    match (a, b) {
        (Relative { .. }, Relative { .. }) => true,
        (DriveAbsolute { letter: l1, .. }, DriveAbsolute { letter: l2, .. })
        | (DriveRelative { letter: l1 }, DriveRelative { letter: l2 }) => {
            fold_units(&[*l1]) == fold_units(&[*l2])
        }
        // A drive-relative path is anchored in the process CWD of that drive:
        // no lexical relation to a drive-absolute path exists.
        (DriveAbsolute { .. }, DriveRelative { .. })
        | (DriveRelative { .. }, DriveAbsolute { .. }) => false,
        (
            Unc {
                server: s1,
                share: sh1,
                ..
            },
            Unc {
                server: s2,
                share: sh2,
                ..
            },
        ) => fold_units(s1) == fold_units(s2) && fold_units(sh1) == fold_units(sh2),
        (Device { name: n1 }, Device { name: n2 }) => fold_units(n1) == fold_units(n2),
        _ => false,
    }
}

/// Opaque comparison key produced by [`as_key`]. Hashable and comparable so it
/// can be used directly as a map/set key for snapshot de-duplication.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathKey(String);

impl PathKey {
    /// The folded key text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PathKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for PathKey {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl std::ops::Deref for PathKey {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    fn as_key_folds_case_and_trailing_separators() {
        assert_eq!(as_key("C:\\Foo\\Bar").as_str(), "C:\\FOO\\BAR");
        assert_eq!(as_key("c:/foo/bar/").as_str(), "C:\\FOO\\BAR");
        assert_eq!(as_key("C:\\FOO\\BAR"), as_key("c:\\foo\\bar"));
        assert_eq!(as_key("C:\\"), as_key("c:\\"));
    }

    #[test]
    fn as_key_treats_extended_and_plain_as_equal() {
        assert_eq!(as_key("\\\\?\\C:\\foo\\bar"), as_key("C:\\foo\\bar"));
        assert_eq!(
            as_key("\\\\?\\UNC\\srv\\share\\x"),
            as_key("\\\\srv\\share\\x")
        );
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
    fn is_within_case_insensitive() {
        assert!(is_within("c:\\FOO\\Bar", "C:\\foo"));
        assert!(is_within("C:\\Foo\\b", "c:\\FOO"));
    }

    #[test]
    fn is_within_extended_and_plain_mix() {
        assert!(is_within("\\\\?\\C:\\foo\\bar", "C:\\foo"));
        assert!(is_within("C:\\foo\\bar", "\\\\?\\C:\\foo"));
        assert!(is_within("\\\\?\\UNC\\srv\\share\\a", "\\\\srv\\share"));
    }

    #[test]
    fn is_within_drive_root_contains_drive_children() {
        assert!(is_within("C:\\Windows\\System32", "C:\\"));
        assert!(!is_within("C:\\Windows", "C:\\Windows\\System32"));
    }

    #[test]
    fn is_within_relative_best_effort() {
        assert!(is_within("a\\b\\c", "a\\b"));
        assert!(!is_within("a\\b", "a\\c"));
        assert!(!is_within("C:\\foo", "foo"));
        assert!(!is_within("foo", "C:\\foo"));
    }

    #[test]
    fn to_extended_produces_verbatim_forms() {
        assert_eq!(to_extended("C:\\foo\\bar"), p("\\\\?\\C:\\foo\\bar"));
        assert_eq!(
            to_extended("\\\\srv\\share\\x"),
            p("\\\\?\\UNC\\srv\\share\\x")
        );
        assert_eq!(to_extended("foo\\bar"), p("foo\\bar"));
    }

    #[test]
    fn pathkey_is_hashable_and_comparable() {
        let a = as_key("C:\\Foo");
        let b = as_key("c:\\foo");
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&b));
    }

    #[test]
    fn drive_relative_vs_absolute_not_confused() {
        assert!(!is_within("C:foo", "C:\\"));
        assert!(!is_within("C:\\foo", "C:foo"));
        assert!(is_within("C:foo\\bar", "C:foo"));
    }
}
