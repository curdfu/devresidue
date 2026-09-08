//! Unknown developer data provider (SPEC §25, Phase 13).
//!
//! Scans the three SPEC §25 zones at **bounded depth** (one candidate level,
//! no full-disk recursion):
//!
//! ```text
//! %USERPROFILE%\.*      hidden one-level directories only
//! %LOCALAPPDATA%\*      first-level sub-directories
//! %APPDATA%\*           first-level sub-directories
//! ```
//!
//! Only directories that *look like developer tool data* are reported:
//!
//! 1. **name heuristic** — a dot-prefixed directory whose name matches known
//!    tool directories or whose tokens contain a developer tool name;
//! 2. **layout heuristic** — ≥ 3 first-level children match developer layout
//!    vocabulary (`bin`/`cache`/`config`/`log`/`src`/...) or a manifest marker
//!    (`package.json` / `Cargo.toml` / `go.mod` / ...) is present.
//!
//! A directory that hits neither heuristic is *not reported at all* (noise
//! control, SPEC §25) — it is not marked Protected or anything else.
//!
//! Exclusions, in order:
//!
//! - the shared seen-set (a path already claimed by an earlier provider —
//!   dev caches, agent layouts — never re-emerges as Unknown);
//! - **known provider roots** (agent layout roots `.codex`/`.claude`/`.cache`/
//!   `.local` + OpenCode alt, dev-cache roots `.cargo`/`.nuget`/`.bun` and the
//!   LOCALAPPDATA cache dirs, APPDATA `Cursor`/`Windsurf`): these regions are
//!   managed by their own fine-grained providers, a whole-root Unknown item
//!   would double-report them;
//! - **built-in protected rules** (`.ssh`/`.gnupg`/... INV-007): never emitted.
//!   A *user* Protect disposition is the opposite — the user asked for the
//!   item to surface as Protected, so it is emitted and the F-2-1 rule gate
//!   (see [`apply_rule_classification`]) reclassifies it.
//!
//! Every emitted item is `RiskLevel::Unknown`, `Category::Unknown`,
//! `CleanupAction::None` with a human explanation ("unknown developer data;
//! manual review required"), INV-001: never auto-deleted. Measurement reuses
//! the shared tree walker (reparse points are never followed).

use std::path::Path;

use devresidue_core::{CleanupAction, Evidence, ResidueCategory, RiskLevel, ScanItem, SourceKind};

use crate::measure::{measure_tree_parallel, path_is_reparse_like};
use crate::registry;
use crate::scan_ctx::{ProgressEvent, ScanContext};

pub const PROVIDER: &str = "unknown";

/// Zone mode for a scan root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootMode {
    /// `%USERPROFILE%`: hidden (dot-prefixed) directories only.
    HomeHidden,
    /// `%LOCALAPPDATA%` / `%APPDATA%`: every first-level directory.
    AppDir,
}

/// Known provider-managed region roots per env zone — never reported as
/// Unknown (their own providers already emit fine-grained items).
const HOME_KNOWN_ROOTS: &[&str] = &[
    // Agent layout roots (Phase 9): codex / claude / opencode share+cache.
    ".codex", ".claude", ".local", ".cache",
    // Dev-cache roots (Phase 8): cargo home, nuget global, bun install.
    ".cargo", ".nuget", ".bun",
];
const LOCALAPPDATA_KNOWN_ROOTS: &[&str] = &["npm-cache", "uv", "pip", "Cursor"];
const APPDATA_KNOWN_ROOTS: &[&str] = &["Cursor", "Windsurf"];

/// Dot-directory names that are strong developer-tool signals but would not be
/// caught by token matching (`.`-stripped name too short / not a tool token).
const DOT_DIR_NAMES: &[&str] = &[".m2", ".gradle"];

