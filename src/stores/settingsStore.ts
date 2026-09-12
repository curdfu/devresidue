import { create } from "zustand";
import { getBackend } from "@/adapters";

/**
 * Local scan settings. The mode is explicit: automatic leaves default root
 * resolution to the backend; custom sends the validated roots as structured
 * scan scope input. This never becomes a cleanup path authority.
 */

export type WorkspaceRootsMode = "automatic" | "custom";

interface SettingsState {
  workspaceRoots: string[];
  workspaceRootsMode: WorkspaceRootsMode;
  legacyRootsPending: boolean;
  load: () => void;
  setWorkspaceRootsMode: (mode: WorkspaceRootsMode) => void;
  addRoot: (root: string) => void;
  removeRoot: (root: string) => void;
}

const KEY = "devresidue.settings.v2";
const LEGACY_KEY = "devresidue.settings.v1";

interface Persisted {
  workspaceRoots?: string[];
  workspaceRootsMode?: WorkspaceRootsMode;
}

export const useSettingsStore = create<SettingsState>((set, get) => ({
  workspaceRoots: [],
  workspaceRootsMode: "automatic",
  legacyRootsPending: false,

  load: () => {
    try {
      const raw = localStorage.getItem(KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Persisted;
        set({
          workspaceRoots: Array.isArray(parsed.workspaceRoots) ? parsed.workspaceRoots : [],
          workspaceRootsMode: parsed.workspaceRootsMode === "custom" ? "custom" : "automatic",
          legacyRootsPending: false,
        });
        return;
      }
      const legacy = localStorage.getItem(LEGACY_KEY);
      if (legacy) {
        const parsed = JSON.parse(legacy) as Persisted;
        if (Array.isArray(parsed.workspaceRoots)) {
          set({ workspaceRoots: parsed.workspaceRoots, workspaceRootsMode: "automatic", legacyRootsPending: true });
        }
      }
    } catch {
      // Corrupt store: keep defaults.
    }
  },

  setWorkspaceRootsMode: (workspaceRootsMode) => {
    set({ workspaceRootsMode, legacyRootsPending: false });
    persist(get().workspaceRoots, workspaceRootsMode);
  },

  addRoot: (root) => {
    const trimmed = normalizeRoot(root);
    if (!trimmed || get().workspaceRoots.includes(trimmed)) return;
    const next = [...get().workspaceRoots, trimmed];
    set({ workspaceRoots: next, workspaceRootsMode: "custom", legacyRootsPending: false });
    persist(next, "custom");
  },

  removeRoot: (root) => {
    const next = get().workspaceRoots.filter((r) => r !== root);
    set({ workspaceRoots: next });
    persist(next, get().workspaceRootsMode);
  },
}));

function normalizeRoot(root: string): string {
  const trimmed = root.trim().replace(/\//g, "\\");
  if (!trimmed) return "";
  let normalized = trimmed;
  while (normalized.length > 3 && normalized.endsWith("\\")) normalized = normalized.slice(0, -1);
  return normalized;
}

function persist(roots: string[], workspaceRootsMode: WorkspaceRootsMode) {
  try {
    localStorage.setItem(KEY, JSON.stringify({ workspaceRoots: roots, workspaceRootsMode } satisfies Persisted));
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

export function defaultScope(mode: WorkspaceRootsMode, roots: string[]): { kind: "default"; workspace_roots?: string[] } {
  return mode === "custom" ? { kind: "default", workspace_roots: [...roots] } : { kind: "default" };
}

export function backendKind(): "tauri" | "mock" {
  return getBackend().kind;
}
