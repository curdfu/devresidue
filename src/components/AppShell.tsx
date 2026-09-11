import { useUiStore } from "@/stores/uiStore";
import { Sidebar } from "./Sidebar";
import { ScanBar } from "./ScanBar";
import { DashboardPage } from "@/pages/DashboardPage";
import { CategoryPage } from "@/pages/CategoryPage";
import { UnknownPage } from "@/pages/UnknownPage";
import { AiReviewPage } from "@/pages/AiReviewPage";
import { RulesPage } from "@/pages/RulesPage";
import { JournalPage } from "@/pages/JournalPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { RiskResultsPage } from "@/pages/RiskResultsPage";

/**
 * Left nav + routed main area. Routing is a plain store switch (no router
 * dependency) — a desktop tool with flat pages doesn't need URLs.
 */
export function AppShell() {
  const page = useUiStore((s) => s.page);

  return (
    <div className="app">
      <Sidebar />
      <div className="main">
        {/* Scan surface lives on every data page except the read-only
            trust/audit pages (rules / journal / settings). */}
        {page !== "rules" && page !== "settings" && page !== "journal" && <ScanBar />}
        {page === "dashboard" && <DashboardPage />}
        {page === "agents" && (
          <CategoryPage
            title="AI Agents"
            subtitle="Agent 数据按缓存、会话、临时、配置与凭据细分（绝不把整个 agent 根目录一刀切）"
            filter={{
              kind: "category",
              categories: [
                "ai-agent",
                "session",
                "temporary",
                "workspace-state",
                "configuration",
                "credential",
              ],
              subtabs: [
                { key: "all", label: "全部", categories: ["ai-agent", "session", "temporary", "workspace-state", "configuration", "credential"] },
                { key: "cache", label: "缓存与临时", categories: ["ai-agent", "temporary"] },
                { key: "sessions", label: "会话记录", categories: ["session"] },
                { key: "protected", label: "配置与凭据", categories: ["configuration", "credential"] },
              ],
            }}
          />
        )}
        {page === "devcache" && (
          <CategoryPage
            title="开发缓存"
            subtitle="包管理器与工具缓存——可回收，代价是重新下载"
            filter={{
              kind: "category",
              categories: ["developer-cache", "package-cache", "dependency"],
              subtabs: [
                { key: "all", label: "全部", categories: ["developer-cache", "package-cache", "dependency"] },
                { key: "caches", label: "工具缓存", categories: ["developer-cache", "package-cache"] },
                { key: "deps", label: "依赖目录（node_modules 等）", categories: ["dependency"] },
              ],
            }}
          />
        )}
        {page === "projects" && (
          <CategoryPage
            title="项目"
            subtitle="Kondo 在工作区根目录中发现的构建产物与依赖"
            filter={{ kind: "category", categories: ["build-artifact", "dependency", "log", "temporary"] }}
          />
        )}
        {page === "unknown" && <UnknownPage />}
        {page === "ai-review" && <AiReviewPage />}
        {page === "risk-results" && <RiskResultsPage />}
        {page === "rules" && <RulesPage />}
        {page === "journal" && <JournalPage />}
        {page === "settings" && <SettingsPage />}
      </div>
    </div>
  );
}
