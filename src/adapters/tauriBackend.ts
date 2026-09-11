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

  async listAiProfiles(): Promise<AiProfileStateDto> {
    return this.invoke<AiProfileStateDto>("ai_list_profiles");
  }

  async upsertAiProfile(input: AiProfileInput, apiKey: string): Promise<AiProfileDto> {
    return this.invoke<AiProfileDto>("ai_upsert_profile", {
      profileId: input.profileId,
      name: input.name,
      baseUrl: input.baseUrl,
      model: input.model,
      structuredOutput: input.structuredOutput,
      timeoutSecs: input.timeoutSecs,
      enabled: input.enabled,
      apiKey,
    });
  }

  async deleteAiProfile(profileId: string): Promise<void> {
    await this.invoke<void>("ai_delete_profile", { profileId });
  }

  async setActiveAiProfile(profileId: string | null): Promise<AiProfileStateDto> {
    return this.invoke<AiProfileStateDto>("ai_set_active_profile", { profileId });
  }

  async setAiMasterEnabled(enabled: boolean): Promise<AiProfileStateDto> {
    return this.invoke<AiProfileStateDto>("ai_set_master_enabled", { enabled });
  }

  async testAiConnection(profileId: string): Promise<void> {
    await this.invoke<void>("ai_test_connection", { profileId });
  }

  async listAiModels(profileId: string): Promise<string[]> {
    return this.invoke<string[]>("ai_list_models", { profileId });
  }

  async prepareAiBatch(
    profileId: string,
    scanGeneration: number,
    itemIds: number[],
    includePaths: boolean,
  ): Promise<AiPreparedBatchDto> {
    return this.invoke<AiPreparedBatchDto>("ai_prepare_batch", {
      profileId,
      scanGeneration,
      itemIds,
      includePaths,
    });
  }

  async analyzeAiBatch(batchId: string): Promise<AiSuggestionDto[]> {
    return this.invoke<AiSuggestionDto[]>("ai_analyze", { batchId });
  }

  async confirmAiBatch(
    batchId: string,
    scanGeneration: number,
    items: AiConfirmItemArg[],
  ): Promise<AiConfirmResultDto> {
    return this.invoke<AiConfirmResultDto>("ai_confirm", {
      batchId,
      scanGeneration,
      items,
    });
  }

  async cancelAiBatch(batchId: string): Promise<boolean> {
    return this.invoke<boolean>("ai_cancel", { batchId });
  }

  async discardAiBatch(batchId: string): Promise<boolean> {
    return this.invoke<boolean>("ai_discard_batch", { batchId });
  }

  async getRules(): Promise<RuleDto[]> {
    return this.invoke<RuleDto[]>("get_rules");
  }

  async validateRules(): Promise<RulesValidationDto> {
    return this.invoke<RulesValidationDto>("validate_rules");
  }

  async deleteUserRule(ruleId: string): Promise<void> {
    await this.invoke<void>("delete_user_rule", { ruleId });
  }
}
