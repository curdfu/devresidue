//! Agent data layouts — one declarative table per agent (SPEC §9).
//!
//! A layout maps fine-grained sub-paths of an agent's data root to a
//! classification. The whole agent root is **never** listed as a single SAFE
//! item; protected material (auth/config/secrets) is deliberately absent from
//! the table and instead covered by `resources/rules/protected/*.yaml` rules.
//!
//! Layout entries are grouped so `Phase 13` can add more agents with just a
//! new constant — no new provider code.

use std::path::{Path, PathBuf};

use devresidue_core::{ResidueCategory, RiskLevel};

/// Environment key that resolves to the agent's data root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSource {
    /// `%USERPROFILE%`
    UserProfile,
    /// `%LOCALAPPDATA%`
    LocalAppData,
    /// `%APPDATA%`
    AppData,
}

/// How an entry is matched under its agent root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A whole sub-directory (measured recursively).
    Dir,
    /// A single file at a fixed relative path (measured as one file).
    File,
    /// Files under a parent matching a name pattern. Relative parent is given
    /// by `rel`, the file-name pattern by `name_prefix`/`glob`; contents are
    /// aggregated into one item. Used for log clusters (`a.log`, `a.1.log`).
    GlobFiles { name_glob: String },
}

/// One cleanable sub-layout of an agent.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Relative path under the agent root (dir or file or glob parent).
    pub rel: &'static str,
    /// Matcher kind.
    pub kind: Kind,
    /// Human purpose label used in explanation text (e.g. "session history").
    pub purpose: &'static str,
    pub category: ResidueCategory,
    pub risk: RiskLevel,
    /// Display title ("Codex session transcripts").
    pub title: &'static str,
    /// Extra evidence note (layout provenance).
    pub note: &'static str,
}

/// One agent's data layout.
#[derive(Debug, Clone)]
pub struct AgentLayout {
    /// Provider slug (registry table).
    pub slug: &'static str,
    /// Product display name ("Codex CLI", "Claude Code").
    pub product: &'static str,
    /// Root source env key.
    pub root: RootSource,
    /// Optional first sub-path under the env root (e.g. `opencode`).
    /// `None` → the env root itself is the agent root.
    pub root_sub: Option<&'static str>,
    /// Cleanable sub-entries.
    pub entries: &'static [Entry],
    /// Extra env key for an alternate root (OpenCode cache split).
    pub alt_root: Option<RootSource>,
    /// Sub-paths under `alt_root` that are cleanable (cache).
    pub alt_sub: Option<&'static str>,
}

/// Agent layout constructors.
pub const fn dir(
    rel: &'static str,
    purpose: &'static str,
    category: ResidueCategory,
    risk: RiskLevel,
    title: &'static str,
) -> Entry {
    Entry {
        rel,
        kind: Kind::Dir,
        purpose,
        category,
        risk,
        title,
        note: "measured as one aggregated directory",
    }
}

pub const fn file(
    rel: &'static str,
    purpose: &'static str,
    category: ResidueCategory,
    risk: RiskLevel,
    title: &'static str,
) -> Entry {
    Entry {
        rel,
        kind: Kind::File,
        purpose,
        category,
        risk,
        title,
        note: "single-file data store, measured directly",
    }
}

/// Builds a `GlobFiles` entry (non-const: holds a `String` glob).
pub fn glob_files(
    rel: &'static str,
    name_glob: &str,
    purpose: &'static str,
    category: ResidueCategory,
    risk: RiskLevel,
    title: &'static str,
) -> Entry {
    Entry {
        rel,
        kind: Kind::GlobFiles {
            name_glob: name_glob.to_string(),
        },
        purpose,
        category,
        risk,
        title,
        note: "aggregated over a name pattern",
    }
}

impl AgentLayout {
    /// The agent root path (env root [+ sub]).
    pub fn root_path(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
        let base = match self.root {
            RootSource::UserProfile => env("USERPROFILE")?,
            RootSource::LocalAppData => env("LOCALAPPDATA")?,
            RootSource::AppData => env("APPDATA")?,
        };
        let mut p = PathBuf::from(base);
        if let Some(sub) = self.root_sub {
            p.push(sub);
        }
        Some(p)
    }

    /// Alternate root (e.g. OpenCode's `~\.cache` half).
    pub fn alt_root_path(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
        let src = self.alt_root?;
        let base = match src {
            RootSource::UserProfile => env("USERPROFILE")?,
            RootSource::LocalAppData => env("LOCALAPPDATA")?,
            RootSource::AppData => env("APPDATA")?,
        };
        let mut p = PathBuf::from(base);
        if let Some(sub) = self.alt_sub {
            p.push(sub);
        }
        Some(p)
    }

    /// Absolute candidate for one entry.
    pub fn entry_path(&self, root: &Path, entry: &Entry) -> PathBuf {
        root.join(entry.rel)
    }
}

/// True when a file name matches a simple glob with at most one `*`
/// wildcard (`oh-my-opencode-slim*.log`, `*.log`). No `?` / `[]`.
pub fn name_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => name.starts_with(prefix) && name.ends_with(suffix),
        None => name == pattern,
    }
}

/// Whether a path exists (file or dir).
pub fn exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_name_matching() {
        assert!(name_matches("*.log", "opencode.20260101.log"));
        assert!(name_matches("*.log", "oh-my-opencode-slim.2026.log"));
        assert!(name_matches(
            "oh-my-opencode-slim*.log",
            "oh-my-opencode-slim.log"
        ));
        assert!(name_matches(
            "oh-my-opencode-slim*.log",
            "oh-my-opencode-slim.2026-01-01.log"
        ));
        assert!(!name_matches("oh-my-opencode-slim*.log", "opencode.log"));
        assert!(!name_matches("*.log", "opencode.db"));
        assert!(name_matches("state.vscdb", "state.vscdb"));
        assert!(!name_matches("state.vscdb", "state.vscdb.backup"));
    }

    #[test]
    fn root_path_joins_env_and_sub() {
        let env = |k: &str| match k {
            "USERPROFILE" => Some(r"C:\Users\alice".to_string()),
            _ => None,
        };
        let layout = AgentLayout {
            slug: "codex",
            product: "Codex CLI",
            root: RootSource::UserProfile,
            root_sub: Some(".codex"),
            entries: &[],
            alt_root: None,
            alt_sub: None,
        };
        assert_eq!(
            layout.root_path(&env),
            Some(PathBuf::from(r"C:\Users\alice\.codex"))
        );
    }
}
