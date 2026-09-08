import { create } from "zustand";
import type { AnalyzerSuggestionDto, CommandError, Disposition } from "@/types";
import { getBackend } from "@/adapters";
import { useScanStore } from "@/stores/scanStore";

/**
 * Unknown-page workflow state: analyzer runs per item id, accepted
 * suggestions (rule creation), and dispositions. Every applied decision
 * refreshes the snapshot so tables update without a re-scan.
 */
interface UnknownWorkflowState {
  /** Analyzer suggestions keyed by item id. */
  suggestions: Map<number, AnalyzerSuggestionDto>;
  analyzing: Set<number>;
  applying: Set<number>;
  error: CommandError | null;

  analyze: (itemId: number) => Promise<void>;
  /** Accept the suggestion: create a user rule with the suggested risk. */
  createRuleFromSuggestion: (itemId: number) => Promise<void>;
  /** Direct ignore/protect decision (no analyzer involved). */
  setDisposition: (itemId: number, disposition: Disposition) => Promise<void>;
  clearSuggestion: (itemId: number) => void;
  dismissError: () => void;
}

async function applyAndRefresh(fn: () => Promise<unknown>, itemId: number) {
  useUnknownWorkflowStore.setState((s) => ({
    applying: new Set(s.applying).add(itemId),
    error: null,
  }));
  try {
    await fn();
    // Pull the refreshed snapshot so tables reflect the decision without a
    // re-scan (the mock applies it server-side; the real backend does the
    // same through its user-rules overlay).
    await useScanStore.getState().loadLatest();
    useUnknownWorkflowStore.setState((s) => {
      const suggestions = new Map(s.suggestions);
      suggestions.delete(itemId);
      const applying = new Set(s.applying);
      applying.delete(itemId);
      return { suggestions, applying };
    });
  } catch (raw) {
    useUnknownWorkflowStore.setState((s) => {
      const applying = new Set(s.applying);
      applying.delete(itemId);
      return { applying, error: raw as CommandError };
    });
  }
}

export const useUnknownWorkflowStore = create<UnknownWorkflowState>((set, get) => ({
  suggestions: new Map(),
  analyzing: new Set(),
  applying: new Set(),
  error: null,

  analyze: async (itemId) => {
    if (get().analyzing.has(itemId)) return;
    set((s) => ({ analyzing: new Set(s.analyzing).add(itemId), error: null }));
    try {
      const suggestion = await getBackend().analyzeItem(itemId);
      set((s) => {
        const suggestions = new Map(s.suggestions);
        suggestions.set(itemId, suggestion);
        const analyzing = new Set(s.analyzing);
        analyzing.delete(itemId);
        return { suggestions, analyzing };
      });
    } catch (raw) {
      set((s) => {
        const analyzing = new Set(s.analyzing);
        analyzing.delete(itemId);
        return { analyzing, error: raw as CommandError };
      });
    }
  },

  createRuleFromSuggestion: async (itemId) => {
    const suggestion = get().suggestions.get(itemId);
    if (!suggestion) return;
    await applyAndRefresh(
      () => getBackend().createRuleFromSuggestion(itemId, suggestion.suggestedRisk),
      itemId,
    );
  },

  setDisposition: async (itemId, disposition) => {
    await applyAndRefresh(() => getBackend().setDisposition(itemId, disposition), itemId);
  },

  clearSuggestion: (itemId) =>
    set((s) => {
      const suggestions = new Map(s.suggestions);
      suggestions.delete(itemId);
      return { suggestions };
    }),

  dismissError: () => set({ error: null }),
}));
