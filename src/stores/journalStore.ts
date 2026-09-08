import { create } from "zustand";
import type { JournalEntryDto } from "@/types";
import { getBackend } from "@/adapters";

/**
 * Journal history — a read-only audit view. One fetch + client-side
 * filtering by session and result; the page never mutates anything.
 */
interface JournalState {
  entries: JournalEntryDto[];
  loading: boolean;
  error: string | null;

  sessionFilter: number | "all";
  resultFilter: string | "all";

  load: () => Promise<void>;
  setSessionFilter: (s: number | "all") => void;
  setResultFilter: (r: string | "all") => void;
}

export const useJournalStore = create<JournalState>((set, get) => ({
  entries: [],
  loading: false,
  error: null,
  sessionFilter: "all",
  resultFilter: "all",

  load: async () => {
    if (get().loading) return;
    set({ loading: true, error: null });
    try {
      const entries = await getBackend().getJournal(200);
      set({ entries, loading: false });
    } catch (raw) {
      const message =
        typeof raw === "object" && raw !== null && "message" in raw
          ? String((raw as { message: unknown }).message)
          : String(raw);
      set({ loading: false, error: message, entries: [] });
    }
  },

  setSessionFilter: (sessionFilter) => set({ sessionFilter }),
  setResultFilter: (resultFilter) => set({ resultFilter }),
}));

/** Distinct sessions present in the entries (for the filter dropdown). */
export function journalSessions(entries: JournalEntryDto[]): number[] {
  return [...new Set(entries.map((e) => e.sessionId))].sort((a, b) => b - a);
}

/** Client-side filter application (audit view never re-fetches per filter). */
export function filterJournal(
  entries: JournalEntryDto[],
  session: number | "all",
  result: string | "all",
): JournalEntryDto[] {
  return entries.filter(
    (e) =>
      (session === "all" || e.sessionId === session) &&
      (result === "all" || (e.result ?? "") === result),
  );
}
