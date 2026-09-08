import { useEffect, useMemo, useState } from "react";
import { ScrollText, Trash2 } from "lucide-react";
import type { JournalEntryDto } from "@/types";
import {
  filterJournal,
  journalSessions,
  useJournalStore,
} from "@/stores/journalStore";
import { getBackend } from "@/adapters";
import { useScanStore } from "@/stores/scanStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useCleanupStore } from "@/stores/cleanupStore";
import { useUiStore } from "@/stores/uiStore";
import { formatDateTime } from "@/utils/format";

/**
 * Cleanup history (SPEC §24): the journal read back as an audit table.
 * Filters only, plus the user-initiated 清空 (wipe all DevResidue state:
 * journal + scan results + plans — never user data on disk).
 */
const RESULT_FILTERS = [
  { key: "all", label: "全部结果" },
  { key: "success", label: "成功" },
  { key: "dry-run", label: "预演" },
  { key: "skipped", label: "已跳过" },
  { key: "failed", label: "失败" },
] as const;

export function JournalPage() {
  const entries = useJournalStore((s) => s.entries);
  const loading = useJournalStore((s) => s.loading);
  const error = useJournalStore((s) => s.error);
  const load = useJournalStore((s) => s.load);
  const sessionFilter = useJournalStore((s) => s.sessionFilter);
  const resultFilter = useJournalStore((s) => s.resultFilter);
  const setSessionFilter = useJournalStore((s) => s.setSessionFilter);
  const setResultFilter = useJournalStore((s) => s.setResultFilter);
  const [reloadKey, setReloadKey] = useState(0);
  const [confirming, setConfirming] = useState(false);
  const [clearing, setClearing] = useState(false);

  useEffect(() => {
    void load();
  }, [load, reloadKey]);

  /** 清空: backend wipe + full frontend store reset (first-run state). */
  const clearAll = async () => {
    setClearing(true);
    try {
      await getBackend().clearAllData();
      // Frontend reset — every piece of derived UI state goes back to
      // first-run: scan results/selections (dashboard + category pages),
      // cleanup flow, the journal list itself and the detail panel.
      useScanStore.getState().reset();
      useSelectionStore.getState().clear();
      useCleanupStore.getState().close();
      useUiStore.getState().openDetail(null);
      useUiStore.getState().setPage("dashboard");
      await load();
      setConfirming(false);
    } finally {
      setClearing(false);
    }
  };

  const sessions = useMemo(() => journalSessions(entries), [entries]);
  const filtered = useMemo(
    () => filterJournal(entries, sessionFilter, resultFilter),
    [entries, sessionFilter, resultFilter],
  );

  const counts = useMemo(() => {
    const c = { success: 0, "dry-run": 0, skipped: 0, failed: 0, other: 0 };
    for (const e of filtered) {
      const r = e.result ?? "";
      if (r === "success") c.success += 1;
      else if (r === "dry-run") c["dry-run"] += 1;
      else if (r === "skipped") c.skipped += 1;
      else if (r === "failed") c.failed += 1;
      else c.other += 1;
    }
    return c;
  }, [filtered]);

  return (
    <>
      {confirming && (
        <div className="modal-veil">
          <div className="modal" style={{ width: 440 }}>
            <div className="modal-head">
              <Trash2 size={18} style={{ color: "var(--danger)" }} />
              <div>
                <h2>清空所有历史记录？</h2>
                <div className="sub">此操作不可撤销</div>
              </div>
            </div>
            <div className="modal-body">
              <p style={{ color: "var(--text-soft)", fontSize: 12.5, lineHeight: 1.6 }}>
                将删除<b>全部清理日志</b>、<b>当前扫描结果</b>（概览与各分类页的明细）
                以及<b>已保存的清理计划</b>，应用回到首次启动状态。
              </p>
              <p style={{ color: "var(--text-mute)", fontSize: 11.5, lineHeight: 1.6 }}>
                只影响 DevResidue 自己的数据记录——你磁盘上的缓存与项目文件不会被触碰；
                需要重新查看明细时再扫描一次即可。
              </p>
            </div>
            <div className="modal-foot">
              <button
                className="btn"
                onClick={() => setConfirming(false)}
                disabled={clearing}
              >
                取消
              </button>
              <button className="btn danger" onClick={() => void clearAll()} disabled={clearing}>
                <Trash2 size={13} /> {clearing ? "清空中…" : "全部清空"}
              </button>
            </div>
          </div>
        </div>
      )}
      <div className="page-head">
        <div>
          <div className="page-title">日志</div>
          <div className="page-sub">
            每次清理操作的审计记录——执行了什么、清理了什么、拒绝了什么。
          </div>
        </div>
        <div className="scanbar" style={{ marginLeft: "auto" }}>
          <button
            className="btn small"
            onClick={() => setReloadKey((k) => k + 1)}
            disabled={loading}
            title="重新读取日志文件"
          >
            <ScrollText size={12} /> {loading ? "读取中…" : "刷新"}
          </button>
          <button
            className="btn small"
            onClick={() => setConfirming(true)}
            disabled={clearing || entries.length === 0}
            title="删除全部历史日志、扫描结果与清理计划，恢复初始状态"
          >
            <Trash2 size={12} /> {clearing ? "清空中…" : "清空"}
          </button>
        </div>
      </div>

      <div className="page-body">
        <div className="rules-layout">
          <div className="validate-strip">
            {error ? (
              <span style={{ color: "var(--danger)" }}>{error}</span>
            ) : (
              <>
                <span>
                  <b>{filtered.length}</b> 条记录
                </span>
                <span className="badge status-ok">成功 {counts.success}</span>
                {counts["dry-run"] > 0 && (
                  <span className="badge status-would">预演 {counts["dry-run"]}</span>
                )}
                <span className="badge status-skipped">跳过 {counts.skipped}</span>
                <span className="badge status-failed">失败 {counts.failed}</span>
              </>
            )}
          </div>

          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <select
              className="scope-select"
              value={String(sessionFilter)}
              onChange={(e) =>
                setSessionFilter(e.target.value === "all" ? "all" : Number(e.target.value))
              }
              title="按会话筛选"
            >
              <option value="all">全部会话</option>
              {sessions.map((s) => (
                <option key={s} value={s}>
                  会话 #{s}（{entries.filter((e) => e.sessionId === s).length} 条）
                </option>
              ))}
            </select>
            <select
              className="scope-select"
              value={resultFilter}
              onChange={(e) => setResultFilter(e.target.value)}
              title="按结果筛选"
            >
              {RESULT_FILTERS.map((f) => (
                <option key={f.key} value={f.key}>
                  {f.label}
                </option>
              ))}
            </select>
          </div>

          <div className="table-card" style={{ maxHeight: "calc(100vh - 260px)" }}>
            <table className="rules journal-table">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>会话</th>
                  <th>产品</th>
                  <th>路径</th>
                  <th>操作</th>
                  <th>大小</th>
                  <th>结果</th>
                  <th>错误</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((e, i) => (
                  <JournalRow key={`${e.sessionId}-${e.timeSecs}-${i}`} entry={e} />
                ))}
              </tbody>
            </table>
            {filtered.length === 0 && !loading && !error && (
              <div className="empty" style={{ padding: "40px 20px" }}>
                <p>暂无日志记录——执行一次清理后会显示在这里。</p>
              </div>
            )}
          </div>
        </div>
      </div>
    </>
  );
}

