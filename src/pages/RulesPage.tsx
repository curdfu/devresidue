import { useEffect, useMemo, useRef, useState } from "react";
import { CheckCircle2, RefreshCw, ShieldCheck, TriangleAlert } from "lucide-react";
import type { RuleDto } from "@/types";
import { getBackend } from "@/adapters";
import { RiskBadge } from "@/components/common";
import { BACKEND_KIND } from "@/App";
import { ModalFrame } from "@/components/common/ModalFrame";
import { useRulesStore } from "@/stores/rulesStore";

/**
 * Rules registry. R11: loads the *actual* merged rule set from
 * the backend (`get_rules` + `validate_rules`) instead of a hardcoded demo
 * table. In mock mode a visible "demo data" badge marks that the table is
 * not the real machine's rule set (review requirement).
 */
export function RulesPage() {
  const rules = useRulesStore((s) => s.rules);
  const validation = useRulesStore((s) => s.validation);
  const storeError = useRulesStore((s) => s.error);
  const loading = useRulesStore((s) => s.loading);
  const loadRules = useRulesStore((s) => s.load);
  const [actionError, setActionError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [sourceFilter, setSourceFilter] = useState<"all" | "user" | "builtin">("all");
  const [healthFilter, setHealthFilter] = useState<"all" | "issues">("all");
  const [deletingRuleId, setDeletingRuleId] = useState<string | null>(null);
  const [ruleToDelete, setRuleToDelete] = useState<RuleDto | null>(null);
  const cancelDeleteRef = useRef<HTMLButtonElement>(null);

  const error = actionError ?? storeError;
  const reload = () => loadRules(true);

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rules.filter((r) =>
      (sourceFilter === "all" ||
        (sourceFilter === "user"
          ? r.source === "user" || r.source === "user-protected"
          : r.source !== "user" && r.source !== "user-protected")) &&
      (healthFilter === "all" || !r.valid || Boolean(validation?.issues.some((issue) => issue.ruleId === r.ruleId))) &&
      (!q || [r.ruleId, r.description ?? "", r.source ?? "", r.category ?? ""].some((s) =>
        s.toLowerCase().includes(q),
      )),
    );
  }, [healthFilter, query, rules, sourceFilter, validation]);

  const deleteRule = (rule: RuleDto) => setRuleToDelete(rule);

  const confirmDeleteRule = async () => {
    const rule = ruleToDelete;
    if (!rule || deletingRuleId !== null) return;
    setDeletingRuleId(rule.ruleId);
    setActionError(null);
    try {
      await getBackend().deleteUserRule(rule.ruleId);
      await reload();
    } catch (raw) {
      const message =
        typeof raw === "object" && raw !== null && "message" in raw
          ? String((raw as { message: unknown }).message)
          : String(raw);
      setActionError(message);
    } finally {
      setDeletingRuleId(null);
      setRuleToDelete(null);
    }
  };

  /** Issues per rule id (joined from the validation aggregate). */
  const issuesByRule = useMemo(() => {
    const map = new Map<string, string[]>();
    if (!validation) return map;
    for (const issue of validation.issues) {
      if (issue.ruleId === null) continue; // file-level: shown in the strip
      const list = map.get(issue.ruleId) ?? [];
      list.push(`${issue.severity}: ${issue.message}`);
      map.set(issue.ruleId, list);
    }
    return map;
  }, [validation]);

  /** File-level findings (ruleId null) for the status strip. */
  const fileIssues = useMemo(() => {
    if (!validation) return [];
    return validation.issues.filter((i) => i.ruleId === null);
  }, [validation]);

  const allValid = validation !== null && validation.errors === 0;
  const mockMode = BACKEND_KIND === "mock";

  return (
    <>
      {ruleToDelete && (
        <ModalFrame
          onDismiss={deletingRuleId ? undefined : () => setRuleToDelete(null)}
          dismissible={!deletingRuleId}
          initialFocusRef={cancelDeleteRef}
          className="rule-delete-modal"
        >
          {({ titleId, descriptionId }) => {
            const isProtected = ruleToDelete.source === "user-protected";
            return (
              <>
                <div className="modal-head">
                  <ShieldCheck size={18} style={{ color: "var(--danger)" }} />
                  <div>
                    <h2 id={titleId}>{isProtected ? "删除用户保护规则？" : "删除用户规则？"}</h2>
                    <div id={descriptionId} className="sub mono">{ruleToDelete.ruleId}</div>
                  </div>
                </div>
                <div className="modal-body">
                  <p style={{ color: "var(--text-soft)", fontSize: 12.5, lineHeight: 1.6 }}>
                    {isProtected
                      ? `删除保护规则“${ruleToDelete.ruleId}”后，下次扫描将不再由该规则保护对应位置。`
                      : `删除用户规则“${ruleToDelete.ruleId}”？下次扫描将不再应用此分类。`}
                  </p>
                  <p style={{ color: "var(--text-mute)", fontSize: 11.5, lineHeight: 1.6 }}>
                    删除规则不会直接删除磁盘数据；请重新扫描后查看新的分类结果。
                  </p>
                </div>
                <div className="modal-foot">
                  <button ref={cancelDeleteRef} className="btn" onClick={() => setRuleToDelete(null)} disabled={Boolean(deletingRuleId)}>
                    取消
                  </button>
                  <button className="btn danger" onClick={() => void confirmDeleteRule()} disabled={Boolean(deletingRuleId)}>
                    {deletingRuleId ? "删除中…" : "确认删除"}
                  </button>
                </div>
              </>
            );
          }}
        </ModalFrame>
      )}
      <div className="page-head">
        <div>
          <h1 className="page-title">规则</h1>
          <div className="page-sub">
            后端实际加载的合并规则集——可删除用户新建规则；每个扫描条目都会引用为它分类的规则
          </div>
        </div>
        <div className="scanbar" style={{ marginLeft: "auto" }}>
          <button className="btn small" onClick={() => void reload()} disabled={loading}>
            <RefreshCw size={12} className={loading ? "spin" : undefined} />
            {loading ? "加载中…" : "刷新"}
          </button>
        </div>
      </div>
      <div className="page-body">
        <div className="rules-layout">
          <div className="validate-strip">
            {error ? (
              <span style={{ color: "var(--danger)" }}>
                规则接口失败：<span className="mono">{error}</span>
              </span>
            ) : validation ? (
              <>
                <ShieldCheck
                  size={15}
                  style={{ color: allValid ? "var(--ok)" : "var(--risk-review)" }}
                />
                <span>
                  已加载 <b>{validation.total}</b> 条规则 ·{" "}
                  <b>{validation.errors}</b> 个错误——加载时已检查结构与路径边界
                </span>
                {validation.errors > 0 && (
                  <span className="badge status-failed">{validation.errors} 错误</span>
                )}
                {validation.warnings > 0 && (
                  <span className="badge status-would">{validation.warnings} 警告</span>
                )}
                {allValid && validation.warnings === 0 && (
                  <span className="badge status-ok" style={{ marginLeft: 8 }}>
                    校验通过
                  </span>
                )}
                {fileIssues.map((issue, i) => (
                  <span key={i} style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
                    {issue.severity}: {issue.message}
                  </span>
                ))}
              </>
            ) : (
              <span>校验中…</span>
            )}
            {mockMode && (
              <span
                className="badge plain"
                title="浏览器演示模式：此表为演示数据，不是本机的规则集"
                style={{ marginLeft: "auto" }}
              >
                演示数据（mock）
              </span>
            )}
          </div>

          <label htmlFor="rules-search" className="field-label">搜索规则
          <input
            id="rules-search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="按规则 ID、描述或路径筛选…"
            style={{
              background: "var(--bg-panel)",
              border: "1px solid var(--border)",
              borderRadius: "var(--radius)",
              padding: "7px 10px",
              color: "var(--text)",
              maxWidth: 380,
            }}
          />
          </label>

          <div className="filter-row" role="group" aria-label="规则筛选">
            <label htmlFor="rules-source-filter" className="field-label">来源
              <select
                id="rules-source-filter"
                className="scope-select"
                value={sourceFilter}
                onChange={(event) => setSourceFilter(event.target.value as typeof sourceFilter)}
              >
                <option value="all">全部来源</option>
                <option value="user">用户规则</option>
                <option value="builtin">内置规则</option>
              </select>
            </label>
            <label htmlFor="rules-health-filter" className="field-label">校验
              <select
                id="rules-health-filter"
                className="scope-select"
                value={healthFilter}
                onChange={(event) => setHealthFilter(event.target.value as typeof healthFilter)}
              >
                <option value="all">全部状态</option>
                <option value="issues">有问题</option>
              </select>
            </label>
          </div>

          <div className="table-card">
            <table className="rules">
              <thead>
                <tr>
                  <th>规则</th>
                  <th>来源</th>
                  <th>风险</th>
                  <th>类别</th>
                  <th>校验</th>
                  <th>操作</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((r) => {
                  const issues = issuesByRule.get(r.ruleId) ?? [];
                  return (
                    <tr key={r.ruleId}>
                      <td>
                        <div style={{ color: "var(--text)", fontWeight: 500 }}>
                          {r.ruleId}
                        </div>
                        {r.description && (
                          <div
                            style={{
                              color: "var(--text-mute)",
                              fontSize: 11.5,
                              whiteSpace: "normal",
                              maxWidth: 360,
                            }}
                          >
                            {r.description}
                          </div>
                        )}
                        {issues.length > 0 && (
                          <ul className="rule-issues">
                            {issues.map((msg, i) => (
                              <li key={i}>
                                <TriangleAlert size={10} /> {msg}
                              </li>
                            ))}
                          </ul>
                        )}
                        <details className="rule-details">
                          <summary>查看规则信息</summary>
                          <div className="rule-detail-copy">
                            <div>规则 ID：<span className="mono">{r.ruleId}</span></div>
                            <div>来源：{r.source ?? "未提供"} · 类别：{r.category ?? "未提供"}</div>
                            <div>风险：{r.risk ?? "未提供"} · 状态：{r.valid ? "有效" : "无效"}</div>
                            {r.issues.length > 0 && <div>问题：{r.issues.join("；")}</div>}
                            <div className="muted-inline">匹配条件未由当前只读规则接口返回；详情不猜测规则命中范围。</div>
                          </div>
                        </details>
                      </td>
                      <td className="mono">{r.source ?? "—"}</td>
                      <td>
                        {r.risk ? (
                          <RiskBadge risk={r.risk} />
                        ) : (
                          <span className="badge plain">—</span>
                        )}
                      </td>
                      <td className="mono">{r.category ?? "—"}</td>
                      <td>
                        {r.valid ? (
                          <span className="badge status-ok" title="结构与路径边界校验通过">
                            有效
                          </span>
                        ) : (
                          <span className="badge status-failed" title="加载时被拒绝">
                            无效
                          </span>
                        )}
                      </td>
                      <td>
                        {(r.source === "user" || r.source === "user-protected") ? (
                          <button
                            className="btn small ghost"
                            disabled={deletingRuleId !== null}
                            onClick={() => void deleteRule(r)}
                            title={r.source === "user-protected" ? "删除用户保护规则" : "删除用户规则"}
                          >
                            {deletingRuleId === r.ruleId ? "删除中…" : "删除"}
                          </button>
                        ) : (
                          <span className="badge plain">不可删除</span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {!loading && filtered.length === 0 && !error && (
              <div className="empty" style={{ padding: "40px 20px" }}>
                <p>没有符合此筛选的规则。</p>
              </div>
            )}
          </div>

          <p style={{ color: "var(--text-mute)", fontSize: 12, display: "flex", gap: 8 }}>
            <CheckCircle2 size={13} style={{ marginTop: 1, flex: "none" }} />
            用户规则可在此删除；内置受保护规则集永不可被覆盖（SPEC §14）。删除后请重新扫描以查看新的分类结果。
          </p>
        </div>
      </div>
    </>
  );
}
