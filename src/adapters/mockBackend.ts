import type {
  AnalyzerSuggestionDto,
  AppSettingsDto,
  CleanupPlanDto,
  CleanupSessionDto,
  ConfirmPolicy,
  Disposition,
  DispositionResultDto,
  JournalEntryDto,
  PlannedItemDto,
  RiskLevel,
  RuleDto,
  RulesValidationDto,
  ScanHandleDto,
  ScanItemDto,
  ScanScope,
  ScanSnapshotDto,
  SessionItemDto,
} from "@/types";
import type { Backend } from "./backend";
import { toCommandError } from "./backend";
import { mockRuleDtos, mockRulesValidation } from "@/data/rules";

/**
 * In-memory backend. Item set mirrors the CLI fixture / rules vocabulary so
 * mock screenshots are production-shaped (SPEC §27 dashboard semantics):
 *
 *   npm cache .............. 22.2 GiB  regenerable-download
 *   dev-cleaner target .....  3.1 GiB  regenerable-local
 *   .codex sessions ........ 67.7 MiB  review
 *   .ssh ...................   24 KiB  protected
 *   opencode.db ............  1.3 GiB  review
 *   … plus the fixture set (Claude cache/sessions, Rust target, node_modules)
 */

const DAY = 86_400;
const now = () => Math.floor(Date.now() / 1000);
const daysAgo = (d: number) => now() - d * DAY;

const GiB = 1024 ** 3;
const MiB = 1024 ** 2;
const KiB = 1024;

let nextId = 1;
const id = () => nextId++;

type MockItem = Omit<ScanItemDto, "id">;

function item(base: MockItem): ScanItemDto {
  return { id: id(), ...base };
}

