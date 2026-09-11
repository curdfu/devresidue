import { useEffect, useMemo, useState } from "react";
import { CheckCircle2, RefreshCw, ShieldCheck, TriangleAlert } from "lucide-react";
import type { RuleDto, RulesValidationDto } from "@/types";
import { getBackend } from "@/adapters";
import { RiskBadge } from "@/components/common";
import { BACKEND_KIND } from "@/App";

/**
 * Rules registry. R11: loads the *actual* merged rule set from
 * the backend (`get_rules` + `validate_rules`) instead of a hardcoded demo
 * table. In mock mode a visible "demo data" badge marks that the table is
 * not the real machine's rule set (review requirement).
 */
export function RulesPage() {
  const [rules, setRules] = useState<RuleDto[]>([]);
  const [validation, setValidation] = useState<RulesValidationDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [deletingRuleId, setDeletingRuleId] = useState<string | null>(null);

  const reload = async () => {
    setLoading(true);
    setError(null);
    try {
      const backend = getBackend();
      const [ruleList, val] = await Promise.all([
        backend.getRules(),
        backend.validateRules(),
      ]);
      setRules(ruleList);
      setValidation(val);
    } catch (raw) {
      const message =
        typeof raw === "object" && raw !== null && "message" in raw
          ? String((raw as { message: unknown }).message)
          : String(raw);
      setError(message);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rules;
    return rules.filter((r) =>
      [r.ruleId, r.description ?? "", r.source ?? "", r.category ?? ""].some((s) =>
        s.toLowerCase().includes(q),
      ),
    );
  }, [rules, query]);

  const deleteRule = async (rule: RuleDto) => {
    const isProtected = rule.source === "user-protected";
    const message = isProtected
      ? `删除保护规则“${rule.ruleId}”后，下次扫描将不再由该规则保护对应位置。是否继续？`
      : `删除用户规则“${rule.ruleId}”？下次扫描将不再应用此分类。`;
    if (!window.confirm(message)) return;

    setDeletingRuleId(rule.ruleId);
    setError(null);
    try {
      await getBackend().deleteUserRule(rule.ruleId);
      await reload();
    } catch (raw) {
      const message =
        typeof raw === "object" && raw !== null && "message" in raw
          ? String((raw as { message: unknown }).message)
          : String(raw);
      setError(message);
    } finally {
      setDeletingRuleId(null);
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
      <div className="page-head">
        <div>
          <div className="page-title">规则</div>
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

          <input
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
