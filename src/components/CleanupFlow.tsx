import { useRef } from "react";
import {
  CheckCircle2,
  Download,
  FileWarning,
  FlaskConical,
  Play,
  ShieldAlert,
  X,
  XCircle,
} from "lucide-react";
import type { CleanupPlanDto, ConfirmPolicy } from "@/types";
import {
  useCleanupStore,
  confirmBreakdown,
  finalActionLabel,
  summarizePlanImpact,
  type PlanImpactSummary,
} from "@/stores/cleanupStore";
import { useScanStore } from "@/stores/scanStore";
import { formatBytes, formatCount } from "@/utils/format";
import {
  actionPresentation,
  confirmationPresentation,
  presentationLabel,
  presentationTitle,
  reasonPresentation,
  type CleanupPresentation,
} from "@/utils/cleanupPresentation";
import { StatusBadge } from "./common";
import { useSelectionStore } from "@/stores/selectionStore";
import { useUiStore } from "@/stores/uiStore";
import { ModalFrame } from "./common/ModalFrame";

/**
 * The whole cleanup flow as overlay states driven by the cleanup store:
 * dock (selection bar) → plan review → dry-run report → escalating
 * confirm dialog → execution progress → session report.
 */
export function CleanupFlow() {
  const step = useCleanupStore((s) => s.step);

  return (
    <>
      <SelectionDock />
      {["planning", "plan-review", "dry-run", "dry-run-review"].includes(step) && <ReviewFlowModal />}
      {step === "confirming" && <ConfirmDialog />}
      {step === "executing" && <ExecutingModal />}
      {step === "session" && <SessionModal />}
      {step === "error" && <ErrorModal />}
    </>
  );
}

/* ---- 1. selection dock --------------------------------------------- */

function SelectionDock() {
  const items = useScanStore((s) => s.items);
  const phase = useScanStore((s) => s.phase);
  const selectedIds = useSelectionStore((s) => s.selected);
  const createPlan = useCleanupStore((s) => s.createPlan);
  const policy = useCleanupStore((s) => s.policy);
  const busy = useCleanupStore((s) => s.busy);
  const step = useCleanupStore((s) => s.step);
  const page = useUiStore((s) => s.page);
  const setPage = useUiStore((s) => s.setPage);
  const clearSelection = useSelectionStore((s) => s.clear);

  if (selectedIds.size === 0 || step !== "selecting") return null;

  const canCreatePlan = page === "agents" || page === "devcache" || page === "projects" || page === "risk-results" || page === "selected-items";

  const chosen = items.filter((item) => selectedIds.has(item.id));
  const bytes = chosen.reduce((sum, item) => sum + item.logical_size, 0);
  // Streamed rows are deliberately not yet an authoritative cleanup snapshot.
  // The backend writes and pins that snapshot only when the scan reaches `done`.
  const scanComplete = phase === "done";
  const scanHint = scanComplete
    ? null
    : phase === "scanning"
      ? "扫描仍在进行；完成后才能生成清理计划。"
      : "请先完成一轮扫描，再生成清理计划。";

  return (
    <div className={`cleanup-dock ${canCreatePlan ? "" : "cleanup-dock-hint-only"}`} role="region" aria-label="已选择的清理条目">
      <span className="summary">
        已勾选 <b>{selectedIds.size}</b> 项 · 逻辑大小估计 <b>{formatBytes(bytes)}</b>
      </span>
      {!canCreatePlan ? (
        <>
          <span className="cleanup-dock-hint" role="status">当前页面不提供清理计划入口。</span>
          <button type="button" className="btn small ghost" onClick={() => setPage("selected-items")}>查看已选</button>
        </>
      ) : scanHint ? (
        <span className="cleanup-dock-hint" role="status">
          {scanHint}
        </span>
      ) : null}
      {canCreatePlan && <select
        className="scope-select"
        value={policy}
        onChange={(event) => useCleanupStore.getState().setPolicy(event.target.value as ConfirmPolicy)}
        title="本次计划允许的确认级别"
        aria-label="本次计划允许的确认级别"
      >
        <option value="default">低风险候选 + 本地重建</option>
        <option value="redownload">包含：需重新下载</option>
        <option value="review">包含：需人工确认</option>
        <option value="all">所有可选风险（不含未知/受保护）</option>
      </select>}
      {canCreatePlan && <button
        type="button"
        className="btn primary"
        disabled={busy || !scanComplete}
        title={scanHint ?? "根据已勾选条目生成清理计划"}
        onClick={() => {
          // Re-check at activation time so a new scan cannot race an enabled button.
          if (useScanStore.getState().phase !== "done") return;
          void createPlan([...selectedIds], policy);
        }}
      >
        生成清理计划
      </button>}
      <button type="button" className="btn small ghost" onClick={() => clearSelection()}>清空全部选择</button>
    </div>
  );
}

