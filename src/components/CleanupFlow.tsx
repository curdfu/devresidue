import { useEffect, useId, useRef, type ReactNode } from "react";
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
import type { CleanupPlanDto, ConfirmPolicy, PlanAction, Confirmation } from "@/types";
import { useCleanupStore, confirmBreakdown } from "@/stores/cleanupStore";
import { useScanStore } from "@/stores/scanStore";
import { formatBytes, formatCount } from "@/utils/format";
import { StatusBadge } from "./common";
import { useSelectionStore } from "@/stores/selectionStore";

/** Wire verbs → human labels (plan/session tables share the vocabulary). */
function actionLabel(action: PlanAction | string): string {
  switch (action) {
    case "recycle":
      // The engine executes RecycleBin as a verified permanent deletion on
      // Windows (the recycle API cannot be handle-bound); label accordingly.
      return "永久删除";
    case "delete":
      return "永久删除";
    case "execute":
      return "执行工具命令";
    default:
      return action;
  }
}

function confirmLabel(c: Confirmation | string): string {
  switch (c) {
    case "none":
      return "无需确认";
    case "redownload":
      return "需重新下载";
    case "review":
      return "需人工确认";
    default:
      return c;
  }
}

/** Mock/backend skip reasons keep their "skip: …" machine prefix; translate the head. */
function skipReasonLabel(reason: string): string {
  if (reason.startsWith("skip: protected-risk")) return "跳过：受保护风险";
  if (reason.startsWith("skip: unknown-risk")) return "跳过：未知风险";
  if (reason.startsWith("skip: action none")) return "跳过：无可执行操作";
  if (reason.startsWith("skip: deferred")) return reason.replace("skip: deferred", "跳过：已暂缓");
  if (reason.startsWith("skip: requires")) {
    return reason
      .replace("skip: requires redownload confirmation", "跳过：需要「需重新下载」级别确认")
      .replace("skip: requires review confirmation", "跳过：需要「需人工确认」级别确认");
  }
  if (reason.startsWith("skip: unknown selection")) return "跳过：未知勾选";
  if (reason.startsWith("skip: unverifiable")) return reason.replace("skip: unverifiable", "跳过：无法验证");
  return reason;
}

/** Per-item session detail → human phrasing.
 *
 * Real-backend engine reasons are `verb:reason-key: tail` ("defer:…",
 * "deny:…", plain gate messages like "mode-mismatch: …"); mock-mode plan
 * skips use the "skip: …" prefix. Every known head maps to a short Chinese
 * label; the tail (the engine's explanatory sentence) is preserved so the
 * user sees the CONCRETE reason, never just "跳过".
 */
function sessionItemDetail(detail: string): string {
  // Engine deny reasons (SPEC §15 vocabulary).
  const denyMap: Array<[string, string]> = [
    ["deny:protected-root-exact", "已拒绝：命中受保护根（精确匹配）"],
    ["deny:protected-root-ancestor", "已拒绝：位于受保护根之上（删除会波及保护目录）"],
    ["deny:protected-risk", "已拒绝：受保护风险"],
    ["deny:unknown-risk", "已拒绝：未知风险"],
    ["deny:reparse-crossing", "已拒绝：路径穿过 reparse point"],
    ["deny:reparse-target", "已拒绝：目标是 reparse point"],
    ["deny:reparse-substitution", "已拒绝：reparse 替换攻击嫌疑"],
    ["deny:identity-changed", "已拒绝：对象与扫描时不符（已被替换）"],
    ["deny:identity-not-recorded", "已拒绝：缺少扫描时身份记录"],
    ["deny:stale-snapshot", "已拒绝：快照已过期"],
    ["deny:target-missing", "已拒绝：目标已不存在"],
    ["deny:read-only-changed", "已拒绝：只读属性发生变化"],
    ["deny:process-unknown", "已拒绝：无法确认相关进程状态"],
    ["deny:unverifiable", "已拒绝：无法验证"],
    ["deny:not-absolute", "已拒绝：路径不是绝对路径"],
    ["deny:path-mismatch", "已拒绝：路径与快照不符"],
    ["deny:requires review confirmation", "已拒绝：需要人工确认"],
    ["deny:requires redownload confirmation", "已拒绝：需要重新下载确认"],
  ];
  // Engine defer reasons.
  const deferMap: Array<[string, string]> = [
    ["defer:process-running", "暂缓：相关工具进程正在运行——退出该工具后重试"],
  ];
  // Plain authorisation-gate messages (R2/R3/R4 vocabulary).
  const gateMap: Array<[string, string]> = [
    ["item-not-in-current-scan", "条目不属于最新扫描——请重新扫描后再计划"],
    ["plan-snapshot-mismatch", "计划快照与扫描记录不一致（可能被篡改）"],
    ["risk-declaration-mismatch", "计划声明的风险与扫描记录不符"],
    ["confirmation-downgraded", "计划确认级别低于该项要求"],
    ["mode-mismatch", "计划的清理方式与扫描授权不符"],
    ["command-mismatch", "计划命令与扫描冻结的命令不符"],
    ["protected-descendant", "目标树内含受保护区域——整树删除会波及"],
    ["discovery-source-invalidated", "分类规则已失效（规则被移除或不再匹配）"],
    ["action-mismatch", "扫描记录未授权删除该项"],
    ["unbound-delete-refused", "对象绑定失败，未执行删除（可重试）"],
    ["identity mismatch at port", "删除时对象已被替换——未删除"],
  ];
  // Dry-run verbs.
  if (detail === "would recycle") return "将永久删除";
  if (detail === "would delete") return "将永久删除";
  if (detail === "would execute") return "将执行工具命令";
  // "skip: …" (mock-mode plan skips).
  const skip = skipReasonLabel(detail);
  if (skip !== detail) return skip;
  // Engine reason heads: translate the head, keep the concrete tail.
  for (const [prefix, label] of [...denyMap, ...deferMap, ...gateMap]) {
    if (detail.startsWith(prefix)) {
      const tail = detail.slice(prefix.length).replace(/^[:\s]+/, "").trim();
      return tail ? `${label}（${tail}）` : label;
    }
  }
  return detail;
}

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
      {step === "plan-review" && <PlanReviewModal />}
      {step === "dry-run-review" && <DryRunModal />}
      {step === "confirming" && <ConfirmDialog />}
      {step === "executing" && <ExecutingModal />}
      {step === "session" && <SessionModal />}
      {step === "error" && <ErrorModal />}
    </>
  );
}

