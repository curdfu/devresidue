import { useState } from "react";
import { FolderPlus, X } from "lucide-react";
import { useSettingsStore, DATA_DIR_HINT } from "@/stores/settingsStore";
import { useUiStore } from "@/stores/uiStore";
import { BACKEND_KIND } from "@/App";
import { useScanStore } from "@/stores/scanStore";
import { projectsScope } from "@/stores/settingsStore";
import { formatDateTime } from "@/utils/format";
import { AiProfilePanel } from "@/components/AiProfilePanel";

export function SettingsPage() {
  const workspaceRoots = useSettingsStore((s) => s.workspaceRoots);
  const addRoot = useSettingsStore((s) => s.addRoot);
  const removeRoot = useSettingsStore((s) => s.removeRoot);
  const scannedAt = useScanStore((s) => s.scannedAt);
  const startScan = useScanStore((s) => s.startScan);
  const lastScanMode = useScanStore((s) => s.items.length > 0);
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);

  const [draft, setDraft] = useState("");

  const add = () => {
    addRoot(draft);
    setDraft("");
  };

  return (
    <>
      <div className="page-head">
        <div>
          <div className="page-title">设置</div>
          <div className="page-sub">本机偏好——按设备存储</div>
        </div>
      </div>
      <div className="page-body">
        <div className="settings">
          <div className="settings-card">
            <h3>数据目录</h3>
            <p>
              扫描快照、清理计划与日志存放在此处。后端接线完善中——此处仅作透明展示。
            </p>
            <div className="mono">{DATA_DIR_HINT}</div>
          </div>

          <div className="settings-card">
            <h3>工作区根目录</h3>
            <p>
              kondo 在这些目录中查找项目产物。用于「项目」扫描范围及默认范围的根目录解析。
            </p>
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              {workspaceRoots.map((root) => (
                <div key={root} className="root-row">
                  {root}
                  <button
                    className="del"
                    onClick={() => removeRoot(root)}
                    title="移除根目录"
                  >
                    <X size={12} />
                  </button>
                </div>
              ))}
            </div>
            <div className="root-add">
              <input
                value={draft}
                placeholder="D:\Code\another-workspace"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && add()}
              />
              <button className="btn" onClick={add} disabled={!draft.trim()}>
                <FolderPlus size={13} /> 添加
              </button>
            </div>
            <button
              className="btn small"
              disabled={workspaceRoots.length === 0}
              onClick={() => void startScan(projectsScope(workspaceRoots))}
            >
              按这些根目录扫描项目
            </button>
          </div>

          <AiProfilePanel />

          <div className="settings-card">
            <h3>界面主题</h3>
            <p>跟随系统时会实时响应 Windows 深浅色切换，无需重启。</p>
            <div className="subtabs" style={{ alignSelf: "flex-start" }}>
              {(
                [
                  ["system", "跟随系统"],
                  ["dark", "深色"],
                  ["light", "浅色"],
                ] as const
              ).map(([mode, label]) => (
                <button
                  key={mode}
                  className={`subtab ${theme === mode ? "active" : ""}`}
                  onClick={() => setTheme(mode)}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          <div className="settings-card">
            <h3>关于</h3>
            <dl className="about-grid">
              <dt>版本</dt>
              <dd>0.1.0（M4 / Phase 12-13）</dd>
              <dt>后端</dt>
              <dd>
                {BACKEND_KIND === "mock"
                  ? "Mock（内存演示数据）"
                  : "Rust 核心之上的 Tauri v2 外壳"}
              </dd>
              <dt>上次扫描</dt>
              <dd>
                {scannedAt
                  ? `${formatDateTime(scannedAt)} · ${lastScanMode ? "结果已加载" : ""}`
                  : "尚未扫描"}
              </dd>
              <dt>删除权限</dt>
              <dd>仅 CleanupEngine——UI 绝不按路径删除</dd>
            </dl>
          </div>
        </div>
      </div>
    </>
  );
}
