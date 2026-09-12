import { useEffect } from "react";
import { ArrowLeft } from "lucide-react";
import { AiReviewPanel } from "@/components/AiReviewPanel";
import { EmptyScanState } from "@/components/common";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { useAiStore } from "@/stores/aiStore";

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
  const setRiskFilter = useUiStore((state) => state.setRiskFilter);
  const returnContext = useAiStore((state) => state.returnContext);
  const clearReturnContext = useAiStore((state) => state.clearReturnContext);

  const contextIsCurrent = returnContext?.scanGeneration === generation;
  useEffect(() => {
    if (returnContext && returnContext.scanGeneration !== generation) {
      clearReturnContext();
    }
  }, [clearReturnContext, generation, returnContext]);

  const goBack = () => {
    const sourceView = returnContext?.sourceView;
    clearReturnContext();
    setPage("unknown");
    if (sourceView === "review") setRiskFilter("review");
  };

  return (
    <div className="storage-view ai-review-page">
      <div className="storage-list">
        <div className="page-head">
          <div>
            <button type="button" className="back-link" onClick={goBack}>
              <ArrowLeft size={14} /> 返回未知数据
            </button>
            <h1 className="page-title">AI研判</h1>
            <div className="page-sub">
              仅在你确认后向远程服务发送选定的脱敏元数据；生成规则仍须经本地安全校验与处置计划。
            </div>
          </div>
        </div>

        <div className="page-body">
          {returnContext && !contextIsCurrent && (
            <div className="warning-banner" role="status">
              扫描结果已变化，之前带入的 AI 候选已清除，请从待判断页面重新选择。
            </div>
          )}
          {phase === "idle" && items.length === 0 ? (
            <EmptyScanState />
          ) : (
            <AiReviewPanel
              items={items}
              generation={generation}
              candidateItemIds={contextIsCurrent ? returnContext?.itemIds : undefined}
            />
          )}
        </div>
      </div>
    </div>
  );
}
