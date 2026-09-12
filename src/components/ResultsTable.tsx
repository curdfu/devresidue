import { useMemo, useState } from "react";
import { ArrowDown, ArrowUp, ArrowUpDown } from "lucide-react";
import type { ScanItemDto } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import { formatAge, formatBytes, isSelectable, riskMeta } from "@/utils/format";
import type { SelectionApi } from "@/hooks/useScanView";

type SortKey = "name" | "size" | "age" | "risk";

interface SortState {
  key: SortKey;
  dir: 1 | -1;
}

/**
 * Decision-first result table. The main surface intentionally shows only the
 * information needed to decide whether an item deserves review; full paths,
 * evidence and provider details stay available in the detail panel.
 *
 * `selection: null` (Unknown page) removes every selection control. UNKNOWN
 * data remains outside the ordinary cleanup flow.
 */
export function ResultsTable({
  items,
  selection,
  emptyText = "此视图下暂无条目。可切换其他标签或重新扫描。",
}: {
  items: ScanItemDto[];
  selection: SelectionApi | null;
  emptyText?: string;
}) {
  const detailItemId = useUiStore((s) => s.detailItemId);
  const openDetail = useUiStore((s) => s.openDetail);
  const [sort, setSort] = useState<SortState>({ key: "size", dir: -1 });

  const sorted = useMemo(() => {
    const valueOf = (item: ScanItemDto): string | number => {
      switch (sort.key) {
        case "name":
          return item.display_name.toLocaleLowerCase();
        case "size":
          return item.logical_size;
        case "age":
          return item.last_modified ?? 0;
        case "risk":
          return item.risk;
      }
    };

    return [...items].sort((a, b) => {
      const aValue = valueOf(a);
      const bValue = valueOf(b);
      const comparison =
        typeof aValue === "number" && typeof bValue === "number"
          ? aValue - bValue
          : String(aValue).localeCompare(String(bValue));
      return comparison * sort.dir;
    });
  }, [items, sort]);

  const setSortKey = (key: SortKey) => {
    setSort((previous) =>
      previous.key === key
        ? { key, dir: previous.dir === 1 ? -1 : 1 }
        : { key, dir: key === "size" || key === "age" ? -1 : 1 },
    );
  };

  const SortHeader = ({ label, sortKey, className }: { label: string; sortKey: SortKey; className?: string }) => (
    <th className={className} aria-sort={sort.key === sortKey ? (sort.dir === 1 ? "ascending" : "descending") : "none"}>
      <button
        type="button"
        className="sort-button"
        onClick={() => setSortKey(sortKey)}
        title={`按${label}排序`}
      >
        <span>{label}</span>
        {sort.key === sortKey ? (
          sort.dir === 1 ? <ArrowUp size={12} /> : <ArrowDown size={12} />
        ) : (
          <ArrowUpDown size={12} opacity={0.45} />
        )}
      </button>
    </th>
  );

  const allEligible = selection?.eligible(items) ?? [];
  const selectedEligibleCount = allEligible.filter((id) => selection?.selected.has(id)).length;

  return (
    <div className="table-scroll">
      <table className="results">
        <thead>
          <tr>
            {selection && (
              <th className="check-cell">
                <input
                  type="checkbox"
                  className="row-check"
                  aria-label="全选当前视图中可清理的条目"
                  checked={allEligible.length > 0 && selectedEligibleCount === allEligible.length}
                  ref={(element) => {
                    if (element) {
                      element.indeterminate = selectedEligibleCount > 0 && selectedEligibleCount < allEligible.length;
                    }
                  }}
                  onChange={() => {
                    const allSelected = allEligible.length > 0 && selectedEligibleCount === allEligible.length;
                    if (allSelected) selection.clear(items);
                    else selection.selectAll(items);
                  }}
                  title="全选可清理项（受保护与未知数据不可勾选）"
                />
              </th>
            )}
            <SortHeader label="名称与位置" sortKey="name" className="col-name" />
            <SortHeader label="逻辑大小估计" sortKey="size" className="col-size" />
            <SortHeader label="清理影响" sortKey="risk" className="col-impact" />
            <SortHeader label="最近修改" sortKey="age" className="col-age" />
          </tr>
        </thead>
        <tbody>
          {sorted.map((item) => {
            const selectable = isSelectable(item.risk);
            const selected = selection?.selected.has(item.id) ?? false;
            const open = detailItemId === item.id;
            const meta = riskMeta(item.risk);
            const openOrClose = () => openDetail(open ? null : item.id);
            const checkboxLabel = selectable
              ? `将 ${item.display_name} 加入清理计划`
              : item.risk === "protected"
                ? `${item.display_name} 已受保护，永不可清理`
                : `${item.display_name} 为未知数据，不能加入清理计划`;

            return (
              <tr
                key={item.id}
                data-detail-trigger={item.id}
                className={`item-row ${selected ? "selected" : ""} ${open ? "detail-open" : ""}`}
                tabIndex={0}
                aria-label={`查看 ${item.display_name} 的详情`}
                onClick={openOrClose}
                onKeyDown={(event) => {
                  // Keep native keyboard behavior for checkboxes and any
                  // future buttons/links rendered inside the row. The row is
                  // activated only while the row itself owns focus.
                  if (event.target !== event.currentTarget) return;
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    openOrClose();
                  }
                }}
              >
                {selection && (
                  <td className="check-cell" onClick={(event) => event.stopPropagation()}>
                    <input
                      type="checkbox"
                      className="row-check"
                      disabled={!selectable}
                      checked={selected}
                      aria-label={checkboxLabel}
                      onChange={() => selection.toggle(item.id)}
                      title={
                        selectable
                          ? "加入清理计划"
                          : item.risk === "protected"
                            ? "受保护——永不可清理（内置规则）"
                            : "未知数据——DevResidue 绝不删除未分类数据"
                      }
                    />
                  </td>
                )}
                <td className="cell-name">
                  <div className="cell-name-title">
                    {item.display_name}
                    {item.overlaps && <span className="overlaps" title={`与其他条目重叠：${item.overlaps}`}>重叠</span>}
                  </div>
                  <div className="cell-name-meta" title={item.path}>
                    {item.product && <span>{item.product}</span>}
                    <span className="compact-path">{compactPath(item.path)}</span>
                  </div>
                </td>
                <td className="cell-size">{formatBytes(item.logical_size)}</td>
                <td className="cell-impact">
                  <span className={`badge ${meta.className}`}>{meta.label}</span>
                </td>
                <td className="cell-age">{formatAge(item.last_modified)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {items.length === 0 && (
        <div className="empty table-empty">
          <p>{emptyText}</p>
        </div>
      )}
    </div>
  );
}

function compactPath(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  if (parts.length <= 3) return path;
  return `…\\${parts.slice(-3).join("\\")}`;
}
