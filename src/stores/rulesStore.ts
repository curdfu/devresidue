import { create } from "zustand";
import type { RuleDto, RulesValidationDto } from "@/types";
import { getBackend } from "@/adapters";

type RulesState = {
  rules: RuleDto[];
  validation: RulesValidationDto | null;
  loading: boolean;
  error: string | null;
  load: (force?: boolean) => Promise<void>;
  clear: () => void;
};

function errorMessage(raw: unknown): string {
  return typeof raw === "object" && raw !== null && "message" in raw
    ? String((raw as { message: unknown }).message)
    : String(raw);
}

/** One authoritative in-memory rule registry shared by RulesPage and details. */
export const useRulesStore = create<RulesState>((set, get) => ({
  rules: [],
  validation: null,
  loading: false,
  error: null,

  load: async (force = false) => {
    if (get().loading || (!force && get().rules.length > 0 && get().validation)) return;
    set({ loading: true, error: null });
    try {
      const backend = getBackend();
      const [rules, validation] = await Promise.all([
        backend.getRules(),
        backend.validateRules(),
      ]);
      set({ rules, validation, loading: false, error: null });
    } catch (raw) {
      set({ loading: false, error: errorMessage(raw), rules: [], validation: null });
    }
  },

  clear: () => set({ rules: [], validation: null, error: null }),
}));
