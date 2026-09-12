import { create } from "zustand";

export type PageKey =
  | "dashboard"
  | "agents"
  | "devcache"
  | "projects"
  | "unknown"
  | "selected-items"
  | "ai-review"
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

const APPEARANCE_KEY = "devresidue.appearance.v1";

function loadTheme(): ThemeMode {
  if (typeof window === "undefined") return "system";
  try {
    const raw = localStorage.getItem(APPEARANCE_KEY);
    const parsed = raw ? JSON.parse(raw) as { themeMode?: unknown } : null;
    return parsed?.themeMode === "dark" || parsed?.themeMode === "light" || parsed?.themeMode === "system"
      ? parsed.themeMode
      : "system";
  } catch {
    return "system";
  }
}

function saveTheme(themeMode: ThemeMode) {
  try {
    localStorage.setItem(APPEARANCE_KEY, JSON.stringify({ themeMode }));
  } catch {
    // Browser storage is optional; the in-memory theme remains authoritative.
  }
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
  theme: loadTheme(),

  setPage: (page) => set({ page, riskFilter: null, detailItemId: null }),
  setRiskFilter: (riskFilter) => set({ riskFilter, detailItemId: null }),
  openDetail: (detailItemId) => set({ detailItemId }),
  // Sidebar toggle cycles the three modes: system → dark → light → system.
  toggleTheme: () =>
    set((s) => {
      const theme = s.theme === "system" ? "dark" : s.theme === "dark" ? "light" : "system";
      saveTheme(theme);
      return { theme };
    }),
  setTheme: (theme) => {
    saveTheme(theme);
    set({ theme });
  },
}));