/* ---- 2. stable plan review / dry-run shell ------------------------- */

function ReviewFlowModal() {
  const step = useCleanupStore((s) => s.step);
  const plan = useCleanupStore((s) => s.plan);
  const planContext = useCleanupStore((s) => s.planContext);
  const session = useCleanupStore((s) => s.dryRunSession);
  const runDryRun = useCleanupStore((s) => s.runDryRun);
  const close = useCleanupStore((s) => s.close);
  const busy = useCleanupStore((s) => s.busy);
  const policy = useCleanupStore((s) => s.policy);
  const selectedIds = useSelectionStore((s) => s.selected);
  const scanItems = useScanStore((s) => s.items);
  const scanGeneration = useScanStore((s) => s.generation);
  const closeRef = useRef<HTMLButtonElement>(null);
  const reviewRef = useRef<HTMLButtonElement>(null);

  const planning = step === "planning";
  const dryRunning = step === "dry-run";
  const busyStage = planning ? "planning" : dryRunning ? "dry-run" : null;
  const impact = plan
    ? summarizePlanImpact(plan, planContext, scanGeneration, [...selectedIds], policy)
    : null;
  const hasExecutableItems = plan ? plan.items.length > 0 : false;
  const canReview = Boolean(plan && hasExecutableItems && impact?.contextValid);
  const canConfirm = Boolean(session && impact?.contextValid);
  const selectedBytes = scanItems
    .filter((item) => selectedIds.has(item.id))
    .reduce((sum, item) => sum + item.logical_size, 0);
  const planTitle = plan ? `清理计划 #${plan.planId}` : "清理计划";
  const title = planning
    ? "正在生成清理计划…"
    : dryRunning
      ? "正在预演清理计划…"
      : step === "dry-run-review"
        ? "预演完成——未删除文件"
        : planTitle;
  const subtitle = planning
    ? "正在根据当前扫描快照计算可执行条目，请稍候。"
    : dryRunning
      ? "预演不会删除文件；工具查询等既有只读验证仍可能执行。"
      : step === "dry-run-review"
        ? "预演未删除文件；工具查询等只读验证可能已执行。"
        : plan
          ? hasExecutableItems
            ? `计划清理 ${plan.items.length} 项 · 逻辑大小估计 ${formatBytes(plan.totalEstimatedBytes)} · 跳过 ${plan.skipped.length} 项`
            : `没有可执行条目 · 跳过 ${plan.skipped.length} 项`
          : "等待清理计划结果。";
  const initialFocusRef = step === "dry-run-review" ? closeRef : step === "plan-review" ? reviewRef : undefined;

  return (
    <ModalFrame
      onDismiss={busyStage ? undefined : close}
      dismissible={!busyStage}
      className="review-modal"
      initialFocusRef={initialFocusRef}
    >
      {({ titleId, descriptionId }) => (
        <>
          <div className="modal-head">
            {planning || dryRunning || step === "dry-run-review" ? <FlaskConical size={18} /> : <FileWarning size={18} />}
            <div>
              <h2 id={titleId}>{title}</h2>
              <div id={descriptionId} className="sub">{subtitle}</div>
            </div>
            {!busyStage && (
              <button
                ref={closeRef}
                className="detail-close"
                onClick={close}
                style={{ marginLeft: "auto" }}
                aria-label="关闭清理复核"
                title="关闭清理复核"
              >
                <X size={16} />
              </button>
            )}
          </div>
          <div className="modal-body" aria-busy={busyStage !== null}>
            {impact ? (
              <ImpactSummaryPanel summary={impact} />
            ) : planning ? (
              <div className="plan-impact-summary" role="status">
                <div><b>已选择对象</b> {selectedIds.size} 项 · 逻辑大小估计 {formatBytes(selectedBytes)}</div>
                <div><b>影响摘要</b> 正在计算动作、风险和授权要求…</div>
              </div>
            ) : null}

            {planning && <ReviewBusyState stage="planning" />}

            {dryRunning && (
              <>
                {plan && <PlanItemsTable plan={plan} />}
                <ReviewBusyState stage="dry-run" />
              </>
            )}

            {step === "plan-review" && plan && (
              hasExecutableItems ? (
                <PlanItemsTable plan={plan} />
              ) : (
                <div className="plan-empty" role="status">
                  <strong>未形成可执行清理计划</strong>
                  <span>已跳过 {plan.skipped.length} 项，请调整选择或提高确认范围。</span>
                </div>
              )
            )}
            {step === "plan-review" && plan && <SkippedItems plan={plan} />}

            {step === "dry-run-review" && (
              session ? <SessionItemsTable session={session} /> : <div className="plan-empty" role="status">预演结果尚未返回，请稍候。</div>
            )}
          </div>
          <div className="modal-foot">
            {planning && <span className="review-wait" role="status">正在等待计划结果…</span>}
            {dryRunning && <span className="review-wait" role="status">正在等待预演结果…</span>}
            {step === "plan-review" && hasExecutableItems && (
              <>
                <button className="btn" onClick={close}>放弃</button>
                <button
                  className="btn primary"
                  ref={reviewRef}
                  disabled={busy || !canReview}
                  title={impact?.contextIssue ?? "先运行不删除预演"}
                  onClick={() => void runDryRun()}
                >
                  <FlaskConical size={14} /> 先预演（不删除）
                </button>
              </>
            )}
            {step === "plan-review" && !hasExecutableItems && (
              <button ref={reviewRef} className="btn primary" onClick={close}>返回调整选择</button>
            )}
            {step === "dry-run-review" && (
              <>
                <button ref={closeRef} className="btn" onClick={close}>关闭</button>
                <button className="btn danger" disabled={!canConfirm} onClick={() => useCleanupStore.setState({ step: "confirming" })}>
                  <Play size={14} /> 继续执行
                </button>
              </>
            )}
          </div>
        </>
      )}
    </ModalFrame>
  );
}

