/**
 * Wire-format types, aligned 1:1 with `src-tauri/src/contract.rs` and the
 * core domain serde attributes. Field casing is NOT uniform on purpose:
 *
 *  - `ScanItemDto` / `ScanSnapshotDto` serialise snake_case (core serde
 *    default, stable since the CLI `scan --json` contract).
 *  - `CleanupPlanDto` / `CleanupSessionDto` / `JournalEntryDto` serialise
 *    camelCase (`rename_all = "camelCase"` in contract.rs).
 *  - enum values are kebab-case strings (`"regenerable-download"` etc.).
 *  - ids are plain numbers (transparent newtypes in Rust).
 */

// ---- Risk ------------------------------------------------------------------

export type RiskLevel =
  | "safe"
  | "regenerable-local"
  | "regenerable-download"
  | "review"
  | "protected"
  | "unknown";

export const RISK_LEVELS: readonly RiskLevel[] = [
  "safe",
  "regenerable-local",
  "regenerable-download",
  "review",
  "protected",
  "unknown",
] as const;

// ---- Categories / sources (kebab-case on the wire) --------------------------

export type ResidueCategory =
  | "ai-agent"
  | "ide"
  | "developer-cache"
  | "package-cache"
  | "build-artifact"
  | "dependency"
  | "log"
  | "temporary"
  | "session"
  | "workspace-state"
  | "configuration"
  | "credential"
  | "unknown";

export type SourceKind =
  | "rule"
  | "kondo"
  | "developer-cache-provider"
  | "package-manager"
  | "agent-provider"
  | "ai-analysis";

// ---- Scan DTOs (snake_case, mirrors core `ScanItem` serde) -------------------

export type ExternalCommandSpec = {
  executable: string;
  args: string[];
  working_directory: string | null;
  timeout_secs: number | null;
};

/** Internally tagged: `{"kind":"recycle-bin"}` … `{"kind":"external-command",…}`. */
export type CleanupAction =
  | { kind: "none" }
  | { kind: "recycle-bin" }
  | { kind: "direct-delete" }
  | {
      kind: "external-command";
      command: ExternalCommandSpec;
    }
  | { kind: "defer"; reason: string };

export type Evidence = {
  /** Numeric rule id (`RuleId`), when the rule engine produced this evidence. */
  rule_id: number | null;
  /** Evidence kind, e.g. "path-layout" | "builtin-protected" | "kondo-project-type". */
  source: string;
  detail: string;
};

export type ScanItemDto = {
  id: number;
  path: string;
  display_name: string;
  product: string | null;
  category: ResidueCategory;
  risk: RiskLevel;
  source: SourceKind;
  logical_size: number;
  file_count: number;
  /** Unix epoch seconds or null. */
  last_modified: number | null;
  explanation: string;
  cleanup_action: CleanupAction;
  evidence: Evidence[];
  /**
   * UI-side overlap annotation (not part of the core wire format yet): a
   * human list of display names this item's tree intersects. Null when no
   * overlap was detected.
   */
  overlaps?: string | null;
};

export type ScanMode =
  | { kind: "real"; workspace_roots: string[] }
  | { kind: "fixtures" };

export type ScanSnapshotDto = {
  /** Unix epoch seconds. */
  scanned_at: number;
  /**
   * Scan generation (R04): increments on every persisted snapshot for this
   * data root. `(generation, item.id)` is the stable cross-scan object key;
   * the UI clears selections/plans/suggestions when it advances.
   */
  generation: number;
  mode: ScanMode;
  items: ScanItemDto[];
  warnings: string[];
  cancelled: boolean;
};

// ---- Plan / session DTOs (camelCase, mirrors contract.rs) --------------------

export type ScanHandleDto = { scanId: number };

/** `none | redownload | review` on the wire. */
export type Confirmation = "none" | "redownload" | "review";

/** `recycle | delete | execute` on the wire. */
export type PlanAction = "recycle" | "delete" | "execute";

export type PlannedItemDto = {
  scanItemId: number;
  action: PlanAction;
  confirmation: Confirmation;
  estimatedSize: number;
  path: string;
};

