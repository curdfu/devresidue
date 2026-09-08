import { create } from "zustand";

export type PageKey =
  | "dashboard"
  | "agents"
  | "devcache"
  | "projects"
  | "unknown"
  | "risk-results"
  | "rules"
  | "journal"
  | "settings";

export type ThemeMode = "system" | "dark" | "light";

/** The concrete palette a ThemeMode resolves to right now. */
export type ResolvedTheme = "dark" | "light";

export function systemTheme(): ResolvedTheme {
  return typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

interface UiState {
  page: PageKey;
  /** Risk group clicked on the dashboard → jumps to a page with that filter. */
  riskFilter: string | null;
  detailItemId: number | null;
  theme: ThemeMode;

  setPage: (p: PageKey) => void;
  setRiskFilter: (r: string | null) => void;
  openDetail: (id: number | null) => void;
  toggleTheme: () => void;
  setTheme: (t: ThemeMode) => void;
}

export const useUiStore = create<UiState>((set) => ({
  page: "dashboard",
  riskFilter: null,
  detailItemId: null,
  theme: "system",

  setPage: (page) => set({ page, riskFilter: null, detailItemId: null }),
  setRiskFilter: (riskFilter) => set({ riskFilter, detailItemId: null }),
  openDetail: (detailItemId) => set({ detailItemId }),
  // Sidebar toggle cycles the three modes: system → dark → light → system.
  toggleTheme: () =>
    set((s) => ({
      theme: s.theme === "system" ? "dark" : s.theme === "dark" ? "light" : "system",
    })),
  setTheme: (theme) => set({ theme }),
}));