/**
 * Shared keyboard-safe dialog frame. Overlays never close on backdrop clicks:
 * cleanup decisions remain deliberate. While the engine is executing the
 * frame is explicitly non-dismissible, including through Escape.
 */
function ModalFrame({
  children,
  onDismiss,
  dismissible = true,
  className = "",
}: {
  children: (titleId: string) => ReactNode;
  onDismiss?: () => void;
  dismissible?: boolean;
  className?: string;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const onDismissRef = useRef(onDismiss);
  const titleId = useId();
  onDismissRef.current = onDismiss;

  useEffect(() => {
    const dialog = dialogRef.current;
    previousFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;

    const focusInitial = () => {
      const target = dialog?.querySelector<HTMLElement>(
        "[data-autofocus], button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
      );
      (target ?? dialog)?.focus();
    };
    focusInitial();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && dismissible) {
        event.preventDefault();
        onDismissRef.current?.();
        return;
      }
      if (event.key !== "Tab" || !dialog) return;

      const focusable = Array.from(
        dialog.querySelectorAll<HTMLElement>(
          "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
        ),
      ).filter((element) => !element.hasAttribute("hidden") && element.getClientRects().length > 0);

      if (focusable.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }

      const first = focusable.at(0);
      const last = focusable.at(-1);
      if (!first || !last) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      if (previousFocusRef.current?.isConnected) previousFocusRef.current.focus();
    };
  }, [dismissible]);

  return (
    <div className="modal-veil">
      <div
        ref={dialogRef}
        className={`modal ${className}`.trim()}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
      >
        {children(titleId)}
      </div>
    </div>
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

  if (selectedIds.size === 0 || step !== "selecting") return null;

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
    <div className="cleanup-dock" role="region" aria-label="已选择的清理条目">
      <span className="summary">
        已勾选 <b>{selectedIds.size}</b> 项 · 占用 <b>{formatBytes(bytes)}</b>
      </span>
      {scanHint && (
        <span className="cleanup-dock-hint" role="status">
          {scanHint}
        </span>
      )}
      <select
        className="scope-select"
        value={policy}
        onChange={(event) => useCleanupStore.getState().setPolicy(event.target.value as ConfirmPolicy)}
        title="本次计划允许的确认级别"
        aria-label="本次计划允许的确认级别"
      >
        <option value="default">低风险：安全 + 本地重建</option>
        <option value="redownload">包含：需重新下载</option>
        <option value="review">包含：需人工确认</option>
        <option value="all">所有可选风险（不含未知/受保护）</option>
      </select>
      <button
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
      </button>
    </div>
  );
}

/* ---- 2. plan review ------------------------------------------------ */