export type SkippedItemDto = {
  scanItemId: number;
  path: string;
  reason: string;
};

export type CleanupPlanDto = {
  planId: number;
  dryRun: boolean;
  totalEstimatedBytes: number;
  items: PlannedItemDto[];
  skipped: SkippedItemDto[];
};

export type SessionTotalsDto = {
  plannedBytes: number;
  completedBytes: number;
  succeeded: number;
  skipped: number;
  failed: number;
};

/** `ok | would | skipped | failed` on the wire. */
export type SessionItemStatus = "ok" | "would" | "skipped" | "failed";

export type SessionItemDto = {
  scanItemId: number;
  path: string;
  product: string | null;
  action: PlanAction;
  estimatedSize: number;
  status: SessionItemStatus;
  detail: string | null;
};

export type CleanupSessionDto = {
  sessionId: number;
  planId: number;
  dryRun: boolean;
  totals: SessionTotalsDto;
  items: SessionItemDto[];
  journalDegraded: boolean;
};

export type JournalEntryDto = {
  sessionId: number;
  timeSecs: number;
  phase: "attempt" | "result";
  product: string | null;
  rule: number | null;
  provider: number | null;
  path: string | null;
  action: PlanAction | null;
  estimatedSize: number;
  result: string | null;
  error: string | null;
};

// ---- Requests / errors -------------------------------------------------------

export type ScanScope =
  | { kind: "agents" }
  | { kind: "dev-cache" }
  | { kind: "projects"; roots: string[] }
  | { kind: "unknown" }
  | { kind: "default" };

export type ConfirmPolicy = "default" | "redownload" | "review" | "all";

/** kebab-case on the wire: `confirmation-required`, `scan-not-found`, … */
export type ErrorCode =
  | "confirmation-required"
  | "scan-not-found"
  | "plan-not-found"
  | "partial-scan"
  | "invalid-item"
  | "analyzer-disabled"
  | "engine";

export type CommandError = {
  code: ErrorCode;
  message: string;
  /** Confirmation level the plan requires when code === "confirmation-required". */
  required?: Confirmation;
};

// ---- Event payloads (contract.rs EV_* shapes) --------------------------------
//
// NOTE: event payloads serialise snake_case (contract.rs structs carry no
// rename_all) — unlike the command DTOs above. Mock events must use the
// same field names or browser dev masks wire breaks (F-M4-2 lesson).

/** scan://progress payload. */
export type ProgressPayload = {
  scan_id: number;
  provider: string;
  /** started | done | project */
  stage: string;
};

/** scan://item payload. */
export type ScanItemPayload = { scan_id: number; item: ScanItemDto };

/** scan://warning payload. */
export type WarningPayload = { scan_id: number; message: string };

/**
 * scan://done payload (R10): `generation` correlates the event with the
 * exact persisted snapshot — assigned before saving, stamped into the
 * snapshot; a stale done event can be discarded by comparing it with
 * `get_scan_results().generation`.
 */
export type ScanDonePayload = {
  scan_id: number;
  cancelled: boolean;
  total: number;
  generation: number;
};

/** scan://error payload (R10): worker failed before a snapshot finalised. */
export type ScanErrorPayload = { scan_id: number; message: string };

/** cleanup://item payload. */
export type CleanupItemPayload = { planId: number; item: SessionItemDto };

export const SCAN_EVENTS = {
  progress: "scan://progress",
  item: "scan://item",
  done: "scan://done",
  warning: "scan://warning",
  error: "scan://error",
} as const;

export const CLEANUP_EVENTS = {
  item: "cleanup://item",
} as const;

// ---- UI-side enums (derived, not on the wire) --------------------------------

/** Dashboard groups; `unknown` renders but is not clickable into cleanup flows. */
export type RiskGroup = RiskLevel;

export const RISK_GROUP_ORDER: readonly RiskGroup[] = [
  "safe",
  "regenerable-local",
  "regenerable-download",
  "review",
  "protected",
  "unknown",
] as const;

