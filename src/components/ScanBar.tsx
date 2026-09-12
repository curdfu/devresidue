import { useState } from "react";
import { Ban, Radar } from "lucide-react";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { defaultScope, useSettingsStore, projectsScope } from "@/stores/settingsStore";
import type { ScanScope } from "@/types";
import { formatBytes } from "@/utils/format";

function providerLabel(provider: string): string {
  switch (provider) {
    case "kondo":
      return "项目产物";
    case "dev-cache":
      return "工具数据";
    case "agents":
      return "Agent 数据";
    case "unknown":
      return "未知数据";
    default:
      return provider;
  }
}

/**
 * Scan scope picker + run/cancel + live status. Sits under the page head
 * (not in it) so every data page shares one scan surface.
 */
export function ScanBar() {
  const phase = useScanStore((s) => s.phase);
  const items = useScanStore((s) => s.items);
  const progress = useScanStore((s) => s.progress);
  const warnings = useScanStore((s) => s.warnings);
  const cancelled = useScanStore((s) => s.cancelled);
  const scannedAt = useScanStore((s) => s.scannedAt);
  const startScan = useScanStore((s) => s.startScan);
  const cancelScan = useScanStore((s) => s.cancelScan);
  const error = useScanStore((s) => s.error);
  const openDetail = useUiStore((s) => s.openDetail);
  const workspaceRoots = useSettingsStore((s) => s.workspaceRoots);
  const workspaceRootsMode = useSettingsStore((s) => s.workspaceRootsMode);

  const [scopeKey, setScopeKey] = useState<
    "default" | "agents" | "dev-cache" | "projects" | "unknown"
  >("default");
  const [warnOpen, setWarnOpen] = useState(false);
  const [scopeNotice, setScopeNotice] = useState<string | null>(null);

  const generationNotice = useScanStore((s) => s.generationNotice);
  const dismissGenerationNotice = useScanStore((s) => s.dismissGenerationNotice);

  const scanning = phase === "scanning";
  const projectScopeAvailable = workspaceRoots.length > 0;

  const run = () => {
    if (scopeKey === "projects" && !projectScopeAvailable) {
      setScopeNotice("请先在设置中添加至少一个工作区根目录，再扫描项目产物。");
      return;
    }
    setScopeNotice(null);
    const s: ScanScope =
      scopeKey === "projects" ? projectsScope(workspaceRoots) : scopeKey === "default" ? defaultScope(workspaceRootsMode, workspaceRoots) : { kind: scopeKey };
    openDetail(null);
    void startScan(s);
  };

  const totalBytes = items.reduce((sum, it) => sum + it.logical_size, 0);

  const stageLabel = (stage: string): string => {
    if (stage === "started") return "开始";
    if (stage === "done") return "完成";
    if (stage === "project") return "发现项目";
    return stage;
  };

  // Latest provider activity for the status line.
  const lastProgress = progress.length > 0 ? progress[progress.length - 1] : null;
  // Keep the live region coarse-grained. The discovered-item count changes
  // for every item and must not cause a screen reader announcement per event.
  const progressStatus = lastProgress
    ? `${providerLabel(lastProgress.provider)} · ${stageLabel(lastProgress.stage)}`
    : "正在启动…";
  const warningListId = "scan-warning-list";

  return (
    <>
      <div className={`scanbar ${scanning ? "is-scanning" : ""}`} aria-busy={scanning}>
        {phase === "scanning" ? (
          <span className="scan-status" role="status" aria-live="polite">
            <span className="dot pulse" aria-hidden="true" />
            <span>{progressStatus}</span>
            <span className="scan-status-detail" aria-hidden="true">
              · 已发现 {items.length} 项
            </span>
          </span>
        ) : phase === "error" ? (
          <span className="scan-status" role="alert" style={{ color: "var(--danger)" }}>
            {error?.message ?? "扫描失败"}
          </span>
        ) : phase === "idle" && items.length === 0 ? (
          <span className="scan-status" role="status" aria-live="polite">
            尚未扫描
          </span>
        ) : (
          <span className="scan-status" role="status" aria-live="polite">
            <span
              className="dot"
              aria-hidden="true"
              style={{ background: cancelled ? "var(--risk-protected)" : "var(--ok)" }}
            />
            {items.length} 项 · {formatBytes(totalBytes)}
            {scannedAt ? ` · ${new Date(scannedAt * 1000).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : ""}
          </span>
        )}

        <label htmlFor="scan-scope" className="field-label">
          <span className="sr-only">扫描范围</span>
          <select
            id="scan-scope"
            className="scope-select"
            value={scopeKey}
            disabled={scanning}
            onChange={(e) => setScopeKey(e.target.value as typeof scopeKey)}
            title="选择要扫描的数据类别"
          >
            <option value="default">默认（全部）</option>
            <option value="agents">Agent 数据</option>
            <option value="dev-cache">工具数据</option>
            <option value="projects" disabled={!projectScopeAvailable}>
              项目（工作区根目录{projectScopeAvailable ? "" : "—请先设置"}）
            </option>
            <option value="unknown">未知数据</option>
          </select>
        </label>

        {scanning ? (
          <button type="button" className="btn danger" onClick={() => void cancelScan()}>
            <Ban size={13} /> 取消
          </button>
        ) : (
          <button type="button" className="btn primary" onClick={run}>
            <Radar size={13} /> {items.length > 0 ? "重新扫描" : "开始扫描"}
          </button>
        )}
      </div>
      <div className={`scan-progress ${scanning ? "active" : ""}`} aria-hidden="true" />

      {scopeNotice && (
        <div className="notice" role="status">
          <span>{scopeNotice}</span>
          <button type="button" className="btn small ghost" onClick={() => useUiStore.getState().setPage("settings")}>
            去设置
          </button>
        </div>
      )}

      {phase === "cancelled" && (
        <div className="notice" role="status" aria-live="polite">
          <Ban size={14} aria-hidden="true" />
          <span>
            扫描已取消——<b>当前为部分结果</b>。数量与大小不完整，重新扫描可获得完整数据。
          </span>
        </div>
      )}

      {generationNotice && (
        <div className="notice" role="status" aria-live="polite">
          <Radar size={14} aria-hidden="true" />
          <span>
            <b>{generationNotice}</b>——条目 ID 随新扫描改变，请重新勾选要清理的内容。
          </span>
          <button
            type="button"
            className="btn small ghost"
            style={{ marginLeft: "auto" }}
            onClick={dismissGenerationNotice}
            title="关闭"
          >
            ✕
          </button>
        </div>
      )}

      {warnings.length > 0 && (
        <div className="warning-banner">
          <Radar size={14} aria-hidden="true" />
          <div className="warning-content">
            <button
              type="button"
              className="warning-head"
              aria-expanded={warnOpen}
              aria-controls={warningListId}
              onClick={() => setWarnOpen((v) => !v)}
            >
              <b>扫描期间出现 {warnings.length} 条警告</b>
              <span className="chev">{warnOpen ? "收起" : "展开"}</span>
            </button>
            {warnOpen && (
              <ul id={warningListId} className="warning-list">
                {warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}
    </>
  );
}
