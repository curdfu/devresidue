//! Concrete agent layout tables (SPEC §9 first batch).
//!
//! Each table mirrors the actual on-disk layout (verified on a real Windows
//! machine in Phase 9) plus sensible fallbacks. Protected faces are handled
//! by `resources/rules/protected/*.yaml`, so the tables intentionally omit
//! auth/config/secret paths.

use devresidue_core::{ResidueCategory, RiskLevel};

use super::layout::{dir, file, AgentLayout, RootSource};

// ---- Risk-level shorthands ------------------------------------------------

const SAFE_AGENT_CACHE: RiskLevel = RiskLevel::Safe;
const SAFE_LOGS: RiskLevel = RiskLevel::Safe;
const SAFE_TEMP: RiskLevel = RiskLevel::Safe;
const REVIEW: RiskLevel = RiskLevel::Review;
const REVIEW_STATE: RiskLevel = RiskLevel::Review;

/// Codex CLI (`%USERPROFILE%\.codex`).
pub const CODEX: AgentLayout = AgentLayout {
    slug: "codex",
    product: "Codex CLI",
    root: RootSource::UserProfile,
    root_sub: Some(".codex"),
    entries: &[
        dir(
            ".tmp",
            "temporary working files",
            ResidueCategory::Temporary,
            SAFE_TEMP,
            "Codex temporary files",
        ),
        // Screen captures record everything visible on screen during an AI
        // computer-use session (password managers, e-mail, documents, ...).
        // They are user session content, not a regenerable agent cache, so
        // this classifies as Session and always requires review (F-1c).
        dir(
            "cache/computer-use",
            "screen captures taken during AI computer-use sessions; may contain sensitive on-screen content",
            ResidueCategory::Session,
            REVIEW,
            "Codex computer-use screen captures",
        ),
        dir(
            "archived_sessions",
            "archived conversation history",
            ResidueCategory::Session,
            REVIEW,
            "Codex archived sessions",
        ),
        dir(
            "sessions",
            "conversation session history",
            ResidueCategory::Session,
            REVIEW,
            "Codex session history",
        ),
        dir(
            "plugins",
            "installed plugin data",
            ResidueCategory::Session,
            REVIEW,
            "Codex plugins",
        ),
        dir(
            ".sandbox-bin",
            "sandbox toolchain binaries",
            ResidueCategory::AiAgent,
            REVIEW,
            "Codex sandbox binaries",
        ),
        dir(
            ".sandbox",
            "sandbox state",
            ResidueCategory::AiAgent,
            REVIEW,
            "Codex sandbox state",
        ),
        file(
            "sqlite/codex-dev.db",
            "local session database",
            ResidueCategory::Session,
            REVIEW,
            "Codex session database",
        ),
    ],
    alt_root: None,
    alt_sub: None,
};

/// Claude Code (`%USERPROFILE%\.claude`).
pub const CLAUDE: AgentLayout = AgentLayout {
    slug: "claude",
    product: "Claude Code",
    root: RootSource::UserProfile,
    root_sub: Some(".claude"),
    entries: &[
        dir(
            "shell-snapshots",
            "shell-snapshot cache",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Claude Code shell-snapshot cache",
        ),
        dir(
            "projects",
            "per-project session transcripts",
            ResidueCategory::Session,
            REVIEW,
            "Claude Code project sessions",
        ),
        dir(
            "sessions",
            "session transcripts",
            ResidueCategory::Session,
            REVIEW,
            "Claude Code sessions",
        ),
        dir(
            "todos",
            "todo tracking state",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Claude Code todos",
        ),
        dir(
            "statsig",
            "experiment flag cache",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Claude Code statsig cache",
        ),
    ],
    alt_root: None,
    alt_sub: None,
};

