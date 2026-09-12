import type { RiskLevel, ScanItemDto } from "@/types";

/** Stable top-level presentation families. This is display grouping only. */
export type PresentationFamily = "agents" | "tools" | "projects";

export interface PresentationSummary {
  count: number;
  bytes: number;
}

const PROJECT_EVIDENCE_SOURCES = new Set(["provider:kondo", "kondo-project-type"]);

export function familyOfItem(item: ScanItemDto): PresentationFamily {
  // The provider source is authoritative for current snapshots. Protected
  // paths may be reclassified as `rule`, but their evidence still records the
  // Kondo producer; use that explicit provenance to keep them in Projects.
  // Risk never changes ownership, and path/product substrings are not guessed.
  if (item.source === "agent-provider") return "agents";
  if (
    item.source === "kondo" ||
    item.evidence.some((entry) => PROJECT_EVIDENCE_SOURCES.has(entry.source))
  ) {
    return "projects";
  }
  return "tools";
}

export function itemsForFamily(items: ScanItemDto[], family: PresentationFamily): ScanItemDto[] {
  return items.filter((item) => familyOfItem(item) === family);
}

export function familySummary(items: ScanItemDto[]): Record<PresentationFamily, PresentationSummary> {
  const summary: Record<PresentationFamily, PresentationSummary> = {
    agents: { count: 0, bytes: 0 },
    tools: { count: 0, bytes: 0 },
    projects: { count: 0, bytes: 0 },
  };
  for (const item of items) {
    const entry = summary[familyOfItem(item)];
    entry.count += 1;
    entry.bytes += item.logical_size;
  }
  return summary;
}

export function riskSummary(items: ScanItemDto[]): Map<RiskLevel, PresentationSummary> {
  const summary = new Map<RiskLevel, PresentationSummary>();
  for (const item of items) {
    const entry = summary.get(item.risk) ?? { count: 0, bytes: 0 };
    entry.count += 1;
    entry.bytes += item.logical_size;
    summary.set(item.risk, entry);
  }
  return summary;
}
