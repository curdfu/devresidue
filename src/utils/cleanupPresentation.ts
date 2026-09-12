import type { Confirmation, PlanAction } from "@/types";

/**
 * A wire value plus the short, user-facing explanation of what it means.
 *
 * `raw` is intentionally kept on every presentation. The frontend must not
 * erase an enum value it does not recognise: an unknown value is a signal to
 * inspect the backend contract, not permission to guess an action.
 */
export interface CleanupPresentation {
  label: string;
  summary: string;
  raw: string | null;
}

function presentation(label: string, summary: string, raw: string | null): CleanupPresentation {
  return { label, summary, raw };
}

/** Wire cleanup actions -> stable Chinese UI vocabulary. */
export function actionPresentation(action: PlanAction | string | null | undefined): CleanupPresentation {
  if (action === null || action === undefined || action === "") {
    return presentation("未提供操作", "后端没有返回清理动作，不能安全推断影响。", action ?? null);
  }

  switch (action) {
    case "recycle":
      // The current WindowsDeletePort engine path dispatches RecycleBin to
      // delete_tree_verified. Do not promise Recycle Bin recovery here.
      return presentation(
        "永久删除",
        "当前 Windows 实现会以验证绑定的永久删除执行；删除后不能从回收站恢复。",
        action,
      );
    case "delete":
      return presentation("永久删除", "删除已验证对象；删除后不能从回收站恢复。", action);
    case "execute":
      return presentation(
        "执行工具清理",
        "调用已批准的外部工具清理命令；实际影响取决于工具返回结果。",
        action,
      );
    default:
      return presentation(
        "未识别操作",
        `无法安全判断清理影响；请核对原始动作值“${action}”。`,
        action,
      );
  }
}

/** Wire confirmation requirements -> risk/authorisation vocabulary. */
export function confirmationPresentation(
  confirmation: Confirmation | string | null | undefined,
): CleanupPresentation {
  if (confirmation === null || confirmation === undefined || confirmation === "") {
    return presentation("未提供确认级别", "后端没有返回确认级别，不能安全推断授权要求。", confirmation ?? null);
  }

  switch (confirmation) {
    case "none":
      return presentation("无需额外风险授权", "此条目不要求额外风险确认，但仍受最终安全校验约束。", confirmation);
    case "redownload":
      return presentation(
        "需重新下载",
        "执行后下次使用可能需要联网重新下载；这是恢复成本提示，不是安全豁免。",
        confirmation,
      );
    case "review":
      return presentation(
        "需人工确认",
        "执行前需要人工确认，可能丢失历史或工作区状态。",
        confirmation,
      );
    default:
      return presentation(
        "未识别确认级别",
        `无法安全判断授权要求；请核对原始确认值“${confirmation}”。`,
        confirmation,
      );
  }
}

function withTail(raw: string, prefix: string, label: string): string {
  const tail = raw.slice(prefix.length).replace(/^[:\s-]+/, "").trim();
  return tail ? `${label}（${tail}）` : label;
}

const SKIP_REASON_RULES: Array<[string, string]> = [
  ["skip: protected-risk", "跳过：受保护风险"],
  ["skip: unknown-risk", "跳过：未知风险"],
  ["skip: action none", "跳过：无可执行操作"],
  ["skip: deferred", "跳过：已暂缓"],
  ["skip: requires redownload confirmation", "跳过：需要「需重新下载」级别确认"],
  ["skip: requires review confirmation", "跳过：需要「需人工确认」级别确认"],
  ["skip: unknown selection", "跳过：未知勾选"],
  ["skip: unverifiable", "跳过：无法验证"],
];

const REASON_RULES: Array<[string, string]> = [
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
  ["defer:process-running", "暂缓：相关工具进程正在运行——退出该工具后重试"],
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

/**
 * Summarise skip, deny, defer and dry-run details without discarding the
 * original backend text. Unknown reasons intentionally remain visible.
 */
export function reasonPresentation(reason: string | null | undefined): CleanupPresentation {
  if (reason === null || reason === undefined || reason === "") {
    return presentation("无详细原因", "后端没有提供原因详情。", reason ?? null);
  }

  if (reason === "would recycle") {
    return presentation("预演", "预演将执行永久删除。", reason);
  }
  if (reason === "would delete") {
    return presentation("预演", "预演将执行永久删除。", reason);
  }
  if (reason === "would execute") {
    return presentation("预演", "预演将执行工具清理。", reason);
  }

  for (const [prefix, label] of SKIP_REASON_RULES) {
    if (reason.startsWith(prefix)) {
      return presentation("已跳过", withTail(reason, prefix, label), reason);
    }
  }

  for (const [prefix, label] of REASON_RULES) {
    if (reason.startsWith(prefix)) {
      const summary = reason === prefix ? label : withTail(reason, prefix, label);
      const kind = prefix.startsWith("defer:") ? "已暂缓" : prefix.startsWith("deny:") ? "已拒绝" : "未执行";
      return presentation(kind, summary, reason);
    }
  }

  return presentation("原始原因", reason, reason);
}

/** Render unknown values explicitly while keeping known labels compact. */
export function presentationLabel(value: CleanupPresentation): string {
  if (value.label === "未识别操作" || value.label === "未识别确认级别") {
    return value.raw ? `${value.label}（原值：${value.raw}）` : value.label;
  }
  return value.label;
}

/** Tooltip text contains both the translated explanation and the wire value. */
export function presentationTitle(value: CleanupPresentation): string {
  if (!value.raw || value.raw === value.summary) return value.summary;
  return `${value.summary}\n原始值：${value.raw}`;
}