/// OpenCode — split across `%USERPROFILE%\.local\share\opencode` and
/// `%USERPROFILE%\.cache\opencode`.
pub const OPENCODE: AgentLayout = AgentLayout {
    slug: "opencode",
    product: "OpenCode",
    root: RootSource::UserProfile,
    root_sub: Some(".local/share/opencode"),
    entries: &[
        file(
            "opencode.db",
            "session database (tool-native management preferred)",
            ResidueCategory::Session,
            REVIEW,
            "OpenCode session database",
        ),
        dir(
            "storage",
            "per-project workspace state",
            ResidueCategory::WorkspaceState,
            REVIEW_STATE,
            "OpenCode workspace state",
        ),
        dir(
            "log",
            "application logs",
            ResidueCategory::Log,
            SAFE_LOGS,
            "OpenCode logs",
        ),
        dir(
            "repos",
            "temporary project clones",
            ResidueCategory::Session,
            REVIEW,
            "OpenCode repo clones",
        ),
        dir(
            "tool-output",
            "tool output cache",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "OpenCode tool output",
        ),
    ],
    alt_root: Some(RootSource::UserProfile),
    alt_sub: Some(".cache/opencode"),
};

/// OMO Slim — OpenCode plugin; the storage part lives nested under the
/// OpenCode layout and the logs under OpenCode's log directory, so this agent
/// has no entries of its own. The caller walks the shared OpenCode layout and
/// additionally applies OMO's two extras.
pub const OMO: AgentLayout = AgentLayout {
    slug: "omo",
    product: "OMO Slim",
    root: RootSource::UserProfile,
    root_sub: Some(".local/share/opencode/storage/oh-my-opencode-slim"),
    entries: &[],
    alt_root: Some(RootSource::UserProfile),
    alt_sub: Some(".local/share/opencode/log"),
};

/// Cursor editor (`%APPDATA%\Cursor`).
pub const CURSOR: AgentLayout = AgentLayout {
    slug: "cursor",
    product: "Cursor",
    root: RootSource::AppData,
    root_sub: Some("Cursor"),
    entries: &[
        dir(
            "User/workspaceStorage",
            "per-workspace editor state",
            ResidueCategory::WorkspaceState,
            REVIEW_STATE,
            "Cursor workspace storage",
        ),
        file(
            "User/globalStorage/state.vscdb",
            "global editor state database",
            ResidueCategory::WorkspaceState,
            REVIEW_STATE,
            "Cursor global state",
        ),
        dir(
            "Cache",
            "editor cache",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Cursor cache",
        ),
        dir(
            "CachedData",
            "editor cached data",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Cursor cached data",
        ),
        dir(
            "logs",
            "editor logs",
            ResidueCategory::Log,
            SAFE_LOGS,
            "Cursor logs",
        ),
    ],
    alt_root: None,
    alt_sub: None,
};

/// Windsurf editor (`%APPDATA%\Windsurf`) — same layout family as Cursor.
pub const WINDSURF: AgentLayout = AgentLayout {
    slug: "windsurf",
    product: "Windsurf",
    root: RootSource::AppData,
    root_sub: Some("Windsurf"),
    entries: &[
        dir(
            "User/workspaceStorage",
            "per-workspace editor state",
            ResidueCategory::WorkspaceState,
            REVIEW_STATE,
            "Windsurf workspace storage",
        ),
        file(
            "User/globalStorage/state.vscdb",
            "global editor state database",
            ResidueCategory::WorkspaceState,
            REVIEW_STATE,
            "Windsurf global state",
        ),
        dir(
            "Cache",
            "editor cache",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Windsurf cache",
        ),
        dir(
            "CachedData",
            "editor cached data",
            ResidueCategory::AiAgent,
            SAFE_AGENT_CACHE,
            "Windsurf cached data",
        ),
        dir(
            "logs",
            "editor logs",
            ResidueCategory::Log,
            SAFE_LOGS,
            "Windsurf logs",
        ),
    ],
    alt_root: None,
    alt_sub: None,
};

/// The full first-batch agent list, in registry order.
pub const ALL: &[AgentLayout] = &[CODEX, CLAUDE, OPENCODE, OMO, CURSOR, WINDSURF];
