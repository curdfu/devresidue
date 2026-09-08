import { create } from "zustand";
import { getBackend } from "@/adapters";

/**
 * Local settings (PLAN: workspace roots edited locally, backend wiring later).
 * Persisted to localStorage; nothing here talks to the backend except the
 * data-dir display, which is a mock constant until a command exposes it.
 */

export const DATA_DIR_HINT = "%LOCALAPPDATA%\\DevResidue";

interface SettingsState {
  workspaceRoots: string[];
  load: () => void;
  addRoot: (root: string) => void;
  removeRoot: (root: string) => void;
}

const KEY = "devresidue.settings.v1";

interface Persisted {
  workspaceRoots?: string[];
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  workspaceRoots: ["D:\\Code"],

  load: () => {
    try {
      const raw = localStorage.getItem(KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Persisted;
        if (parsed.workspaceRoots) set({ workspaceRoots: parsed.workspaceRoots });
      }
    } catch {
      // Corrupt store: keep defaults.
    }
  },

  addRoot: (root) => {
    const trimmed = root.trim().replace(/\\+$/, "");
    if (!trimmed || get().workspaceRoots.includes(trimmed)) return;
    const next = [...get().workspaceRoots, trimmed];
    set({ workspaceRoots: next });
    persist(next);
  },

  removeRoot: (root) => {
    const next = get().workspaceRoots.filter((r) => r !== root);
    set({ workspaceRoots: next });
    persist(next);
  },
}));

function persist(roots: string[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify({ workspaceRoots: roots } satisfies Persisted));
  } catch {
    // Storage unavailable: settings stay session-only.
  }
}

/**
 * Effective workspace roots for the Projects scope: local settings joined
 * with whatever the backend's Default scope would resolve.
 */
export function projectsScope(roots: string[]): { kind: "projects"; roots: string[] } {
  return { kind: "projects", roots };
}

export function backendKind(): "tauri" | "mock" {
  return getBackend().kind;
}