/// Developer tool tokens; a directory name containing one (split on
/// `._- `) is treated as likely tool data.
const TOOL_TOKENS: &[&str] = &[
    "node",
    "npm",
    "npx",
    "yarn",
    "pnpm",
    "deno",
    "bun",
    "go",
    "golang",
    "rust",
    "rustup",
    "cargo",
    "python",
    "pip",
    "uv",
    "poetry",
    "conda",
    "mamba",
    "pixi",
    "java",
    "jdk",
    "gradle",
    "maven",
    "mvn",
    "scala",
    "sbt",
    "kotlin",
    "dotnet",
    "nuget",
    "flutter",
    "dart",
    "android",
    "swift",
    "xcode",
    "clang",
    "cmake",
    "zig",
    "nix",
    "docker",
    "terraform",
    "vagrant",
    "vscode",
    "jetbrains",
    "goenv",
    "pyenv",
    "nvm",
    "volta",
    "asdf",
    "sdkman",
    "jenv",
    "wasm",
    "emscripten",
    "meson",
    "ninja",
    "eslintcache",
    "parcel-cache",
    "turbo",
    "serverless",
    "nextjs",
    "helm",
];

/// First-level *directory* names that contribute to the layout heuristic.
const LAYOUT_DIR_HINTS: &[&str] = &[
    "bin",
    "cache",
    "config",
    "log",
    "logs",
    "repos",
    "src",
    "db",
    "data",
    "temp",
    "tmp",
    "var",
    "lib",
    "obj",
    "target",
    "build",
    "node_modules",
    "toolchains",
    "packages",
    "registry",
    "resources",
    "share",
    "state",
    "history",
];

/// First-level *file* markers that strongly indicate developer tool data.
const LAYOUT_FILE_MARKERS: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "composer.json",
    "Gemfile",
    "pom.xml",
    "build.gradle",
    "CMakeLists.txt",
    "yarn.lock",
    "package-lock.json",
    "poetry.lock",
    "uv.lock",
    "settings.json",
];

/// Runs the unknown-data scan (provider-level progress wrapper).
pub fn scan(ctx: &ScanContext) -> Vec<ScanItem> {
    if ctx.cancelled() {
        return Vec::new();
    }
    ctx.progress(ProgressEvent::ProviderStart(PROVIDER));
    let items = scan_impl(ctx);
    if !ctx.cancelled() {
        ctx.progress(ProgressEvent::ProviderDone(PROVIDER));
    }
    items
}

/// The actual walk (see [`scan`] for the progress wrapper).
fn scan_impl(ctx: &ScanContext) -> Vec<ScanItem> {
    let mut items = Vec::new();

    if let Some(profile) = ctx.env("USERPROFILE") {
        collect_zone(ctx, Path::new(&profile), RootMode::HomeHidden, &mut items);
    }
    if let Some(local) = ctx.env("LOCALAPPDATA") {
        collect_zone(ctx, Path::new(&local), RootMode::AppDir, &mut items);
    }
    if let Some(roaming) = ctx.env("APPDATA") {
        collect_zone(ctx, Path::new(&roaming), RootMode::AppDir, &mut items);
    }

    items
}

/// Reads one zone root and emits candidates that pass the heuristics.
fn collect_zone(ctx: &ScanContext, root: &Path, mode: RootMode, items: &mut Vec<ScanItem>) {
    if !root.is_dir() || ctx.cancelled() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if ctx.cancelled() {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };

        match mode {
            RootMode::HomeHidden => {
                if !name.starts_with('.') {
                    continue;
                }
            }
            RootMode::AppDir => {}
        }
        if is_known_provider_root(mode, &name) {
            continue;
        }
        if path_is_reparse_like(&path) {
            continue;
        }

        let (hit, reason) = heuristic_hit(ctx, &name, &path);
        if !hit {
            continue;
        }
        // Built-in protected roots are never residue (INV-007); a *user*
        // Protect disposition must surface as Protected instead.
        if ctx.is_builtin_protected(&path) {
            continue;
        }
        if !ctx.seen_insert(&path) {
            continue;
        }
        if let Some(item) = emit_unknown(ctx, &path, name, reason) {
            items.push(item);
        }
    }
}

