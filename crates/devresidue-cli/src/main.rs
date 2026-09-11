//! DevResidue CLI — entry point.
//!
//! Phase 6 scope adds the cleanup surface on top of the Phase 2/3 scan +
//! rules commands:
//!
//! ```text
//! devresidue plan --safe | --items <ids> [--confirm-*|--yes]
//! devresidue clean --plan <id> [--dry-run] [--confirm-*|--yes]
//! devresidue clean --safe [--dry-run]
//! devresidue journal [--last N]
//! ```
//!
//! Deletion is only ever reachable through persisted plan ids (INV-013):
//! there is **no** `delete <path>` / `clean <path>` command. The CleanupEngine
//! in core remains the single deletion authority (INV-010) and the Windows
//! DeletePort adapter its only real executor.
//!
//! Phase 10 additions:
//!
//! - `scan` installs a Ctrl-C handler that flips a cancellation flag; the scan
//!   stops at its next provider/project/entry boundary and reports partial
//!   results (exit 0). `plan` / `clean` deliberately install no handler —
//!   deleting is never interrupted mid-item (per-item atomicity comes from the
//!   DeletePort), so Ctrl-C there terminates the process as usual.

mod ai_cmd;
mod clean_cmd;
mod journal_cmd;
mod plan_cmd;
mod rules_cmd;
mod scan;
mod support;
mod unknown_cmd;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "devresidue",
    version,
    about = "Windows developer / AI-agent storage manager",
    long_about = "DevResidue: scan, classify, plan and — only through a persisted \
                   plan — clean developer residue (agent data, dev caches, build \
                   artifacts). Cleanup commands operate on plan ids, never on \
                   arbitrary paths (INV-013)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Flags shared by plan/clean for confirmation levels (SPEC §19).
#[derive(Debug, Clone, clap::Args)]
struct PlanFlags {
    /// Also allow RegenerableDownload items.
    #[arg(long)]
    confirm_redownload: bool,
    /// Also allow Review items.
    #[arg(long)]
    confirm_review: bool,
    /// Confirm everything (shortcut for the strictest level).
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Clone, clap::Args)]
struct CleanFlags {
    /// Also allow RegenerableDownload items.
    #[arg(long)]
    confirm_redownload: bool,
    /// Also allow Review items.
    #[arg(long)]
    confirm_review: bool,
    /// Confirm everything.
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan for residue (real providers: dev caches + kondo projects).
    Scan {
        /// Emit a stable JSON array instead of the human-readable table.
        #[arg(long)]
        json: bool,
        /// Only run the developer-cache providers (npm/pip/uv/...).
        #[arg(long)]
        dev_cache: bool,
        /// Kondo workspace root to scan (repeatable).
        #[arg(long = "projects", value_name = "PATH")]
        projects: Vec<String>,
        /// AI-agent data providers (Phase 9) — currently prints a notice.
        #[arg(long)]
        agents: bool,
        /// Built-in demo fixtures instead of the real machine scan.
        #[arg(long)]
        fixtures: bool,
        /// Suppress stderr progress lines (CI/script friendly).
        #[arg(long)]
        quiet: bool,
        /// Only run the unknown developer-data provider (SPEC §25). The default
        /// all-family scan includes it too.
        #[arg(long)]
        unknown: bool,
    },
    /// Inspect the rules directory under resources/.
    Rules {
        #[command(subcommand)]
        cmd: RulesCommand,
    },
    /// Build and persist a cleanup plan from the most recent scan.
    Plan {
        /// Select every item from the most recent scan.
        #[arg(long)]
        safe: bool,
        /// Comma-separated scan item ids from the most recent scan.
        #[arg(long, value_name = "IDS")]
        items: Option<String>,
        /// R3-G03: the scan generation the `--items` ids came from (shown by
        /// the scan that produced them). Refuses the plan when the persisted
        /// scan is a different generation (the ids may alias other objects).
        #[arg(long, value_name = "GEN")]
        scan_generation: Option<u64>,
        /// Plan from a cancelled (partial) scan — the plan is then based on
        /// incomplete scan results.
        #[arg(long)]
        allow_partial: bool,
        #[command(flatten)]
        flags: PlanFlags,
    },
    /// Execute (or dry-run) a persisted cleanup plan.
    Clean {
        /// Persisted plan id to execute.
        #[arg(long, value_name = "ID")]
        plan: Option<u64>,
        /// Build-and-run a plan over the most recent real scan, selecting only
        /// Safe / RegenerableLocal items (same semantics as `plan --safe`).
        #[arg(long)]
        safe: bool,
        /// Simulate: full validation, no deletion (SPEC §23).
        #[arg(long)]
        dry_run: bool,
        /// Clean from a cancelled (partial) scan — the plan is then based on
        /// incomplete scan results.
        #[arg(long)]
        allow_partial: bool,
        #[command(flatten)]
        flags: CleanFlags,
    },
    /// Show recent cleanup journal entries.
    Journal {
        /// Maximum number of entries to show.
        #[arg(long, default_value_t = 20, value_name = "N")]
        last: usize,
    },
    /// Ignore / Protect one item from the most recent scan (SPEC §25 actions).
    Unknown {
        #[command(subcommand)]
        cmd: UnknownCommand,
    },
    /// Configure and use the optional remote AI advisor (disabled by default).
    Ai {
        #[command(subcommand)]
        cmd: AiCommand,
    },
}

