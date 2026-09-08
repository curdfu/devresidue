import type { RiskLevel } from "@/types";

/** Binary size, engineering notation (1.2 GiB / 384 MiB / 24 KiB). */
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const v = bytes / 1024 ** i;
  const digits = v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return `${v.toFixed(digits)} ${units[i]}`;
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}

/** "3 周前" style ages; null → "—". */
export function formatAge(epochSecs: number | null): string {
  if (epochSecs === null) return "—";
  const days = (Date.now() / 1000 - epochSecs) / 86_400;
  if (days < 1) return "今天";
  if (days < 2) return "昨天";
  if (days < 7) return `${Math.floor(days)} 天前`;
  if (days < 30) {
    const w = Math.floor(days / 7);
    return w === 1 ? "1 周前" : `${w} 周前`;
  }
  if (days < 365) {
    const m = Math.floor(days / 30);
    return m === 1 ? "1 个月前" : `${m} 个月前`;
  }
  const y = Math.floor(days / 365);
  return y === 1 ? "1 年前" : `${y} 年前`;
}

export function formatDateTime(epochSecs: number): string {
  return new Date(epochSecs * 1000).toLocaleString("zh-CN", {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

// ---- Risk vocabulary (kebab-case → UI label / color token / one-liner) ------

export interface RiskMeta {
  label: string;
  groupLabel: string;
  className: string;
  hint: string;
}

const RISK_META: Record<RiskLevel, RiskMeta> = {
  safe: {
    label: "安全",
    groupLabel: "可安全清理",
    className: "risk-safe",
    hint: "删除后无持久影响，工具会按需重新生成。",
  },
  "regenerable-local": {
    label: "本地可重建",
    groupLabel: "本地可重建",
    className: "risk-local",
    hint: "下次构建时在本地重新生成，无需联网下载。",
  },
  "regenerable-download": {
    label: "需重新下载",
    groupLabel: "删除后需重新下载",
    className: "risk-download",
    hint: "下次安装时需要从网络重新下载。",
  },
  review: {
    label: "需人工确认",
    groupLabel: "需人工确认",
    className: "risk-review",
    hint: "可能包含有价值的数据（会话记录、历史、工作区状态）。",
  },
  protected: {
    label: "受保护",
    groupLabel: "受保护",
    className: "risk-protected",
    hint: "永不清理——内置保护规则（凭据、配置）。",
  },
  unknown: {
    label: "未知",
    groupLabel: "未知数据",
    className: "risk-unknown",
    hint: "没有规则识别此数据；永不自动删除。",
  },
};

export function riskMeta(risk: RiskLevel): RiskMeta {
  return RISK_META[risk];
}

/** Items that may ever be selected for a cleanup plan. */
export function isSelectable(risk: RiskLevel): boolean {
  return risk !== "protected" && risk !== "unknown";
}

// ---- Category vocabulary -----------------------------------------------------

export function categoryLabel(category: string): string {
  const map: Record<string, string> = {
    "ai-agent": "AI Agent 数据",
    ide: "IDE 数据",
    "developer-cache": "开发缓存",
    "package-cache": "包缓存",
    "build-artifact": "构建产物",
    dependency: "依赖目录",
    log: "日志",
    temporary: "临时文件",
    session: "会话历史",
    "workspace-state": "工作区状态",
    configuration: "配置",
    credential: "凭据",
    unknown: "未知数据",
  };
  return map[category] ?? category;
}

/** Provider source → who detected it (PLAN six-question vocabulary). */
export function sourceLabel(source: string): string {
  const map: Record<string, string> = {
    rule: "规则引擎",
    kondo: "Kondo（项目扫描）",
    "developer-cache-provider": "开发缓存 Provider",
    "package-manager": "包管理器",
    "agent-provider": "Agent Provider",
    "ai-analysis": "AI 分析",
  };
  return map[source] ?? source;
}

/** Human phrasing of the deletion impact, derived from risk + explanation. */
export function deletionImpact(risk: RiskLevel, size: number): string {
  switch (risk) {
    case "safe":
      return "无持久影响，工具会按需重新生成。";
    case "regenerable-local":
      return `下次构建时本地重新生成（约 ${formatBytes(size)} 的编译时间）。`;
    case "regenerable-download":
      return `下次安装时需重新下载 ${formatBytes(size)}。`;
    case "review":
      return `不可恢复——${formatBytes(size)} 可能包含有价值的数据，请先确认。`;
    case "protected":
      return "拒绝删除。此路径受内置规则保护。";
    case "unknown":
      return "无法从这里删除。DevResidue 绝不删除无法分类的数据。";
  }
}
