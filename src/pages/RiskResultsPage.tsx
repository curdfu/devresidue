import type { RiskLevel } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { riskMeta } from "@/utils/format";
import { CategoryPage } from "./CategoryPage";

/** A dashboard card must open the full matching set, never a best-guess category. */
export function RiskResultsPage() {
  const activeRisk = useUiStore((s) => s.riskFilter) as RiskLevel | null;
  const risk = activeRisk ?? "safe";
  const meta = riskMeta(risk);

  return (
    <CategoryPage
      title={`${meta.label}条目`}
      subtitle={`${meta.hint}。这里汇总本次扫描中所有匹配条目；详细依据可在右侧详情查看。`}
      filter={{ kind: "risk", risk }}
      backToDashboard
    />
  );
}