function PlanReviewModal() {
  const plan = useCleanupStore((s) => s.plan);
  const runDryRun = useCleanupStore((s) => s.runDryRun);
  const close = useCleanupStore((s) => s.close);
  const busy = useCleanupStore((s) => s.busy);
  if (!plan) return null;

  const hasExecutableItems = plan.items.length > 0;
  return (
    <ModalFrame onDismiss={close}>
      {(titleId) => (
        <>
          <div className="modal-head">
            <FileWarning size={18} />
            <div>
              <h2 id={titleId}>清理计划 #{plan.planId}</h2>
              <div className="sub">
                {hasExecutableItems
                  ? `计划清理 ${plan.items.length} 项 · 预计 ${formatBytes(plan.totalEstimatedBytes)} · 跳过 ${plan.skipped.length} 项`
                  : `没有可执行条目 · 跳过 ${plan.skipped.length} 项`}
              </div>
            </div>
            <button
              className="detail-close"
              onClick={close}
              style={{ marginLeft: "auto" }}
              aria-label="关闭清理计划"
              title="关闭清理计划"
            >
              <X size={16} />
            </button>
          </div>
          <div className="modal-body">
            {hasExecutableItems ? (
              <PlanItemsTable plan={plan} />
            ) : (
              <div className="plan-empty" role="status">
                <strong>未形成可执行清理计划</strong>
                <span>已跳过 {plan.skipped.length} 项，请调整选择或提高确认范围。</span>
              </div>
            )}
            <SkippedItems plan={plan} />
          </div>
          <div className="modal-foot">
            {hasExecutableItems ? (
              <>
                <button className="btn" onClick={close}>放弃</button>
                <button className="btn primary" data-autofocus disabled={busy} onClick={() => void runDryRun()}>
                  <FlaskConical size={14} /> 先预演（不删除）
                </button>
              </>
            ) : (
              <button className="btn primary" data-autofocus onClick={close}>返回调整选择</button>
            )}
          </div>
        </>
      )}
    </ModalFrame>
  );
}