/** A page-level filter over items: category face or risk face. */
export type ItemFilter =
  | { kind: "category"; categories: ResidueCategory[]; subtabs?: SubtabDef[] }
  | { kind: "risk"; risk: RiskLevel; subtabs?: SubtabDef[] };

export type SubtabDef = {
  key: string;
  label: string;
  categories: ResidueCategory[];
};

// ---- M4: settings / dispositions / analyzer (aligned with contract.rs) ------
//
// Wire names verified against src-tauri/src/contract.rs:
//   - DispositionArg kebab-case: "ignore" | "protect" (flat command args)
//   - DispositionResultDto camelCase: itemId / ruleId(string) / path / effect
//   - SuggestionDto camelCase: itemId / productGuess / confidence(0..1 f32) /
//     category / suggestedRisk / explanation / suggestedRuleId / path
//     (Rust field `suggested_risk` + rename_all = "camelCase" → `suggestedRisk`)
//   - SettingsDto camelCase: analyzerEnabled
//   - ErrorCode has "analyzer-disabled" (SPEC §26 default off)

/** `get_settings` result; `set_analyzer_enabled` returns void on success. */
export type AppSettingsDto = {
  /** AI analyzer toggle (SPEC §26: default off, suggestions only). */
  analyzerEnabled: boolean;
};

/** Wire values of `DispositionArg` (kebab-case command argument). */
export type Disposition = "ignore" | "protect";

/** `set_disposition` result (rule written, effect preview). */
export type DispositionResultDto = {
  itemId: number;
  /** The persisted rule's string id (user-rules file slug). */
  ruleId: string;
  path: string;
  /** Human sentence: what the next scan will show. */
  effect: string;
};

/**
 * `analyze_item` result — a suggestion, never an action (SPEC §26: the AI
 * has no delete/plan authority; the user must accept it explicitly).
 */
export type AnalyzerSuggestionDto = {
  itemId: number;
  productGuess: string | null;
  /** 0..=1 (wire f32; the UI renders it as a percentage). */
  confidence: number;
  /** kebab-case category label, e.g. "build-artifact". */
  category: string;
  /**
   * kebab-case risk label, e.g. "regenerable-local".
   * Wire name is `suggestedRisk` (Rust `suggested_risk`, camelCase serde).
   */
  suggestedRisk: string;
  explanation: string;
  /** The analyzer's own rule slug for this suggestion, when known. */
  suggestedRuleId: string | null;
  path: string;
};

// ---- R11: rules API (aligned with contract.rs, batch 3) ----------------------
//
// Wire shapes verified against src-tauri/src/contract.rs (camelCase serde):
//   - RuleDto: ruleId / source? / risk? / category? / description? /
//     valid / issues[] — metadata fields are null for rules whose
//     declaration failed to load (they are returned as valid: false).
//   - RuleIssueDto: ruleId (null = file-level finding) / message /
//     severity ("error" | "warning").
//   - RulesValidationDto: total (loaded rules only) / errors / warnings /
//     issues (errors first, then warnings).

/** One entry of the merged, loaded rule set (get_rules result). */
export type RuleDto = {
  /** Rule slug, e.g. "builtin-protected/ssh", "user-protected/…". */
  ruleId: string;
  /** Source label; null for invalid rules (declaration did not load). */
  source: string | null;
  /** Kebab-case risk; null for invalid rules. */
  risk: RiskLevel | null;
  /** Kebab-case category; null for invalid rules. */
  category: string | null;
  description: string | null;
  /** Whether this rule passed load-time validation. */
  valid: boolean;
  /** Validation findings that reference this rule (messages). */
  issues: string[];
};

/** One validation finding (validate_rules result entries). */
export type RuleIssueDto = {
  /** Rule slug when the finding is rule-specific; null = file-level. */
  ruleId: string | null;
  message: string;
  severity: "error" | "warning";
};

/** validate_rules result: aggregate health of the loaded rule set. */
export type RulesValidationDto = {
  /** Rules that loaded and validated (invalid ones are NOT counted here). */
  total: number;
  errors: number;
  warnings: number;
  issues: RuleIssueDto[];
};
