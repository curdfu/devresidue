//! Rule validation — the release-blocking checks (SPEC §31, §32; PLAN Phase 3).
//!
//! Everything that could make a rule dangerous is rejected here **before** a
//! rule is compiled and becomes resolvable:
//!
//! - INV-006: no rule may target a protected root itself (`%USERPROFILE%`,
//!   any drive root, `%SYSTEMROOT%`, ...) or wholesale-sweep its contents
//!   (`C:\*`, `%USERPROFILE%/*`, `%USERPROFILE%/**`, `C:\{*,**}`, ...).
//! - SPEC §9: a detection rule must not classify a whole AI agent root
//!   (`%USERPROFILE%\.claude`, ...) as `safe`.
//! - Protected sources must carry `risk: protected`.
//! - Schema problems (unanchored rules, ambiguous anchors, relative paths,
//!   unknown env vars) fail closed.
//!
//! # Semantic (not literal) coverage checks
//!
//! The validation surface must be **at least as wide as the match surface**:
//! an attacker who can express a sweep with alternation (`{*,**}`), an
//! extended/verbatim prefix (`\\?\C:\*`, `\\.\C:\*`, `\\?\UNC\srv\share\*`)
//! or `..` segments (`%USERPROFILE%\..\..\..`) must not sail past a literal
//! string check. Every anchor therefore goes through one shared lexical
//! pipeline before it is judged:
//!
//! ```text
//! expand (%VAR% / ${VAR})  →  strip verbatim prefixes (\\?\UNC\, \\?\, \.\)
//!   →  lexical `..` resolution  →  semantic sweep probe
//! ```
//!
//! Lexical dot-resolution reuses `crate::safety::canonical` (the single
//! semantic source the safety layer relies on — no second normaliser is
//! invented here; the verbatim-prefix stripping is a thin pre-adaptor for
//! forms canonical itself does not parse, kept next to its one caller).
//! Sweep coverage is decided by compiling the anchor with globset and probing
//! it against representative sample sub-paths of each protected root, so
//! `{*,**}`-style alternation cannot bypass literal `first segment` checks.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use globset::GlobMatcher;

use crate::rules::matcher::{
    compile_glob, expand_env, first_meta_offset, normalize_slashes, rel_under_root,
};
use crate::rules::priority::RuleSource;
use crate::rules::schema::{RuleDoc, RuleFile};
use crate::safety::canonical;
use crate::RiskLevel;

/// Built-in permanent protected roots, expressed as env templates
/// (SPEC §18). Their expanded values are never allowed as rule targets.
pub const PROTECTED_ROOT_ENV_VARS: &[&str] = &[
    "%USERPROFILE%",
    "%SYSTEMROOT%",
    "%PROGRAMFILES%",
    "%PROGRAMFILES(X86)%",
    "%PROGRAMDATA%",
];

/// Known AI agent root directory names under `%USERPROFILE%` (SPEC §9).
/// Whole-root `safe` classification is forbidden. Extend here as new agents
/// are supported (Phase 9).
pub const AGENT_ROOT_DIR_NAMES: &[&str] = &[
    ".codex",
    ".claude",
    ".opencode",
    ".cursor",
    ".windsurf",
    ".cline",
    ".roo",
    ".codeium",
];

/// Probe-name generation for the semantic sweep samples (F11 fix).
///
/// The old implementation probed with two hard-coded constants
/// (`devresidue__probe__a`/`b`). An attacker who can author a rules file could
/// therefore construct a pattern that excludes exactly those names while still
/// sweeping real content:
///
/// - `%USERPROFILE%/[!d]*` (everything except `d...`) missed the probe because
///   it happened to start with `d`;
/// - `%USERPROFILE%/**` + `exclude: [devresidue__probe__a, ...]` removed the
///   probes from the match surface.
///
/// The fix makes the probe names **unpredictable per validation session**
/// (see [`default_probe_seed`]) and draws several names from different
/// first-character zones so a single character class (`[!d]`, `[a-z]`, ...)
/// cannot dodge all of them. The primary closure for such classes is the
/// conservative symbolic layer in [`covers_root`]; the randomised samples are
/// the second line of defence (they also feed the include/exclude
/// narrowing/emptying judgement, closing the exclude-evasion path).
const PROBE_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

/// First-character "zones". Step [`PROBE_LEADER_STEP`] (coprime with the
/// length) makes every batch of probes start with characters from different
/// zones (lower case, upper case, digits, underscore).
const PROBE_LEADERS: &[u8] = b"aeikzBMZ40_";
/// Step between successive probe leaders; coprime with [`PROBE_LEADERS`]'s
/// length so a batch of [`DEFAULT_PROBE_COUNT`] probes never repeats a zone.
const PROBE_LEADER_STEP: usize = 3;
/// How many distinct probe names each sweep judgement samples.
const DEFAULT_PROBE_COUNT: usize = 4;

/// Seeds the probe generator for one validation pass.
///
/// Unpredictable to a rule author: wall-clock nanoseconds XORed with a
/// process-wide atomic counter (then rotated). No `rand` dependency — this is
/// enough entropy to stop an attacker who writes a rules file from knowing the
/// probe names of the *next* validation session. Tests inject a fixed seed via
/// [`validate_rule_file_with_seed`] for determinism.
fn default_probe_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let tick = COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    nanos ^ tick.rotate_left(17)
}

/// SplitMix64 — a tiny deterministic PRNG (no third-party dependency).
struct ProbeGen {
    state: u64,
}

impl ProbeGen {
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9e37_79b9_7f4a_7c15),
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Produces one probe name: a zone-distinct leader plus ≥15 random body
    /// characters (total ≥16) from the alphanumeric + underscore alphabet.
    fn next_name(&mut self, leader: u8) -> String {
        let mut name = String::with_capacity(16);
        name.push(leader as char);
        for _ in 1..16 {
            let idx = (self.next_u64() as usize) % PROBE_ALPHABET.len();
            name.push(PROBE_ALPHABET[idx] as char);
        }
        name
    }
}

/// Generates `count` distinct-zone probe names for a validation session.
fn probe_names(seed: u64, count: usize) -> Vec<String> {
    let mut gen = ProbeGen::new(seed);
    let base = (gen.next_u64() as usize) % PROBE_LEADERS.len();
    (0..count)
        .map(|i| {
            let leader_idx = (base + i * PROBE_LEADER_STEP) % PROBE_LEADERS.len();
            gen.next_name(PROBE_LEADERS[leader_idx])
        })
        .collect()
}

