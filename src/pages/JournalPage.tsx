import { useEffect, useMemo, useRef, useState } from "react";
import { Copy, ScrollText, Trash2 } from "lucide-react";
import type { JournalEntryDto } from "@/types";
import {
  filterJournal,
  journalSessions,
  useJournalStore,
} from "@/stores/journalStore";
import { getBackend } from "@/adapters";
import { formatDateTime } from "@/utils/format";
import {
  actionPresentation,
  presentationLabel,
  presentationTitle,
  reasonPresentation,
} from "@/utils/cleanupPresentation";
import { ModalFrame } from "@/components/common/ModalFrame";

/**
 * Cleanup history (SPEC §24): the journal read back as an audit table.
 * Filters only, plus the user-initiated journal-only maintenance action.
 */
const RESULT_FILTERS = [
  { key: "all", label: "全部结果" },
  { key: "success", label: "成功" },
  { key: "dry-run", label: "预演" },
  { key: "skipped", label: "已跳过" },
  { key: "failed", label: "失败" },
  { key: "unrecorded", label: "结果未记录" },
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
  const [query, setQuery] = useState("");
  const [reloadKey, setReloadKey] = useState(0);
  const [confirming, setConfirming] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [maintenanceError, setMaintenanceError] = useState<string | null>(null);
  const cancelClearRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    void load();
  }, [load, reloadKey]);

  /** Journal maintenance intentionally leaves scan, plans and selections intact. */
  const clearJournal = async () => {
    setClearing(true);
    setMaintenanceError(null);
    try {
      await getBackend().clearJournal();
      await load();
      setConfirming(false);
    } catch (raw) {
      setMaintenanceError(
        typeof raw === "object" && raw !== null && "message" in raw
          ? String((raw as { message: unknown }).message)
          : String(raw),
      );
    } finally {
      setClearing(false);
    }
  };

  const sessions = useMemo(() => journalSessions(entries), [entries]);
  const filtered = useMemo(() => {
    const base = filterJournal(entries, sessionFilter, resultFilter);
    const q = query.trim().toLocaleLowerCase();
    if (!q) return base;
    return base.filter((entry) =>
      [entry.product, entry.path, entry.error, entry.action, entry.phase, entry.result]
        .filter((value): value is string => Boolean(value))
        .some((value) => value.toLocaleLowerCase().includes(q)),
    );
  }, [entries, query, resultFilter, sessionFilter]);

  const sessionGroups = useMemo(() => {
    const groups = new Map<number, JournalEntryDto[]>();
    for (const entry of filtered) {
      const list = groups.get(entry.sessionId) ?? [];
      list.push(entry);
      groups.set(entry.sessionId, list);
    }
    return [...groups.entries()].sort((a, b) => b[0] - a[0]);
  }, [filtered]);

  const counts = useMemo(() => {
    const c = { success: 0, "dry-run": 0, skipped: 0, failed: 0, unrecorded: 0, other: 0 };
    for (const e of filtered) {
      const r = e.result ?? "";
      if (r === "success") c.success += 1;
      else if (r === "dry-run") c["dry-run"] += 1;
      else if (r === "skipped") c.skipped += 1;
      else if (r === "failed") c.failed += 1;
      else if (!r) c.unrecorded += 1;
      else c.other += 1;
    }
    return c;
  }, [filtered]);

  return (
    <>
      {confirming && (
        <ModalFrame
          onDismiss={clearing ? undefined : () => setConfirming(false)}
          dismissible={!clearing}
          initialFocusRef={cancelClearRef}
          className="journal-clear-modal"
        >
          {({ titleId, descriptionId }) => (
            <>
            <div className="modal-head">
              <Trash2 size={18} style={{ color: "var(--danger)" }} />
              <div>
                <h2 id={titleId}>清除清理记录？</h2>
                <div id={descriptionId} className="sub">此操作不可撤销</div>
              </div>
            </div>
            <div className="modal-body">
              {maintenanceError && (
                <div className="error-banner" role="alert">{maintenanceError}</div>
              )}
              <p style={{ color: "var(--text-soft)", fontSize: 12.5, lineHeight: 1.6 }}>
                将删除 DevResidue 命名规则下的<b>清理日志分片</b>，不改变当前扫描结果、
                已保存的清理计划或全局选择。
              </p>
              <p style={{ color: "var(--text-mute)", fontSize: 11.5, lineHeight: 1.6 }}>
                只影响 DevResidue 自己的审计记录——扫描快照、规则、AI 配置、偏好、
                编号水位以及你磁盘上的缓存与项目文件都会保留。
              </p>
            </div>
            <div className="modal-foot">
              <button
                ref={cancelClearRef}
                type="button"
                className="btn"
                onClick={() => setConfirming(false)}
                disabled={clearing}
              >
                取消
              </button>
              <button type="button" className="btn danger" onClick={() => void clearJournal()} disabled={clearing}>
                <Trash2 size={13} /> {clearing ? "清除中…" : "清除记录"}
              </button>
            </div>
            </>
          )}
        </ModalFrame>
      )}
      <div className="page-head">
        <div>
          <h1 className="page-title">日志</h1>
          <div className="page-sub">
            每次清理操作的审计记录——执行了什么、清理了什么、拒绝了什么。
          </div>
        </div>
        <div className="scanbar" style={{ marginLeft: "auto" }}>
          <button
            type="button"
            className="btn small"
            onClick={() => setReloadKey((k) => k + 1)}
            disabled={loading}
            title="重新读取日志文件"
          >
            <ScrollText size={12} /> {loading ? "读取中…" : "刷新"}
          </button>
          <button
            type="button"
            className="btn small"
            onClick={() => setConfirming(true)}
            disabled={clearing || entries.length === 0}
            title="清除 DevResidue 清理日志，不影响扫描结果与清理计划"
          >
            <Trash2 size={12} /> {clearing ? "清除中…" : "清除记录"}
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
                {counts.unrecorded > 0 && (
                  <span className="badge plain">未记录 {counts.unrecorded}</span>
                )}
              </>
            )}
          </div>

          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <label htmlFor="journal-search" className="field-label">搜索已加载记录
              <input
                id="journal-search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="产品、路径、原因或阶段…"
                className="text-input"
                aria-describedby="journal-search-help"
              />
            </label>
            <span id="journal-search-help" className="muted-inline" style={{ alignSelf: "end", paddingBottom: 8 }}>
              仅搜索当前已加载的最近 200 条记录
            </span>
            <label htmlFor="journal-session-filter" className="field-label">会话
            <select
              id="journal-session-filter"
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
            </label>
            <label htmlFor="journal-result-filter" className="field-label">结果
            <select
              id="journal-result-filter"
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
            </label>
          </div>

          <div className="table-card journal-groups" style={{ maxHeight: "calc(100vh - 260px)" }}>
            {sessionGroups.map(([sessionId, sessionEntries]) => (
              <JournalSession key={sessionId} sessionId={sessionId} entries={sessionEntries} />
            ))}
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