function JournalRow({ entry }: { entry: JournalEntryDto }) {
  const result = entry.result ?? "";
  const badgeClass =
    result === "success"
      ? "status-ok"
      : result === "dry-run"
        ? "status-would"
        : result === "skipped"
          ? "status-skipped"
          : result === "failed"
            ? "status-failed"
            : "plain";

  return (
    <tr>
      <td className="mono" style={{ whiteSpace: "nowrap" }}>
        {formatDateTime(entry.timeSecs)}
      </td>
      <td className="mono">#{entry.sessionId}</td>
      <td>{entry.product ?? "—"}</td>
      <td className="mono" title={entry.path ?? undefined}>
        {entry.path ?? "—"}
      </td>
      <td className="mono">{entry.action ?? "—"}</td>
      <td className="num">{entry.estimatedSize > 0 ? sizeLabel(entry) : "—"}</td>
      <td>
        <span className={`badge ${badgeClass}`}>{resultLabel(result)}</span>
      </td>
      <td
        className="mono"
        style={{
          maxWidth: 280,
          overflow: "hidden",
          textOverflow: "ellipsis",
          color: "var(--danger)",
          fontSize: 11,
        }}
        title={entry.error ?? undefined}
      >
        {entry.error ?? "—"}
      </td>
    </tr>
  );
}

function sizeLabel(entry: JournalEntryDto): string {
  const bytes = entry.estimatedSize;
  if (bytes <= 0) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const v = bytes / 1024 ** i;
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

/** Journal result wire verbs → badge text. */
function resultLabel(result: string): string {
  switch (result) {
    case "success":
      return "成功";
    case "dry-run":
      return "预演";
    case "skipped":
      return "已跳过";
    case "failed":
      return "失败";
    case "":
      return "—";
    default:
      return result;
  }
}
