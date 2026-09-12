import {
  Bot,
  Boxes,
  CircleHelp,
  FolderCog,
  Gauge,
  Monitor,
  Moon,
  Package,
  ScrollText,
  Settings as SettingsIcon,
  Sun,
  ShieldCheck,
} from "lucide-react";
import { useMemo } from "react";
import type { LucideIcon } from "lucide-react";
import { useUiStore, type PageKey } from "@/stores/uiStore";
import { BACKEND_KIND } from "@/App";
import { useScanStore } from "@/stores/scanStore";
import { familyOfItem } from "@/selectors/scanPresentation";

interface NavDef {
  key: PageKey;
  label: string;
  icon: LucideIcon;
  section: string;
}

const NAV: NavDef[] = [
  { key: "dashboard", label: "概览", icon: Gauge, section: "总览" },
  { key: "agents", label: "Agent 数据", icon: Bot, section: "存储" },
  { key: "devcache", label: "工具数据", icon: Package, section: "存储" },
  { key: "projects", label: "项目产物", icon: Boxes, section: "存储" },
  { key: "unknown", label: "待判断", icon: CircleHelp, section: "存储" },
  { key: "rules", label: "规则", icon: ShieldCheck, section: "信任" },
  { key: "journal", label: "日志", icon: ScrollText, section: "信任" },
  { key: "settings", label: "设置", icon: SettingsIcon, section: "信任" },
];

export function Sidebar() {
  const page = useUiStore((s) => s.page);
  const setPage = useUiStore((s) => s.setPage);
  const theme = useUiStore((s) => s.theme);
  const toggleTheme = useUiStore((s) => s.toggleTheme);
  // Per-page item counts (data pages only): consistent badges across
  // AI Agents / 开发缓存 / 项目 / 未知数据 instead of a dot only on Unknown.
  // Select the items ARRAY (stable identity between scans) and derive the
  // counts in a memo — a selector returning a fresh object every call would
  // loop zustand's identity check (React #185).
  const items = useScanStore((s) => s.items);
  const counts = useMemo(() => {
    const c: Record<PageKey, number> = {
      agents: 0,
      devcache: 0,
      projects: 0,
      unknown: 0,
      "selected-items": 0,
      "ai-review": 0,
      dashboard: 0,
      "risk-results": 0,
      rules: 0,
      journal: 0,
      settings: 0,
    };
    for (const it of items) {
      if (it.risk === "unknown" || it.risk === "review") c.unknown += 1;
      const family = familyOfItem(it);
      if (family === "agents") c.agents += 1;
      else if (family === "tools") c.devcache += 1;
      else c.projects += 1;
    }
    return c;
  }, [items]);

  let lastSection = "";
  return (
    <nav className="nav" aria-label="主导航">
      <div className="nav-brand">
        <div className="nav-brand-mark">
          <FolderCog size={15} strokeWidth={2.4} />
        </div>
        <div>
          <div className="nav-brand-name">DevResidue</div>
          <div className="nav-brand-sub">存储管理</div>
        </div>
      </div>

      {NAV.map((def) => {
        const sectionHeader =
          def.section !== lastSection ? (
            <div key={`s-${def.section}`} className="nav-section">
              {def.section}
            </div>
          ) : null;
        lastSection = def.section;
        const Icon = def.icon;
        return (
          <div key={def.key} style={{ display: "contents" }}>
            {sectionHeader}
            <button
              type="button"
              className={`nav-item ${page === def.key ? "active" : ""}`}
              aria-current={page === def.key ? "page" : undefined}
              onClick={() => setPage(def.key)}
            >
              <Icon size={15} strokeWidth={1.9} />
              {def.label}
              {counts[def.key] > 0 && (
                <span className="nav-badge" aria-label={`${counts[def.key]} 项`}>
                  {counts[def.key]}
                </span>
              )}
            </button>
          </div>
        );
      })}

      <div className="nav-footer">
        <span>
          v0.1.0
          <span className={`backend-tag ${BACKEND_KIND === "mock" ? "mock" : ""}`}>
            {BACKEND_KIND === "mock" ? "演示" : "正式"}
          </span>
        </span>
        <button
          type="button"
          className="theme-toggle"
          aria-label={
            theme === "system"
              ? "当前跟随系统，切换到深色主题"
              : theme === "dark"
                ? "当前为深色主题，切换到浅色主题"
                : "当前为浅色主题，切换到跟随系统"
          }
          onClick={toggleTheme}
          title={
            theme === "system"
              ? "跟随系统（点击切换到深色）"
              : theme === "dark"
                ? "深色（点击切换到浅色）"
                : "浅色（点击恢复跟随系统）"
          }
        >
          {theme === "system" ? (
            <Monitor size={13} />
          ) : theme === "dark" ? (
            <Moon size={13} />
          ) : (
            <Sun size={13} />
          )}
        </button>
      </div>
    </nav>
  );
}