function ReviewBusyState({ stage }: { stage: "planning" | "dry-run" }) {
  const planning = stage === "planning";
  return (
    <div className="review-busy" role="status" aria-live="polite" aria-busy="true">
      <div className="exec-progress">
        <div className="exec-bar" aria-hidden="true"><i /></div>
      </div>
      <strong>{planning ? "正在生成清理计划…" : "正在预演清理计划…"}</strong>
      <span>{planning ? "按当前扫描代次校验条目和风险。" : "逐项验证计划；不会删除文件，工具查询等只读验证可能执行。"}</span>
    </div>
  );
}

function PlanItemsTable({ plan }: { plan: CleanupPlanDto }) {
  return (
    <div className="mini-table">
      <table className="mini">
        <thead>
          <tr><th>条目</th><th>操作</th><th>确认级别</th><th className="num">逻辑大小估计</th></tr>
        </thead>
        <tbody>
          {plan.items.map((item) => (
            <tr key={item.scanItemId}>
              <td className="mono" title={item.path}>{item.path}</td>
              <td><PresentationText presentation={actionPresentation(item.action)} /></td>
              <td><PresentationText presentation={confirmationPresentation(item.confirmation)} /></td>
              <td className="num">{formatBytes(item.estimatedSize)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SkippedItems({ plan }: { plan: CleanupPlanDto }) {
  if (plan.skipped.length === 0) return null;
  return (
    <div>
      <div className="q skip-title">跳过（{plan.skipped.length} 项）——规划器拒绝了这些勾选</div>
      <div className="skip-list">
        {plan.skipped.map((item) => (
          <div key={item.scanItemId} className="skip-row">
            <span className="mono">{item.path}</span>
            <PresentationText className="why" presentation={reasonPresentation(item.reason)} />
          </div>
        ))}
      </div>
    </div>
  );
}

/* ---- 4. escalating confirm ------------------------------------------ */

function ConfirmDialog() {
  const plan = useCleanupStore((s) => s.plan);
  const planContext = useCleanupStore((s) => s.planContext);
  const policy = useCleanupStore((s) => s.policy);
  const busy = useCleanupStore((s) => s.busy);
  const execute = useCleanupStore((s) => s.execute);
  const close = useCleanupStore((s) => s.close);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const back = () => useCleanupStore.setState({ step: "dry-run-review" });
  const scanGeneration = useScanStore((s) => s.generation);
  if (!plan || plan.items.length === 0) return null;

  const selectedIds = useSelectionStore((s) => s.selected);
  const impact = summarizePlanImpact(plan, planContext, scanGeneration, [...selectedIds], policy);
  const { redownload, review } = confirmBreakdown(plan);
  const finalAction = finalActionLabel(impact);
  // Risk consequences are always shown. `policy` only controls the backend
  // authorisation gate and must not hide the actual impact of this plan.
  const needsRedownload = redownload > 0;
  const needsReview = review > 0;

  return (
    <ModalFrame onDismiss={close} initialFocusRef={cancelRef}>
      {({ titleId, descriptionId }) => (
        <>
          <div className="modal-head">
            <ShieldAlert size={18} style={{ color: "var(--risk-review)" }} />
            <div>
              <h2 id={titleId}>确认最终清理</h2>
              <div id={descriptionId} className="sub">{finalAction} · 逻辑大小估计 {formatBytes(plan.totalEstimatedBytes)} · 策略「{policyLabel(policy)}」</div>
            </div>
          </div>
          <div className="modal-body">
            <ImpactSummaryPanel summary={impact} />
            {needsRedownload && (
              <div className="confirm-note redownload">
                <Download size={16} />
                <span><b>{redownload} 项具有重新下载成本。</b> 当前占用估计 {formatBytes(bytesOfConfirmation(plan, "redownload"))}；下次使用需要联网重新下载，实际流量和耗时取决于使用内容。</span>
              </div>
            )}
            {needsReview && (
              <div className="confirm-note review">
                <FileWarning size={16} />
                <span><b>{review} 项可能包含你重视的数据。</b> 会话历史与工作区状态一旦删除便无法找回；请确认已逐项查看影响。</span>
              </div>
            )}
            {!needsRedownload && !needsReview && (
              <div className="confirm-note redownload">
                <CheckCircle2 size={16} />
                <span>本计划没有需重新下载或人工确认的条目；这只是适用集合的授权提示，不代表零影响。执行前 CleanupEngine 仍会重新验证对象与安全边界。</span>
              </div>
            )}
          </div>
          <div className="modal-foot">
            <button className="btn" onClick={back}>返回预演</button>
            <button ref={cancelRef} className="btn" onClick={close}>取消</button>
            <button
              className="btn danger"
              disabled={busy || !impact.contextValid}
              title={impact.contextIssue ?? "执行当前清理计划"}
              onClick={() => void execute()}
            >
              <Play size={14} /> {finalAction}
            </button>
          </div>
        </>
      )}
    </ModalFrame>
  );
}

function bytesOfConfirmation(plan: CleanupPlanDto, confirmation: "redownload" | "review"): number {
  return plan.items
    .filter((item) => item.confirmation === confirmation)
    .reduce((sum, item) => sum + item.estimatedSize, 0);
}

function policyLabel(policy: ConfirmPolicy): string {
  switch (policy) {
    case "default": return "低风险";
    case "redownload": return "包含重新下载";
    case "review": return "包含人工确认";
    case "all": return "所有可选风险";
  }
}

/* ---- 5. executing ---------------------------------------------------- */

function ExecutingModal() {
  const session = useCleanupStore((s) => s.finalSession);
  const plan = useCleanupStore((s) => s.plan);
  if (session || !plan || plan.items.length === 0) return null;

  return (
    <ModalFrame dismissible={false} className="modal-compact">
      {({ titleId, descriptionId }) => (
        <>
          <div className="modal-head">
            <Play size={18} />
            <div>
              <h2 id={titleId}>正在执行清理…</h2>
              <div id={descriptionId} className="sub">CleanupEngine 正在处理，请勿关闭窗口</div>
            </div>
          </div>
          <div className="modal-body">
            <div className="exec-progress">
              <div className="exec-bar" aria-hidden="true"><i /></div>
              <div className="exec-line"><span>已排队 {plan.items.length} 项</span><span>{formatBytes(plan.totalEstimatedBytes)}</span></div>
            </div>
          </div>
        </>
      )}
    </ModalFrame>
  );
}

/* ---- 6. session report ---------------------------------------------- */

function SessionModal() {
  const session = useCleanupStore((s) => s.finalSession);
  const plan = useCleanupStore((s) => s.plan);
  const planContext = useCleanupStore((s) => s.planContext);
  const close = useCleanupStore((s) => s.close);
  const reload = useScanStore((s) => s.loadLatest);
  const doneRef = useRef<HTMLButtonElement>(null);
  if (!session) return null;

  const okBytes = session.items.filter((item) => item.status === "ok").reduce((sum, item) => sum + item.estimatedSize, 0);
  const impact = plan && planContext
    ? summarizePlanImpact(plan, planContext, planContext.scanGeneration)
    : null;

  return (
    <ModalFrame onDismiss={close} initialFocusRef={doneRef}>
      {({ titleId, descriptionId }) => (
        <>
          <div className="modal-head">
            <CheckCircle2 size={18} style={{ color: "var(--ok)" }} />
            <div>
              <h2 id={titleId}>清理完成——计划 #{session.planId} 的结果</h2>
              <div id={descriptionId} className="sub">会话 #{session.sessionId} · 原扫描代次 {planContext?.scanGeneration ?? "未知"} · 成功 {session.totals.succeeded} 项 · 跳过 {session.totals.skipped} 项 · 失败 {session.totals.failed} 项</div>
            </div>
          </div>
          <div className="modal-body">
            <div className="session-report">
              <div className="totals-grid">
                <div className="total-cell good"><div className="k">成功处理估计</div><div className="v">{formatBytes(okBytes)}</div></div>
                <div className="total-cell"><div className="k">计划处理估计</div><div className="v">{formatBytes(session.totals.plannedBytes)}</div></div>
                <div className="total-cell warn"><div className="k">已跳过</div><div className="v">{formatCount(session.totals.skipped)}</div></div>
                <div className="total-cell bad"><div className="k">失败</div><div className="v">{formatCount(session.totals.failed)}</div></div>
              </div>
              {session.journalDegraded && <div className="journal-flag"><XCircle size={15} /> 日志降级——本次会话记录不完整（请检查日志目录）。</div>}
              {impact && <ImpactSummaryPanel summary={impact} />}
              <SessionItemsTable session={session} />
            </div>
          </div>
          <div className="modal-foot">
            <button ref={doneRef} className="btn primary" onClick={() => { void reload(); close(); }}>完成</button>
          </div>
        </>
      )}
    </ModalFrame>
  );
}

function SessionItemsTable({ session }: { session: NonNullable<ReturnType<typeof useCleanupStore.getState>["dryRunSession"]> }) {
  return (
    <div className="mini-table">
      <table className="mini">
        <thead><tr><th>条目</th><th>操作</th><th>结果</th><th className="num">逻辑大小估计</th></tr></thead>
        <tbody>
          {session.items.map((item) => (
            <tr key={item.scanItemId}>
              <td className="mono" title={item.path}>{item.path}</td>
              <td><PresentationText presentation={actionPresentation(item.action)} /></td>
              <td>
                <StatusBadge status={item.status} />
                {item.detail && <PresentationText className="session-detail" presentation={reasonPresentation(item.detail)} />}
              </td>
              <td className="num">{formatBytes(item.estimatedSize)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function ImpactSummaryPanel({ summary }: { summary: PlanImpactSummary }) {
  const actions = [
    `永久删除 ${summary.actions.permanentDelete} 项`,
    `执行工具清理 ${summary.actions.toolCleanup} 项`,
    summary.actions.unrecognized > 0 && `未识别动作 ${summary.actions.unrecognized} 项`,
  ].filter(Boolean).join(" · ");
  const impacts = [
    `低风险候选 ${summary.risks.safe.count} 项（${formatBytes(summary.risks.safe.bytes)}）`,
    `本地重建 ${summary.risks.regenerableLocal.count} 项（${formatBytes(summary.risks.regenerableLocal.bytes)}）`,
    `需重新下载 ${summary.risks.regenerableDownload.count} 项（${formatBytes(summary.risks.regenerableDownload.bytes)}）`,
    `可能失去历史 ${summary.risks.review.count} 项（${formatBytes(summary.risks.review.bytes)}）`,
    summary.risks.unmatched.count > 0 && `风险未匹配 ${summary.risks.unmatched.count} 项`,
  ].filter(Boolean).join(" · ");
  const authorization = [
    summary.authorization.none > 0 && `无需额外风险授权 ${summary.authorization.none} 项`,
    summary.authorization.redownload > 0 && `需重新下载授权 ${summary.authorization.redownload} 项`,
    summary.authorization.review > 0 && `需人工确认授权 ${summary.authorization.review} 项`,
    summary.authorization.unrecognized > 0 && `未识别确认级别 ${summary.authorization.unrecognized} 项`,
  ].filter(Boolean).join(" · ");

  return (
    <div className="plan-impact-summary" role={summary.contextValid ? "status" : "alert"}>
      <div><b>处理对象</b> {summary.itemCount} 项 · 逻辑大小估计 {formatBytes(summary.estimatedBytes)}</div>
      <div><b>处理方式</b> {actions || "—"}</div>
      <div><b>影响与恢复成本</b> {impacts || "未能匹配风险信息"}</div>
      <div><b>授权要求</b> {authorization || "—"}</div>
      {!summary.contextValid && <strong>{summary.contextIssue ?? "计划上下文不完整，请重新生成"}</strong>}
    </div>
  );
}

/* ---- error ---------------------------------------------------------- */

function ErrorModal() {
  const error = useCleanupStore((s) => s.error);
  const close = useCleanupStore((s) => s.close);
  const closeRef = useRef<HTMLButtonElement>(null);
  if (!error) return null;

  return (
    <ModalFrame onDismiss={close} className="modal-error" initialFocusRef={closeRef}>
      {({ titleId, descriptionId }) => (
        <>
          <div className="modal-head">
            <XCircle size={18} style={{ color: "var(--danger)" }} />
            <div><h2 id={titleId}>清理失败</h2><div id={descriptionId} className="sub code">{error.code}</div></div>
          </div>
          <div className="modal-body">
            <p className="error-copy">{error.message}</p>
            {error.required && (
              <p className="error-required">
                本计划需要 <b><PresentationText presentation={confirmationPresentation(error.required)} /></b> 级别的确认。请提高策略级别后重试。
              </p>
            )}
          </div>
          <div className="modal-foot"><button ref={closeRef} className="btn" onClick={close}>关闭</button></div>
        </>
      )}
    </ModalFrame>
  );
}

function PresentationText({
  presentation,
  className,
}: {
  presentation: CleanupPresentation;
  className?: string;
}) {
  return (
    <span className={className} title={presentationTitle(presentation)}>
      {presentationLabel(presentation)}
    </span>
  );
}
