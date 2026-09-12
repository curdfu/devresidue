import { create } from "zustand";
import type { CommandError, Disposition } from "@/types";
import { getBackend } from "@/adapters";
import { useScanStore } from "@/stores/scanStore";

/**
 * Unknown-page workflow state for explicit ignore/protect dispositions.
 * Every applied decision refreshes the snapshot so tables update without a
 * re-scan.
 */
interface UnknownWorkflowState {
  applying: Set<number>;
  error: CommandError | null;

  /** Direct ignore/protect decision. */
  setDisposition: (itemId: number, disposition: Disposition) => Promise<void>;
  dismissError: () => void;
  /** Clears only transient in-flight/error UI state after scan-data reset. */
  reset: () => void;
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
      const applying = new Set(s.applying);
      applying.delete(itemId);
      return { applying };
    });
  } catch (raw) {
    useUnknownWorkflowStore.setState((s) => {
      const applying = new Set(s.applying);
      applying.delete(itemId);
      return { applying, error: raw as CommandError };
    });
  }
}

export const useUnknownWorkflowStore = create<UnknownWorkflowState>((set) => ({
  applying: new Set(),
  error: null,

  setDisposition: async (itemId, disposition) => {
    await applyAndRefresh(() => getBackend().setDisposition(itemId, disposition), itemId);
  },

  dismissError: () => set({ error: null }),
  reset: () => set({ applying: new Set(), error: null }),
}));
