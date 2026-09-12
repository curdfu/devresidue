import { create } from "zustand";
import type {
  AiConfirmResultDto,
  AiFinalCategory,
  AiFinalRisk,
  AiPreparedBatchDto,
  AiProfileDto,
  AiProfileInput,
  AiSuggestionDto,
  CommandError,
} from "@/types";
import { getBackend } from "@/adapters";
import { toCommandError } from "@/adapters/backend";

export type AiReturnContext = {
  sourcePage: "unknown";
  sourceView: "unknown" | "review";
  itemIds: number[];
  scanGeneration: number;
};

/**
 * Remote-AI UI state is intentionally process-only. In particular, this
 * module does not use Zustand persist and has no API Key field: the settings
 * form owns its uncontrolled password input for exactly one submit call.
 */
interface AiStoreState {
  masterEnabled: boolean;
  profiles: AiProfileDto[];
  loaded: boolean;
  profileBusy: boolean;
  connectionProfileId: string | null;
  modelProfileId: string | null;
  modelsLoading: boolean;
  availableModels: string[];

  preparedBatch: AiPreparedBatchDto | null;
  suggestions: AiSuggestionDto[];
  finalRisks: Map<number, AiFinalRisk>;
  finalCategories: Map<number, AiFinalCategory>;
  selectedSuggestionIds: Set<number>;
  /** True once any row is assigned Safe or RegenerableLocal. */
  requiresLowRiskWarning: boolean;
  preparing: boolean;
  analyzing: boolean;
  confirming: boolean;
  confirmationResult: AiConfirmResultDto | null;
  /** Process-only navigation context; never contains paths, keys or payloads. */
  returnContext: AiReturnContext | null;
  error: CommandError | null;

  loadProfiles: () => Promise<void>;
  upsertProfile: (input: AiProfileInput, apiKey: string) => Promise<AiProfileDto | null>;
  deleteProfile: (profileId: string) => Promise<boolean>;
  setActiveProfile: (profileId: string | null) => Promise<void>;
  setMasterEnabled: (enabled: boolean) => Promise<void>;
  testConnection: (profileId: string) => Promise<boolean>;
  loadModels: (profileId: string) => Promise<string[] | null>;

  prepareBatch: (profileId: string, scanGeneration: number, itemIds: number[], includePaths: boolean) => Promise<void>;
  setPreparedBatch: (batch: AiPreparedBatchDto) => void;
  analyzePreparedBatch: () => Promise<void>;
  setFinalRisk: (itemId: number, risk: AiFinalRisk) => void;
  setFinalCategory: (itemId: number, category: AiFinalCategory) => void;
  toggleSuggestion: (itemId: number) => void;
  confirmSelected: () => Promise<void>;
  cancelAnalysis: () => Promise<void>;
  discardPreparedBatch: () => Promise<void>;
  setReturnContext: (context: AiReturnContext | null) => void;
  clearReturnContext: () => void;
  resetForNewScan: (generation: number) => void;
  dismissError: () => void;
}

const emptyReviewState = () => ({
  preparedBatch: null as AiPreparedBatchDto | null,
  suggestions: [] as AiSuggestionDto[],
  finalRisks: new Map<number, AiFinalRisk>(),
  finalCategories: new Map<number, AiFinalCategory>(),
  selectedSuggestionIds: new Set<number>(),
  requiresLowRiskWarning: false,
  preparing: false,
  analyzing: false,
  confirming: false,
  confirmationResult: null as AiConfirmResultDto | null,
});

function requiresLowRiskWarning(finalRisks: Map<number, AiFinalRisk>): boolean {
  for (const risk of finalRisks.values()) {
    if (risk === "safe" || risk === "regenerable-local") return true;
  }
  return false;
}

function invalidSelection(message: string): CommandError {
  return { code: "invalid-item", message };
}

