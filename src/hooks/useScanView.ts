import { useMemo } from "react";
import type { ItemFilter, RiskLevel, ScanItemDto, SubtabDef } from "@/types";
import { isSelectable } from "@/utils/format";
import { familyOfItem } from "@/selectors/scanPresentation";

/** Per-page filter derivation (category faces + optional subtabs). */
export function useFilteredItems(
  items: ScanItemDto[],
  filter: ItemFilter,
  activeSubtab: string | null,
  riskFilter: string | null,
): { items: ScanItemDto[]; subtabs: SubtabDef[] } {
  return useMemo(() => {
    let list = items;
    if (filter.kind === "category" && filter.categories) {
      list = list.filter((it) => filter.categories?.includes(it.category));
    }
    if (filter.kind === "family") {
      list = list.filter((it) => familyOfItem(it) === filter.family);
    }
    if (filter.kind === "risk" && filter.risk) {
      list = list.filter((it) => it.risk === filter.risk);
    }
    if (filter.subtabs?.length && activeSubtab) {
      const tab = filter.subtabs.find((t) => t.key === activeSubtab);
      if (tab) list = list.filter((it) => tab.categories.includes(it.category));
    }
    if (riskFilter) {
      list = list.filter((it) => it.risk === riskFilter);
    }
    return { items: list, subtabs: filter.subtabs ?? [] };
  }, [items, filter, activeSubtab, riskFilter]);
}

/** Aggregate size/count per risk level over an item list. */
export function useRiskGroups(items: ScanItemDto[]): Map<RiskLevel, { bytes: number; count: number }> {
  return useMemo(() => {
    const map = new Map<RiskLevel, { bytes: number; count: number }>();
    for (const it of items) {
      const cur = map.get(it.risk) ?? { bytes: 0, count: 0 };
      cur.bytes += it.logical_size;
      cur.count += 1;
      map.set(it.risk, cur);
    }
    return map;
  }, [items]);
}

export interface SelectionApi {
  selected: Set<number>;
  toggle: (id: number) => void;
  selectAll: (items: ScanItemDto[]) => void;
  clear: (items: ScanItemDto[]) => void;
  /** Selectable ids among a list (PROTECTED / UNKNOWN excluded). */
  eligible: (items: ScanItemDto[]) => number[];
}

export function selectionOf(
  selected: Set<number>,
  setSelected: (fn: (prev: Set<number>) => Set<number>) => void,
): SelectionApi {
  return {
    selected,
    toggle: (id) =>
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
    }),
    selectAll: (items) =>
      setSelected((prev) => {
        const next = new Set(prev);
        for (const id of items.filter((it) => isSelectable(it.risk)).map((it) => it.id)) next.add(id);
        return next;
      }),
    clear: (items) =>
      setSelected((prev) => {
        const next = new Set(prev);
        for (const id of items.filter((it) => isSelectable(it.risk)).map((it) => it.id)) next.delete(id);
        return next;
      }),
    eligible: (items) => items.filter((it) => isSelectable(it.risk)).map((it) => it.id),
  };
}
