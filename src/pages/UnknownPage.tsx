import { useState } from "react";
import { Search, Sparkles, X } from "lucide-react";
import type { ScanItemDto } from "@/types";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { useUnknownWorkflowStore } from "@/stores/unknownWorkflowStore";
import { formatBytes } from "@/utils/format";
import { ResultsTable } from "@/components/ResultsTable";
import { DetailPanel } from "@/components/DetailPanel";
import { EmptyScanState } from "@/components/common";
/**
 * Unknown Developer Data (SPEC §25 / PLAN Phase 13).
 *
 * Zero selection surface: no checkbox column, no cleanup dock — UNKNOWN
 * items can only be opened, ignored, protected, or submitted for explicit
 * remote AI review. The
 * disposition work happens in the Detail Panel per item.
 */
export function UnknownPage() {
  const items = useScanStore((s) => s.items);
  const phase = useScanStore((s) => s.phase);
  const detailItemId = useUiStore((s) => s.detailItemId);
  const setPage = useUiStore((s) => s.setPage);
  const setRiskFilter = useUiStore((s) => s.setRiskFilter);
  const riskFilter = useUiStore((s) => s.riskFilter);
  const error = useUnknownWorkflowStore((s) => s.error);
  const dismissError = useUnknownWorkflowStore((s) => s.dismissError);
  const [query, setQuery] = useState("");

  // The Unknown face is the risk=unknown subset; riskFilter from the
  // dashboard's Unknown card lands here too.
  const [activeView, setActiveView] = useState<"unknown" | "review">(riskFilter === "review" ? "review" : "unknown");
  const pending = items.filter((it) => it.risk === activeView);
  const visible = pending.filter((item) => {
    const needle = query.trim().toLocaleLowerCase();
    return !needle || [item.display_name, item.product ?? "", item.path, item.explanation]
      .some((value) => value.toLocaleLowerCase().includes(needle));
  });

  const detail: ScanItemDto | null =
    detailItemId !== null ? (visible.find((it) => it.id === detailItemId) ?? null) : null;

  if (phase === "idle" && items.length === 0) {
    return (
      <>
        <div className="page-head">
          <div>
            <h1 className="page-title">待判断</h1>
            <div className="page-sub">未知数据与需人工确认的交叉视图；不会因风险分类自动进入清理计划</div>
          </div>
        </div>
        <div className="page-body"><EmptyScanState /></div>
      </>
    );
  }

  const totalBytes = visible.reduce((s, it) => s + it.logical_size, 0);

  return (
    <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
      <div style={{ display: "flex", flexDirection: "column", flex: 1, minWidth: 0 }}>
        <div className="page-head">
          <div>
            <h1 className="page-title">{activeView === "unknown" ? "未知数据" : "需人工确认"}</h1>
            <div className="page-sub">
              {activeView === "unknown" ? "形似开发工具数据但无规则识别；永不自动删除，需逐项人工决定" : "规则已识别但影响需要人工确认；可查看证据后再决定"}
            </div>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 8, marginLeft: "auto" }}>
            <button type="button" className="btn small primary" onClick={() => setPage("ai-review")}>
              <Sparkles size={13} /> AI 研判
            </button>
            {riskFilter === "unknown" && (
              <button type="button" className="btn small ghost" onClick={() => setRiskFilter(null)}>
                来自概览 ✕
              </button>
            )}
          </div>
        </div>

        {error && (
          <div className="error-banner" style={{ marginTop: 0 }}>
            <X size={14} />
            <div>
              <span className="code">{error.code}</span>
              <div>{error.message}</div>
            </div>
            <button type="button" className="btn small ghost" style={{ marginLeft: "auto" }} onClick={dismissError}>
              关闭
            </button>
          </div>
        )}

        <div className="results-head">
          <div className="subtabs" aria-label="待判断子视图">
            <button type="button" className={`subtab ${activeView === "unknown" ? "active" : ""}`} aria-pressed={activeView === "unknown"} onClick={() => { setActiveView("unknown"); setRiskFilter(null); }}>
              未知数据
            </button>
            <button type="button" className={`subtab ${activeView === "review" ? "active" : ""}`} aria-pressed={activeView === "review"} onClick={() => { setActiveView("review"); setRiskFilter("review"); }}>
              需人工确认
            </button>
          </div>
          <label htmlFor="pending-search" className="search-field">
            <Search size={15} aria-hidden="true" />
            <span className="sr-only">搜索名称、产品或路径</span>
            <input id="pending-search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索名称、产品或路径" aria-label="搜索名称、产品或路径" />
          </label>
          <span className="results-count">
            {visible.length} 项 · 逻辑大小估计 {formatBytes(totalBytes)}（筛选前 {pending.length} 项）
          </span>
        </div>

        {phase !== "idle" || items.length > 0 ? (
          <ResultsTable
            items={visible}
            selection={null}
            emptyText={
              items.length > 0
                ? activeView === "unknown" ? "没有未识别的开发数据。" : "没有需要人工确认的条目。"
                : "运行扫描以查找待判断数据。"
            }
          />
        ) : null}
      </div>

      {detail && (
        <aside className="detail">
          <DetailPanel item={detail} unknownMode={activeView === "unknown"} />
        </aside>
      )}
    </div>
  );
}
