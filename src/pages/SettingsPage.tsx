import { useEffect, useRef, useState } from "react";
import { CheckCircle2, FolderOpen, FolderPlus, LoaderCircle, Trash2, X } from "lucide-react";
import { getBackend } from "@/adapters";
import { useSettingsStore, type WorkspaceRootsMode } from "@/stores/settingsStore";
import { useUiStore } from "@/stores/uiStore";
import { useScanStore } from "@/stores/scanStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useCleanupStore } from "@/stores/cleanupStore";
import { useAiStore } from "@/stores/aiStore";
import { useUnknownWorkflowStore } from "@/stores/unknownWorkflowStore";
import { projectsScope } from "@/stores/settingsStore";
import { formatDateTime } from "@/utils/format";
import { AiProfilePanel } from "@/components/AiProfilePanel";
import { ModalFrame } from "@/components/common/ModalFrame";
import type { AppDataInfoDto, WorkspaceRootValidationDto } from "@/types";

export function SettingsPage() {
  const workspaceRoots = useSettingsStore((s) => s.workspaceRoots);
  const addRoot = useSettingsStore((s) => s.addRoot);
  const removeRoot = useSettingsStore((s) => s.removeRoot);
  const workspaceRootsMode = useSettingsStore((s) => s.workspaceRootsMode);
  const legacyRootsPending = useSettingsStore((s) => s.legacyRootsPending);
  const setWorkspaceRootsMode = useSettingsStore((s) => s.setWorkspaceRootsMode);
  const scannedAt = useScanStore((s) => s.scannedAt);
  const startScan = useScanStore((s) => s.startScan);
  const lastScanMode = useScanStore((s) => s.items.length > 0);
  const theme = useUiStore((s) => s.theme);
  const setTheme = useUiStore((s) => s.setTheme);

  const [draft, setDraft] = useState("");
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [maintenanceError, setMaintenanceError] = useState<string | null>(null);
  const [rootStatus, setRootStatus] = useState<string | null>(null);
  const [rootValidation, setRootValidation] = useState<WorkspaceRootValidationDto[]>([]);
  const [validatingRoot, setValidatingRoot] = useState(false);
  const [appDataInfo, setAppDataInfo] = useState<AppDataInfoDto | null>(null);
  const [appDataError, setAppDataError] = useState<string | null>(null);
  const cancelResetRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    let cancelled = false;
    void getBackend().getAppDataInfo()
      .then((info) => {
        if (!cancelled) setAppDataInfo(info);
      })
      .catch((raw) => {
        if (!cancelled) {
          setAppDataError(raw instanceof Error ? raw.message : String(raw));
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const add = async () => {
    const candidate = draft.trim();
    if (!candidate || validatingRoot) return;
    setValidatingRoot(true);
    setRootStatus(null);
    try {
      const results = await getBackend().validateWorkspaceRoots([...workspaceRoots, candidate]);
      setRootValidation(results);
      const last = results[results.length - 1];
      if (!last?.valid) {
        setRootStatus(last?.message ?? "目录校验失败，请修正后重试");
        return;
      }
      addRoot(last.normalized ?? candidate);
      setDraft("");
      setRootStatus("扫描位置已保存；未修改磁盘内容。");
    } catch (raw) {
      setRootStatus(raw instanceof Error ? raw.message : String(raw));
    } finally {
      setValidatingRoot(false);
    }
  };

  const pickRoot = async () => {
    setRootStatus(null);
    try {
      const picked = await getBackend().pickWorkspaceDirectory();
      if (!picked) {
        setRootStatus("当前后端未提供原生目录选择，请使用下方路径输入框。");
        return;
      }
      setDraft(picked);
      setRootStatus("已填入目录，请点击“添加”完成校验和保存。");
    } catch (raw) {
      setRootStatus(raw instanceof Error ? raw.message : String(raw));
    }
  };

  const resetScanData = async () => {
    if (resetting) return;
    setResetting(true);
    setMaintenanceError(null);
    try {
      await getBackend().resetScanData();
      useScanStore.getState().reset();
      useSelectionStore.getState().clear();
      useCleanupStore.getState().close();
      useAiStore.getState().resetForNewScan(0);
      useUnknownWorkflowStore.getState().reset();
      useUiStore.getState().openDetail(null);
      useUiStore.getState().setPage("dashboard");
      setConfirmReset(false);
    } catch (raw) {
      setMaintenanceError(
        typeof raw === "object" && raw !== null && "message" in raw
          ? String((raw as { message: unknown }).message)
          : String(raw),
      );
    } finally {
      setResetting(false);
    }
  };

  return (
    <>
      {confirmReset && (
        <ModalFrame
          onDismiss={resetting ? undefined : () => setConfirmReset(false)}
          dismissible={!resetting}
          initialFocusRef={cancelResetRef}
          className="settings-reset-modal"
        >
          {({ titleId, descriptionId }) => (
            <>
              <div className="modal-head">
                <Trash2 size={18} style={{ color: "var(--danger)" }} />
                <div>
                  <h2 id={titleId}>重置扫描与计划？</h2>
                  <div id={descriptionId} className="sub">不会删除项目、缓存或配置</div>
                </div>
              </div>
              <div className="modal-body">
                {maintenanceError && (
                  <div className="error-banner" role="alert">{maintenanceError}</div>
                )}
                <p style={{ color: "var(--text-soft)", fontSize: 12.5, lineHeight: 1.6 }}>
                  将清除当前扫描快照、已保存的清理计划以及依附这些条目的临时选择和 AI 批次。
                </p>
                <p style={{ color: "var(--text-mute)", fontSize: 11.5, lineHeight: 1.6 }}>
                  清理日志、用户规则、AI 服务配置、界面偏好、编号水位以及磁盘上的项目和缓存文件都会保留。
                </p>
              </div>
              <div className="modal-foot">
                <button ref={cancelResetRef} type="button" className="btn" onClick={() => setConfirmReset(false)} disabled={resetting}>
                  取消
                </button>
                <button type="button" className="btn danger" onClick={() => void resetScanData()} disabled={resetting}>
                  <Trash2 size={13} /> {resetting ? "重置中…" : "重置扫描与计划"}
                </button>
              </div>
            </>
          )}
        </ModalFrame>
      )}
      <div className="page-head">
        <div>
          <h1 className="page-title">设置</h1>
          <div className="page-sub">本机偏好——按设备存储</div>
        </div>
      </div>
      <div className="page-body">
        <div className="settings">
          <div className="settings-card">
            <h3>扫描位置</h3>
            <p>
              自动解析使用后端默认范围；自定义模式只使用下面已经逐项校验并保存的位置。
              目录设置只影响扫描范围和保护根，不会删除或移动磁盘文件。
            </p>
            <div className="subtabs" aria-label="默认扫描范围模式">
              {([['automatic', '自动解析'], ['custom', '使用这些位置']] as const).map(([mode, label]) => (
                <button key={mode} type="button" className={`subtab ${workspaceRootsMode === mode ? "active" : ""}`} aria-pressed={workspaceRootsMode === mode} onClick={() => setWorkspaceRootsMode(mode as WorkspaceRootsMode)}>
                  {label}
                </button>
              ))}
            </div>
            {legacyRootsPending && <div className="notice" role="status">检测到旧版本保存的位置；当前仍使用自动解析，确认“使用这些位置”后才会切换默认扫描。</div>}
            {rootStatus && (
              <div className={rootValidation.some((result) => !result.valid) ? "error-banner" : "confirm-note review"} role="status">
                {rootValidation.some((result) => !result.valid) ? <X size={13} /> : <CheckCircle2 size={13} />}
                {rootStatus}
              </div>
            )}
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              {workspaceRoots.map((root) => (
                <div key={root} className="root-row">
                  {root}
                  <button
                    type="button"
                    className="del"
                    onClick={() => removeRoot(root)}
                    aria-label={`移除工作区根目录 ${root}`}
                    title="移除根目录"
                  >
                    <X size={12} />
                  </button>
                </div>
              ))}
            </div>
            <label htmlFor="workspace-root-input" style={{ display: "grid", gap: 4, color: "var(--text-mute)", fontSize: 12 }}>
              新增工作区根目录
            </label>
            <div className="root-add">
              <input
                id="workspace-root-input"
                name="workspace-root"
                value={draft}
                placeholder="D:\Code\another-workspace"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && add()}
              />
              <button type="button" className="btn" onClick={add} disabled={!draft.trim()}>
                {validatingRoot ? <LoaderCircle size={13} className="spin" /> : <FolderPlus size={13} />} {validatingRoot ? "校验中…" : "添加"}
              </button>
            </div>
            <button type="button" className="btn small ghost" onClick={() => void pickRoot()} disabled={validatingRoot}>
              <FolderOpen size={13} /> 选择文件夹
            </button>
            {rootValidation.filter((result) => !result.valid).map((result) => (
              <div key={`${result.input}-${result.errorCode}`} className="muted-inline">
                <span className="mono">{result.input || "（空）"}</span>：{result.message}
                {result.containedBy ? `（包含于 ${result.containedBy}）` : ""}
              </div>
            ))}
            <button
              type="button"
              className="btn small"
              disabled={workspaceRoots.length === 0 || workspaceRootsMode !== "custom"}
              onClick={() => void startScan(projectsScope(workspaceRoots))}
            >
              按这些位置扫描项目
            </button>
          </div>

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
                  type="button"
                  className={`subtab ${theme === mode ? "active" : ""}`}
                  aria-pressed={theme === mode}
                  onClick={() => setTheme(mode)}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          <AiProfilePanel />

          <div className="settings-card">
            <h3>应用数据</h3>
            <p>
              维护 DevResidue 自己的扫描快照和清理计划。此处操作不会删除项目、缓存或其他用户文件。
            </p>
            <div className="app-data-info">
              <div>数据目录</div>
              <div className="mono">{appDataInfo?.dataDir ?? (appDataError ? "读取失败" : "读取中…")}</div>
              {appDataError && <div className="muted-inline">读取应用数据目录失败：{appDataError}</div>}
              {appDataInfo && <div className="muted-inline">后端模式：{appDataInfo.backendMode === "mock" ? "演示（Mock）" : "Tauri"}</div>}
            </div>
            <button type="button" className="btn small danger" onClick={() => setConfirmReset(true)} disabled={resetting}>
              <Trash2 size={13} /> {resetting ? "重置中…" : "重置扫描与计划"}
            </button>
          </div>

          <div className="settings-card">
            <h3>关于</h3>
            <dl className="about-grid">
              <dt>版本</dt>
              <dd>0.1.0</dd>
              <dt>产品</dt>
              <dd>开发残留清理助手</dd>
              <dt>上次扫描</dt>
              <dd>
                {scannedAt
                  ? `${formatDateTime(scannedAt)} · ${lastScanMode ? "结果已加载" : ""}`
                  : "尚未扫描"}
              </dd>
              <dt>数据安全</dt>
              <dd>清理前需要确认，未知和受保护数据不会自动处理</dd>
            </dl>
          </div>
        </div>
      </div>
    </>
  );
}