/** Full "default scope" data set (agents + dev cache + projects). */
function defaultItems(): ScanItemDto[] {
  return [
    // ---- Dev cache / package managers (download class) ----
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\npm-cache",
      display_name: "npm cache",
      product: "npm",
      category: "package-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 22.2 * GiB,
      file_count: 284_310,
      last_modified: daysAgo(2),
      explanation:
        "npm package download cache. Deleting frees 22.2 GiB; future installs re-download every package from the registry. Cleaned via the tool-native command (npm cache clean --force), not a raw delete.",
      cleanup_action: {
        kind: "external-command",
        command: {
          executable: "npm",
          args: ["cache", "clean", "--force"],
          working_directory: null,
          timeout_secs: 300,
        },
      },
      evidence: [
        {
          rule_id: null,
          source: "tool-reported-path",
          detail: "npm config get cache reported this location",
        },
        {
          rule_id: null,
          source: "developer-cache-provider",
          detail: "npm provider measured cache size on disk",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\pip\\cache",
      display_name: "pip HTTP cache",
      product: "pip",
      category: "developer-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 1.9 * GiB,
      file_count: 61_204,
      last_modified: daysAgo(6),
      explanation:
        "pip wheel/HTTP download cache. Safe to remove; packages re-download on next pip install. 1.9 GiB reclaimable.",
      cleanup_action: { kind: "direct-delete" },
      evidence: [
        {
          rule_id: null,
          source: "tool-reported-path",
          detail: "pip cache dir reported this location",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.cargo\\registry",
      display_name: "Cargo registry cache",
      product: "cargo",
      category: "developer-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 4.3 * GiB,
      file_count: 96_118,
      last_modified: daysAgo(9),
      explanation:
        "Downloaded .crate files and extracted sources for every dependency cargo ever resolved. Deleting frees 4.3 GiB; the next build of each project re-downloads its crates.",
      cleanup_action: { kind: "direct-delete" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "cargo registry layout under .cargo/registry",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\uv\\cache",
      display_name: "uv cache",
      product: "uv",
      category: "developer-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 2.8 * GiB,
      file_count: 48_922,
      last_modified: daysAgo(1),
      explanation:
        "uv package cache. Regenerable by re-download; 2.8 GiB. uv cache clean is the tool-native path.",
      cleanup_action: {
        kind: "external-command",
        command: {
          executable: "uv",
          args: ["cache", "clean"],
          working_directory: null,
          timeout_secs: 120,
        },
      },
      evidence: [
        {
          rule_id: null,
          source: "tool-reported-path",
          detail: "uv cache directory reported by uv",
        },
      ],
    }),

    // ---- AI agents (split per SPEC §9) ----
    item({
      path: "C:\\Users\\demo\\.claude\\shell-snapshots",
      display_name: "Claude Code shell-snapshots",
      product: "Claude Code",
      category: "ai-agent",
      risk: "safe",
      source: "agent-provider",
      logical_size: 384 * MiB,
      file_count: 91,
      last_modified: daysAgo(12),
      explanation:
        "Shell environment snapshots Claude Code takes at session start. Pure cache, regenerated on demand; deleting has no lasting effect.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: 12,
          source: "path-layout",
          detail: "matched builtin-detection/claude-shell-snapshots-cache (rule 12)",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.claude\\projects\\demo-app",
      display_name: "Claude Code session history (demo-app)",
      product: "Claude Code",
      category: "session",
      risk: "review",
      source: "agent-provider",
      logical_size: 16_384_000,
      file_count: 248,
      last_modified: daysAgo(60),
      explanation:
        "Transcripts of every Claude Code conversation for demo-app. May contain context you want to keep (decisions, hard-won fixes). Review before deleting.",
      cleanup_action: {
        kind: "defer",
        reason: "Review risk class; user confirmation required before cleanup",
      },
      evidence: [
        {
          rule_id: 13,
          source: "path-layout",
          detail: "matched builtin-detection/claude-session-history (rule 13)",
        },
        {
          rule_id: null,
          source: "agent-provider",
          detail: "claude provider session transcript directory",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.codex\\sessions",
      display_name: "Codex CLI session history",
      product: "Codex CLI",
      category: "session",
      risk: "review",
      source: "agent-provider",
      logical_size: 67.7 * MiB,
      file_count: 1_204,
      last_modified: daysAgo(3),
      explanation:
        "Rolling session files for Codex CLI conversations. 67.7 MiB. Contains the actual conversation history — treat as user data until reviewed.",
      cleanup_action: {
        kind: "defer",
        reason: "Review risk class; user confirmation required before cleanup",
      },
      evidence: [
        {
          rule_id: 21,
          source: "path-layout",
          detail: "matched builtin-detection/codex-session-history (rule 21)",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.codex\\.tmp",
      display_name: "Codex CLI temp files",
      product: "Codex CLI",
      category: "temporary",
      risk: "safe",
      source: "agent-provider",
      logical_size: 18.4 * MiB,
      file_count: 37,
      last_modified: daysAgo(0.5),
      explanation:
        "Scratch working files Codex CLI regenerates per run. No persistent value; 18 MiB.",
      cleanup_action: { kind: "direct-delete" },
      evidence: [
        {
          rule_id: 20,
          source: "path-layout",
          detail: "matched builtin-detection/codex-temp (rule 20)",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Roaming\\opencode\\opencode.db",
      display_name: "OpenCode session database",
      product: "OpenCode",
      category: "session",
      risk: "review",
      source: "agent-provider",
      logical_size: 1.3 * GiB,
      file_count: 1,
      last_modified: daysAgo(1),
      explanation:
        "Single SQLite database holding OpenCode sessions and messages. 1.3 GiB of conversation history — deleting it is irreversible. Review first.",
      cleanup_action: {
        kind: "defer",
        reason: "Review risk class; user confirmation required before cleanup",
      },
      evidence: [
        {
          rule_id: 31,
          source: "path-layout",
          detail: "opencode session store detected by file name",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.claude\\projects\\demo-app\\.trash",
      display_name: "Claude Code trash cache (demo-app)",
      product: "Claude Code",
      category: "temporary",
      risk: "safe",
      source: "agent-provider",
      logical_size: 92 * MiB,
      file_count: 214,
      last_modified: daysAgo(40),
      explanation:
        "Claude Code trash/interrupt cache for one project. Safe to clean.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: 14,
          source: "path-layout",
          detail: "matched builtin-detection/claude-project-trash-cache (rule 14)",
        },
      ],
    }),

    // ---- Projects (kondo) ----
    item({
      path: "D:\\Code\\dev-cleaner\\target",
      display_name: "Rust build artifacts (dev-cleaner)",
      product: "Rust",
      category: "build-artifact",
      risk: "regenerable-local",
      source: "kondo",
      logical_size: 3.1 * GiB,
      file_count: 19_204,
      last_modified: daysAgo(21),
      explanation:
        "Rust incremental build output. 3.1 GiB, rebuilt locally by cargo build — no downloads involved. Moving to the recycle bin is fully reversible.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: null,
          source: "kondo-project-type",
          detail: "kondo detected a Rust project (Cargo.toml) at D:\\Code\\dev-cleaner",
        },
      ],
    }),
    item({
      path: "D:\\Code\\demo-app\\node_modules",
      display_name: "node_modules (demo-app)",
      product: "npm",
      category: "dependency",
      risk: "regenerable-download",
      source: "kondo",
      logical_size: 1.4 * GiB,
      file_count: 210_433,
      last_modified: daysAgo(30),
      explanation:
        "Installed dependencies for demo-app. Restorable with npm ci; the next install re-downloads all packages from the registry.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: 5,
          source: "kondo-project-type",
          detail: "node project node_modules, parent holds package.json (rule 5)",
        },
      ],
    }),
    item({
      path: "D:\\Code\\demo-app\\build",
      display_name: "CMake build directory (demo-app)",
      product: "CMake",
      category: "build-artifact",
      risk: "regenerable-local",
      source: "kondo",
      logical_size: 640 * MiB,
      file_count: 3_921,
      last_modified: daysAgo(45),
      explanation:
        "CMake/Ninja build output. Rebuilt locally from sources; deleting costs only the next compile.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: null,
          source: "kondo-project-type",
          detail: "kondo detected a CMake project at D:\\Code\\demo-app",
        },
      ],
    }),

    // ---- Protected ----
    item({
      path: "C:\\Users\\demo\\.ssh",
      display_name: "SSH keys & config",
      product: "OpenSSH",
      category: "credential",
      risk: "protected",
      source: "rule",
      logical_size: 24_576,
      file_count: 6,
      last_modified: daysAgo(400),
      explanation:
        "SSH private keys, config and known_hosts. Permanently protected by a built-in rule; DevResidue will never plan it for deletion.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: 2,
          source: "builtin-protected",
          detail: "builtin-protected/ssh (rule 2): user-profile/.ssh",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.config\\opencode",
      display_name: "OpenCode config directory",
      product: "OpenCode",
      category: "credential",
      risk: "protected",
      source: "rule",
      logical_size: 132 * KiB,
      file_count: 4,
      last_modified: daysAgo(120),
      explanation:
        "OpenCode auth.json, credentials and user config. Protected by built-in rule builtin-protected/opencode-config; never cleanable.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: 33,
          source: "builtin-protected",
          detail: "builtin-protected/opencode-config (rule 33)",
        },
      ],
    }),

    // ---- Unknown (SPEC §25: unrecognized dev-shaped data, never auto-deleted) ----
    item({
      path: "C:\\Users\\demo\\AppData\\Roaming\\some-tool\\data",
      display_name: "Unrecognised developer data (some-tool)",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 512 * MiB,
      file_count: 8_004,
      last_modified: daysAgo(70),
      explanation:
        "Found under AppData with developer-tool-like layout, but no rule or provider recognises it. Classified UNKNOWN — DevResidue never auto-deletes unknown data.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "suspected dev-data layout, no matching rule",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.qagent",
      display_name: ".qagent",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 128 * MiB,
      file_count: 942,
      last_modified: daysAgo(14),
      explanation:
        "Dot-directory in the user profile with an agent-like name, but not on the known-agent list. Classified UNKNOWN until reviewed.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "profile dot-directory, agent-shaped name, no matching rule",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\.tigerproxy",
      display_name: ".tigerproxy",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 34 * MiB,
      file_count: 61,
      last_modified: daysAgo(190),
      explanation:
        "Profile dot-directory that looks like tool state, untouched for six months. No rule recognises it — needs a human decision.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "profile dot-directory, no matching rule",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Roaming\\RustDeskTool\\sessions",
      display_name: "RustDeskTool sessions",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 240 * MiB,
      file_count: 2_310,
      last_modified: daysAgo(9),
      explanation:
        "AppData sub-directory with a session-store layout. The vendor is not in the rule set, so the data stays UNKNOWN — review before any action.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "AppData session-store layout, unknown vendor",
        },
      ],
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\Temp\\build-cache-v3",
      display_name: "build-cache-v3 (Temp)",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 780 * MiB,
      file_count: 41_066,
      last_modified: daysAgo(3),
      explanation:
        "A build-cache-shaped directory under Temp, but no owning product could be identified. UNKNOWN — decide manually.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "Temp build-cache layout, no owning product identified",
        },
      ],
    }),
  ];
}

