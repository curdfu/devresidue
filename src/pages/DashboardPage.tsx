import { useMemo } from "react";
import { ChevronRight, Radar, Sparkles } from "lucide-react";
import type { RiskLevel } from "@/types";
import { useScanStore } from "@/stores/scanStore";
import { useUiStore } from "@/stores/uiStore";
import { formatBytes, formatCount, riskMeta } from "@/utils/format";
import { BACKEND_KIND } from "@/App";
import { familySummary, riskSummary, type PresentationFamily } from "@/selectors/scanPresentation";
import { defaultScope, useSettingsStore } from "@/stores/settingsStore";

interface FaceRow {
  name: string;
  family: PresentationFamily;
}

/** Storage faces (SPEC §27 dashboard): agents / caches / artifacts / other. */
const FACES: FaceRow[] = [
  { name: "Agent 数据", family: "agents" },
  { name: "工具数据", family: "tools" },
  { name: "项目产物", family: "projects" },
];

const CLEANUP_RISKS: RiskLevel[] = [
  "safe",
  "regenerable-local",
  "regenerable-download",
  "review",
];

const NON_AUTOMATIC_RISKS: RiskLevel[] = ["protected", "unknown"];

export function DashboardPage() {
  const items = useScanStore((s) => s.items);
  const phase = useScanStore((s) => s.phase);
  const startScan = useScanStore((s) => s.startScan);
  const workspaceRoots = useSettingsStore((s) => s.workspaceRoots);
  const workspaceRootsMode = useSettingsStore((s) => s.workspaceRootsMode);
  const setPage = useUiStore((s) => s.setPage);
  const setRiskFilter = useUiStore((s) => s.setRiskFilter);

  const groups = riskSummary(items);
  const families = familySummary(items);
  const total = useMemo(() => items.reduce((sum, item) => sum + item.logical_size, 0), [items]);
  const lowRisk = groups.get("safe") ?? { bytes: 0, count: 0 };

  if (phase === "idle" && items.length === 0) {
    return (
      <>
        <DashboardHeading />
        <div className="page-body dashboard-empty">
          <div className="empty">
            <Radar size={34} strokeWidth={1.3} />
            <h2>尚未扫描</h2>
            <p>
              扫描后将看到 AI Agent、开发缓存与构建产物分别有多少逻辑大小估计——
              每一项都附带风险等级与删除影响说明。
            </p>
            <button className="btn primary btn-lg" onClick={() => void startScan(defaultScope(workspaceRootsMode, workspaceRoots))}>
              <Radar size={14} /> 开始扫描
            </button>
            {BACKEND_KIND === "mock" && (
              <button
                className="btn"
                onClick={() => void startScan(defaultScope(workspaceRootsMode, workspaceRoots))}
                title="演示模式下使用内置示例数据"
              >
                <Sparkles size={13} /> 使用演示数据
              </button>
            )}
          </div>
        </div>
      </>
    );
  }

  const jump = (risk: RiskLevel) => {
    // The dashboard aggregates the entire scan, so its cards must lead to an
    // equally global filtered result—not the first category that happens to match.
    setPage("risk-results");
    setRiskFilter(risk);
  };

  const riskCard = (risk: RiskLevel, index: number) => {
    const group = groups.get(risk) ?? { bytes: 0, count: 0 };
    const meta = riskMeta(risk);
    return (
      <button
        key={risk}
        className={`group-card ${meta.className}`}
        style={{ animationDelay: `${index * 45}ms` }}
        onClick={() => jump(risk)}
        aria-label={`查看所有${meta.label}条目，共 ${formatCount(group.count)} 项`}
      >
        <div className="group-head">
          <span className="group-label">{meta.groupLabel}</span>
          <ChevronRight size={16} opacity={0.62} aria-hidden="true" />
        </div>
        <div className="group-size">{formatBytes(group.bytes)}</div>
        <div className="group-foot">
          <span className="group-count">{formatCount(group.count)} 项</span>
          <span className="group-hint">{shortHint(risk)}</span>
        </div>
      </button>
    );
  };

  return (
    <>
      <DashboardHeading />
      <div className="page-body">
      <div className="dash">
        <section className="dash-section dash-overview" aria-labelledby="overview-title">
          <div className="dash-total">
            <span id="overview-title" className="label">开发数据逻辑大小估计</span>
            <span className="value">{formatBytes(total)}</span>
            <span className="meta">{formatCount(items.length)} 项 · 覆盖 Agent、缓存与项目</span>
          </div>
          <div className="dash-action-row">
            <div>
              <div className="dash-action-label">低风险候选</div>
              <p>
                {formatCount(lowRisk.count)} 项 · 逻辑大小估计 {formatBytes(lowRisk.bytes)}；生成计划和实际执行前仍会进行安全检查。
              </p>
            </div>
            <button className="btn primary dash-action" onClick={() => jump("safe")}>
              查看低风险候选 <ChevronRight size={14} />
            </button>
          </div>
        </section>

        <section className="dash-section" aria-labelledby="cost-title">
          <div className="dash-section-head">
            <div>
              <h2 id="cost-title" className="dash-section-title">清理代价</h2>
              <p className="dash-section-copy">先按恢复成本查看候选，再逐项确认清理影响。</p>
            </div>
          </div>
          <div className="dash-groups dash-groups-action">
            {CLEANUP_RISKS.map(riskCard)}
          </div>
        </section>

        <section className="dash-section dash-protected-section" aria-labelledby="protected-title">
          <div className="dash-section-head">
            <div>
              <h2 id="protected-title" className="dash-section-title">不会自动清理</h2>
              <p className="dash-section-copy">受保护和未知数据没有普通删除入口；查看详情可了解保留或处置依据。</p>
            </div>
          </div>
          <div className="dash-groups dash-groups-locked">
            {NON_AUTOMATIC_RISKS.map((risk, index) => riskCard(risk, index + CLEANUP_RISKS.length))}
          </div>
        </section>

        <section className="dash-breakdown" aria-labelledby="distribution-title">
          <h2 id="distribution-title">空间分布</h2>
          {FACES.map((face) => {
            const summary = families[face.family];
            const bytes = summary.bytes;
            const percent = total > 0 ? (bytes / total) * 100 : 0;
            return (
              <div key={face.name} className="breakdown-row">
                <span className="name">{face.name}</span>
                <div className="breakdown-bar" aria-label={`${face.name} 占 ${percent.toFixed(1)}%`}>
                  <i style={{ width: `${Math.max(percent, bytes > 0 ? 1.5 : 0)}%` }} />
                </div>
                <span className="size">{formatCount(summary.count)} 项 · {formatBytes(bytes)}</span>
              </div>
            );
          })}
        </section>
      </div>
      </div>
    </>
  );
}

function DashboardHeading() {
  return (
    <div className="page-head">
      <div>
        <h1 className="page-title">概览</h1>
        <div className="page-sub">扫描结果、风险分布与清理入口</div>
      </div>
    </div>
  );
}

function shortHint(risk: RiskLevel): string {
  switch (risk) {
    case "safe":
      return "低风险候选；执行前仍检查";
    case "regenerable-local":
      return "下次使用可在本地重建";
    case "regenerable-download":
      return "下次使用需联网重新下载";
    case "review":
      return "删除前请人工确认";
    case "protected":
      return "受保护，永不进入计划";
    case "unknown":
      return "需人工分类，不自动清理";
  }
}