/// Severity of a validation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Load-blocking defect. Rules flagged `Error` are not loaded.
    Error,
    /// Advisory finding; does not block loading.
    Warning,
}

/// One structured validation finding (file + rule + field + message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleIssue {
    /// Rule file the finding refers to (filled in by the loader).
    pub file: Option<PathBuf>,
    /// Rule slug, when the finding is rule-specific.
    pub rule_id: Option<String>,
    /// Schema field involved, when applicable (e.g. `match.glob`).
    pub field: Option<String>,
    /// Severity.
    pub severity: Severity,
    /// Human-readable explanation.
    pub message: String,
}

impl RuleIssue {
    /// Creates a rule-level error.
    #[must_use]
    pub fn error(rule_id: &str, field: &str, message: impl Into<String>) -> Self {
        Self {
            file: None,
            rule_id: Some(rule_id.to_string()),
            field: Some(field.to_string()),
            severity: Severity::Error,
            message: message.into(),
        }
    }

    /// Creates a rule-level warning.
    #[must_use]
    pub fn warning(rule_id: &str, field: &str, message: impl Into<String>) -> Self {
        Self {
            file: None,
            rule_id: Some(rule_id.to_string()),
            field: Some(field.to_string()),
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    /// True when this issue blocks loading.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self.severity, Severity::Error)
    }
}

/// Validates every rule in a parsed file against the semantic checks.
///
/// Probe names used by the sweep judgements are randomised per call (see
/// [`default_probe_seed`]); tests that need determinism should call
/// [`validate_rule_file_with_seed`] instead.
pub fn validate_rule_file(
    file: &RuleFile,
    expected_source: RuleSource,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<RuleIssue> {
    validate_rule_file_with_seed(file, expected_source, lookup, default_probe_seed())
}

/// Same as [`validate_rule_file`] but with an explicit probe seed, so tests
/// are fully reproducible.
pub(crate) fn validate_rule_file_with_seed(
    file: &RuleFile,
    expected_source: RuleSource,
    lookup: &dyn Fn(&str) -> Option<String>,
    probe_seed: u64,
) -> Vec<RuleIssue> {
    let probes = probe_names(probe_seed, DEFAULT_PROBE_COUNT);
    let mut issues = Vec::new();
    for rule in &file.rules {
        // 1. Anchor must exist and be unambiguous.
        if rule.match_spec.has_no_anchor() {
            issues.push(RuleIssue::error(
                &rule.id,
                "match",
                "rule must declare exactly one of match.exact or match.glob",
            ));
        }
        if rule.match_spec.exact.is_some() && rule.match_spec.glob.is_some() {
            issues.push(RuleIssue::error(
                &rule.id,
                "match",
                "match.exact and match.glob are mutually exclusive",
            ));
        }

        // 2. Source declaration must match the rules directory it lives in.
        if rule.source != expected_source {
            issues.push(RuleIssue::error(
                &rule.id,
                "source",
                format!(
                    "rule source `{source}` does not match the `{expected}` rules directory \
                     it is loaded from",
                    source = rule.source,
                    expected = expected_source
                ),
            ));
        }

        // 3. Protected sources must be protected risk (INV-002/INV-007).
        if matches!(
            rule.source,
            RuleSource::BuiltinProtected | RuleSource::UserProtected
        ) && rule.risk != RiskLevel::Protected
        {
            issues.push(RuleIssue::error(
                &rule.id,
                "risk",
                format!(
                    "rule from protected source `{}` must declare risk: protected (INV-002)",
                    rule.source
                ),
            ));
        }

        // 4. Expand + safety checks for the anchor pattern.
        let anchor = rule
            .match_spec
            .exact
            .as_deref()
            .or(rule.match_spec.glob.as_deref());
        let anchor_field = if rule.match_spec.exact.is_some() {
            "match.exact"
        } else {
            "match.glob"
        };
        let is_glob = rule.match_spec.glob.is_some();
        if let Some(pattern) = anchor {
            check_anchor(
                &rule.id,
                anchor_field,
                is_glob,
                pattern,
                rule,
                lookup,
                &probes,
                &mut issues,
            );
        }

        // 5. include/exclude with an exact anchor cannot refine anything.
        if rule.match_spec.exact.is_some() && (!rule.include.is_empty() || !rule.exclude.is_empty())
        {
            issues.push(RuleIssue::warning(
                &rule.id,
                "include",
                "include/exclude refine relative sub-paths under a glob anchor; \
                 they have no effect with match.exact",
            ));
        }

        // 6. Optional AiAdvisor provenance semantics (Phase A). Unknown YAML
        //    provenance fields and free-text suggestion ids are already
        //    rejected at the serde parse layer (RuleProvenance is
        //    deny_unknown_fields and suggestion_id is an AiSuggestionId); this
        //    layer rejects the semantic residue (provenance on a non-user
        //    rule, user_final_risk not matching rule risk, negative time).
        if let Some(provenance) = &rule.provenance {
            check_provenance(rule, provenance, &mut issues);
        }
    }
    issues
}

/// Validates the semantic content of an optional AI provenance block against
/// the whole rule it is attached to.
///
/// Structural guarantees already enforced by the types at the parse layer:
/// canonical profile/suggestion UUIDs, `origin: ai-advisor` (single-variant
/// enum) and a nonnegative `u64` scan generation. This function rejects the
/// residual semantic problems: provenance on a rule the user does not own, a
/// `user_final_risk` that disagrees with the rule's risk, and a negative
/// creation time.
fn check_provenance(
    rule: &RuleDoc,
    provenance: &crate::ai::RuleProvenance,
    issues: &mut Vec<RuleIssue>,
) {
    let rule_id = rule.id.as_str();

    // Provenance records a user's review decision, so it may only ride on a
    // rule the user owns. Built-in, community, and detection sources are
    // never AI-review products.
    if !matches!(rule.source, RuleSource::User | RuleSource::UserProtected) {
        issues.push(RuleIssue::error(
            rule_id,
            "provenance",
            "provenance is only permitted on user rules (source: user or user-protected); \
             built-in/community/detection rules cannot carry AI provenance",
        ));
    }

    // The recorded final risk must be exactly the risk the rule now assigns,
    // so the provenance cannot be made to disagree with the persisted rule.
    if provenance.user_final_risk != rule.risk {
        issues.push(RuleIssue::error(
            rule_id,
            "provenance.user_final_risk",
            format!(
                "provenance.user_final_risk `{:?}` must equal the rule risk `{:?}`",
                provenance.user_final_risk, rule.risk
            ),
        ));
    }

    if provenance.created_at_epoch_secs < 0 {
        issues.push(RuleIssue::error(
            rule_id,
            "provenance.created_at_epoch_secs",
            "provenance.created_at_epoch_secs must be a nonnegative epoch-seconds timestamp",
        ));
    }
}

/// Checks one anchor pattern: expansion, lexical normalisation, INV-006 root
/// coverage and the SPEC §9 whole-agent-root-safe guard.
#[allow(clippy::too_many_arguments)]
fn check_anchor(
    rule_id: &str,
    field: &str,
    is_glob: bool,
    pattern: &str,
    rule: &crate::rules::schema::RuleDoc,
    lookup: &dyn Fn(&str) -> Option<String>,
    probes: &[String],
    issues: &mut Vec<RuleIssue>,
) {
    // Env expansion — unknown variables are fail-closed errors.
    let expanded = match expand_env(pattern, lookup) {
        Ok(value) => value,
        Err(err) => {
            issues.push(RuleIssue::error(
                rule_id,
                field,
                format!("cannot expand rule path: {err}"),
            ));
            return;
        }
    };

    // Absolute check happens on the stripped text (canonical understands the
    // extended `\\?\` / UNC forms natively; `\\.\` is pre-adapted).
    let abs_input = strip_verbatim_prefixes(&expanded);
    if !canonical::is_absolute(abs_input.as_ref()) {
        issues.push(RuleIssue::error(
            rule_id,
            field,
            format!(
                "rule path must be an absolute Windows path after expansion \
                 (e.g. `%USERPROFILE%/...` or `C:/...`), got `{expanded}`"
            ),
        ));
        return;
    }

    // Lexical pipeline: strip verbatim prefixes, resolve `..` (never above the
    // anchor), collapse separators. Output is `/`-normalised text.
    let norm = lexical_normalize(&expanded);

    // A bare drive root or UNC share root as the whole target.
    if is_drive_root(&norm) {
        issues.push(RuleIssue::error(
            rule_id,
            field,
            format!("rule targets a drive root `{expanded}` — protected (INV-006)"),
        ));
        return;
    }
    if is_share_root(&norm) {
        issues.push(RuleIssue::error(
            rule_id,
            field,
            format!("rule targets a UNC share root `{expanded}` — protected (INV-006)"),
        ));
        return;
    }

    // Glob anchors whose literal prefix is a drive / UNC share root sweep the
    // whole volume/share (`C:\*`, `\\srv\share\*`, `\\.\C:\*`, `\\?\C:\*`,
    // `C:\{*,**}` — the prefix survives whatever alternation follows it).
    if is_glob {
        if let Some(mi) = first_meta_offset(&norm) {
            let prefix = norm[..mi].trim_end_matches('/');
            if is_drive_root(prefix) {
                issues.push(RuleIssue::error(
                    rule_id,
                    field,
                    format!(
                        "rule glob is anchored at a drive root and sweeps its contents \
                         (INV-006): `{expanded}`"
                    ),
                ));
                return;
            }
            if is_share_root(prefix) {
                issues.push(RuleIssue::error(
                    rule_id,
                    field,
                    format!(
                        "rule glob is anchored at a UNC share root and sweeps its contents \
                         (INV-006): `{expanded}`"
                    ),
                ));
                return;
            }
        }
    }

    // Compile the anchor + include/exclude matchers once for semantic probing.
    let (anchor_matcher, lit_root): (Option<GlobMatcher>, Option<String>) = if is_glob {
        match compile_glob(&norm) {
            Ok(matcher) => {
                let prefix = norm[..first_meta_offset(&norm).unwrap_or(norm.len())]
                    .trim_end_matches('/')
                    .to_string();
                (Some(matcher), Some(prefix))
            }
            Err(err) => {
                issues.push(RuleIssue::error(
                    rule_id,
                    field,
                    format!("invalid glob pattern `{pattern}`: {err}"),
                ));
                return;
            }
        }
    } else {
        (None, None)
    };
    let include_matchers = match compile_matchers(&rule.include) {
        Ok(m) => m,
        Err(err) => {
            issues.push(RuleIssue::error(rule_id, "include", err));
            return;
        }
    };
    let exclude_matchers = match compile_matchers(&rule.exclude) {
        Ok(m) => m,
        Err(err) => {
            issues.push(RuleIssue::error(rule_id, "exclude", err));
            return;
        }
    };

    // INV-006: semantic sweep check against every protected root.
    for template in PROTECTED_ROOT_ENV_VARS {
        let Some(root_value) = expand_env(template, lookup).ok() else {
            continue;
        };
        let root_norm = lexical_normalize(&root_value);
        if covers_root(
            &norm,
            is_glob,
            anchor_matcher.as_ref(),
            lit_root.as_deref(),
            &include_matchers,
            &exclude_matchers,
            &root_norm,
            probes,
        ) {
            issues.push(RuleIssue::error(
                rule_id,
                field,
                format!(
                    "rule matches protected root `{template}` or wholesale-sweeps its \
                     contents (INV-006): `{expanded}`"
                ),
            ));
            return;
        }
    }

    // SPEC §9: whole agent root classified `safe` by a detection-style rule.
    if rule.risk == RiskLevel::Safe && rule.source != RuleSource::BuiltinProtected {
        if let Some(userprofile) = lookup("USERPROFILE") {
            let home_root = lexical_normalize(&userprofile);
            for agent in AGENT_ROOT_DIR_NAMES {
                let agent_root = format!("{home_root}/{agent}");
                if covers_root(
                    &norm,
                    is_glob,
                    anchor_matcher.as_ref(),
                    lit_root.as_deref(),
                    &include_matchers,
                    &exclude_matchers,
                    &agent_root,
                    probes,
                ) {
                    issues.push(RuleIssue::error(
                        rule_id,
                        field,
                        format!(
                            "rule classifies the whole agent root `%USERPROFILE%\\{agent}` as \
                             safe (SPEC §9): agent roots must be split into fine-grained \
                             cache/log/temp/session items"
                        ),
                    ));
                    return;
                }
            }
        }
    }
}

/// True when a rule's *effective* match surface covers `root_norm` itself or
/// sweeps everything below it.
///
/// Two independent layers (F11 fix):
///
/// 1. **Conservative symbolic layer.** After stripping `root_norm` off the
///    anchor pattern, the *first remaining path segment* is inspected. If that
///    segment contains any glob metacharacter (`* ? [ ] { }`) the anchor
///    matches arbitrary first-level children of the root — declared a sweep
///    outright. This closes character-class evasions such as `[!d]*` and
///    `[a-z]*` for which no finite set of sample names can ever be
///    representative. The only exemptions are:
///    - an `include` list that *genuinely narrows* the match to a concrete
///      sub-set (probed against random names — an include that matches every
///      random name is an "anything" pattern and grants no exemption);
///    - an `exclude` list that excludes *every first-level child* (matches
///      every random name). Such a rule can still hit the root itself through
///      a zero-width `**`; that case is reported honestly by the sample layer.
/// 2. **Randomised sample layer.** The compiled matcher is probed against the
///    root itself plus a batch of unpredictable per-session names (multiple
///    zones, ≥4 samples) with include/exclude refinement simulated exactly like
///    `rule_matches`. Because the names are random the attacker cannot write an
///    `exclude` that targets them; this closes the exclude-evasion path and
///    catches `{*,**}`-style alternation sweeps.
///
/// This mirrors SPEC §9: a first-level wildcard under an agent root must be
/// narrowed by an `include`.
#[allow(clippy::too_many_arguments)]
fn covers_root(
    norm_pattern: &str,
    is_glob: bool,
    anchor_matcher: Option<&GlobMatcher>,
    lit_root: Option<&str>,
    include_matchers: &[GlobMatcher],
    exclude_matchers: &[GlobMatcher],
    root_norm: &str,
    probes: &[String],
) -> bool {
    if !is_glob {
        return canonical::normalized_eq_path(
            PathBuf::from(norm_pattern),
            PathBuf::from(root_norm),
        );
    }
    let matcher = anchor_matcher.expect("glob rules carry a compiled anchor");
    let lit_root = lit_root.expect("glob rules carry a literal root");

    // ---- 1. Conservative symbolic layer --------------------------------
    if let Some(rel) = rel_under_root(norm_pattern, root_norm) {
        if rel.is_empty() {
            // The pattern targets the root itself.
            return true;
        }
        let first_seg = rel.split('/').next().unwrap_or("");
        if segment_has_glob_meta(first_seg) {
            let include_narrows =
                !include_matchers.is_empty() && include_is_genuine_narrow(include_matchers, probes);
            let exclude_empties =
                !include_narrows && exclude_is_fully_excluding(exclude_matchers, probes);
            if !include_narrows && !exclude_empties {
                return true;
            }
        }
    }

    // ---- 2. Randomised sample layer ------------------------------------
    if candidate_hits(
        matcher,
        lit_root,
        include_matchers,
        exclude_matchers,
        root_norm,
    ) {
        return true;
    }
    for (i, probe) in probes.iter().enumerate() {
        let child = format!("{root_norm}/{probe}");
        if candidate_hits(
            matcher,
            lit_root,
            include_matchers,
            exclude_matchers,
            &child,
        ) {
            return true;
        }
        let next = &probes[(i + 1) % probes.len()];
        let grandchild = format!("{root_norm}/{probe}/{next}");
        if candidate_hits(
            matcher,
            lit_root,
            include_matchers,
            exclude_matchers,
            &grandchild,
        ) {
            return true;
        }
    }
    false
}

/// True when the first path segment of an anchor (the segment directly below a
/// protected root) carries any glob metacharacter, i.e. it matches arbitrary
/// first-level children rather than one concrete name.
fn segment_has_glob_meta(segment: &str) -> bool {
    segment
        .bytes()
        .any(|b| matches!(b, b'*' | b'?' | b'[' | b']' | b'{' | b'}'))
}

/// True when a non-empty `include` list genuinely restricts the match surface.
///
/// An include pattern that matches *every* random probe name is an "anything"
/// pattern (e.g. `*`, `{*,**}`, `[a-z]*` if all probes are lower-case — which
/// the multi-zone probe batch prevents) and grants no narrowing exemption.
fn include_is_genuine_narrow(include_matchers: &[GlobMatcher], probes: &[String]) -> bool {
    debug_assert!(!include_matchers.is_empty());
    !include_matchers
        .iter()
        .any(|m| probes.iter().all(|p| m.is_match(p.as_str())))
}

/// True when an `exclude` list excludes **every first-level child** of the
/// root — at least one exclude pattern matches every random probe name.
///
/// This exempts a first-level-wildcard anchor from the conservative symbolic
/// layer *only* (a rule whose children never match is harmless). It does not
/// cover the root itself: `exclude` refines sub-paths, so an anchor whose
/// glob also matches the root (e.g. `foo/**` zero-width `**`) still hits the
/// root and is reported as a sweep by the sample layer.
fn exclude_is_fully_excluding(exclude_matchers: &[GlobMatcher], probes: &[String]) -> bool {
    !exclude_matchers.is_empty()
        && exclude_matchers
            .iter()
            .any(|m| probes.iter().all(|p| m.is_match(p.as_str())))
}

/// Simulates `rule_matches` for a glob rule against a `/`-normalised candidate
/// string (anchor glob + include/exclude refinement on the relative path).
fn candidate_hits(
    anchor: &GlobMatcher,
    lit_root: &str,
    include_matchers: &[GlobMatcher],
    exclude_matchers: &[GlobMatcher],
    candidate: &str,
) -> bool {
    if !anchor.is_match(candidate) {
        return false;
    }
    // Same shape as `matcher::rule_matches`: a candidate outside the literal
    // root cannot be refined, so it is treated as a miss.
    let Some(rel) = rel_under_root(candidate, lit_root) else {
        return false;
    };
    if rel.is_empty() {
        return include_matchers.is_empty();
    }
    if !include_matchers.is_empty() && !include_matchers.iter().any(|m| m.is_match(rel)) {
        return false;
    }
    if exclude_matchers.iter().any(|m| m.is_match(rel)) {
        return false;
    }
    true
}

fn compile_matchers(patterns: &[String]) -> Result<Vec<GlobMatcher>, String> {
    patterns
        .iter()
        .map(|p| compile_glob(p).map_err(|e| format!("invalid include/exclude glob `{p}`: {e}")))
        .collect()
}

/// Strips the extended/verbatim Windows prefixes `\\?\UNC\`, `\\?\` and `\\.\`
/// so the remainder is an ordinary drive / UNC path canonical can parse.
/// `\\?\UNC\srv\share\...` becomes `\\srv\share\...`; `\\?\C:\...` and
/// `\\.\C:\...` become `C:\...`.
fn strip_verbatim_prefixes(text: &str) -> Cow<'_, str> {
    if text.len() >= 8 && text[..8].eq_ignore_ascii_case("\\\\?\\UNC\\") {
        return Cow::Owned(format!("\\\\{}", &text[8..]));
    }
    if text.len() >= 4 && text.starts_with("\\\\?\\") {
        return Cow::Borrowed(&text[4..]);
    }
    if text.len() >= 4 && text.starts_with("\\\\.\\") {
        return Cow::Borrowed(&text[4..]);
    }
    Cow::Borrowed(text)
}

/// Shared lexical pipeline: strip verbatim prefixes, resolve `.`/`..` via
/// `canonical::normalize` (never above the drive/UNC/share anchor), then emit
/// `/`-normalised, trailing-slash-free text.
fn lexical_normalize(text: &str) -> String {
    let stripped = strip_verbatim_prefixes(text);
    let canon = canonical::normalize(stripped.as_ref());
    let text = normalize_slashes(&canon.to_string_lossy());
    text.trim_end_matches('/').to_string()
}

/// True when `text` (slash-normalised, trailing slash removed) is a bare drive
/// root such as `C:`.
fn is_drive_root(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// True when `text` (slash-normalised) is a UNC share root such as
/// `//srv/share` (server + share, nothing else).
fn is_share_root(text: &str) -> bool {
    let rest = text.strip_prefix("//").unwrap_or(text);
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    segs.len() == 2 && text.starts_with("//")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiProfileId, AiSuggestionId, RuleOrigin, RuleProvenance};
    use crate::rules::schema::{MatchSpec, RuleDoc};
    use crate::{ResidueCategory, RiskLevel};
    use std::collections::BTreeMap;

    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_ascii_uppercase(), (*v).to_string()))
            .collect();
        move |name: &str| map.get(&name.to_ascii_uppercase()).cloned()
    }

    fn doc(id: &str, risk: RiskLevel, source: RuleSource, spec: MatchSpec) -> RuleDoc {
        RuleDoc {
            id: id.into(),
            description: "test rule".into(),
            product: None,
            category: ResidueCategory::Temporary,
            risk,
            source,
            match_spec: spec,
            include: Vec::new(),
            exclude: Vec::new(),
            provenance: None,
        }
    }

    fn exact(p: &str) -> MatchSpec {
        MatchSpec {
            exact: Some(p.into()),
            glob: None,
            parent_marker: None,
            exists: false,
        }
    }

    fn glob(p: &str) -> MatchSpec {
        MatchSpec {
            exact: None,
            glob: Some(p.into()),
            parent_marker: None,
            exists: false,
        }
    }

    fn standard_env() -> impl Fn(&str) -> Option<String> {
        fake_env(&[
            ("USERPROFILE", r"C:\Users\demo"),
            ("SYSTEMROOT", r"C:\Windows"),
            ("PROGRAMFILES", r"C:\Program Files"),
            ("PROGRAMFILES(X86)", r"C:\Program Files (x86)"),
            ("PROGRAMDATA", r"C:\ProgramData"),
        ])
    }

    /// Fixed seed used by every validator test so sweep probing is fully
    /// deterministic (same inputs → same verdicts on every run).
    const TEST_PROBE_SEED: u64 = 0xD3AD_BEEF_5EED_0001;

    fn validate_one(rule: RuleDoc) -> Vec<RuleIssue> {
        validate_rule_file_with_seed(
            &RuleFile { rules: vec![rule] },
            RuleSource::BuiltinDetection,
            &standard_env(),
            TEST_PROBE_SEED,
        )
    }

    // ---- oracle regression: {*,**} alternation sweep (F1) ----------------

    #[test]
    fn rejects_brace_alternation_sweeps_of_home_and_drive_roots() {
        for pattern in [
            "%USERPROFILE%/{*,**}",
            "%SYSTEMROOT%/{*,**}",
            r"C:\{*,**}",
            r"D:\{*,**}",
            "%PROGRAMDATA%/{*,**}",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues
                    .iter()
                    .any(|i| i.is_error() && i.message.contains("INV-006")),
                "pattern {pattern} must be rejected, got {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_brace_alternation_sweep_of_agent_root_as_safe() {
        for pattern in [
            "%USERPROFILE%/.claude/{*,**}",
            "%USERPROFILE%/.codex/{*,**}",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues.iter().any(|i| i.message.contains("SPEC §9")),
                "pattern {pattern} must trip the agent-root guard, got {issues:?}"
            );
        }
    }

    // ---- oracle regression: verbatim / UNC prefixes (F2) ------------------

    #[test]
    fn rejects_verbatim_and_device_prefix_volume_sweeps() {
        for pattern in [
            r"\\?\C:\*",
            r"\\.\C:\*",
            r"\\?\C:\**",
            r"\\?\UNC\srv\share\*",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues
                    .iter()
                    .any(|i| i.is_error() && i.message.contains("INV-006")),
                "pattern {pattern} must be rejected, got {issues:?}"
            );
        }
    }

    #[test]
    fn verbatim_equivalents_of_scoped_rules_stay_legal() {
        // Same volume, fine-grained path: must not be rejected.
        let issues = validate_one(doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            exact(r"\\?\C:\Users\demo\.claude\shell-snapshots"),
        ));
        assert!(!issues.iter().any(|i| i.is_error()), "got {issues:?}");
    }

    // ---- oracle regression: `..` lexical resolution (F3) ------------------

    #[test]
    fn rejects_dotdot_that_resolves_to_a_root() {
        // %USERPROFILE%\..\..\..\.. climbs to C:\ (clamped by lexical rules).
        for pattern in [
            r"%USERPROFILE%\..\..\..\..",
            r"%USERPROFILE%\x\..\..\..\..\..",
            r"%USERPROFILE%\sub\..\..\..\..\..\..",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                exact(pattern),
            ));
            assert!(
                issues.iter().any(|i| i.is_error()),
                "exact {pattern} must be rejected (resolves to drive root), got {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_dotdot_glob_that_sweeps_home() {
        // C:\Users\demo\..\** resolves to C:\Users\** which covers the home
        // profile (protected root) — old literal logic missed this.
        for pattern in [
            r"%USERPROFILE%\..\**",
            r"%USERPROFILE%\sub\..\..\**",
            r"%USERPROFILE%\x\..\..\..\..\**",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues.iter().any(|i| i.is_error()),
                "glob {pattern} must be rejected, got {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_dotdot_injected_through_environment_values() {
        // The env *value* itself contains `..` that climbs above the anchor.
        let env = fake_env(&[
            (
                "USERPROFILE",
                r"C:\Users\demo\sub\..\..\..\..\..\Users\demo",
            ),
            ("SYSTEMROOT", r"C:\Windows"),
            ("PROGRAMFILES", r"C:\Program Files"),
            ("PROGRAMFILES(X86)", r"C:\Program Files (x86)"),
            ("PROGRAMDATA", r"C:\ProgramData"),
        ]);
        let file = RuleFile {
            rules: vec![doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob("%USERPROFILE%/{*,**}"),
            )],
        };
        let issues = validate_rule_file_with_seed(
            &file,
            RuleSource::BuiltinDetection,
            &env,
            TEST_PROBE_SEED,
        );
        // Clamped to the drive root, so the sweep targets a drive root.
        assert!(
            issues
                .iter()
                .any(|i| i.is_error() && i.message.contains("INV-006")),
            "injected .. must be rejected, got {issues:?}"
        );
    }

    // ---- oracle regression: fake include narrowing (F4 test case) ---------

    #[test]
    fn rejects_fake_include_narrowing_of_agent_root_sweep() {
        // include: [{*,**}] looks like it narrows to a concrete sub-tree but
        // in fact still matches arbitrary children — the agent-root guard must
        // not be fooled.
        let mut rule = doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/.claude/*"),
        );
        rule.include = vec!["{*,**}".into()];
        let issues = validate_one(rule);
        assert!(
            issues.iter().any(|i| i.message.contains("SPEC §9")),
            "fake include narrowing must be rejected, got {issues:?}"
        );
    }

    #[test]
    fn rejects_literal_star_include_narrowing_too() {
        let mut rule = doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/.claude/*"),
        );
        rule.include = vec!["*".into()];
        let issues = validate_one(rule);
        assert!(
            issues.iter().any(|i| i.message.contains("SPEC §9")),
            "literal `*` include must not narrow, got {issues:?}"
        );
    }

    // ---- oracle regression: legit rules stay legal ------------------------

    #[test]
    fn allows_scoped_rules_below_roots() {
        for (spec, pattern) in [
            (exact("%USERPROFILE%/.ssh"), "%USERPROFILE%/.ssh"),
            (
                glob("%USERPROFILE%/.claude/projects/*"),
                "%USERPROFILE%/.claude/projects/*",
            ),
            (glob(r"C:\Windows\Temp\*\*.log"), r"C:\Windows\Temp\*\*.log"),
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                spec,
            ));
            assert!(
                !issues.iter().any(|i| i.is_error()),
                "{pattern} must pass, got {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_first_level_wildcard_below_agent_root_without_include() {
        // F11-B adjustment: `.claude/*/node_modules` wildcard-matches the whole
        // first level under an agent root with no include narrowing. The
        // conservative symbolic layer treats any first-level wildcard segment
        // as a sweep (SPEC §9: agent roots must be split into fine-grained
        // items) — previously this pattern slipped through.
        let issues = validate_one(doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/.claude/*/node_modules"),
        ));
        assert!(
            issues.iter().any(|i| i.is_error()),
            "first-level wildcard under an agent root without include must be \
             rejected (SPEC §9), got {issues:?}"
        );
    }

    #[test]
    fn allows_genuine_include_narrowing() {
        let mut rule = doc(
            "ok2",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/.claude/*"),
        );
        rule.include = vec!["shell-snapshots/**".into()];
        let issues = validate_one(rule);
        assert!(!issues.iter().any(|i| i.is_error()), "got {issues:?}");
    }

    #[test]
    fn allows_fine_grained_agent_rules() {
        // Specific cache/log/session sub-paths are the intended usage.
        let issues = validate_one(doc(
            "ok",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            exact("%USERPROFILE%/.claude/shell-snapshots"),
        ));
        assert!(!issues.iter().any(|i| i.is_error()), "got {issues:?}");
    }

    // ---- existing behaviour kept -----------------------------------------

    #[test]
    fn rejects_exact_root_and_drive_root_targets() {
        for pattern in [
            "%USERPROFILE%",
            r"%USERPROFILE%\.ssh",
            r"C:\",
            r"D:\",
            r"\\srv\share",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                exact(pattern),
            ));
            let is_root = matches!(pattern, "%USERPROFILE%" | r"C:\" | r"D:\" | r"\\srv\share");
            assert_eq!(
                issues.iter().any(|i| i.is_error()),
                is_root,
                "pattern {pattern}"
            );
        }
    }

    #[test]
    fn rejects_wholesale_glob_sweeps() {
        for pattern in [
            r"C:\*",
            r"C:\**",
            r"D:\*",
            "%USERPROFILE%/*",
            "%USERPROFILE%/**",
            "%SYSTEMROOT%/*",
            "%PROGRAMDATA%/**",
            r"C:\*",
            r"\\srv\share\*",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues
                    .iter()
                    .any(|i| i.is_error() && i.message.contains("INV-006")),
                "glob {pattern} must be rejected, got {issues:?}"
            );
        }
    }

    #[test]
    fn builtin_protected_requires_protected_risk() {
        let rule = doc(
            "bad",
            RiskLevel::Safe,
            RuleSource::BuiltinProtected,
            exact("%USERPROFILE%/.ssh"),
        );
        let issues = validate_one(rule);
        assert!(issues.iter().any(|i| i.field == Some("risk".into())));
    }

    #[test]
    fn rejects_whole_agent_root_as_safe() {
        for pattern in [
            "%USERPROFILE%/.claude",
            "%USERPROFILE%/.claude/*",
            "%USERPROFILE%/.codex/**",
            "%USERPROFILE%/.windsurf",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob_or_exact(pattern),
            ));
            assert!(
                issues.iter().any(|i| i.message.contains("SPEC §9")),
                "pattern {pattern} must trip the agent-root guard, got {issues:?}"
            );
        }
    }

    fn glob_or_exact(pattern: &str) -> MatchSpec {
        if pattern.contains(['*', '?', '{']) {
            glob(pattern)
        } else {
            exact(pattern)
        }
    }

    #[test]
    fn rejects_unknown_env_vars_and_relative_paths() {
        let issues = validate_one(doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            exact("%NOT_SET%/x"),
        ));
        assert!(issues.iter().any(|i| i.message.contains("cannot expand")));

        let issues = validate_one(doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            exact("relative/path"),
        ));
        assert!(issues
            .iter()
            .any(|i| i.message.contains("absolute Windows path")));
    }

    #[test]
    fn source_mismatch_and_bad_anchors_are_reported() {
        let mut rule = doc(
            "t",
            RiskLevel::Protected,
            RuleSource::BuiltinDetection,
            exact("%USERPROFILE%/.ssh"),
        );
        rule.source = RuleSource::BuiltinProtected;
        let issues = validate_one(rule);
        assert!(issues.iter().any(|i| i.field == Some("source".into())));
    }

    // ---- lexical pipeline unit behaviour ----------------------------------

    #[test]
    fn lexical_normalize_strips_prefixes_and_resolves_dots() {
        assert_eq!(lexical_normalize(r"C:\Users\demo"), "C:/Users/demo");
        assert_eq!(lexical_normalize(r"C:\Users\demo\..\.."), "C:");
        assert_eq!(
            lexical_normalize(r"\\?\C:\Users\demo\.ssh"),
            "C:/Users/demo/.ssh"
        );
        assert_eq!(lexical_normalize(r"\\.\C:\Windows\..\x"), "C:/x");
        assert_eq!(
            lexical_normalize(r"\\?\UNC\srv\share\a\..\b"),
            "//srv/share/b"
        );
        assert_eq!(lexical_normalize(r"\\srv\share"), "//srv/share");
        // `C:\Users\..\demo` pops `Users` -> `C:\demo`.
        assert_eq!(lexical_normalize(r"C:\Users\..\demo"), "C:/demo");
    }

    #[test]
    fn is_share_root_detection() {
        assert!(is_share_root("//srv/share"));
        assert!(is_share_root("//srv/share/"));
        assert!(!is_share_root("//srv/share/a"));
        assert!(!is_share_root("C:/Users"));
    }

    // ---- F11 regression: character-class & exclude sweep evasions ----------

    #[test]
    fn rejects_character_class_sweeps_of_home_root() {
        // F11-1: globset `[!d]*` matches every first-level child except those
        // starting with `d` — with the old fixed `d...` probe name this swept
        // the whole HOME profile while the probe samples missed. The
        // conservative symbolic layer now rejects any first-level wildcard
        // segment outright.
        for pattern in [
            "%USERPROFILE%/[!d]*",
            "%USERPROFILE%/[!a-z]*",
            "%USERPROFILE%/[a-z]*",
            "%USERPROFILE%/[!d]x*",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues
                    .iter()
                    .any(|i| i.is_error() && i.message.contains("INV-006")),
                "pattern {pattern} must be rejected (INV-006), got {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_character_class_sweep_of_agent_root_as_safe() {
        for pattern in ["%USERPROFILE%/.claude/[!d]*", "%USERPROFILE%/.codex/[a-z]*"] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues.iter().any(|i| i.message.contains("SPEC §9")),
                "pattern {pattern} must trip the agent-root guard, got {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_exclude_targeting_known_probe_names() {
        // F11-2: anchor `**` plus an exclude that names the *old* fixed probe
        // constants. The symbolic layer rejects the `**` first segment because
        // this exclude does not empty the rule (random per-session probes are
        // not affected by literal names the attacker hard-coded).
        let mut rule = doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/**"),
        );
        rule.exclude = vec![
            "devresidue__probe__a".into(),
            "devresidue__probe__a/**".into(),
        ];
        let issues = validate_one(rule);
        assert!(
            issues
                .iter()
                .any(|i| i.is_error() && i.message.contains("INV-006")),
            "probe-named exclude must not hide a ** sweep, got {issues:?}"
        );
    }

    #[test]
    fn exclude_everything_empties_rule_and_is_allowed() {
        // F11-3: an exclude that matches *every* child name (`*`) empties the
        // rule for all first-level children — nothing below the root can
        // match, so a first-level-wildcard anchor is harmless. The symbolic
        // layer's random samples make this judgement (any name is excluded).
        let mut rule = doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/*"),
        );
        rule.exclude = vec!["*".into()];
        let issues = validate_one(rule);
        assert!(
            !issues.iter().any(|i| i.is_error()),
            "exclude [`*`] empties all child matches and must pass, got {issues:?}"
        );

        // Sanity: without the exclude the same anchor is rejected, proving the
        // allow above is not vacuous.
        let bare = doc(
            "t2",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/*"),
        );
        let issues = validate_one(bare);
        assert!(
            issues
                .iter()
                .any(|i| i.is_error() && i.message.contains("INV-006")),
            "bare first-level sweep must still be rejected, got {issues:?}"
        );
    }

    #[test]
    fn exclude_children_does_not_hide_a_root_matching_doublestar() {
        // globset's `foo/**` matches `foo` itself (zero-width `**`). A
        // sub-path exclude like `*` cannot remove that root-self match, so the
        // anchor still targets the whole protected root — INV-006 stands.
        // (Semantics note: `exclude` refines *children* of the root, never the
        // root itself.)
        let mut rule = doc(
            "t",
            RiskLevel::Safe,
            RuleSource::BuiltinDetection,
            glob("%USERPROFILE%/**"),
        );
        rule.exclude = vec!["*".into()];
        let issues = validate_one(rule);
        assert!(
            issues
                .iter()
                .any(|i| i.is_error() && i.message.contains("INV-006")),
            "`**` matches the root itself, so excluding children is not enough, got {issues:?}"
        );
    }

    #[test]
    fn rejects_alternation_sweeps_again() {
        // F11-4 (regression guard): both `{*,**}` orderings stay rejected.
        for pattern in [
            "%USERPROFILE%/{*,**}",
            "%USERPROFILE%/{**,*}",
            "%USERPROFILE%/.claude/{*,**}",
            r"C:\{*,**}",
        ] {
            let issues = validate_one(doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            ));
            assert!(
                issues
                    .iter()
                    .any(|i| i.is_error() && i.message.contains("INV-006"))
                    || issues.iter().any(|i| i.message.contains("SPEC §9")),
                "pattern {pattern} must be rejected, got {issues:?}"
            );
        }
    }

    // ---- F11: randomised probe machinery -----------------------------------

    #[test]
    fn probe_names_are_deterministic_per_seed_and_distinct() {
        // F11-6: a fixed seed always yields the same probes (reproducible
        // tests); different seeds yield different batches.
        let a = probe_names(0xABCD_0001, DEFAULT_PROBE_COUNT);
        let a2 = probe_names(0xABCD_0001, DEFAULT_PROBE_COUNT);
        let b = probe_names(0xABCD_0002, DEFAULT_PROBE_COUNT);
        assert_eq!(a, a2);
        assert_ne!(a, b);
        // All names are ≥16 chars, alphanumeric + underscore only.
        for name in &a {
            assert!(name.len() >= 16, "probe too short: {name}");
            assert!(
                name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'),
                "probe has a non [A-Za-z0-9_] char: {name}"
            );
        }
    }

    #[test]
    fn probe_batches_span_multiple_first_char_zones() {
        // The whole point of zone-distinct leaders: a single character class
        // like `[!d]*` or `[a-z]*` must not be able to dodge every probe.
        let zones = probe_names(TEST_PROBE_SEED, DEFAULT_PROBE_COUNT);
        let is_lower = |c: u8| c.is_ascii_lowercase();
        let is_upper = |c: u8| c.is_ascii_uppercase();
        let is_digit = |c: u8| c.is_ascii_digit();
        let firsts: Vec<u8> = zones.iter().map(|s| s.as_bytes()[0]).collect();
        // At least two different case/digit zones must be represented.
        let lower = firsts.iter().filter(|&&c| is_lower(c)).count();
        let upper = firsts.iter().filter(|&&c| is_upper(c)).count();
        let digits = firsts.iter().filter(|&&c| is_digit(c)).count();
        assert!(
            lower >= 1 && (upper >= 1 || digits >= 1),
            "expected multi-zone probe leaders, got {firsts:?}"
        );
        // All leaders distinct.
        let mut sorted = firsts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), firsts.len(), "probe leaders must be distinct");
    }

    #[test]
    fn seeded_validation_rejects_and_allows_deterministically() {
        // Same seed → same verdict, exercised through the public entry point
        // that loader tests and production use (random default there).
        let mk = |pattern: &str| {
            doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                glob(pattern),
            )
        };
        for seed in [1_u64, 2, 3, 4] {
            let a = validate_rule_file_with_seed(
                &RuleFile {
                    rules: vec![mk("%USERPROFILE%/{*,**}")],
                },
                RuleSource::BuiltinDetection,
                &standard_env(),
                seed,
            );
            let b = validate_rule_file_with_seed(
                &RuleFile {
                    rules: vec![mk("%USERPROFILE%/{*,**}")],
                },
                RuleSource::BuiltinDetection,
                &standard_env(),
                seed,
            );
            assert!(a.iter().any(|i| i.is_error()));
            assert_eq!(a, b, "seed {seed} must be deterministic");
        }
    }

    #[test]
    fn narrow_globs_never_hit_random_probes() {
        // Sanity for the sample layer: a concrete/narrow glob must not match
        // any random probe path, while a sweep glob hits the child sample.
        let probes = probe_names(TEST_PROBE_SEED, DEFAULT_PROBE_COUNT);
        let child = format!("C:/Users/demo/{}", probes[0]);
        let grandchild = format!("C:/Users/demo/{}/{}", probes[0], probes[1]);

        let narrow = compile_glob("C:/Users/demo/.claude/projects/*").expect("glob");
        assert!(!narrow.is_match(&child));
        assert!(!narrow.is_match(&grandchild));

        let sweep = compile_glob("C:/Users/demo/{*,**}").expect("glob");
        assert!(sweep.is_match(&child));
        let deep = compile_glob("C:/Users/demo/**").expect("glob");
        assert!(deep.is_match(&grandchild));
    }

    // ---- optional provenance semantic validation (AiAdvisor Phase A) -------
    //
    // Unknown provenance YAML fields and free-text suggestion ids are rejected
    // at the serde parse layer (RuleProvenance is deny_unknown_fields and
    // suggestion_id is an AiSuggestionId). This layer rejects the semantic
    // residue: provenance on a non-user rule, user_final_risk not matching the
    // rule risk, and a negative creation time.

    const PROVENANCE_CANON: &str = "018f7e21-9d15-7b17-a5fd-4f0f2bcadc72";
    const SUGGESTION_CANON: &str = "9f3c7d21-1a2b-4c5d-8e6f-0a1b2c3d4e5f";

    fn valid_provenance() -> RuleProvenance {
        RuleProvenance {
            origin: RuleOrigin::AiAdvisor,
            profile_id: AiProfileId::parse(PROVENANCE_CANON).unwrap(),
            scan_generation: 7,
            suggestion_id: AiSuggestionId::parse(SUGGESTION_CANON).unwrap(),
            user_final_risk: RiskLevel::Safe,
            created_at_epoch_secs: 1_760_000_000,
        }
    }

    fn with_provenance(mut rule: RuleDoc, provenance: RuleProvenance) -> RuleDoc {
        rule.provenance = Some(provenance);
        rule
    }

    fn provenance_error_messages(rule: RuleDoc) -> Vec<String> {
        let issues = validate_one(rule);
        issues
            .iter()
            .filter(|i| i.field.as_deref().is_some_and(|f| f.starts_with("provenance")))
            .map(|i| i.message.clone())
            .collect()
    }

    /// A valid *user* rule (source User) that provenance may legally attach to.
    fn provenance_rule() -> RuleDoc {
        doc(
            "t",
            RiskLevel::Safe,
            RuleSource::User,
            exact("C:/x"),
        )
    }

    #[test]
    fn valid_provenance_is_accepted_by_the_validator() {
        let errors = provenance_error_messages(with_provenance(provenance_rule(), valid_provenance()));
        assert!(errors.is_empty(), "unexpected provenance errors: {errors:?}");
    }

    #[test]
    fn provenance_rejected_on_non_user_source() {
        // Fix-round 1: provenance is only meaningful on a rule the user owns
        // (source User / UserProtected). A detection rule carrying provenance
        // must be rejected.
        let provenance = valid_provenance();
        let rule = with_provenance(
            doc(
                "t",
                RiskLevel::Safe,
                RuleSource::BuiltinDetection,
                exact("C:/x"),
            ),
            provenance,
        );
        let msgs = provenance_error_messages(rule);
        assert!(
            msgs.iter().any(|m| m.contains("source")),
            "provenance on a non-user rule must be rejected, got {msgs:?}"
        );
    }

    #[test]
    fn provenance_rejects_risk_mismatch() {
        // Fix-round 1: provenance.user_final_risk must equal the rule's risk.
        let provenance = valid_provenance(); // user_final_risk = Safe
        let rule = with_provenance(
            doc(
                "t",
                RiskLevel::Protected,
                RuleSource::User,
                exact("C:/x"),
            ),
            provenance,
        );
        let msgs = provenance_error_messages(rule);
        assert!(
            msgs.iter().any(|m| m.contains("user_final_risk")),
            "provenance.user_final_risk must equal rule risk, got {msgs:?}"
        );
    }

    #[test]
    fn provenance_rejects_negative_creation_time() {
        let mut provenance = valid_provenance();
        provenance.created_at_epoch_secs = -1;
        let errors = provenance_error_messages(with_provenance(provenance_rule(), provenance));
        assert!(
            errors
                .iter()
                .any(|m| m.contains("created_at_epoch_secs")),
            "negative created_at_epoch_secs must be rejected, got {errors:?}"
        );
    }
}
