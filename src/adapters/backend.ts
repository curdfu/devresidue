import type {
  AnalyzerSuggestionDto,
  AppSettingsDto,
  CleanupPlanDto,
  CleanupSessionDto,
  CommandError,
  ConfirmPolicy,
  Disposition,
  DispositionResultDto,
  JournalEntryDto,
  RuleDto,
  RulesValidationDto,
  ScanHandleDto,
  ScanScope,
  ScanSnapshotDto,
} from "@/types";

/**
 * Backend port — the ONLY place that knows how data arrives. Two
 * implementations: `TauriBackend` (real invoke/events) and `MockBackend`
 * (in-memory, CLI-fixture-shaped). Swapping them never touches UI semantics
 * (the integration contract for Phase 12).
 */
export interface Backend {
  readonly kind: "tauri" | "mock";

  /** Fire a scan; UI then listens on the event stream. */
  scan(scope: ScanScope): Promise<ScanHandleDto>;
  cancelScan(scanId: number): Promise<boolean>;
  /** Most recent finished snapshot (may be partial/cancelled). */
  getScanResults(): Promise<ScanSnapshotDto>;
  createCleanupPlan(
    itemIds: number[],
    policy: ConfirmPolicy,
    /** R3-G03: the scan generation the selection was made against. */
    scanGeneration?: number | null,
  ): Promise<CleanupPlanDto>;
  executeCleanupPlan(
    planId: number,
    policy: ConfirmPolicy,
    dryRun: boolean,
  ): Promise<CleanupSessionDto>;
  getJournal(lastN?: number): Promise<JournalEntryDto[]>;
  /**
   * 清空 (log page): wipes journal shards, the persisted scan snapshot and
   * the plan store, then resets the in-memory model — the whole app returns
   * to first-run state. Never touches user data.
   */
  clearAllData(): Promise<number>;

  // ---- M4: unknown dispositions / settings / analyzer (aligned) ----------
  //
  // Wire shape (contract.rs): all three id-referenced commands take a flat
  // `itemId` argument; `set_disposition` additionally takes the kebab-case
  // disposition value ("ignore" | "protect").

  /** Opens the item's folder in Explorer (id-referenced, never a raw path). */
  openFolder(itemId: number): Promise<void>;
  /** Persists an ignore/protect decision for an Unknown item as a user rule. */
  setDisposition(itemId: number, disposition: Disposition): Promise<DispositionResultDto>;
  /** Reads the app settings (analyzer toggle etc.). */
  getSettings(): Promise<AppSettingsDto>;
  /** Flips the AI analyzer switch (default off, SPEC §26). Returns void. */
  setAnalyzerEnabled(enabled: boolean): Promise<void>;
  /** Runs metadata-only analysis on one Unknown item (suggestions only). */
  analyzeItem(itemId: number): Promise<AnalyzerSuggestionDto>;
  /**
   * Turns an accepted suggestion into a user rule carrying the suggested
   * risk (backend command `create_rule_from_suggestion`; registered
   * separately from set_disposition because the risk comes from the
   * analyzer's proposal, not the fixed ignore/protect pair).
   */
  createRuleFromSuggestion(itemId: number, suggestedRisk: string): Promise<DispositionResultDto>;

  // ---- R11: real rules API ----------------------------------------------

  /** The merged rule set the backend actually loaded (builtin + user). */
  getRules(): Promise<RuleDto[]>;
  /** Load-time validation results for the rule set. */
  validateRules(): Promise<RulesValidationDto>;
}

/** Turns a raw invoke rejection into the typed `CommandError` shape. */
export function toCommandError(raw: unknown): CommandError {
  if (typeof raw === "object" && raw !== null && "code" in raw && "message" in raw) {
    return raw as CommandError;
  }
  return { code: "engine", message: String(raw) };
}

/** Returns true when the given error means "nothing scanned yet". */
export function isScanNotFound(err: CommandError): boolean {
  return err.code === "scan-not-found";
}