const PROVIDER_SLUGS: Record<string, string[]> = {
  "dev-cache": ["npm", "bun", "pip", "uv", "cargo", "nuget"],
  agents: ["codex", "claude-code", "opencode", "cursor", "windsurf"],
  projects: ["kondo"],
  unknown: ["unknown-dev-data"],
  default: [
    "npm",
    "bun",
    "pip",
    "uv",
    "cargo",
    "nuget",
    "kondo",
    "codex",
    "claude-code",
    "opencode",
    "cursor",
    "windsurf",
    "unknown-dev-data",
  ],
};

interface Pending {
  resolve: (h: ScanHandleDto) => void;
  scope: ScanScope;
  cancel: boolean;
}

type Listener<T> = (payload: T) => void;

/**
 * MockBackend drives the exact same event vocabulary as the Tauri layer
 * (`scan://progress|item|warning|done`, `cleanup://item`), delivered through
 * simple subscriber lists. The scan store subscribes once at startup; the
 * mock then "streams" items with small delays.
 */
export class MockBackend implements Backend {
  readonly kind = "mock" as const;

  private snapshot: ScanSnapshotDto | null = null;
  private journal: JournalEntryDto[] = [];
  private nextScanId = 1;
  private nextPlanId = 1;
  private nextSessionId = 1;
  /** R04: snapshot generation counter (mirrors scan_store::next_generation). */
  private nextGeneration = 0;
  private active: Pending | null = null;
  /** Plans by id, so execution targets exactly what the UI planned. */
  private plans = new Map<
    number,
    { itemIds: number[]; policy: ConfirmPolicy; scanGeneration?: number | null }
  >();

  // ---- M4 mock state -------------------------------------------------------

  /** Dispositions keyed by *path* (stable across scans, unlike item ids). */
  private dispositions = new Map<string, Disposition>();
  /** Analyzer toggle — OFF by default (SPEC §26), persisted in localStorage. */
  private analyzerEnabled = false;
  private loadedSettings = false;

  constructor() {
    this.loadPersisted();
    this.seedJournal();
  }

  private loadPersisted() {
    if (this.loadedSettings) return;
    this.loadedSettings = true;
    try {
      const raw = localStorage.getItem("devresidue.dispositions.v1");
      if (raw) {
        const parsed = JSON.parse(raw) as Record<string, Disposition>;
        for (const [path, d] of Object.entries(parsed)) {
          if (d === "ignore" || d === "protect") this.dispositions.set(path, d);
          // Legacy values from the pre-alignment format migrate in place.
          else if (d === ("ignored" as Disposition)) this.dispositions.set(path, "ignore");
          else if (d === ("protected" as Disposition)) this.dispositions.set(path, "protect");
        }
      }
      const rawSettings = localStorage.getItem("devresidue.settings.backend.v1");
      if (rawSettings) {
        const parsed = JSON.parse(rawSettings) as { analyzerEnabled?: boolean };
        if (typeof parsed.analyzerEnabled === "boolean") {
          this.analyzerEnabled = parsed.analyzerEnabled;
        }
      }
    } catch {
      // Corrupt store: fall back to defaults (analyzer off).
    }
  }

