import type {
  AiConfirmItemArg,
  AiConfirmResultDto,
  AiFinalCategory,
  AiFinalRisk,
  AiPreparedBatchDto,
  AiPreparedEntryDto,
  AiProfileDto,
  AiProfileInput,
  AiProfileStateDto,
  AiSuggestionDto,
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
  AppDataInfoDto,
  ScanScopePreviewDto,
  WorkspaceRootValidationDto,
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

type MockItem = Omit<ScanItemDto, "id" | "classification_rule_id"> &
  Partial<Pick<ScanItemDto, "classification_rule_id">>;

function item(base: MockItem): ScanItemDto {
  return { id: id(), classification_rule_id: null, ...base };
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
        "npm package download cache. Current logical size estimate: 22.2 GiB; next use may require packages to be downloaded again. Actual traffic depends on the packages used. Cleaned via the tool-native command (npm cache clean --force), not a raw delete.",
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
        "pip wheel/HTTP download cache. Low-risk candidate; current logical size estimate: 1.9 GiB. Packages may need to be downloaded again on next use; actual traffic depends on the packages used.",
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
        "Downloaded .crate files and extracted sources for dependencies cargo resolved. Current logical size estimate: 4.3 GiB; next use may require crates to be downloaded again. Actual traffic depends on the projects built.",
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
        "uv package cache. Current logical size estimate: 2.8 GiB; next use may require packages to be downloaded again. Actual traffic depends on the packages used. uv cache clean is the tool-native path.",
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
        "Claude Code trash/interrupt cache for one project. Low-risk candidate; the engine revalidates the object before cleanup.",
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
        "Rust incremental build output. Current logical size estimate: 3.1 GiB; next build can recreate it locally, with time depending on the project. Cleanup remains subject to final validation.",
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

/** The mock persists only this metadata shape — never the submitted Key. */
type StoredMockAiProfile = Omit<AiProfileDto, "isActive">;

type StoredMockAiState = {
  masterEnabled: boolean;
  activeProfileId: string | null;
  profiles: StoredMockAiProfile[];
};

type MockAiBatch = {
  batch: AiPreparedBatchDto;
  suggestions: AiSuggestionDto[];
};

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
  private persistedLoaded = false;

  // Remote-AI mock state is process-local except the non-secret profile
  // metadata written by persistAiProfiles(). No API Key field exists here.
  private aiMasterEnabled = false;
  private aiProfiles: StoredMockAiProfile[] = [];
  private activeAiProfileId: string | null = null;
  private aiBatches = new Map<string, MockAiBatch>();
  private deletedRuleIds = new Set<string>();
  private activeAiBatchId: string | null = null;
  private nextAiBatchId = 1;

  constructor() {
    this.loadPersisted();
    this.seedJournal();
  }

  private mockWorkspaceRoots(scope: ScanScope): string[] {
    if (scope.kind === "projects") return [...scope.roots];
    if (scope.kind === "default" && scope.workspace_roots) return [...scope.workspace_roots];
    return ["演示目录（Mock）"];
  }

  private loadPersisted() {
    if (this.persistedLoaded) return;
    this.persistedLoaded = true;
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
      const rawAi = localStorage.getItem("devresidue.ai-profiles.v1");
      if (rawAi) {
        const parsed = JSON.parse(rawAi) as Partial<StoredMockAiState>;
        if (typeof parsed.masterEnabled === "boolean") {
          this.aiMasterEnabled = parsed.masterEnabled;
        }
        if (typeof parsed.activeProfileId === "string" || parsed.activeProfileId === null) {
          this.activeAiProfileId = parsed.activeProfileId;
        }
        if (Array.isArray(parsed.profiles)) {
          this.aiProfiles = parsed.profiles.flatMap(readStoredMockAiProfile);
        }
      }
    } catch {
      // Corrupt persisted state: fall back to in-memory defaults.
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

  /** Deliberately serializes a non-secret allowlist rather than request input. */
  private persistAiProfiles() {
    try {
      const payload: StoredMockAiState = {
        masterEnabled: this.aiMasterEnabled,
        activeProfileId: this.activeAiProfileId,
        profiles: this.aiProfiles.map((profile) => ({
          id: profile.id,
          name: profile.name,
          baseUrl: profile.baseUrl,
          model: profile.model,
          apiProtocol: profile.apiProtocol,
          structuredOutput: profile.structuredOutput,
          enabled: profile.enabled,
        })),
      };
      localStorage.setItem("devresidue.ai-profiles.v1", JSON.stringify(payload));
    } catch {
      // Storage unavailable: metadata stays session-only; no Key fallback.
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
        mode: { kind: "real", workspace_roots: this.mockWorkspaceRoots(scope) },
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
            case "default":
              return !(scope.workspace_roots && scope.workspace_roots.length === 0 && it.source === "kondo");
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
    const slugs = scope.kind === "default" && scope.workspace_roots?.length === 0
      ? (PROVIDER_SLUGS.default ?? []).filter((slug) => slug !== "kondo")
      : PROVIDER_SLUGS[scope.kind] ?? [];
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
          return !(scope.workspace_roots && scope.workspace_roots.length === 0 && it.source === "kondo");
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
      mode: { kind: "real", workspace_roots: this.mockWorkspaceRoots(scope) },
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

  async pickWorkspaceDirectory(): Promise<string | null> {
    // Browser demo has no native directory picker; keep the manual field
    // authoritative and make cancellation a no-op.
    return null;
  }

  async validateWorkspaceRoots(roots: string[]): Promise<WorkspaceRootValidationDto[]> {
    const normalized = roots.map((input) => {
      const value = input.trim().replace(/[\\/]+$/, "");
      const absolute = /^[A-Za-z]:[\\/]/.test(value) || value.startsWith("\\\\");
      return { input, value: value || null, absolute };
    });
    return normalized.map(({ input, value, absolute }, index) => {
      const duplicate = value !== null && normalized.some((other, otherIndex) => otherIndex < index && other.value?.toLocaleLowerCase() === value.toLocaleLowerCase());
      const containedBy = value === null ? null : normalized.slice(0, index).find((other) => other.value && value.toLocaleLowerCase().startsWith(`${other.value.toLocaleLowerCase()}\\`))?.value ?? null;
      return {
        input,
        normalized: value,
        valid: Boolean(value && absolute && !duplicate),
        errorCode: !value ? "blank" : !absolute ? "not-absolute" : duplicate ? "duplicate" : null,
        message: !value ? "目录不能为空" : !absolute ? "请输入绝对目录路径" : duplicate ? "与前面的目录重复" : null,
        duplicate,
        containedBy,
      };
    });
  }

  async getScanScopePreview(scope: ScanScope): Promise<ScanScopePreviewDto> {
    const providers = scope.kind === "agents"
      ? ["Agent 数据"]
      : scope.kind === "dev-cache"
        ? ["工具缓存"]
        : scope.kind === "projects"
          ? ["Kondo 项目"]
          : scope.kind === "unknown"
            ? ["未知开发数据"]
            : scope.workspace_roots?.length === 0
              ? ["工具缓存", "Agent 数据", "未知开发数据"]
              : ["工具缓存", "Kondo 项目", "Agent 数据", "未知开发数据"];
    return {
      scope,
      providers,
      workspaceRoots: this.mockWorkspaceRoots(scope),
      knownLocations: [],
      deferredLocations: ["外部工具报告的缓存目录"],
      warnings: ["当前为演示后端；未访问本机目录。"],
    };
  }

  async getAppDataInfo(): Promise<AppDataInfoDto> {
    return { dataDir: "演示数据目录（Mock）", backendMode: "mock" };
  }

  async clearJournal(): Promise<{ removedShardCount: number }> {
    const removedShardCount = this.journal.length > 0 ? 1 : 0;
    this.journal = [];
    return { removedShardCount };
  }

  async resetScanData(): Promise<{ removedPlanCount: number; hadSnapshot: boolean }> {
    const removedPlanCount = this.plans.size;
    const hadSnapshot = this.snapshot !== null;
    this.snapshot = null;
    this.plans.clear();
    this.clearAiBatches();
    // Keep nextPlanId/nextGeneration monotonic so reset cannot resurrect a
    // stale plan or generation. Journal, dispositions, preferences and AI
    // profile metadata intentionally remain intact.
    return { removedPlanCount, hadSnapshot };
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

  async listAiProfiles(): Promise<AiProfileStateDto> {
    this.loadPersisted();
    return this.aiProfileState();
  }

  async upsertAiProfile(input: AiProfileInput, apiKey: string): Promise<AiProfileDto> {
    // The mock intentionally has no Key store. Keeping this explicit makes a
    // browser-dev implementation unable to drift into localStorage leakage.
    void apiKey;
    const name = input.name.trim();
    const baseUrl = input.baseUrl.trim();
    const model = input.model.trim();
    if (!name || !baseUrl || !model || !Number.isSafeInteger(input.timeoutSecs) || input.timeoutSecs < 1) {
      throw aiMockError("ai-not-configured", "远程 AI 配置不完整");
    }

    const id = input.profileId ?? `mock-ai-${Date.now()}-${this.aiProfiles.length + 1}`;
    const profile: StoredMockAiProfile = {
      id,
      name,
      baseUrl,
      model,
      apiProtocol: input.apiProtocol,
      structuredOutput: input.structuredOutput,
      enabled: input.enabled,
    };
    const index = this.aiProfiles.findIndex((candidate) => candidate.id === id);
    if (index >= 0) this.aiProfiles[index] = profile;
    else this.aiProfiles.push(profile);
    if (this.activeAiProfileId === null) this.activeAiProfileId = id;
    this.clearAiBatches();
    this.persistAiProfiles();
    return this.profileDto(profile);
  }

  async deleteAiProfile(profileId: string): Promise<void> {
    const before = this.aiProfiles.length;
    this.aiProfiles = this.aiProfiles.filter((profile) => profile.id !== profileId);
    if (this.aiProfiles.length === before) {
      throw aiMockError("ai-not-configured", "远程 AI 配置不可用");
    }
    if (this.activeAiProfileId === profileId) this.activeAiProfileId = null;
    this.clearAiBatches();
    this.persistAiProfiles();
  }

  async setActiveAiProfile(profileId: string | null): Promise<AiProfileStateDto> {
    if (profileId !== null && !this.aiProfiles.some((profile) => profile.id === profileId)) {
      throw aiMockError("ai-not-configured", "远程 AI 配置不可用");
    }
    this.activeAiProfileId = profileId;
    this.clearAiBatches();
    this.persistAiProfiles();
    return this.aiProfileState();
  }

  async setAiMasterEnabled(enabled: boolean): Promise<AiProfileStateDto> {
    this.aiMasterEnabled = enabled;
    if (!enabled) this.clearAiBatches();
    this.persistAiProfiles();
    return this.aiProfileState();
  }

  async testAiConnection(profileId: string): Promise<void> {
    this.requireReadyAiProfile(profileId);
    // Fixture-only success: this intentionally performs no fetch and sees no
    // scan item, profile Key or request payload.
  }

  async listAiModels(profileId: string): Promise<string[]> {
    const profile = this.aiProfiles.find((candidate) => candidate.id === profileId);
    if (!profile || !profile.enabled) {
      throw aiMockError("ai-not-configured", "远程 AI 配置不可用");
    }
    // Fixture-only deterministic discovery. Like the real backend this does
    // not depend on the analysis master switch and receives no scan metadata.
    return ["gpt-4.1-mini", "gpt-5.6-luna", "gpt-5.6-sol"];
  }

  async prepareAiBatch(
    profileId: string,
    scanGeneration: number,
    itemIds: number[],
    includePaths: boolean,
  ): Promise<AiPreparedBatchDto> {
    this.requireReadyAiProfile(profileId);
    const snapshot = await this.getScanResults();
    if (snapshot.generation !== scanGeneration) {
      throw aiMockError("ai-batch-expired", "远程 AI 批次不再匹配当前扫描");
    }
    if (itemIds.length === 0 || new Set(itemIds).size !== itemIds.length) {
      throw toCommandError({ code: "invalid-item", message: "请选择不重复的可研判条目" });
    }
    const byId = new Map(snapshot.items.map((item) => [item.id, item]));
    const selected = itemIds.map((id) => byId.get(id));
    if (
      selected.some(
        (item) => item === undefined || (item.risk !== "unknown" && item.risk !== "review"),
      )
    ) {
      throw toCommandError({
        code: "invalid-item",
        message: "仅 Unknown 或 Review 条目可进入远程 AI 研判",
      });
    }

    const entries = selected.map((item) => preparedMockEntry(item!));
    const batch: AiPreparedBatchDto = {
      batchId: `mock-ai-batch-${this.nextAiBatchId++}`,
      scanGeneration,
      profileId,
      includesPaths: includePaths,
      entries,
    };
    this.aiBatches.set(batch.batchId, {
      batch,
      suggestions: entries.map(mockAiSuggestion),
    });
    return batch;
  }

  async analyzeAiBatch(batchId: string): Promise<AiSuggestionDto[]> {
    const batch = this.aiBatches.get(batchId);
    if (!batch) throw aiMockError("ai-batch-expired", "远程 AI 批次不再匹配当前扫描");
    if (this.activeAiBatchId !== null && this.activeAiBatchId !== batchId) {
      throw aiMockError("ai-batch-in-progress", "已有远程 AI 研判正在进行");
    }
    this.activeAiBatchId = batchId;
    try {
      // This is deliberately a deterministic local fixture, not a simulated
      // remote service. It proves review UI behavior without sending data.
      return batch.suggestions.map((suggestion) => ({ ...suggestion }));
    } finally {
      this.activeAiBatchId = null;
    }
  }

  async confirmAiBatch(
    batchId: string,
    scanGeneration: number,
    items: AiConfirmItemArg[],
  ): Promise<AiConfirmResultDto> {
    const stored = this.aiBatches.get(batchId);
    if (!stored || stored.batch.scanGeneration !== scanGeneration) {
      throw aiMockError("ai-batch-expired", "远程 AI 批次不再匹配当前扫描");
    }
    if (items.length === 0 || new Set(items.map((item) => item.itemId)).size !== items.length) {
      throw toCommandError({ code: "invalid-item", message: "请选择不重复的建议后再确认" });
    }
    const eligible = new Set(stored.suggestions.map((suggestion) => suggestion.itemId));
    if (items.some((item) =>
      !eligible.has(item.itemId)
      || !isAiFinalRisk(item.finalRisk)
      || !isAiFinalCategory(item.finalCategory),
    )) {
      throw toCommandError({ code: "invalid-item", message: "确认内容不属于当前远程 AI 批次" });
    }

    const latest = await this.getScanResults();
    if (latest.generation !== scanGeneration) {
      throw aiMockError("ai-batch-expired", "远程 AI 批次不再匹配当前扫描");
    }
    const finalRisks = new Map(items.map((item) => [item.itemId, item.finalRisk]));
    const finalCategories = new Map(items.map((item) => [item.itemId, item.finalCategory]));
    this.snapshot = {
      ...latest,
      items: latest.items.map((item) => {
        const finalRisk = finalRisks.get(item.id);
        const finalCategory = finalCategories.get(item.id);
        if (!finalRisk || !finalCategory) return item;
        return {
          ...item,
          risk: finalRisk,
          category: finalCategory,
          // Like the real Core transaction, mock confirmation classifies but
          // never builds or executes a cleanup plan.
          cleanup_action: { kind: "none" as const },
          explanation: "由你审阅远程 AI 建议后创建的本地分类规则。",
          evidence: [
            ...item.evidence,
            { rule_id: null, source: "ai-advisor", detail: "user-confirmed fixture classification" },
          ],
        };
      }),
    };
    this.aiBatches.delete(batchId);
    return { confirmedCount: items.length, auditWarning: false };
  }

  async cancelAiBatch(batchId: string): Promise<boolean> {
    if (this.activeAiBatchId !== batchId) return false;
    this.activeAiBatchId = null;
    return true;
  }

  async discardAiBatch(batchId: string): Promise<boolean> {
    if (this.activeAiBatchId === batchId || !this.aiBatches.has(batchId)) return false;
    this.aiBatches.delete(batchId);
    return true;
  }

  async getRules(): Promise<RuleDto[]> {
    // Demo set (data/rules.ts) — the Rules page badges it as mock data.
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    await sleep(120);
    return mockRuleDtos().filter((rule) => !this.deletedRuleIds.has(rule.ruleId));
  }

  async validateRules(): Promise<RulesValidationDto> {
    const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
    await sleep(180);
    return mockRulesValidation();
  }

  async deleteUserRule(ruleId: string): Promise<void> {
    const rule = mockRuleDtos().find((candidate) => candidate.ruleId === ruleId);
    if (!rule || (rule.source !== "user" && rule.source !== "user-protected")) {
      throw toCommandError({ code: "engine", message: "仅可删除用户新建的规则" });
    }
    this.deletedRuleIds.add(ruleId);
  }

  private aiProfileState(): AiProfileStateDto {
    return {
      masterEnabled: this.aiMasterEnabled,
      profiles: this.aiProfiles.map((profile) => this.profileDto(profile)),
    };
  }

  private profileDto(profile: StoredMockAiProfile): AiProfileDto {
    return {
      id: profile.id,
      name: profile.name,
      baseUrl: profile.baseUrl,
      model: profile.model,
      apiProtocol: profile.apiProtocol,
      structuredOutput: profile.structuredOutput,
      enabled: profile.enabled,
      isActive: profile.id === this.activeAiProfileId,
    };
  }

  private requireReadyAiProfile(profileId: string): StoredMockAiProfile {
    if (!this.aiMasterEnabled) {
      throw aiMockError("ai-disabled", "远程 AI 已关闭");
    }
    const profile = this.aiProfiles.find((candidate) => candidate.id === profileId);
    if (!profile || !profile.enabled || this.activeAiProfileId !== profileId) {
      throw aiMockError("ai-not-configured", "远程 AI 配置不可用");
    }
    return profile;
  }

  private clearAiBatches(): void {
    this.activeAiBatchId = null;
    this.aiBatches.clear();
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
        ? `classified via accepted remote AI review (risk: ${risk})`
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

/** Strictly projects persisted mock metadata, discarding unknown fields. */
function readStoredMockAiProfile(value: unknown): StoredMockAiProfile[] {
  if (typeof value !== "object" || value === null) return [];
  const candidate = value as Record<string, unknown>;
  if (
    typeof candidate.id !== "string" ||
    typeof candidate.name !== "string" ||
    typeof candidate.baseUrl !== "string" ||
    typeof candidate.model !== "string" ||
    typeof candidate.enabled !== "boolean"
  ) {
    return [];
  }
  return [
    {
      id: candidate.id,
      name: candidate.name,
      baseUrl: candidate.baseUrl,
      model: candidate.model,
      apiProtocol: candidate.apiProtocol === "openai-responses" ? "openai-responses" : "openai-compatible",
      structuredOutput:
        candidate.structuredOutput === "json-schema" || candidate.structuredOutput === "json-object"
          ? candidate.structuredOutput
          : "auto",
      enabled: candidate.enabled,
    },
  ];
}

function aiMockError(
  code: "ai-disabled" | "ai-not-configured" | "ai-batch-in-progress" | "ai-batch-expired",
  message: string,
) {
  return toCommandError({ code, message });
}

/** Maps a fixture item to the same path-free consent vocabulary as Tauri. */
function preparedMockEntry(item: ScanItemDto): AiPreparedEntryDto {
  const ageDays = item.last_modified === null ? null : (now() - item.last_modified) / DAY;
  return {
    itemId: item.id,
    zone: "user-data",
    relativeDepth: 2,
    displayName: item.display_name,
    sourceKind: item.source,
    categoryHint: item.category,
    productHint: item.product,
    sizeBucket: sizeBucket(item.logical_size),
    ageBucket: ageBucket(ageDays),
    signals: ["local-fixture", item.risk === "review" ? "review-risk" : "unknown-risk"],
  };
}

/** Deterministic local fixture, intentionally independent of item.path. */
function mockAiSuggestion(entry: AiPreparedEntryDto): AiSuggestionDto {
  const suggestedRisk: AiFinalRisk =
    entry.categoryHint === "session" || entry.signals.includes("review-risk") ? "review" : "safe";
  return {
    itemId: entry.itemId,
    suggestedRisk,
    confidence: suggestedRisk === "review" ? 0.68 : 0.61,
    reason:
      suggestedRisk === "review"
        ? "演示夹具：会话或既有 Review 信号需保守处理，请逐项决定最终风险。"
        : "演示夹具：仅根据已展示的脱敏元数据给出低置信度建议，请逐项决定最终风险。",
    productGuess: entry.productHint,
    finalRiskOptions: [
      "safe",
      "regenerable-local",
      "regenerable-download",
      "review",
      "protected",
    ],
  };
}

function isAiFinalRisk(value: string): value is AiFinalRisk {
  return (
    value === "safe" ||
    value === "regenerable-local" ||
    value === "regenerable-download" ||
    value === "review" ||
    value === "protected"
  );
}

function isAiFinalCategory(value: string): value is AiFinalCategory {
  return value !== "unknown" && [
    "ai-agent",
    "ide",
    "developer-cache",
    "package-cache",
    "build-artifact",
    "dependency",
    "log",
    "temporary",
    "session",
    "workspace-state",
    "configuration",
    "credential",
  ].includes(value);
}

function sizeBucket(size: number): string {
  if (size < MiB) return "under-1MiB";
  if (size < 100 * MiB) return "1MiB-100MiB";
  if (size < GiB) return "100MiB-1GiB";
  return "over-1GiB";
}

function ageBucket(days: number | null): string {
  if (days === null) return "unknown";
  if (days < 7) return "under-7d";
  if (days < 30) return "7d-30d";
  if (days < 90) return "30d-90d";
  return "over-90d";
}

/** Slug form of a Windows path for demo rule ids. */
function slugOf(path: string): string {
  return path
    .toLowerCase()
    .replace(/[\\/]+/g, "-")
    .replace(/[^a-z0-9-]/g, "")
    .replace(/^-+|-+$/g, "");
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
