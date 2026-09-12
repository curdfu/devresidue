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
import { SelectedItemsPage } from "@/pages/SelectedItemsPage";

/**
 * Left nav + routed main area. Routing is a plain store switch (no router
 * dependency) — a desktop tool with flat pages doesn't need URLs.
 */
export function AppShell() {
  const page = useUiStore((s) => s.page);

  return (
    <div className="app">
      <Sidebar />
      <main className="main">
        {/* Scan surface lives on every data page except the read-only
            trust/audit pages (rules / journal / settings). */}
        {page !== "rules" && page !== "settings" && page !== "journal" && <ScanBar />}
        {page === "dashboard" && <DashboardPage />}
        {page === "agents" && (
          <CategoryPage
            title="Agent 数据"
            subtitle="Agent 数据按缓存、会话、临时、配置与凭据细分（绝不把整个 agent 根目录一刀切）"
            filter={{
              kind: "family",
              family: "agents",
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
            title="工具数据"
            subtitle="包管理器、开发工具和其他开发数据；按缓存、依赖与其他分组"
            filter={{
              kind: "family",
              family: "tools",
              subtabs: [
                { key: "all", label: "全部", categories: ["developer-cache", "package-cache", "dependency", "ide", "log", "temporary", "configuration", "credential", "unknown"] },
                { key: "caches", label: "缓存", categories: ["developer-cache", "package-cache"] },
                { key: "deps", label: "依赖", categories: ["dependency"] },
                { key: "other", label: "配置/其他", categories: ["ide", "log", "temporary", "configuration", "credential", "unknown"] },
              ],
            }}
          />
        )}
        {page === "projects" && (
          <CategoryPage
            title="项目产物"
            subtitle="Kondo 在工作区根目录中发现的构建产物与依赖"
            filter={{
              kind: "family",
              family: "projects",
              subtabs: [
                { key: "all", label: "全部", categories: ["build-artifact", "dependency", "log", "temporary"] },
                { key: "artifacts", label: "构建产物", categories: ["build-artifact", "log", "temporary"] },
                { key: "deps", label: "依赖目录", categories: ["dependency"] },
              ],
            }}
          />
        )}
        {page === "unknown" && <UnknownPage />}
        {page === "selected-items" && <SelectedItemsPage />}
        {page === "ai-review" && <AiReviewPage />}
        {page === "risk-results" && <RiskResultsPage />}
        {page === "rules" && <RulesPage />}
        {page === "journal" && <JournalPage />}
        {page === "settings" && <SettingsPage />}
      </main>
    </div>
  );
}