function JournalSession({ sessionId, entries }: { sessionId: number; entries: JournalEntryDto[] }) {
  const totalBytes = entries.reduce((sum, entry) => sum + entry.estimatedSize, 0);
  const success = entries.filter((entry) => entry.result === "success").length;
  const skipped = entries.filter((entry) => entry.result === "skipped").length;
  const failed = entries.filter((entry) => entry.result === "failed").length;
  const unrecorded = entries.filter((entry) => !entry.result).length;
  const firstTime = entries.reduce((latest, entry) => Math.max(latest, entry.timeSecs), 0);
  return (
    <section className="journal-session" aria-labelledby={`journal-session-${sessionId}`}>
      <div className="journal-session-head">
        <div>
          <h2 id={`journal-session-${sessionId}`}>会话 #{sessionId}</h2>
          <span className="muted-inline">{formatDateTime(firstTime)} · {entries.length} 条记录 · 逻辑大小估计 {sizeLabelBytes(totalBytes)}</span>
        </div>
        <div className="journal-session-counts">
          <span className="badge status-ok">成功 {success}</span>
          <span className="badge status-skipped">跳过 {skipped}</span>
          <span className="badge status-failed">失败 {failed}</span>
          {unrecorded > 0 && <span className="badge plain">未记录 {unrecorded}</span>}
        </div>
      </div>
      <table className="rules journal-table">
        <thead>
          <tr>
            <th>时间</th>
            <th>对象与位置</th>
            <th>操作</th>
            <th>逻辑大小估计</th>
            <th>结果</th>
            <th>原因摘要</th>
            <th>详情</th>
          </tr>
        </thead>
        <tbody>
          {entries.map((entry, index) => <JournalRow key={`${entry.timeSecs}-${index}`} entry={entry} />)}
        </tbody>
      </table>
    </section>
  );
}

