import { create } from "zustand";
import type {
  CleanupPlanDto,
  CleanupSessionDto,
  CommandError,
  ConfirmPolicy,
  RiskLevel,
  SessionItemDto,
} from "@/types";
import { getBackend } from "@/adapters";
import { useScanStore } from "./scanStore";
import { useSelectionStore } from "./selectionStore";

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

export type CleanupRequestKind = "planning" | "dry-run" | "executing";

export type CleanupRequestContext = {
  token: number;
  kind: CleanupRequestKind;
  scanGeneration: number;
  planId: number | null;
};

/** Frontend-only context captured when a plan is created. */
export type CleanupPlanContext = {
  planId: number;
  scanGeneration: number;
  riskByItemId: Record<string, RiskLevel>;
  selectedItemIds: number[];
  policy: ConfirmPolicy;
};

export type PlanImpactBucket = { count: number; bytes: number };

export type PlanImpactSummary = {
  itemCount: number;
  estimatedBytes: number;
  actions: {
    permanentDelete: number;
    toolCleanup: number;
    unrecognized: number;
  };
  risks: {
    safe: PlanImpactBucket;
    regenerableLocal: PlanImpactBucket;
    regenerableDownload: PlanImpactBucket;
    review: PlanImpactBucket;
    protected: PlanImpactBucket;
    unknown: PlanImpactBucket;
    unmatched: PlanImpactBucket;
  };
  authorization: {
    none: number;
    redownload: number;
    review: number;
    unrecognized: number;
  };
  contextValid: boolean;
  contextIssue: string | null;
};

interface CleanupState {
  step: CleanupStep;
  plan: CleanupPlanDto | null;
  planContext: CleanupPlanContext | null;
  dryRunSession: CleanupSessionDto | null;
  finalSession: CleanupSessionDto | null;
  /** Live per-item outcomes while executing (progress feel). */
  liveItems: SessionItemDto[];
  error: CommandError | null;
  busy: boolean;
  /** Monotonic identity of the latest cleanup request. */
  requestToken: number;
  /** Request currently allowed to publish an async result, if any. */
  activeRequest: CleanupRequestContext | null;

  /** Policy the user has escalated to (mirrors the confirm dialog ladder). */
  policy: ConfirmPolicy;

  createPlan: (itemIds: number[], policy: ConfirmPolicy) => Promise<void>;
  runDryRun: () => Promise<void>;
  execute: () => Promise<void>;
  setPolicy: (p: ConfirmPolicy) => void;
  close: () => void;
  invalidateForGeneration: (generation: number) => void;
}

const CLEAR: Omit<
  CleanupState,
  "policy" | "createPlan" | "runDryRun" | "execute" | "setPolicy" | "close"
  | "invalidateForGeneration"
> = {
  step: "selecting",
  plan: null,
  planContext: null,
  dryRunSession: null,
  finalSession: null,
  liveItems: [],
  error: null,
  busy: false,
  requestToken: 0,
  activeRequest: null,
};

let requestTokenCounter = 0;

function issueRequest(
  kind: CleanupRequestKind,
  scanGeneration: number,
  planId: number | null,
): CleanupRequestContext {
  return {
    token: ++requestTokenCounter,
    kind,
    scanGeneration,
    planId,
  };
}

function requestIsCurrent(
  state: Pick<CleanupState, "step" | "activeRequest">,
  request: CleanupRequestContext,
  step: CleanupStep,
): boolean {
  return state.step === step
    && state.activeRequest?.token === request.token
    && state.activeRequest.kind === request.kind
    && state.activeRequest.scanGeneration === request.scanGeneration
    && state.activeRequest.planId === request.planId;
}

function sameItemSet(left: readonly number[], right: readonly number[]): boolean {
  if (left.length !== right.length) return false;
  const a = [...left].sort((x, y) => x - y);
  const b = [...right].sort((x, y) => x - y);
  return a.every((value, index) => value === b[index]);
}

