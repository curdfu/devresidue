import { useState } from "react";
import { Ban, Radar } from "lucide-react";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { useSettingsStore, projectsScope } from "@/stores/settingsStore";
import type { ScanScope } from "@/types";
import { formatBytes } from "@/utils/format";
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

  const [scopeKey, setScopeKey] = useState<
    "default" | "agents" | "dev-cache" | "projects" | "unknown"
  >("default");
  const [warnOpen, setWarnOpen] = useState(false);

  const generationNotice = useScanStore((s) => s.generationNotice);
  const dismissGenerationNotice = useScanStore((s) => s.dismissGenerationNotice);

  const scanning = phase === "scanning";

  const run = () => {
    const s: ScanScope =
      scopeKey === "projects" ? projectsScope(workspaceRoots) : { kind: scopeKey };
    openDetail(null);
    void startScan(s);
  };

  const totalBytes = items.reduce((sum, it) => sum + it.logical_size, 0);

  // Latest provider activity for the status line.
  const lastProgress = progress.length > 0 ? progress[progress.length - 1] : null;

  const stageLabel = (stage: string): string => {
    if (stage === "started") return "开始";
    if (stage === "done") return "完成";
    if (stage === "project") return "发现项目";
    return stage;
  };

  return (
    <>
      <div className="scanbar">
        {phase === "scanning" ? (
          <span className="scan-status">
            <span className="dot pulse" />
            {lastProgress
              ? `${lastProgress.provider} · ${stageLabel(lastProgress.stage)} · 已发现 ${items.length} 项`
              : "正在启动…"}
          </span>
        ) : phase === "error" ? (
          <span className="scan-status" style={{ color: "var(--danger)" }}>
            {error?.message ?? "扫描失败"}
          </span>
        ) : phase === "idle" && items.length === 0 ? (
          <span className="scan-status">尚未扫描</span>
        ) : (
          <span className="scan-status">
            <span className="dot" style={{ background: cancelled ? "var(--risk-protected)" : "var(--ok)" }} />
            {items.length} 项 · {formatBytes(totalBytes)}
            {scannedAt ? ` · ${new Date(scannedAt * 1000).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : ""}
          </span>
        )}

        <select
          className="scope-select"
          value={scopeKey}
          disabled={scanning}
          onChange={(e) => setScopeKey(e.target.value as typeof scopeKey)}
          title="选择要扫描的数据类别"
        >
          <option value="default">默认（全部）</option>
          <option value="agents">AI Agent</option>
          <option value="dev-cache">开发缓存</option>
          <option value="projects">项目（工作区根目录）</option>
          <option value="unknown">未知数据</option>
        </select>

        {scanning ? (
          <button className="btn danger" onClick={() => void cancelScan()}>
            <Ban size={13} /> 取消
          </button>
        ) : (
          <button className="btn primary" onClick={run}>
            <Radar size={13} /> {items.length > 0 ? "重新扫描" : "开始扫描"}
          </button>
        )}
      </div>
      <div className={`scan-progress ${scanning ? "active" : ""}`} />

      {phase === "cancelled" && (
        <div className="notice">
          <Ban size={14} />
          <span>
            扫描已取消——<b>当前为部分结果</b>。数量与大小不完整，重新扫描可获得完整数据。
          </span>
        </div>
      )}

      {generationNotice && (
        <div className="notice">
          <Radar size={14} />
          <span>
            <b>{generationNotice}</b>——条目 ID 随新扫描改变，请重新勾选要清理的内容。
          </span>
          <button
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
          <Radar size={14} />
          <div className="warning-content">
            <div className="warning-head" onClick={() => setWarnOpen((v) => !v)}>
              <b>扫描期间出现 {warnings.length} 条警告</b>
              <span className="chev">{warnOpen ? "收起" : "展开"}</span>
            </div>
            {warnOpen && (
              <ul className="warning-list">
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
