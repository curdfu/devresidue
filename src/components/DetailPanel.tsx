import {
  BadgeCheck,
  Box,
  Clock,
  Trash2,
  User,
  Info,
  ShieldCheck,
  Sparkles,
} from "lucide-react";
import { useEffect, useRef } from "react";
import type { ScanItemDto } from "@/types";
import { useUiStore } from "@/stores/uiStore";
import {
  categoryLabel,
  deletionImpact,
  formatAge,
  formatBytes,
  formatCount,
  riskMeta,
  sourceLabel,
} from "@/utils/format";
import { RiskBadge } from "./common";
import { UnknownActions } from "./UnknownActions";
import { useRulesStore } from "@/stores/rulesStore";

/**
 * Detail evidence is deliberately ordered by the decision a person needs to
 * make: impact and classification first, provenance and rules afterwards.
 * Unknown items keep their separate, human-confirmed disposition actions.
 */
export function DetailPanel({
  item,
  unknownMode = false,
}: {
  item: ScanItemDto;
  unknownMode?: boolean;
}) {
  const openDetail = useUiStore((s) => s.openDetail);
  const rules = useRulesStore((s) => s.rules);
  const loadRules = useRulesStore((s) => s.load);
  const closeRef = useRef<HTMLButtonElement>(null);
  const meta = riskMeta(item.risk);
  const classificationRule = item.classification_rule_id
    ? rules.find((rule) => rule.ruleId === item.classification_rule_id) ?? null
    : null;
  const evidenceRuleIds = item.evidence
    .map((e) => e.rule_id)
    .filter((rule): rule is number => rule !== null);

  useEffect(() => {
    if (rules.length === 0) void loadRules();
  }, [loadRules, rules.length]);

  useEffect(() => {
    closeRef.current?.focus({ preventScroll: true });
  }, [item.id]);

  const close = () => {
    openDetail(null);
    // Return keyboard focus to the row that opened this detail. The row may
    // have been removed by a filter or a new snapshot, so failing closed is
    // intentional; the page remains usable and the next heading is available.
    requestAnimationFrame(() => {
      document
        .querySelector<HTMLElement>(`[data-detail-trigger="${item.id}"]`)
        ?.focus({ preventScroll: true });
    });
  };

  return (
    <>
      <div className="detail-head">
        <div className="detail-title">
          <h2>{item.display_name}</h2>
          <div className="path">{item.path}</div>
        </div>
        <button
          ref={closeRef}
          type="button"
          className="detail-close"
          onClick={close}
          title="关闭详情"
          aria-label="关闭详情"
        >
          ✕
        </button>
      </div>

      <div className="detail-stats">
        <div className="stat">
          <div className="k">逻辑大小估计</div>
          <div className="v">{formatBytes(item.logical_size)}</div>
        </div>
        <div className="stat">
          <div className="k">文件数</div>
          <div className="v">{formatCount(item.file_count)}</div>
        </div>
        <div className="stat">
          <div className="k">最近修改</div>
          <div className="v detail-age">{formatAge(item.last_modified)}</div>
        </div>
      </div>

      {unknownMode && <UnknownActions item={item} />}

      <div className="qa">
        <Qa icon={<Trash2 size={14} />} q="删除会有什么影响">
          <span className="a impact-copy" style={{ color: deletionColor(item.risk) }}>
            {unknownMode && item.risk === "unknown"
              ? "未知数据不会加入普通清理计划；任何处置都需要人工确认。"
              : deletionImpact(item.risk, item.logical_size)}
          </span>
        </Qa>

        <Qa icon={<Info size={14} />} q="为什么这么分类">
          <div className="a classification-copy">
            <RiskBadge risk={item.risk} />
            <span>{meta.hint}</span>
          </div>
        </Qa>

        <Qa icon={<Box size={14} />} q="这是什么">
          <span className="a">
            {categoryLabel(item.category)}——{item.explanation.split(/(?<=[.。;；])\s*/)[0]}
          </span>
        </Qa>

        <Qa icon={<User size={14} />} q="属于谁">
          <span className="a">{item.product ?? "未知产品"}</span>
        </Qa>

        {item.cleanup_action.kind === "external-command" && (
          <Qa icon={<ShieldCheck size={14} />} q="将如何清理">
            <span className="a mono">
              {item.cleanup_action.command.executable} {item.cleanup_action.command.args.join(" ")}
              {item.cleanup_action.command.timeout_secs !== null && (
                <span className="muted-inline">（超时 {item.cleanup_action.command.timeout_secs} 秒）</span>
              )}
            </span>
          </Qa>
        )}
        {item.cleanup_action.kind === "defer" && (
          <Qa icon={<Clock size={14} />} q="已暂缓">
            <span className="a" style={{ color: "var(--risk-review)" }}>
              {item.cleanup_action.reason}
            </span>
          </Qa>
        )}

        <Qa icon={<RadarGlyph />} q="由谁检测">
          <span className="a">{sourceLabel(item.source)}</span>
          <div className="qa-evidence">
            {item.evidence.map((evidence, index) => (
              <div key={index} className="ev">
                <span className="src">{evidence.source}</span>
                <span className="txt">{evidence.detail}</span>
              </div>
            ))}
          </div>
        </Qa>

        <Qa icon={<Sparkles size={14} />} q="对应哪条规则">
          {classificationRule ? (
            <div className="qa-evidence">
              <div className="ev">
                <span className="src">{classificationRule.ruleId} · {classificationRule.source ?? "来源未知"}</span>
                <span className="txt">{classificationRule.description ?? "未提供规则描述"}</span>
              </div>
            </div>
          ) : item.classification_rule_id ? (
            <div className="qa-evidence">
              <div className="ev">
                <span className="src">{item.classification_rule_id}</span>
                <span className="txt">扫描结果提供了规则 ID，但当前规则列表未返回该规则，不能伪造关联。</span>
              </div>
            </div>
          ) : evidenceRuleIds.length > 0 ? (
            <div className="qa-evidence">
              <div className="ev">
                <span className="src">扫描证据 RuleId：{evidenceRuleIds.join(", ")}</span>
                <span className="txt">这是数字证据标识，未提供可与规则 slug 精确匹配的关联。</span>
              </div>
            </div>
          ) : (
            <span className="a muted-inline">未提供可跳转规则——可能由 Provider 自身启发式分类。</span>
          )}
        </Qa>
      </div>
    </>
  );
}

function Qa({
  icon,
  q,
  children,
}: {
  icon: React.ReactNode;
  q: string;
  children: React.ReactNode;
}) {
  return (
    <div className="qa-item">
      <span className="icon">{icon}</span>
      <div className="q">{q}</div>
      {children}
    </div>
  );
}

function RadarGlyph() {
  return <BadgeCheck size={14} />;
}

function deletionColor(risk: ScanItemDto["risk"]): string {
  switch (risk) {
    case "safe":
      return "var(--risk-safe)";
    case "regenerable-local":
      return "var(--risk-local)";
    case "regenerable-download":
      return "var(--risk-download)";
    case "review":
      return "var(--risk-review)";
    case "protected":
      return "var(--risk-protected)";
    case "unknown":
      return "var(--risk-unknown)";
  }
}