export const useAiStore = create<AiStoreState>((set, get) => ({
  masterEnabled: false,
  profiles: [],
  loaded: false,
  profileBusy: false,
  connectionProfileId: null,
  modelProfileId: null,
  modelsLoading: false,
  availableModels: [],
  ...emptyReviewState(),
  returnContext: null,
  error: null,

  loadProfiles: async () => {
    try {
      const state = await getBackend().listAiProfiles();
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        loaded: true,
        error: null,
      });
    } catch (raw) {
      set({ loaded: true, error: toCommandError(raw) });
    }
  },

  upsertProfile: async (input, apiKey) => {
    set({ profileBusy: true, error: null });
    try {
      const profile = await getBackend().upsertAiProfile(input, apiKey);
      const state = await getBackend().listAiProfiles();
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...emptyReviewState(),
      });
      return profile;
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
      return null;
    }
  },

  deleteProfile: async (profileId) => {
    set({ profileBusy: true, error: null });
    try {
      await getBackend().deleteAiProfile(profileId);
      const state = await getBackend().listAiProfiles();
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...emptyReviewState(),
      });
      return true;
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
      return false;
    }
  },

  setActiveProfile: async (profileId) => {
    set({ profileBusy: true, error: null });
    try {
      const state = await getBackend().setActiveAiProfile(profileId);
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...emptyReviewState(),
      });
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
    }
  },

  setMasterEnabled: async (enabled) => {
    set({ profileBusy: true, error: null });
    try {
      const state = await getBackend().setAiMasterEnabled(enabled);
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...(enabled ? {} : emptyReviewState()),
      });
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
    }
  },

  testConnection: async (profileId) => {
    set({ connectionProfileId: profileId, error: null });
    try {
      await getBackend().testAiConnection(profileId);
      set({ connectionProfileId: null });
      return true;
    } catch (raw) {
      set({ connectionProfileId: null, error: toCommandError(raw) });
      return false;
    }
  },

  loadModels: async (profileId) => {
    set({ modelProfileId: profileId, modelsLoading: true, error: null });
    try {
      const availableModels = await getBackend().listAiModels(profileId);
      set({ modelProfileId: profileId, modelsLoading: false, availableModels });
      return availableModels;
    } catch (raw) {
      set({ modelProfileId: profileId, modelsLoading: false, error: toCommandError(raw) });
      return null;
    }
  },

  prepareBatch: async (profileId, scanGeneration, itemIds, includePaths) => {
    if (get().preparing || get().analyzing || get().confirming) return;
    set({ preparing: true, error: null, confirmationResult: null });
    try {
      const batch = await getBackend().prepareAiBatch(profileId, scanGeneration, itemIds, includePaths);
      set({ ...emptyReviewState(), preparedBatch: batch });
    } catch (raw) {
      set({ preparing: false, error: toCommandError(raw) });
    }
  },

  setPreparedBatch: (batch) => set({ ...emptyReviewState(), preparedBatch: batch, error: null }),

  analyzePreparedBatch: async () => {
    const batch = get().preparedBatch;
    if (!batch || get().analyzing || get().confirming) return;
    set({ analyzing: true, error: null, confirmationResult: null });
    try {
      const suggestions = await getBackend().analyzeAiBatch(batch.batchId);
      set({
        analyzing: false,
        suggestions,
        finalRisks: new Map(),
        finalCategories: new Map(),
        selectedSuggestionIds: new Set(),
        requiresLowRiskWarning: false,
      });
    } catch (raw) {
      set({ analyzing: false, error: toCommandError(raw) });
    }
  },

  setFinalRisk: (itemId, risk) =>
    set((state) => {
      const finalRisks = new Map(state.finalRisks);
      finalRisks.set(itemId, risk);
      return { finalRisks, requiresLowRiskWarning: requiresLowRiskWarning(finalRisks) };
    }),

  setFinalCategory: (itemId, category) =>
    set((state) => {
      const finalCategories = new Map(state.finalCategories);
      finalCategories.set(itemId, category);
      return { finalCategories };
    }),

  toggleSuggestion: (itemId) =>
    set((state) => {
      if (!state.finalRisks.has(itemId) || !state.finalCategories.has(itemId)) return {};
      const selectedSuggestionIds = new Set(state.selectedSuggestionIds);
      if (selectedSuggestionIds.has(itemId)) selectedSuggestionIds.delete(itemId);
      else selectedSuggestionIds.add(itemId);
      return { selectedSuggestionIds };
    }),

  confirmSelected: async () => {
    const { preparedBatch, finalRisks, finalCategories, selectedSuggestionIds } = get();
    if (!preparedBatch) return;
    const items = [...selectedSuggestionIds].flatMap((itemId) => {
      const finalRisk = finalRisks.get(itemId);
      const finalCategory = finalCategories.get(itemId);
      return finalRisk && finalCategory ? [{ itemId, finalRisk, finalCategory }] : [];
    });
    if (items.length === 0) {
      set({ error: invalidSelection("请先为至少一项建议选择最终风险、最终类别并勾选确认") });
      return;
    }
    set({ confirming: true, error: null });
    try {
      const confirmationResult = await getBackend().confirmAiBatch(
        preparedBatch.batchId,
        preparedBatch.scanGeneration,
        items,
      );
      set({ ...emptyReviewState(), confirmationResult });
    } catch (raw) {
      set({ confirming: false, error: toCommandError(raw) });
    }
  },

  cancelAnalysis: async () => {
    const batch = get().preparedBatch;
    if (!batch) return;
    try {
      await getBackend().cancelAiBatch(batch.batchId);
    } catch {
      // Cancellation is best-effort. The backend's bounded request still
      // cannot write rules or cleanup state without a later confirmation.
    } finally {
      set({ analyzing: false });
    }
  },

  discardPreparedBatch: async () => {
    const batch = get().preparedBatch;
    if (!batch || get().analyzing || get().confirming) return;
    try {
      const discarded = await getBackend().discardAiBatch(batch.batchId);
      if (!discarded) {
        set({ error: invalidSelection("该联网 AI 批次已失效，请重新选择条目") });
        return;
      }
      set({ ...emptyReviewState(), error: null });
    } catch (raw) {
      set({ error: toCommandError(raw) });
    }
  },

  resetForNewScan: (_generation) => {
    const batch = get().preparedBatch;
    if (batch) void getBackend().cancelAiBatch(batch.batchId).catch(() => undefined);
    set({ ...emptyReviewState(), returnContext: null, error: null });
  },

  setReturnContext: (returnContext) => set({ returnContext }),
  clearReturnContext: () => set({ returnContext: null }),

  dismissError: () => set({ error: null }),
}));
