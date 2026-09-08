import type {
  AnalyzerSuggestionDto,
  AppSettingsDto,
  CleanupPlanDto,
  CleanupSessionDto,
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
import type { Backend } from "./backend";
import { toCommandError } from "./backend";

/**
 * Real backend over Tauri v2 invoke + event listen. Thin on purpose: the
 * scan store owns all interpretation of events; this class only bridges
 * them through.
 */
export class TauriBackend implements Backend {
  readonly kind = "tauri" as const;

  private invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return this.doInvoke<T>(cmd, args);
  }

  private async doInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const tauri = await import("@tauri-apps/api/core");
    try {
      return (await tauri.invoke(cmd, args)) as T;
    } catch (raw) {
      throw toCommandError(raw);
    }
  }

  async scan(scope: ScanScope): Promise<ScanHandleDto> {
    return this.invoke<ScanHandleDto>("scan", { scope });
  }

  async cancelScan(scanId: number): Promise<boolean> {
    return this.invoke<boolean>("cancel_scan", { scanId });
  }

  async getScanResults(): Promise<ScanSnapshotDto> {
    return this.invoke<ScanSnapshotDto>("get_scan_results");
  }

  async createCleanupPlan(
    itemIds: number[],
    policy: ConfirmPolicy,
    scanGeneration?: number | null,
  ): Promise<CleanupPlanDto> {
    // R3-G03: pin the selection to the generation the ids were selected
    // against — the backend refuses a cross-generation re-resolve.
    return this.invoke<CleanupPlanDto>("create_cleanup_plan", {
      itemIds,
      policy,
      scanGeneration: scanGeneration ?? null,
    });
  }

  async executeCleanupPlan(
    planId: number,
    policy: ConfirmPolicy,
    dryRun: boolean,
  ): Promise<CleanupSessionDto> {
    return this.invoke<CleanupSessionDto>("execute_cleanup_plan", {
      planId,
      policy,
      dryRun,
    });
  }

  async getJournal(lastN?: number): Promise<JournalEntryDto[]> {
    return this.invoke<JournalEntryDto[]>("get_journal", { lastN: lastN ?? null });
  }

  async clearAllData(): Promise<number> {
    return this.invoke<number>("clear_all_data");
  }

  async openFolder(itemId: number): Promise<void> {
    await this.invoke<void>("open_folder", { itemId });
  }

  async setDisposition(
    itemId: number,
    disposition: Disposition,
  ): Promise<DispositionResultDto> {
    return this.invoke<DispositionResultDto>("set_disposition", { itemId, disposition });
  }

  async getSettings(): Promise<AppSettingsDto> {
    return this.invoke<AppSettingsDto>("get_settings");
  }

  async setAnalyzerEnabled(enabled: boolean): Promise<void> {
    await this.invoke<void>("set_analyzer_enabled", { enabled });
  }

  async analyzeItem(itemId: number): Promise<AnalyzerSuggestionDto> {
    return this.invoke<AnalyzerSuggestionDto>("analyze_item", { itemId });
  }

  async createRuleFromSuggestion(
    itemId: number,
    suggestedRisk: string,
  ): Promise<DispositionResultDto> {
    return this.invoke<DispositionResultDto>("create_rule_from_suggestion", {
      itemId,
      suggestedRisk,
    });
  }

  async getRules(): Promise<RuleDto[]> {
    return this.invoke<RuleDto[]>("get_rules");
  }

  async validateRules(): Promise<RulesValidationDto> {
    return this.invoke<RulesValidationDto>("validate_rules");
  }
}