  private persistDispositions() {
    try {
      const obj: Record<string, Disposition> = {};
      for (const [path, d] of this.dispositions) obj[path] = d;
      localStorage.setItem("devresidue.dispositions.v1", JSON.stringify(obj));
    } catch {
      // Storage unavailable: session-only.
    }
  }

  private persistSettings() {
    try {
      localStorage.setItem(
        "devresidue.settings.backend.v1",
        JSON.stringify({ analyzerEnabled: this.analyzerEnabled }),
      );
    } catch {
      // Storage unavailable: session-only.
    }
  }

  /** Journal seed: two historical sessions with realistic mixed outcomes. */
  private seedJournal() {
    const t0 = daysAgo(6);
    const t1 = daysAgo(1);
    let minute = 0;
    const push = (
      sessionId: number,
      timeSecs: number,
      product: string | null,
      path: string,
      action: "recycle" | "delete" | "execute",
      estimatedSize: number,
      result: string,
      error: string | null,
    ) => {
      this.journal.push({
        sessionId,
        timeSecs,
        phase: "result",
        product,
        rule: null,
        provider: null,
        path,
        action,
        estimatedSize,
        result,
        error,
      });
    };

    // Session 1 (six days ago): 11 items, one failure.
    const s1 = [
      ["npm", "C:\\Users\\demo\\AppData\\Local\\npm-cache", "execute", 1_258_291_200, "success", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\shell-snapshots", "recycle", 384 * MiB, "success", null],
      ["Rust", "D:\\Code\\dev-cleaner\\target", "recycle", 3.1 * GiB, "success", null],
      ["Codex CLI", "C:\\Users\\demo\\.codex\\.tmp", "delete", 18.4 * MiB, "success", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\projects\\demo-app\\.trash", "recycle", 92 * MiB, "success", null],
      ["OpenSSH", "C:\\Users\\demo\\.ssh", "recycle", 24_576, "skipped", "deny:protected-risk"],
      ["CMake", "D:\\Code\\demo-app\\build", "recycle", 640 * MiB, "success", null],
      ["pip", "C:\\Users\\demo\\AppData\\Local\\pip\\cache", "delete", 1.9 * GiB, "success", null],
      ["uv", "C:\\Users\\demo\\AppData\\Local\\uv\\cache", "execute", 2.8 * GiB, "failed", "process exited with code 1: uv cache clean (timeout 120s)"],
      ["cargo", "C:\\Users\\demo\\.cargo\\registry", "delete", 4.3 * GiB, "success", null],
      ["npm", "D:\\Code\\demo-app\\node_modules", "recycle", 1.4 * GiB, "success", null],
    ] as const;
    for (const [product, path, action, size, result, error] of s1) {
      push(1, t0 + minute * 60, product, path, action, size, result, error);
      minute += 1;
    }

    // Session 2 (yesterday): dry run over the npm cache + a real small one.
    minute = 0;
    const s2 = [
      ["npm", "C:\\Users\\demo\\AppData\\Local\\npm-cache", "execute", 22.2 * GiB, "dry-run", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\shell-snapshots", "recycle", 384 * MiB, "success", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\projects\\demo-app", "recycle", 16_384_000, "skipped", "deny:requires review confirmation"],
      ["OpenCode", "C:\\Users\\demo\\AppData\\Roaming\\opencode\\opencode.db", "delete", 1.3 * GiB, "skipped", "deny:unknown-risk"],
    ] as const;
    for (const [product, path, action, size, result, error] of s2) {
      push(2, t1 + minute * 60, product, path, action, size, result, error);
      minute += 1;
    }
  }

  // Event subscribers (mock event bus). Payload field names mirror the
  // backend's snake_case event wire exactly (contract.rs EV_* structs have
  // no rename_all) — a mock that drifts masks contract breaks.
  private scanProgressSubs: Listener<{
    scan_id: number;
    provider: string;
    stage: string;
  }>[] = [];
  private scanItemSubs: Listener<{ scan_id: number; item: ScanItemDto }>[] = [];
  private scanWarningSubs: Listener<{ scan_id: number; message: string }>[] = [];
  private scanDoneSubs: Listener<{
    scan_id: number;
    cancelled: boolean;
    total: number;
    generation: number;
  }>[] = [];
  private scanErrorSubs: Listener<{ scan_id: number; message: string }>[] = [];

  onScanProgress(
    fn: Listener<{ scan_id: number; provider: string; stage: string }>,
  ) {
    this.scanProgressSubs.push(fn);
    return () => this.drop(this.scanProgressSubs, fn);
  }
  onScanItem(fn: Listener<{ scan_id: number; item: ScanItemDto }>) {
    this.scanItemSubs.push(fn);
    return () => this.drop(this.scanItemSubs, fn);
  }
  onScanWarning(fn: Listener<{ scan_id: number; message: string }>) {
    this.scanWarningSubs.push(fn);
    return () => this.drop(this.scanWarningSubs, fn);
  }
  onScanDone(
    fn: Listener<{
      scan_id: number;
      cancelled: boolean;
      total: number;
      generation: number;
    }>,
  ) {
    this.scanDoneSubs.push(fn);
    return () => this.drop(this.scanDoneSubs, fn);
  }
  onScanError(fn: Listener<{ scan_id: number; message: string }>) {
    this.scanErrorSubs.push(fn);
    return () => this.drop(this.scanErrorSubs, fn);
  }

  private drop<T>(list: T[], value: T) {
    const i = list.indexOf(value);
    if (i >= 0) list.splice(i, 1);
  }

  private emitProgress(scanId: number, provider: string, stage: string) {
    for (const fn of [...this.scanProgressSubs]) fn({ scan_id: scanId, provider, stage });
  }
  private emitItem(scanId: number, item: ScanItemDto) {
    for (const fn of [...this.scanItemSubs]) fn({ scan_id: scanId, item });
  }
  private emitWarning(scanId: number, message: string) {
    for (const fn of [...this.scanWarningSubs]) fn({ scan_id: scanId, message });
  }
  private emitDone(
    scanId: number,
    cancelled: boolean,
    total: number,
    generation: number,
  ) {
    for (const fn of [...this.scanDoneSubs])
      fn({ scan_id: scanId, cancelled, total, generation });
  }
  private emitError(scanId: number, message: string) {
    for (const fn of [...this.scanErrorSubs]) fn({ scan_id: scanId, message });
  }

  async scan(scope: ScanScope): Promise<ScanHandleDto> {
    if (this.active) {
      throw toCommandError({
        code: "partial-scan",
        message: "a scan is already running",
      });
    }
    const scanId = this.nextScanId++;
    this.active = { resolve: null as never, scope, cancel: false };

    // F10 demo: the real backend can complete the scan on its worker
    // thread BEFORE the command response resolves (fast scans / second
    // consecutive scans). Opt-in flag that reproduces that ordering —
    // terminal events fire while the response is still in flight.
    if (localStorage.getItem("devresidue.demoEarlyTerminal") === "1") {
      this.snapshot = {
        scanned_at: now(),
        generation: ++this.nextGeneration,
        mode: { kind: "real", workspace_roots: ["D:\\Code"] },
        items: defaultItems().filter((it) => {
          switch (scope.kind) {
            case "agents":
              return it.source === "agent-provider" || it.source === "rule";
            case "dev-cache":
              return it.source === "developer-cache-provider";
            case "projects":
              return it.source === "kondo";
            case "unknown":
              return it.risk === "unknown";
            default:
              return true;
          }
        }),
        warnings: [],
        cancelled: false,
      };
      this.emitDone(
        scanId,
        false,
        this.snapshot.items.length,
        this.snapshot.generation,
      );
      this.active = null;
      return { scanId };
    }

    void this.runScan(scanId, scope);
    return { scanId };
  }

  private async runScan(scanId: number, scope: ScanScope): Promise<void> {
    const slugs = PROVIDER_SLUGS[scope.kind] ?? [];
    const all = defaultItems();

    // Scope filtering mirrors the CLI flag semantics.
    const keep = (it: ScanItemDto): boolean => {
      switch (scope.kind) {
        case "agents":
          return it.source === "agent-provider" || it.source === "rule";
        case "dev-cache":
          return it.source === "developer-cache-provider";
        case "projects":
          return it.source === "kondo";
        case "unknown":
          return it.risk === "unknown";
        case "default":
        default:
          return true;
      }
    };
    const items = all.filter(keep);

    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    await sleep(250);

    // R10 failure-terminal demo: the scan gate can fail before any snapshot
    // is finalised (rule-set load error in the real backend). Opt-in via
    // localStorage so normal mock runs stay green.
    if (localStorage.getItem("devresidue.demoScanFailure") === "1") {
      this.emitError(scanId, "rule gate failed: cannot load resources/rules (demo failure)");
      this.active = null;
      return;
    }

    let cursor = 0;
    for (const slug of slugs) {
      if (this.active?.cancel) break;
      this.emitProgress(scanId, slug, "started");
      await sleep(200 + Math.random() * 250);
      if (this.active?.cancel) break;

      // Stream a slice of items per provider slug (round-robin bucketing is
      // enough for a demo: split items evenly across slugs).
      const bucket = Math.ceil(items.length / slugs.length);
      const slice = items.slice(cursor, cursor + bucket);
      cursor += bucket;
      for (const it of slice) {
        if (this.active?.cancel) break;
        this.emitItem(scanId, it);
        await sleep(90 + Math.random() * 120);
      }
      this.emitProgress(scanId, slug, "done");
      if (slug === "kondo") this.emitProgress(scanId, "kondo", "project");
    }

    const cancelled = this.active?.cancel ?? false;
    if (!cancelled && scope.kind === "default") {
      this.emitWarning(
        scanId,
        "go: GOPATH not set; scanned the known default %LOCALAPPDATA%\\go-build instead",
      );
      this.emitWarning(
        scanId,
        "gradle: GRADLE_USER_HOME not set; skipped (no tool-reported cache root)",
      );
    }

    // Apply persisted dispositions to the fresh snapshot: ignored items drop
    // out entirely, protected ones flip risk (mirrors the backend's user-rule
    // overlay semantics for SPEC §25).
    const effective = (cancelled ? items.slice(0, Math.ceil(items.length / 2)) : items)
      .filter((it) => it.risk !== "unknown" || !this.dispositions.has(it.path))
      .map((it) => {
        if (it.risk === "unknown" && this.dispositions.get(it.path) === "protect") {
          return {
            ...it,
            risk: "protected" as const,
            category: "credential" as const,
            explanation:
              "Marked protected by your disposition — DevResidue will never plan it for deletion.",
            cleanup_action: { kind: "none" as const },
            evidence: [
              ...it.evidence,
              {
                rule_id: null,
                source: "user-disposition",
                detail: "protected via Unknown page disposition",
              },
            ],
          };
        }
        return it;
      });

    // R10c order (mirrors the backend fix): publish the authoritative
    // snapshot FIRST, then emit `done` — the frontend's done handler reads
    // the latest snapshot and can verify the generation advanced.
    this.snapshot = {
      scanned_at: now(),
      generation: ++this.nextGeneration,
      mode: { kind: "real", workspace_roots: ["D:\\Code"] },
      items: effective,
      warnings: cancelled
        ? []
        : [
            "go: GOPATH not set; scanned the known default %LOCALAPPDATA%\\go-build instead",
            "gradle: GRADLE_USER_HOME not set; skipped (no tool-reported cache root)",
          ],
      cancelled,
    };

    const kept = items.filter((_, i) => i < cursor || !cancelled);
    this.emitDone(scanId, cancelled, kept.length, this.snapshot?.generation ?? 0);
    this.active = null;
  }

  async cancelScan(scanId: number): Promise<boolean> {
    void scanId;
    if (this.active) {
      this.active.cancel = true;
      return true;
    }
    return false;
  }

  async getScanResults(): Promise<ScanSnapshotDto> {
    if (!this.snapshot) {
      throw toCommandError({
        code: "scan-not-found",
        message: "no scan result yet — run a scan first",
      });
    }
    return this.snapshot;
  }

  async createCleanupPlan(
    itemIds: number[],
    policy: ConfirmPolicy,
    scanGeneration?: number | null,
  ): Promise<CleanupPlanDto> {
    const snap = await this.getScanResults();
    // R3-G03: mirror the real backend's generation pin — a selection made
    // against an older generation is refused instead of re-resolved against
    // the newest snapshot's ids.
    if (scanGeneration != null && scanGeneration !== snap.generation) {
      throw toCommandError({
        code: "invalid-item",
        message:
          `selection-generation-mismatch: the selection was made against scan generation ` +
          `${scanGeneration} but the latest scan is generation ${snap.generation}; re-scan and re-select`,
      });
    }
    const byId = new Map(snap.items.map((it) => [it.id, it]));

    const planId = this.nextPlanId++;
    this.plans.set(planId, { itemIds, policy, scanGeneration });

    const planned: PlannedItemDto[] = [];
    const skipped: { scanItemId: number; path: string; reason: string }[] = [];

    for (const rawId of itemIds) {
      const it = byId.get(rawId);
      if (!it) {
        skipped.push({
          scanItemId: rawId,
          path: "(unknown)",
          reason: "skip: unknown selection",
        });
        continue;
      }
      if (it.risk === "protected") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: protected-risk",
        });
        continue;
      }
      if (it.risk === "unknown") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: unknown-risk",
        });
        continue;
      }
      if (it.cleanup_action.kind === "none") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: action none",
        });
        continue;
      }
      if (it.cleanup_action.kind === "defer") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: `skip: deferred (${it.cleanup_action.reason})`,
        });
        continue;
      }
      const confirmation =
        it.risk === "regenerable-download"
          ? "redownload"
          : it.risk === "review"
            ? "review"
            : "none";
      if (confirmation === "redownload" && policy === "default") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: requires redownload confirmation",
        });
        continue;
      }
      if (confirmation === "review" && (policy === "default" || policy === "redownload")) {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: requires review confirmation",
        });
        continue;
      }
      planned.push({
        scanItemId: it.id,
        action:
          it.cleanup_action.kind === "external-command"
            ? "execute"
            : it.cleanup_action.kind === "direct-delete"
              ? "delete"
              : "recycle",
        confirmation,
        estimatedSize: it.logical_size,
        path: it.path,
      });
    }

    return {
      planId,
      dryRun: false,
      totalEstimatedBytes: planned.reduce((s, p) => s + p.estimatedSize, 0),
      items: planned,
      skipped,
    };
  }

  async executeCleanupPlan(
    planId: number,
    policy: ConfirmPolicy,
    dryRun: boolean,
  ): Promise<CleanupSessionDto> {
    const snap = await this.getScanResults();
    const byId = new Map(snap.items.map((it) => [it.id, it]));

    // Execute exactly the persisted plan (same contract as the Rust
    // PlanStore: unknown id → PlanNotFound).
    const stored = this.plans.get(planId);
    if (!stored) {
      throw toCommandError({
        code: "plan-not-found",
        message: `cannot load plan ${planId}: unknown plan id`,
      });
    }
    const plan = await this.createCleanupPlan(stored.itemIds, policy, stored.scanGeneration);

    const sessionId = this.nextSessionId++;
    const results: SessionItemDto[] = [];
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

    for (const p of plan.items) {
      const it = byId.get(p.scanItemId);
      const status: SessionItemDto["status"] = dryRun ? "would" : "ok";
      const action = p.action;
      results.push({
        scanItemId: p.scanItemId,
        path: p.path,
        product: it?.product ?? null,
        action,
        estimatedSize: p.estimatedSize,
        status,
        detail: dryRun ? `would ${action}` : null,
      });
      await sleep(140 + Math.random() * 120);
    }

    const okItems = results.filter((r) => r.status === "ok" || r.status === "would");
    const session: CleanupSessionDto = {
      sessionId,
      planId,
      dryRun,
      totals: {
        plannedBytes: plan.totalEstimatedBytes,
        completedBytes: okItems.reduce((s, r) => s + r.estimatedSize, 0),
        succeeded: dryRun ? 0 : okItems.length,
        skipped: plan.skipped.length,
        failed: 0,
      },
      items: results,
      journalDegraded: false,
    };

    if (!dryRun) {
      this.appendJournal(session);
      // Simulate the data disappearing from subsequent scans.
      const removed = new Set(plan.items.map((p) => p.scanItemId));
      if (this.snapshot) {
        this.snapshot = {
          ...this.snapshot,
          items: this.snapshot.items.filter((it) => !removed.has(it.id)),
        };
      }
    }
    return session;
  }

  private appendJournal(session: CleanupSessionDto) {
    for (const r of session.items) {
      this.journal.push({
        sessionId: session.sessionId,
        timeSecs: now(),
        phase: "result",
        product: r.product,
        rule: null,
        provider: null,
        path: r.path,
        action: r.action,
        estimatedSize: r.estimatedSize,
        result: r.status,
        error: null,
      });
    }
  }

  async getJournal(lastN?: number): Promise<JournalEntryDto[]> {
    const limit = lastN ?? 50;
    return this.journal.slice(-limit).reverse();
  }

  async clearAllData(): Promise<number> {
    const cleared = this.journal.length;
    this.journal = [];
    // Reset every store-shaped field to first-run state (mirrors the real
    // backend: journal + snapshot + plans + in-memory latest).
    this.snapshot = null;
    this.plans.clear();
    this.nextPlanId = 1;
    this.nextSessionId = 1;
    this.nextGeneration = 0;
    return cleared;
  }

  // ---- M4 commands (wire-aligned with contract.rs) ----------------------------

  async openFolder(itemId: number): Promise<void> {
    // The real backend shells out to Explorer via the id-resolved path; the
    // mock just verifies the item exists (and would no-op the open).
    const snap = await this.getScanResults();
    if (!snap.items.some((it) => it.id === itemId)) {
      throw toCommandError({
        code: "invalid-item",
        message: `item id ${itemId} does not belong to the latest scan`,
      });
    }
  }

  async setDisposition(
    itemId: number,
    disposition: Disposition,
  ): Promise<DispositionResultDto> {
    const snap = await this.getScanResults();
    const it = snap.items.find((i) => i.id === itemId);
    if (!it) {
      throw toCommandError({
        code: "invalid-item",
        message: `item id ${itemId} does not belong to the latest scan`,
      });
    }
    if (it.risk !== "unknown") {
      throw toCommandError({
        code: "invalid-item",
        message: "dispositions apply to unknown-risk items only",
      });
    }

    this.dispositions.set(it.path, disposition);
    this.persistDispositions();
    if (disposition === "ignore") {
      this.removeSnapshotItem(itemId);
    } else {
      this.applyDispositionToSnapshot(itemId, "protected", "protect");
    }

    return {
      itemId,
      ruleId: `user-disposition/${slugOf(it.path)}`,
      path: it.path,
      effect:
        disposition === "ignore"
          ? "ignored; will not appear in future scans"
          : "protected; future scans list it as Protected",
    };
  }

  async getSettings(): Promise<AppSettingsDto> {
    this.loadPersisted();
    return { analyzerEnabled: this.analyzerEnabled };
  }

  async setAnalyzerEnabled(enabled: boolean): Promise<void> {
    this.analyzerEnabled = enabled;
    this.persistSettings();
  }

  async analyzeItem(itemId: number): Promise<AnalyzerSuggestionDto> {
    if (!this.analyzerEnabled) {
      throw toCommandError({
        code: "analyzer-disabled",
        message: "directory analyzer is disabled — enable it in Settings",
      });
    }
    const snap = await this.getScanResults();
    const it = snap.items.find((i) => i.id === itemId);
    if (!it) {
      throw toCommandError({
        code: "invalid-item",
        message: `item id ${itemId} does not belong to the latest scan`,
      });
    }

    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    await sleep(700 + Math.random() * 500);

    // Deterministic per-path suggestion so the demo tells one coherent story.
    const guess = analyzerGuess(it.path);
    return {
      itemId,
      productGuess: guess.product,
      // Wire contract: confidence is 0..=1 (rendered as % in the UI).
      confidence: guess.confidence / 100,
      category: guess.category,
      // Wire field name mirrors the real backend's SuggestionDto exactly
      // (Rust `suggested_risk` → camelCase `suggestedRisk`) — a mock that
      // drifts from the wire shape masks contract breaks in browser dev.
      suggestedRisk: guess.risk,
      explanation: guess.explanation,
      suggestedRuleId: `analyzer-suggestion/${slugOf(it.path)}`,
      path: it.path,
    };
  }

  async createRuleFromSuggestion(
    itemId: number,
    suggestedRisk: string,
  ): Promise<DispositionResultDto> {
    const snap = await this.getScanResults();
    const it = snap.items.find((i) => i.id === itemId);
    if (!it) {
      throw toCommandError({
        code: "invalid-item",
        message: `item id ${itemId} does not belong to the latest scan`,
      });
    }
    if (it.risk !== "unknown") {
      throw toCommandError({
        code: "invalid-item",
        message: "suggestions apply to unknown-risk items only",
      });
    }

    // Same persisted-rule path as set_disposition, but the risk comes from
    // the analyzer's suggestion (not the fixed ignore/protect pair).
    const risk = normalizeRisk(suggestedRisk);
    this.dispositions.set(it.path, "protect");
    this.persistDispositions();
    this.applyDispositionToSnapshot(itemId, risk, "rule");

    return {
      itemId,
      ruleId: `user-rule/${slugOf(it.path)}`,
      path: it.path,
      effect: `rule created with suggested risk “${risk}”; future scans list it as ${risk}`,
    };
  }

  async getRules(): Promise<RuleDto[]> {
    // Demo set (data/rules.ts) — the Rules page badges it as mock data.
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    await sleep(120);
    return mockRuleDtos();
  }

  async validateRules(): Promise<RulesValidationDto> {
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    await sleep(180);
    return mockRulesValidation();
  }

  /** Removes an ignored item from the live snapshot (mirrors the next scan). */
  private removeSnapshotItem(itemId: number): void {
    if (!this.snapshot) return;
    this.snapshot = {
      ...this.snapshot,
      items: this.snapshot.items.filter((x) => x.id !== itemId),
    };
  }

  /**
   * Applies a decision to the live snapshot so the UI reflects it
   * immediately (without a re-scan). `ignored` removes the item entirely;
   * any other risk re-classifies it (fixed flip or accepted suggestion).
   */
  private applyDispositionToSnapshot(
    itemId: number,
    risk: RiskLevel,
    evidenceKind: "protect" | "rule",
  ): void {
    if (!this.snapshot) return;
    if (risk === "unknown") return;
    const detail =
      evidenceKind === "rule"
        ? `classified via accepted analyzer suggestion (risk: ${risk})`
        : risk === "protected"
          ? "protected via Unknown page disposition"
          : `classified via disposition (risk: ${risk})`;
    this.snapshot = {
      ...this.snapshot,
      items: this.snapshot.items.map((x) =>
        x.id === itemId
          ? {
              ...x,
              risk,
              category: riskCategory(risk),
              explanation:
                risk === "protected"
                  ? "Marked protected by your disposition — DevResidue will never plan it for deletion."
                  : `Re-classified from an accepted suggestion (${risk}); classified by a user rule.`,
              cleanup_action: { kind: "none" as const },
              evidence: [...x.evidence, {
                rule_id: null,
                source: "user-disposition",
                detail,
              }],
            }
          : x,
      ),
    };
  }
}

