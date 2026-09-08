import { create } from "zustand";

/**
 * Cleanup selection set — shared by all category pages so a selection made
 * on AI Agents persists while browsing Developer Cache. Cleared when a
 * session finishes or the user discards.
 */
interface SelectionState {
  selected: Set<number>;
  toggle: (id: number) => void;
  setAll: (ids: number[]) => void;
  clear: () => void;
}

export const useSelectionStore = create<SelectionState>((set) => ({
  selected: new Set<number>(),

  toggle: (id) =>
    set((s) => {
      const next = new Set(s.selected);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return { selected: next };
    }),

  setAll: (ids) => set({ selected: new Set(ids) }),

  clear: () => set({ selected: new Set<number>() }),
}));