function JournalRow({ entry }: { entry: JournalEntryDto }) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const result = entry.result ?? "";
  const action = actionPresentation(entry.action);
  const error = reasonPresentation(entry.error);
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

  const rawText = [
    `会话 #${entry.sessionId}`,
    `时间：${formatDateTime(entry.timeSecs)}`,
    `阶段：${entry.phase}`,
    `产品：${entry.product ?? "未提供"}`,
    `路径：${entry.path ?? "未提供"}`,
    `原始动作：${entry.action ?? "未提供"}`,
    `结果：${entry.result ?? "未记录"}`,
    `原始错误：${entry.error ?? "未提供"}`,
  ].join("\n");

  const copyDetails = async () => {
    try {
      await navigator.clipboard.writeText(rawText);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch {
      setCopied(false);
    }
  };

  return (
    <>
    <tr>
      <td className="mono" style={{ whiteSpace: "nowrap" }}>
        {formatDateTime(entry.timeSecs)}
      </td>
      <td>
        <div>{entry.product ?? "未提供产品"}</div>
        <div className="mono journal-path" title={entry.path ?? undefined}>{compactText(entry.path ?? "未提供路径")}</div>
      </td>
      <td className="mono" title={entry.action ? presentationTitle(action) : undefined}>
        {entry.action ? presentationLabel(action) : "未提供操作"}
      </td>
      <td className="num">{entry.estimatedSize > 0 ? sizeLabel(entry) : "—"}</td>
      <td>
        <span className={`badge ${badgeClass}`}>{resultLabel(result)}</span>
      </td>
      <td
        style={{ color: entry.error ? "var(--danger)" : "var(--text-mute)", fontSize: 11 }}
        title={entry.error ? presentationTitle(error) : undefined}
      >
        {entry.error ? error.summary : entry.phase === "attempt" ? "已记录尝试，等待结果" : "—"}
      </td>
      <td>
        <button type="button" className="btn small ghost" onClick={() => setExpanded((value) => !value)}>
          {expanded ? "收起" : "查看"}
        </button>
      </td>
    </tr>
    {expanded && (
      <tr className="journal-detail-row">
        <td colSpan={7}>
          <div className="journal-detail-copy">
            <div className="journal-detail-actions">
              <b>原始记录详情</b>
              <button type="button" className="btn small ghost" onClick={() => void copyDetails()}>
                <Copy size={12} /> {copied ? "已复制" : "复制纯文本"}
              </button>
            </div>
            <pre>{rawText}</pre>
          </div>
        </td>
      </tr>
    )}
    </>
  );
}

function sizeLabel(entry: JournalEntryDto): string {
  return sizeLabelBytes(entry.estimatedSize);
}

function sizeLabelBytes(bytes: number): string {
  if (bytes <= 0) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const v = bytes / 1024 ** i;
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

function compactText(value: string, max = 58): string {
  if (value.length <= max) return value;
  return `${value.slice(0, Math.max(10, max - 1))}…`;
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