function PlanItemsTable({ plan }: { plan: CleanupPlanDto }) {
  return (
    <div className="mini-table">
      <table className="mini">
        <thead>
          <tr><th>条目</th><th>操作</th><th>确认级别</th><th className="num">大小</th></tr>
        </thead>
        <tbody>
          {plan.items.map((item) => (
            <tr key={item.scanItemId}>
              <td className="mono" title={item.path}>{item.path}</td>
              <td>{actionLabel(item.action)}</td>
              <td>{confirmLabel(item.confirmation)}</td>
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
            <span className="why">{skipReasonLabel(item.reason)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

/* ---- 3. dry-run report ---------------------------------------------- */

function DryRunModal() {
  const session = useCleanupStore((s) => s.dryRunSession);
  const close = useCleanupStore((s) => s.close);
  const setStep = () => useCleanupStore.setState({ step: "confirming" });
  if (!session) return null;

  return (
    <ModalFrame onDismiss={close}>
      {(titleId) => (
        <>
          <div className="modal-head">
            <FlaskConical size={18} />
            <div>
              <h2 id={titleId}>预演完成——未删除任何数据</h2>
              <div className="sub">引擎对计划 #{session.planId} 将执行的操作预览</div>
            </div>
          </div>
          <div className="modal-body"><SessionItemsTable session={session} /></div>
          <div className="modal-foot">
            <button className="btn" onClick={close}>关闭</button>
            <button className="btn danger" data-autofocus onClick={setStep}>
              <Play size={14} /> 继续执行
            </button>
          </div>
        </>
      )}
    </ModalFrame>
  );
}

/* ---- 4. escalating confirm ------------------------------------------ */

function ConfirmDialog() {
  const plan = useCleanupStore((s) => s.plan);
  const policy = useCleanupStore((s) => s.policy);
  const execute = useCleanupStore((s) => s.execute);
  const close = useCleanupStore((s) => s.close);
  const back = () => useCleanupStore.setState({ step: "dry-run-review" });
  if (!plan || plan.items.length === 0) return null;

  const { redownload, review } = confirmBreakdown(plan);
  const needsRedownload = redownload > 0 && policy === "default";
  const needsReview = review > 0 && (policy === "default" || policy === "redownload");

  return (
    <ModalFrame onDismiss={close}>
      {(titleId) => (
        <>
          <div className="modal-head">
            <ShieldAlert size={18} style={{ color: "var(--risk-review)" }} />
            <div>
              <h2 id={titleId}>确认执行清理？</h2>
              <div className="sub">{plan.items.length} 项 · {formatBytes(plan.totalEstimatedBytes)} · 策略「{policyLabel(policy)}」</div>
            </div>
          </div>
          <div className="modal-body">
            {needsRedownload && (
              <div className="confirm-note redownload">
                <Download size={16} />
                <span><b>{redownload} 项删除后需重新下载。</b> 下次安装时会从网络重新下载 {formatBytes(bytesOfConfirmation(plan, "redownload"))}；不会损坏工具，但会花费时间和带宽。</span>
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
                <span>本计划仅包含低风险候选。执行前 CleanupEngine 仍会重新验证对象与安全边界。</span>
              </div>
            )}
          </div>
          <div className="modal-foot">
            <button className="btn" onClick={back}>返回预演</button>
            <button className="btn" onClick={close}>取消</button>
            <button className="btn danger" data-autofocus onClick={() => void execute()}>
              <Play size={14} /> 执行（{formatBytes(plan.totalEstimatedBytes)}）
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
      {(titleId) => (
        <>
          <div className="modal-head">
            <Play size={18} />
            <div>
              <h2 id={titleId}>正在执行清理…</h2>
              <div className="sub">CleanupEngine 正在处理，请勿关闭窗口</div>
            </div>
          </div>
          <div className="modal-body">
            <div className="exec-progress">
              <div className="exec-bar"><i style={{ width: "35%", animation: "sweep 1.2s linear infinite" }} /></div>
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
  const close = useCleanupStore((s) => s.close);
  const reload = useScanStore((s) => s.loadLatest);
  if (!session) return null;

  const okBytes = session.items.filter((item) => item.status === "ok").reduce((sum, item) => sum + item.estimatedSize, 0);

  return (
    <ModalFrame onDismiss={close}>
      {(titleId) => (
        <>
          <div className="modal-head">
            <CheckCircle2 size={18} style={{ color: "var(--ok)" }} />
            <div>
              <h2 id={titleId}>清理完成——会话 #{session.sessionId}</h2>
              <div className="sub">成功 {session.totals.succeeded} 项 · 跳过 {session.totals.skipped} 项 · 失败 {session.totals.failed} 项</div>
            </div>
          </div>
          <div className="modal-body">
            <div className="session-report">
              <div className="totals-grid">
                <div className="total-cell good"><div className="k">已释放</div><div className="v">{formatBytes(okBytes)}</div></div>
                <div className="total-cell"><div className="k">计划清理</div><div className="v">{formatBytes(session.totals.plannedBytes)}</div></div>
                <div className="total-cell warn"><div className="k">已跳过</div><div className="v">{formatCount(session.totals.skipped)}</div></div>
                <div className="total-cell bad"><div className="k">失败</div><div className="v">{formatCount(session.totals.failed)}</div></div>
              </div>
              {session.journalDegraded && <div className="journal-flag"><XCircle size={15} /> 日志降级——本次会话记录不完整（请检查日志目录）。</div>}
              <SessionItemsTable session={session} />
            </div>
          </div>
          <div className="modal-foot">
            <button className="btn primary" data-autofocus onClick={() => { void reload(); close(); }}>完成</button>
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
        <thead><tr><th>条目</th><th>操作</th><th>结果</th><th className="num">大小</th></tr></thead>
        <tbody>
          {session.items.map((item) => (
            <tr key={item.scanItemId}>
              <td className="mono" title={item.path}>{item.path}</td>
              <td>{actionLabel(item.action)}</td>
              <td>
                <StatusBadge status={item.status} />
                {item.detail && <span className="session-detail" title={item.detail}>{sessionItemDetail(item.detail)}</span>}
              </td>
              <td className="num">{formatBytes(item.estimatedSize)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/* ---- error ---------------------------------------------------------- */

function ErrorModal() {
  const error = useCleanupStore((s) => s.error);
  const close = useCleanupStore((s) => s.close);
  if (!error) return null;

  return (
    <ModalFrame onDismiss={close} className="modal-error">
      {(titleId) => (
        <>
          <div className="modal-head">
            <XCircle size={18} style={{ color: "var(--danger)" }} />
            <div><h2 id={titleId}>清理失败</h2><div className="sub code">{error.code}</div></div>
          </div>
          <div className="modal-body">
            <p className="error-copy">{error.message}</p>
            {error.required && <p className="error-required">本计划需要 <b>{confirmLabel(error.required)}</b> 级别的确认。请提高策略级别后重试。</p>}
          </div>
          <div className="modal-foot"><button className="btn" data-autofocus onClick={close}>关闭</button></div>
        </>
      )}
    </ModalFrame>
  );
}