export const useCleanupStore = create<CleanupState>((set, get) => ({
  ...CLEAR,
  policy: "default",

  createPlan: async (itemIds, policy) => {
    if (get().busy || get().step !== "selecting") return;
    // Streamed UI rows are not an authoritative scan snapshot. Keep the
    // store boundary aligned with the disabled dock in case another caller
    // invokes this action while a scan is still settling.
    if (useScanStore.getState().phase !== "done") return;

    const scan = useScanStore.getState();
    const scanGeneration = scan.generation;
    const selectedItemIds = [...new Set(itemIds)].sort((a, b) => a - b);
    const riskByItemId = Object.fromEntries(
      selectedItemIds.flatMap((itemId) => {
        const risk = scan.items.find((item) => item.id === itemId)?.risk;
        return risk ? [[String(itemId), risk] as const] : [];
      }),
    );

    const request = issueRequest("planning", scanGeneration, null);
    set({
      busy: true,
      error: null,
      step: "planning",
      plan: null,
      planContext: null,
      dryRunSession: null,
      finalSession: null,
      requestToken: request.token,
      activeRequest: request,
    });
    try {
      // R3-G03: pin the selection to the generation the user selected in —
      // the backend refuses ids that were selected against an older scan.
      const plan = await getBackend().createCleanupPlan(itemIds, policy, scanGeneration);
      if (!requestIsCurrent(get(), request, "planning")) return;
      const latest = useScanStore.getState();
      if (latest.phase !== "done" || latest.generation !== scanGeneration) {
        set({
          plan: null,
          planContext: null,
          step: "error",
          error: {
            code: "invalid-item",
            message: "扫描结果已更新，清理计划已失效，请重新选择并生成计划",
          },
          busy: false,
          activeRequest: null,
        });
        return;
      }
      const currentSelection = [...useSelectionStore.getState().selected];
      if (!sameItemSet(selectedItemIds, currentSelection) || get().policy !== policy) {
        set({
          plan: null,
          planContext: null,
          step: "error",
          error: {
            code: "invalid-item",
            message: "选择集合或授权策略已变化，请重新选择并生成清理计划",
          },
          busy: false,
          activeRequest: null,
        });
        return;
      }
      set({
        plan,
        planContext: {
          planId: plan.planId,
          scanGeneration,
          riskByItemId,
          selectedItemIds,
          policy,
        },
        step: "plan-review",
        busy: false,
        policy,
        activeRequest: null,
      });
    } catch (raw) {
      if (!requestIsCurrent(get(), request, "planning")) return;
      set({
        plan: null,
        planContext: null,
        step: "error",
        error: raw as CommandError,
        busy: false,
        activeRequest: null,
      });
    }
  },

  runDryRun: async () => {
    const { plan, planContext, policy } = get();
    if (get().busy || get().step !== "plan-review" || !plan || plan.items.length === 0) return;
    const currentSelection = [...useSelectionStore.getState().selected];
    const context = summarizePlanImpact(
      plan,
      planContext,
      useScanStore.getState().generation,
      currentSelection,
      policy,
    );
    if (!context.contextValid) {
      set({
        step: "error",
        error: { code: "invalid-item", message: context.contextIssue ?? "清理计划已失效，请重新生成" },
        busy: false,
      });
      return;
    }
    const request = issueRequest("dry-run", planContext?.scanGeneration ?? 0, plan.planId);
    set({
      busy: true,
      error: null,
      step: "dry-run",
      dryRunSession: null,
      requestToken: request.token,
      activeRequest: request,
    });
    try {
      const session = await getBackend().executeCleanupPlan(plan.planId, policy, true);
      const current = get();
      if (!requestIsCurrent(current, request, "dry-run")) return;
      if (!current.plan || current.plan.planId !== request.planId) return;
      const latestContext = summarizePlanImpact(
        current.plan,
        current.planContext,
        useScanStore.getState().generation,
        [...useSelectionStore.getState().selected],
        current.policy,
      );
      if (!latestContext.contextValid) {
        set({
          step: "error",
          error: { code: "invalid-item", message: latestContext.contextIssue ?? "清理计划已变化，请重新生成" },
          busy: false,
          activeRequest: null,
        });
        return;
      }
      set({ dryRunSession: session, step: "dry-run-review", busy: false, activeRequest: null });
    } catch (raw) {
      if (!requestIsCurrent(get(), request, "dry-run")) return;
      set({ step: "error", error: raw as CommandError, busy: false, activeRequest: null });
    }
  },

  execute: async () => {
    const { plan, planContext, policy } = get();
    if (get().busy || get().step !== "confirming" || !plan || plan.items.length === 0) return;
    const currentSelection = [...useSelectionStore.getState().selected];
    const context = summarizePlanImpact(
      plan,
      planContext,
      useScanStore.getState().generation,
      currentSelection,
      policy,
    );
    if (!context.contextValid) {
      set({
        step: "error",
        error: { code: "invalid-item", message: context.contextIssue ?? "清理计划已失效，请重新生成" },
        busy: false,
      });
      return;
    }
    const request = issueRequest("executing", planContext?.scanGeneration ?? 0, plan.planId);
    set({
      busy: true,
      error: null,
      step: "executing",
      liveItems: [],
      finalSession: null,
      requestToken: request.token,
      activeRequest: request,
    });
    try {
      const session = await getBackend().executeCleanupPlan(plan.planId, policy, false);
      const current = get();
      // An executing request stays bound to its original generation. A later
      // scan must not discard its terminal result or retarget it to new IDs.
      if (!requestIsCurrent(current, request, "executing")) return;
      if (!current.plan || current.plan.planId !== request.planId) return;
      set({ finalSession: session, step: "session", busy: false, activeRequest: null });
    } catch (raw) {
      if (!requestIsCurrent(get(), request, "executing")) return;
      set({ step: "error", error: raw as CommandError, busy: false, activeRequest: null });
    }
  },

  setPolicy: (p) => {
    const state = get();
    if (state.busy || state.step === "executing") return;
    if (state.step !== "selecting") {
      const requestToken = issueRequest("planning", useScanStore.getState().generation, null).token;
      set({ ...CLEAR, policy: p, requestToken, activeRequest: null });
      return;
    }
    set({ policy: p });
  },

  close: () => {
    const state = get();
    // A real execution is a separate non-dismissible boundary. Its result
    // remains available even if a new scan lands while the backend is busy.
    if (state.step === "executing") return;
    const requestToken = issueRequest("planning", state.planContext?.scanGeneration ?? 0, state.plan?.planId ?? null).token;
    set({ ...CLEAR, requestToken, activeRequest: null });
  },

  invalidateForGeneration: (_generation) => {
    const state = get();
    if (state.step === "executing" || state.step === "session") return;
    if (state.step === "selecting" && !state.activeRequest) return;
    const requestToken = issueRequest("planning", state.planContext?.scanGeneration ?? 0, state.plan?.planId ?? null).token;
    set({ ...CLEAR, policy: state.policy, requestToken, activeRequest: null });
  },
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

/** Final confirmation CTA derived from actual planned actions, never policy. */
export function finalActionLabel(summary: PlanImpactSummary): string {
  const { permanentDelete, toolCleanup, unrecognized } = summary.actions;
  if (permanentDelete > 0 && toolCleanup === 0 && unrecognized === 0) {
    return `永久删除 ${summary.itemCount} 项`;
  }
  if (toolCleanup > 0 && permanentDelete === 0 && unrecognized === 0) {
    return "执行工具清理";
  }
  return `执行清理（${summary.itemCount} 项）`;
}

function bucket(): PlanImpactBucket {
  return { count: 0, bytes: 0 };
}

/**
 * Derives actual impact from plan items and the scan-time risk snapshot.
 * Policy is deliberately absent: it controls authorisation, not risk labels.
 */
export function summarizePlanImpact(
  plan: CleanupPlanDto,
  context: CleanupPlanContext | null,
  currentGeneration: number,
  currentSelection?: readonly number[],
  currentPolicy?: ConfirmPolicy,
): PlanImpactSummary {
  const risks = {
    safe: bucket(),
    regenerableLocal: bucket(),
    regenerableDownload: bucket(),
    review: bucket(),
    protected: bucket(),
    unknown: bucket(),
    unmatched: bucket(),
  };
  const summary: PlanImpactSummary = {
    itemCount: plan.items.length,
    estimatedBytes: plan.totalEstimatedBytes,
    actions: { permanentDelete: 0, toolCleanup: 0, unrecognized: 0 },
    risks,
    authorization: { none: 0, redownload: 0, review: 0, unrecognized: 0 },
    contextValid: true,
    contextIssue: null,
  };

  if (!context) {
    summary.contextValid = false;
    summary.contextIssue = "缺少计划对应的扫描风险快照，请重新生成清理计划";
  } else if (context.scanGeneration !== currentGeneration) {
    summary.contextValid = false;
    summary.contextIssue = "扫描代次已变化，清理计划已过期，请重新选择并生成计划";
  } else if (context.planId !== plan.planId) {
    summary.contextValid = false;
    summary.contextIssue = "清理计划标识已变化，请重新生成计划";
  } else if (currentSelection && !sameItemSet(context.selectedItemIds, currentSelection)) {
    summary.contextValid = false;
    summary.contextIssue = "选择集合已变化，请重新生成清理计划";
  } else if (currentPolicy && context.policy !== currentPolicy) {
    summary.contextValid = false;
    summary.contextIssue = "授权策略已变化，请重新生成清理计划";
  }

  for (const item of plan.items) {
    if (item.action === "recycle" || item.action === "delete") summary.actions.permanentDelete += 1;
    else if (item.action === "execute") summary.actions.toolCleanup += 1;
    else {
      summary.actions.unrecognized += 1;
      summary.contextValid = false;
      summary.contextIssue ??= `计划条目 #${item.scanItemId} 包含未识别清理动作，请重新生成清理计划`;
    }

    if (item.confirmation === "none") summary.authorization.none += 1;
    else if (item.confirmation === "redownload") summary.authorization.redownload += 1;
    else if (item.confirmation === "review") summary.authorization.review += 1;
    else {
      summary.authorization.unrecognized += 1;
      summary.contextValid = false;
      summary.contextIssue ??= `计划条目 #${item.scanItemId} 包含未识别确认级别，请重新生成清理计划`;
    }

    const risk = context?.riskByItemId[String(item.scanItemId)];
    const target = risk ? riskBucket(risks, risk) : risks.unmatched;
    if (!risk) {
      summary.contextValid = false;
      summary.contextIssue ??= `计划条目 #${item.scanItemId} 没有匹配的扫描风险，请重新生成清理计划`;
    }
    target.count += 1;
    target.bytes += item.estimatedSize;
    if (risk === "protected" || risk === "unknown") {
      summary.contextValid = false;
      summary.contextIssue ??= `计划条目 #${item.scanItemId} 属于不可执行风险，请重新生成清理计划`;
    }
  }

  return summary;
}

function riskBucket(
  risks: PlanImpactSummary["risks"],
  risk: RiskLevel,
): PlanImpactBucket {
  switch (risk) {
    case "safe": return risks.safe;
    case "regenerable-local": return risks.regenerableLocal;
    case "regenerable-download": return risks.regenerableDownload;
    case "review": return risks.review;
    case "protected": return risks.protected;
    case "unknown": return risks.unknown;
  }
}
