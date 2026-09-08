import { X } from "lucide-react";
import type { ScanItemDto } from "@/types";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { useUnknownWorkflowStore } from "@/stores/unknownWorkflowStore";
import { formatBytes } from "@/utils/format";
import { ResultsTable } from "@/components/ResultsTable";
import { DetailPanel } from "@/components/DetailPanel";
import { SuggestionCard } from "@/components/UnknownActions";
import { EmptyScanState } from "@/components/common";
/**
 * Unknown Developer Data (SPEC §25 / PLAN Phase 13).
 *
 * Zero selection surface: no checkbox column, no cleanup dock — UNKNOWN
 * items can only be opened, ignored, protected, analyzed or ruled. The
 * disposition work happens in the Detail Panel per item.
 */
export function UnknownPage() {
  const items = useScanStore((s) => s.items);
  const phase = useScanStore((s) => s.phase);
  const detailItemId = useUiStore((s) => s.detailItemId);
  const setRiskFilter = useUiStore((s) => s.setRiskFilter);
  const riskFilter = useUiStore((s) => s.riskFilter);
  const error = useUnknownWorkflowStore((s) => s.error);
  const dismissError = useUnknownWorkflowStore((s) => s.dismissError);

  // The Unknown face is the risk=unknown subset; riskFilter from the
  // dashboard's Unknown card lands here too.
  const unknown = items.filter((it) => it.risk === "unknown");

  const detail: ScanItemDto | null =
    detailItemId !== null ? (unknown.find((it) => it.id === detailItemId) ?? null) : null;

  if (phase === "idle" && items.length === 0) {
    return <EmptyScanState />;
  }

  const totalBytes = unknown.reduce((s, it) => s + it.logical_size, 0);

  return (
    <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
      <div style={{ display: "flex", flexDirection: "column", flex: 1, minWidth: 0 }}>
        <div className="page-head">
          <div>
            <div className="page-title">未知开发数据</div>
            <div className="page-sub">
              形似开发工具数据但无规则识别的文件夹——永不自动删除，需逐个人工决定
            </div>
          </div>
          {riskFilter === "unknown" && (
            <button className="btn small ghost" onClick={() => setRiskFilter(null)}>
              来自概览 ✕
            </button>
          )}
        </div>

        {error && (
          <div className="error-banner" style={{ marginTop: 0 }}>
            <X size={14} />
            <div>
              <span className="code">{error.code}</span>
              <div>{error.message}</div>
            </div>
            <button className="btn small ghost" style={{ marginLeft: "auto" }} onClick={dismissError}>
              关闭
            </button>
          </div>
        )}

        <div className="results-head">
          <span className="results-count">
            {unknown.length} 个文件夹 · {formatBytes(totalBytes)} 未分类
          </span>
        </div>

        {phase !== "idle" || items.length > 0 ? (
          <ResultsTable
            items={unknown}
            selection={null}
            emptyText={
              items.length > 0
                ? "没有未识别的开发数据文件夹——很干净。"
                : "运行扫描以查找未识别的开发数据。"
            }
          />
        ) : null}
      </div>

      {detail && (
        <aside className="detail">
          <SuggestionCard item={detail} />
          <DetailPanel item={detail} unknownMode />
        </aside>
      )}
    </div>
  );
}
