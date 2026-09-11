import type {
  AiConfirmItemArg,
  AiConfirmResultDto,
  AiPreparedBatchDto,
  AiProfileDto,
  AiProfileInput,
  AiProfileStateDto,
  AiSuggestionDto,
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

  // ---- M4: Unknown dispositions (aligned) ---------------------------------
  //
  // Wire shape (contract.rs): all three id-referenced commands take a flat
  // `itemId` argument; `set_disposition` additionally takes the kebab-case
  // disposition value ("ignore" | "protect").

  /** Opens the item's folder in Explorer (id-referenced, never a raw path). */
  openFolder(itemId: number): Promise<void>;
  /** Persists an ignore/protect decision for an Unknown item as a user rule. */
  setDisposition(itemId: number, disposition: Disposition): Promise<DispositionResultDto>;
  // ---- Remote AI Advisor (Task 10; metadata-only / ID-only) -------------

  /** Returns profile metadata and the remote-AI master gate; never a Key. */
  listAiProfiles(): Promise<AiProfileStateDto>;
  /** `apiKey` is one-shot input and must not be retained by callers. */
  upsertAiProfile(input: AiProfileInput, apiKey: string): Promise<AiProfileDto>;
  deleteAiProfile(profileId: string): Promise<void>;
  setActiveAiProfile(profileId: string | null): Promise<AiProfileStateDto>;
  setAiMasterEnabled(enabled: boolean): Promise<AiProfileStateDto>;
  /** Minimal connectivity only; sends no scan metadata or prompt. */
  testAiConnection(profileId: string): Promise<void>;
  /** Lists only safe model IDs from one saved profile; sends no scan metadata. */
  listAiModels(profileId: string): Promise<string[]>;
  /** Prepares a consent preview for Unknown/Review item ids in one generation. */
  prepareAiBatch(
    profileId: string,
    scanGeneration: number,
    itemIds: number[],
    includePaths: boolean,
  ): Promise<AiPreparedBatchDto>;
  analyzeAiBatch(batchId: string): Promise<AiSuggestionDto[]>;
  /** Confirmation carries only batch/generation/item id/final risk/category. */
  confirmAiBatch(
    batchId: string,
    scanGeneration: number,
    items: AiConfirmItemArg[],
  ): Promise<AiConfirmResultDto>;
  cancelAiBatch(batchId: string): Promise<boolean>;
  /** Discards a prepared non-running batch so candidates can be reselected. */
  discardAiBatch(batchId: string): Promise<boolean>;

  // ---- R11: real rules API ----------------------------------------------

  /** The merged rule set the backend actually loaded (builtin + user). */
  getRules(): Promise<RuleDto[]>;
  /** Load-time validation results for the rule set. */
  validateRules(): Promise<RulesValidationDto>;
  /** Removes one user-owned rule by exact rule ID; never accepts a path. */
  deleteUserRule(ruleId: string): Promise<void>;
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
