import { useMemo, useState } from "react";
import { ArrowLeft, Search, X } from "lucide-react";
import type { ItemFilter, RiskLevel, ScanItemDto } from "@/types";
import { RISK_GROUP_ORDER } from "@/types";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useFilteredItems, selectionOf } from "@/hooks/useScanView";
import { formatBytes, riskMeta } from "@/utils/format";
import { ResultsTable } from "@/components/ResultsTable";
import { DetailPanel } from "@/components/DetailPanel";
import { EmptyScanState } from "@/components/common";
import { familyOfItem } from "@/selectors/scanPresentation";

function riskLabelOf(risk: string): string {
  return riskMeta(risk as RiskLevel).label;
}

/** Shared decision-first storage list for category and dashboard risk views. */
export function CategoryPage({
  title,
  subtitle,
  filter,
  backToDashboard = false,
}: {
  title: string;
  subtitle: string;
  filter: ItemFilter;
  backToDashboard?: boolean;
}) {
  const items = useScanStore((s) => s.items);
  const phase = useScanStore((s) => s.phase);
  const scope = useScanStore((s) => s.scope);
  const riskFilter = useUiStore((s) => s.riskFilter);
  const detailItemId = useUiStore((s) => s.detailItemId);
  const setRiskFilter = useUiStore((s) => s.setRiskFilter);
  const openDetail = useUiStore((s) => s.openDetail);
  const setPage = useUiStore((s) => s.setPage);

  const selected = useSelectionStore((s) => s.selected);

  const [activeTab, setActiveTab] = useState<string>(filter.subtabs?.[0]?.key ?? "all");
  const [query, setQuery] = useState("");

  const { items: tabFiltered } = useFilteredItems(items, filter, activeTab, riskFilter);
  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    if (!normalized) return tabFiltered;
    return tabFiltered.filter((item) =>
      [item.display_name, item.product ?? "", item.path, item.explanation]
        .some((value) => value.toLocaleLowerCase().includes(normalized)),
    );
  }, [query, tabFiltered]);

  const pageItems =
    filter.kind === "category"
      ? items.filter((item) => filter.categories.includes(item.category))
      : filter.kind === "family"
        ? items.filter((item) => familyOfItem(item) === filter.family)
        : items.filter((item) => item.risk === filter.risk);
  const projectScopeHasNoRoots =
    filter.kind === "family" &&
    filter.family === "projects" &&
    ((scope.kind === "projects" && scope.roots.length === 0) ||
      (scope.kind === "default" && scope.workspace_roots?.length === 0));
  const projectEmpty =
    filter.kind === "family" &&
    filter.family === "projects" &&
    pageItems.length === 0 &&
    phase !== "idle" &&
    phase !== "scanning";
  const pageRisks = Array.from(new Set(pageItems.map((item) => item.risk))).sort(
    (a, b) => RISK_GROUP_ORDER.indexOf(a) - RISK_GROUP_ORDER.indexOf(b),
  );

  const selection = selectionOf(selected, (transform) =>
    useSelectionStore.setState((state) => ({ selected: transform(state.selected) })),
  );

  const detail: ScanItemDto | null = detailItemId !== null ? (items.find((item) => item.id === detailItemId) ?? null) : null;

  if (phase === "idle" && items.length === 0) {
    return (
      <>
        <div className="page-head">
          <div>
            <h1 className="page-title">{title}</h1>
            <div className="page-sub">{subtitle}</div>
          </div>
        </div>
        <div className="page-body"><EmptyScanState /></div>
      </>
    );
  }

  const totalBytes = filtered.reduce((sum, item) => sum + item.logical_size, 0);
  const selectedBytes = filtered.filter((item) => selected.has(item.id)).reduce((sum, item) => sum + item.logical_size, 0);

  return (
    <div className="storage-view">
      <div className="storage-list">
        <div className="page-head">
          <div>
            {backToDashboard && (
              <button className="back-link" onClick={() => setPage("dashboard")}>
                <ArrowLeft size={14} /> 返回概览
              </button>
            )}
            <h1 className="page-title">{title}</h1>
            <div className="page-sub">{subtitle}</div>
          </div>
          {riskFilter && !backToDashboard && (
              <button type="button" className="btn small ghost" onClick={() => setRiskFilter(null)} title="清除风险筛选">
              筛选：{riskLabelOf(riskFilter)} <X size={13} />
            </button>
          )}
        </div>

        <div className="results-head category-results-head">
          <div className="results-controls">
            <label htmlFor="category-search" className="search-field">
              <Search size={15} aria-hidden="true" />
              <span className="sr-only">搜索名称、产品或路径</span>
              <input
                id="category-search"
                value={query}
                onChange={(event) => {
                  setQuery(event.target.value);
                  openDetail(null);
                }}
                placeholder="搜索名称、产品或路径"
                aria-label="搜索名称、产品或路径"
              />
              {query && (
                <button type="button" onClick={() => setQuery("")} aria-label="清空搜索" title="清空搜索">
                  <X size={14} />
                </button>
              )}
            </label>
            {filter.subtabs && filter.subtabs.length > 0 && (
              <div className="subtabs" aria-label="分类筛选">
                {filter.subtabs.map((tab) => (
                  <button
                    type="button"
                    key={tab.key}
                    className={`subtab ${activeTab === tab.key ? "active" : ""}`}
                    aria-pressed={activeTab === tab.key}
                    onClick={() => {
                      setActiveTab(tab.key);
                      openDetail(null);
                    }}
                  >
                    {tab.label}
                  </button>
                ))}
              </div>
            )}
            {!backToDashboard && (
              <label htmlFor="category-risk-filter" className="field-label">
                <span className="sr-only">风险筛选</span>
                <select
                  id="category-risk-filter"
                  className="scope-select risk-filter"
                  value={riskFilter ?? ""}
                  onChange={(event) => {
                    setRiskFilter(event.target.value === "" ? null : event.target.value);
                    openDetail(null);
                  }}
                  title="按风险等级筛选本页条目"
                  aria-label="按风险等级筛选"
                >
                  <option value="">全部风险</option>
                  {[...new Set([...pageRisks, riskFilter as RiskLevel].filter(Boolean))].map((risk) => (
                    <option key={risk} value={risk}>{riskLabelOf(risk)}</option>
                  ))}
                </select>
              </label>
            )}
          </div>
          <span className="results-count">
            {filtered.length} 项 · 逻辑大小估计 {formatBytes(totalBytes)}
            {selected.size > 0 && <> · 已勾选逻辑大小估计 {formatBytes(selectedBytes)}</>}
          </span>
        </div>

        {projectEmpty ? (
          <div className="empty project-empty" role="status">
            <h3>{projectScopeHasNoRoots ? "本次扫描未包含项目目录" : "本次扫描未发现项目产物"}</h3>
            <p>
              {projectScopeHasNoRoots
                ? "当前扫描范围没有已保存的工作区根目录。请在设置中添加项目目录，或切换为自动解析后重新扫描。"
                : "请确认工作区根目录已添加且目录中存在构建产物或依赖目录，然后重新扫描。"}
            </p>
            <button type="button" className="btn small" onClick={() => setPage("settings")}>
              配置扫描位置
            </button>
          </div>
        ) : (
          <ResultsTable items={filtered} selection={selection} />
        )}
      </div>
      {detail && (
        <aside className="detail" aria-label={`${detail.display_name} 的详情`}>
          <DetailPanel item={detail} />
        </aside>
      )}
    </div>
  );
}
