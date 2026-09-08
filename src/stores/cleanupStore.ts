import { create } from "zustand";
import type {
  CleanupPlanDto,
  CleanupSessionDto,
  CommandError,
  ConfirmPolicy,
  SessionItemDto,
} from "@/types";
import { getBackend } from "@/adapters";
import { useScanStore } from "./scanStore";

export type CleanupStep =
  | "selecting"
  | "planning"
  | "plan-review"
  | "dry-run"
  | "dry-run-review"
  | "confirming"
  | "executing"
  | "session"
  | "error";

interface CleanupState {
  step: CleanupStep;
  plan: CleanupPlanDto | null;
  dryRunSession: CleanupSessionDto | null;
  finalSession: CleanupSessionDto | null;
  /** Live per-item outcomes while executing (progress feel). */
  liveItems: SessionItemDto[];
  error: CommandError | null;
  busy: boolean;

  /** Policy the user has escalated to (mirrors the confirm dialog ladder). */
  policy: ConfirmPolicy;

  createPlan: (itemIds: number[], policy: ConfirmPolicy) => Promise<void>;
  runDryRun: () => Promise<void>;
  execute: () => Promise<void>;
  setPolicy: (p: ConfirmPolicy) => void;
  close: () => void;
}

const CLEAR: Omit<
  CleanupState,
  "policy" | "createPlan" | "runDryRun" | "execute" | "setPolicy" | "close"
> = {
  step: "selecting",
  plan: null,
  dryRunSession: null,
  finalSession: null,
  liveItems: [],
  error: null,
  busy: false,
};

export const useCleanupStore = create<CleanupState>((set, get) => ({
  ...CLEAR,
  policy: "default",

  createPlan: async (itemIds, policy) => {
    // Streamed UI rows are not an authoritative scan snapshot. Keep the
    // store boundary aligned with the disabled dock in case another caller
    // invokes this action while a scan is still settling.
    if (useScanStore.getState().phase !== "done") return;

    set({ busy: true, error: null, step: "planning" });
    try {
      // R3-G03: pin the selection to the generation the user selected in —
      // the backend refuses ids that were selected against an older scan.
      const scanGeneration = useScanStore.getState().generation;
      const plan = await getBackend().createCleanupPlan(itemIds, policy, scanGeneration);
      set({ plan, step: "plan-review", busy: false, policy });
    } catch (raw) {
      set({ step: "error", error: raw as CommandError, busy: false });
    }
  },

  runDryRun: async () => {
    const { plan, policy } = get();
    if (!plan || plan.items.length === 0) return;
    set({ busy: true, error: null, step: "dry-run", dryRunSession: null });
    try {
      const session = await getBackend().executeCleanupPlan(plan.planId, policy, true);
      set({ dryRunSession: session, step: "dry-run-review", busy: false });
    } catch (raw) {
      set({ step: "error", error: raw as CommandError, busy: false });
    }
  },

  execute: async () => {
    const { plan, policy } = get();
    if (!plan || plan.items.length === 0) return;
    set({ busy: true, error: null, step: "executing", liveItems: [], finalSession: null });
    try {
      const session = await getBackend().executeCleanupPlan(plan.planId, policy, false);
      set({ finalSession: session, step: "session", busy: false });
    } catch (raw) {
      set({ step: "error", error: raw as CommandError, busy: false });
    }
  },

  setPolicy: (p) => set({ policy: p }),

  close: () => set(CLEAR),
}));

/**
 * Policy needed to cover a plan's highest confirmation requirement.
 * Mirrors the backend's gate: none < redownload < review.
 */
export function policyForPlan(plan: CleanupPlanDto): ConfirmPolicy {
  const levels = plan.items.map((i) => i.confirmation);
  if (levels.includes("review")) return "review";
  if (levels.includes("redownload")) return "redownload";
  return "default";
}

/**
 * Aggregates how many items sit at each confirmation level (drives the
 * escalating confirm dialogs: redownload wording vs review wording).
 */
export function confirmBreakdown(plan: CleanupPlanDto): {
  redownload: number;
  review: number;
} {
  return {
    redownload: plan.items.filter((i) => i.confirmation === "redownload").length,
    review: plan.items.filter((i) => i.confirmation === "review").length,
  };
}