/// Remote-AI commands. They never accept a path, cleanup plan or rule text.
#[derive(Debug, Subcommand)]
enum AiCommand {
    /// Show non-secret AI configuration and recovery state.
    Status,
    /// Enable remote AI globally. A selected usable profile is still required.
    Enable,
    /// Disable remote AI globally.
    Disable,
    /// Manage non-secret AI profile metadata and its derived key.
    Profile {
        #[command(subcommand)]
        cmd: AiProfileCommand,
    },
    /// Inspect or repair an interrupted non-secret profile transaction.
    Recovery {
        #[command(subcommand)]
        cmd: AiRecoveryCommand,
    },
    /// Analyze explicitly selected IDs from the latest verified real scan.
    Analyze {
        /// Comma-separated scan item ids (at most 20 eligible entries).
        #[arg(long, value_name = "IDS")]
        items: String,
        /// After showing the validated suggestions, read an explicit local
        /// confirmation line from stdin in the form ITEM_ID=RISK:CATEGORY,... .
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AiProfileCommand {
    /// List stored non-secret profile metadata.
    List,
    /// Create a profile. The key is read only from stdin, never an argument.
    Create {
        #[command(flatten)]
        input: AiProfileInputArgs,
    },
    /// Replace an existing profile's non-secret settings and supplied key.
    Update {
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
        #[command(flatten)]
        input: AiProfileInputArgs,
    },
    /// Delete exactly one profile and its Core-derived environment key.
    Delete {
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
    },
    /// Select the active profile used by remote AI requests.
    Select {
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
    },
    /// Clear the active profile selection without changing any profile.
    ClearSelection,
}

#[derive(Debug, Subcommand)]
enum AiRecoveryCommand {
    /// Show whether profile recovery is required.
    Status,
    /// Re-submit a create/update recovery using fresh non-secret settings and
    /// a key read from stdin.
    Resubmit {
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
        #[command(flatten)]
        input: AiProfileInputArgs,
    },
    /// Complete a pending deletion recovery for exactly one marked profile.
    Delete {
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
    },
    /// Disable the marked profile and remove only its derived environment key.
    Abandon {
        #[arg(value_name = "PROFILE_ID")]
        profile_id: String,
    },
}

/// Non-secret profile metadata. `--key-stdin` is deliberately a boolean gate:
/// no command-line argument can ever carry an API key value.
#[derive(Debug, Clone, clap::Args)]
struct AiProfileInputArgs {
    #[arg(long)]
    name: String,
    #[arg(long = "base-url")]
    base_url: String,
    #[arg(long)]
    model: String,
    #[arg(long, default_value = "auto", value_name = "MODE")]
    structured_output: String,
    #[arg(long, default_value_t = 120, value_name = "SECONDS")]
    timeout_secs: u64,
    /// Store a disabled profile; it cannot be used until it is updated.
    #[arg(long)]
    disabled: bool,
    /// Read the API key from standard input. Do not pass keys as arguments.
    #[arg(long)]
    key_stdin: bool,
}

/// Disposition subcommands of `devresidue unknown`.
#[derive(Debug, Subcommand)]
enum UnknownCommand {
    /// Never report this path again (writes a user ignore rule).
    Ignore {
        #[arg(value_name = "ITEM_ID")]
        item_id: u64,
    },
    /// Report this path as Protected from now on (writes a user protected rule).
    Protect {
        #[arg(value_name = "ITEM_ID")]
        item_id: u64,
    },
}

#[derive(Debug, Subcommand)]
enum RulesCommand {
    /// Load and list the rules shipped under resources/rules.
    List {
        /// Emit a stable JSON array instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Load and validate every rule file (exit code 1 on any error).
    Validate,
}

/// Plain option bundles passed to the command modules (keeps the clap types
/// out of their signatures for testability).
#[derive(Debug, Clone)]
pub(crate) struct ScanOptions {
    pub json: bool,
    pub dev_cache: bool,
    pub projects: Vec<String>,
    pub agents: bool,
    pub fixtures: bool,
    pub quiet: bool,
    pub unknown: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PlanOptions {
    pub safe: bool,
    pub items: Option<String>,
    /// R3-G03: the scan generation the user's `--items` selection was made
    /// against (from the scan's output). When given, a mismatch with the
    /// persisted snapshot's generation refuses the plan build.
    pub scan_generation: Option<u64>,
    pub allow_partial: bool,
    pub confirm_redownload: bool,
    pub confirm_review: bool,
    pub yes: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CleanOptions {
    pub plan: Option<u64>,
    pub safe: bool,
    pub dry_run: bool,
    pub allow_partial: bool,
    pub confirm_redownload: bool,
    pub confirm_review: bool,
    pub yes: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Scan {
            json,
            dev_cache,
            projects,
            agents,
            fixtures,
            quiet,
            unknown,
        } => scan::run(ScanOptions {
            json,
            dev_cache,
            projects,
            agents,
            fixtures,
            quiet,
            unknown,
        }),
        Command::Rules { cmd } => rules_cmd::run(cmd),
        Command::Plan {
            safe,
            items,
            scan_generation,
            allow_partial,
            flags,
        } => plan_cmd::run(PlanOptions {
            safe,
            items,
            scan_generation,
            allow_partial,
            confirm_redownload: flags.confirm_redownload,
            confirm_review: flags.confirm_review,
            yes: flags.yes,
        }),
        Command::Clean {
            plan,
            safe,
            dry_run,
            allow_partial,
            flags,
        } => clean_cmd::run(CleanOptions {
            plan,
            safe,
            dry_run,
            allow_partial,
            confirm_redownload: flags.confirm_redownload,
            confirm_review: flags.confirm_review,
            yes: flags.yes,
        }),
        Command::Journal { last } => journal_cmd::run(last),
        Command::Unknown { cmd } => unknown_cmd::run(cmd),
        Command::Ai { cmd } => ai_cmd::run(cmd),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