/** Slug form of a Windows path for demo rule ids. */
function slugOf(path: string): string {
  return path
    .toLowerCase()
    .replace(/[\\/]+/g, "-")
    .replace(/[^a-z0-9-]/g, "")
    .replace(/^-+|-+$/g, "");
}

/** Clamps an arbitrary wire risk label to a known one. */
function normalizeRisk(risk: string): RiskLevel {
  const known: RiskLevel[] = [
    "safe",
    "regenerable-local",
    "regenerable-download",
    "review",
    "protected",
    "unknown",
  ];
  return (known as string[]).includes(risk) ? (risk as RiskLevel) : "unknown";
}

/** A plausible category for a re-classified item. */
function riskCategory(risk: RiskLevel): ScanItemDto["category"] {
  switch (risk) {
    case "safe":
      return "temporary";
    case "regenerable-local":
      return "build-artifact";
    case "regenerable-download":
      return "dependency";
    case "review":
      return "session";
    case "protected":
      return "credential";
    case "unknown":
      return "unknown";
  }
}

/** Metadata-only guess table for the mock analyzer. */
function analyzerGuess(path: string): {
  product: string;
  /** 0-100 (converted to the wire's 0..=1 before returning). */
  confidence: number;
  risk: RiskLevel;
  category: string;
  explanation: string;
} {
  const name = path.toLowerCase();
  if (name.includes("qagent")) {
    return {
      product: "QAgent CLI",
      confidence: 74,
      risk: "review",
      category: "session",
      explanation:
        "Directory name and a sessions/ sub-layout match a small-agent CLI pattern; it probably holds conversation state. Suggested review rather than safe.",
    };
  }
  if (name.includes("tigerproxy")) {
    return {
      product: "TigerProxy",
      confidence: 61,
      risk: "safe",
      category: "temporary",
      explanation:
        "Tool state untouched for six months with a lock-file layout typical of a proxy's scratch dir. Likely regenerable, but the vendor is unverified.",
    };
  }
  if (name.includes("rustdesktool")) {
    return {
      product: "RustDeskTool",
      confidence: 68,
      risk: "review",
      category: "session",
      explanation:
        "Session-store layout (many small .json files, recent writes) resembles agent session history. Treat as review until confirmed.",
    };
  }
  if (name.includes("build-cache")) {
    return {
      product: "Unknown build tool",
      confidence: 82,
      risk: "regenerable-local",
      category: "build-artifact",
      explanation:
        "Content-addressed blob layout under Temp is characteristic of a build cache; deleting costs only recompilation. Confidence is high on the class, low on the owner.",
    };
  }
  return {
    product: "some-tool",
    confidence: 55,
    risk: "review",
    category: "unknown",
    explanation:
      "AppData layout with no distinguishing markers. Default suggestion is review — not enough metadata for anything stronger.",
  };
}
