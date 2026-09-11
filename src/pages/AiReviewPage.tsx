import { ArrowLeft } from "lucide-react";
import { AiReviewPanel } from "@/components/AiReviewPanel";
import { EmptyScanState } from "@/components/common";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";

/**
 * Explicit remote-AI review is a subpage of Unknown data rather than a
 * section embedded above its results table. This keeps the review flow and
 * the Unknown-data disposition list separately focused while the page owns
 * the single vertical scroll surface.
 */
export function AiReviewPage() {
  const items = useScanStore((state) => state.items);
  const generation = useScanStore((state) => state.generation);
  const phase = useScanStore((state) => state.phase);
  const setPage = useUiStore((state) => state.setPage);

  return (
    <div className="storage-view ai-review-page">
      <div className="storage-list">
        <div className="page-head">
          <div>
            <button className="back-link" onClick={() => setPage("unknown")}>
              <ArrowLeft size={14} /> 返回未知数据
            </button>
            <div className="page-title">AI研判</div>
            <div className="page-sub">
              仅在你确认后向远程服务发送选定的脱敏元数据；生成规则仍须经本地安全校验与处置计划。
            </div>
          </div>
        </div>

        <div className="page-body">
          {phase === "idle" && items.length === 0 ? (
            <EmptyScanState />
          ) : (
            <AiReviewPanel items={items} generation={generation} />
          )}
        </div>
      </div>
    </div>
  );
}
