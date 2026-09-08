import { create } from "zustand";
import type { AppSettingsDto } from "@/types";
import { getBackend } from "@/adapters";

/**
 * App settings (analyzer toggle etc.). Backend-backed when the commands
 * exist; the mock persists to localStorage. Default: analyzer OFF (SPEC §26).
 */
interface AppSettingsState {
  analyzerEnabled: boolean;
  loaded: boolean;
  load: () => Promise<void>;
  setAnalyzerEnabled: (enabled: boolean) => Promise<void>;
}

export const useAppSettingsStore = create<AppSettingsState>((set) => ({
  analyzerEnabled: false,
  loaded: false,

  load: async () => {
    try {
      const s: AppSettingsDto = await getBackend().getSettings();
      set({ analyzerEnabled: s.analyzerEnabled, loaded: true });
    } catch {
      // Backend without the settings command yet: keep defaults.
      set({ loaded: true });
    }
  },

  setAnalyzerEnabled: async (enabled) => {
    try {
      // Wire: set_analyzer_enabled returns void; the toggle state we keep
      // locally mirrors what get_settings would report back.
      await getBackend().setAnalyzerEnabled(enabled);
      set({ analyzerEnabled: enabled });
    } catch {
      // Optimistic fall-through: the UI toggle already moved; the backend
      // catch-up is best-effort until the command merges.
      set({ analyzerEnabled: enabled });
    }
  },
}));
