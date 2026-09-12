import { ArrowLeft, Trash2 } from "lucide-react";
import { useEffect, useMemo } from "react";
import type { ScanItemDto } from "@/types";
import { useScanStore } from "@/stores/scanStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useUiStore } from "@/stores/uiStore";
import { selectionOf } from "@/hooks/useScanView";
import { formatBytes } from "@/utils/format";
import { ResultsTable } from "@/components/ResultsTable";
import { DetailPanel } from "@/components/DetailPanel";
import { EmptyScanState } from "@/components/common";

/** Auxiliary, non-navigation page for reviewing the global selection set. */
export function SelectedItemsPage() {
  const items = useScanStore((s) => s.items);
  const phase = useScanStore((s) => s.phase);
  const selected = useSelectionStore((s) => s.selected);
  const removeMany = useSelectionStore((s) => s.removeMany);
  const detailItemId = useUiStore((s) => s.detailItemId);
  const setPage = useUiStore((s) => s.setPage);

  const selectedItems = useMemo(() => items.filter((item) => selected.has(item.id)), [items, selected]);
  const staleIds = useMemo(() => [...selected].filter((id) => !items.some((item) => item.id === id)), [items, selected]);

  useEffect(() => {
    if (staleIds.length > 0) useSelectionStore.getState().removeMany(staleIds);
  }, [staleIds]);

  const selection = selectionOf(selected, (transform) =>
    useSelectionStore.setState((state) => ({ selected: transform(state.selected) })),
  );
  const detail: ScanItemDto | null = detailItemId !== null
    ? (selectedItems.find((item) => item.id === detailItemId) ?? null)
    : null;
  const bytes = selectedItems.reduce((sum, item) => sum + item.logical_size, 0);

  if (phase === "idle" && items.length === 0) {
    return (
      <>
        <div className="page-head"><div><h1 className="page-title">已选清单</h1><div className="page-sub">跨页面保留的清理选择</div></div></div>
        <div className="page-body"><EmptyScanState /></div>
      </>
    );
  }

  return (
    <div className="storage-view selected-items-view">
      <div className="storage-list">
        <div className="page-head">
          <div>
            <button type="button" className="back-link" onClick={() => setPage("dashboard")}><ArrowLeft size={14} /> 返回概览</button>
            <h1 className="page-title">已选清单</h1>
            <div className="page-sub">跨页面保留的清理选择；未知和受保护条目不会进入此清单。</div>
          </div>
          {selectedItems.length > 0 && (
            <button type="button" className="btn small ghost" onClick={() => removeMany(selectedItems.map((item) => item.id))}>
              <Trash2 size={13} /> 清空全部选择
            </button>
          )}
        </div>
        <div className="results-head">
          <span className="results-count">共选 {selectedItems.length} 项 · 逻辑大小估计 {formatBytes(bytes)}</span>
        </div>
        <ResultsTable items={selectedItems} selection={selection} emptyText="尚未选择可清理条目。请从分类结果页勾选。" />
      </div>
      {detail && <aside className="detail" aria-label={`${detail.display_name} 的详情`}><DetailPanel item={detail} /></aside>}
      {selectedItems.length === 0 && <div className="selected-items-empty">尚未选择可清理条目。请返回分类结果页继续选择。</div>}
    </div>
  );
}