/// One-level content probe + name probe. Returns `(report, reason)`.
fn heuristic_hit(ctx: &ScanContext, name: &str, path: &Path) -> (bool, String) {
    let lower = name.to_lowercase();
    let dot_name = if let Some(stripped) = lower.strip_prefix('.') {
        format!(".{stripped}")
    } else {
        lower.clone()
    };
    if DOT_DIR_NAMES.iter().any(|d| *d == dot_name) {
        return (true, "name matches a known developer tool directory".into());
    }
    if tokenize(&lower)
        .iter()
        .any(|t| TOOL_TOKENS.contains(&t.as_str()))
    {
        return (true, "name matches developer tool conventions".into());
    }
    if ctx.cancelled() {
        return (false, String::new());
    }
    layout_hit(path)
        .map(|count| {
            (
                true,
                format!("directory layout suggests tool data ({count} layout hits)"),
            )
        })
        .unwrap_or((false, String::new()))
}

/// Splits a lower-cased directory name on non-alphanumeric characters.
fn tokenize(lower: &str) -> Vec<String> {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Counts first-level children that look like developer layout vocabulary.
fn layout_hit(path: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(path).ok()?;
    let mut hits = 0u64;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_lowercase) else {
            continue;
        };
        if ft.is_dir() && LAYOUT_DIR_HINTS.iter().any(|h| *h == name) {
            hits += 1;
        } else if ft.is_file() && LAYOUT_FILE_MARKERS.iter().any(|m| *m == name) {
            hits += 2; // a manifest marker is a strong signal
        }
        if hits >= 3 {
            return Some(hits);
        }
    }
    (hits >= 3).then_some(hits)
}

/// Whether the name belongs to a provider-managed region for this zone.
fn is_known_provider_root(mode: RootMode, name: &str) -> bool {
    let lower = name.to_lowercase();
    match mode {
        RootMode::HomeHidden => HOME_KNOWN_ROOTS.iter().any(|r| *r == lower),
        RootMode::AppDir => {
            // LOCALAPPDATA and APPDATA share the Cursor exclusion; LOCALAPPDATA
            // additionally excludes the dev-cache dirs. (Windsurf lives under
            // APPDATA.) We apply the union — reporting fewer false positives is
            // the conservative direction.
            LOCALAPPDATA_KNOWN_ROOTS
                .iter()
                .chain(APPDATA_KNOWN_ROOTS.iter())
                .any(|r| *r == lower)
        }
    }
}

/// Measures and emits one unknown-data candidate.
fn emit_unknown(ctx: &ScanContext, path: &Path, name: String, reason: String) -> Option<ScanItem> {
    let measure = measure_tree_parallel(path, ctx.cancel_fn());
    if ctx.cancelled() {
        return None;
    }
    let explanation = format!(
        "unknown developer data; manual review required — {reason}. \
         {} files, {} bytes. No automatic cleanup (INV-001).",
        measure.file_count, measure.logical_size
    );

    Some(ScanItem {
        id: ctx.allocate_id(),
        path: path.to_path_buf(),
        display_name: name,
        product: None,
        category: ResidueCategory::Unknown,
        risk: RiskLevel::Unknown,
        source: SourceKind::UnknownProvider,
        logical_size: measure.logical_size,
        file_count: measure.file_count,
        last_modified: measure.last_modified,
        explanation,
        cleanup_action: CleanupAction::None,
        evidence: vec![
            Evidence::new(
                registry::evidence_tag(PROVIDER),
                "heuristic unknown-data discovery",
            ),
            Evidence::new("unknown-heuristic", reason),
        ],
        scan_snapshot: None, // filled by the scan assembler when a probe is wired
        classification_rule_id: None,
    })
}
