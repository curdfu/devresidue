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
import { RULES } from "@/data/rules";

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
  const meta = riskMeta(item.risk);
  const ruleIds = item.evidence
    .map((e) => e.rule_id)
    .filter((rule): rule is number => rule !== null);
  const rules = RULES.filter((rule) => ruleIds.includes(rule.id));

  return (
    <>
      <div className="detail-head">
        <div className="detail-title">
          <h2>{item.display_name}</h2>
          <div className="path">{item.path}</div>
        </div>
        <button
          className="detail-close"
          onClick={() => openDetail(null)}
          title="关闭详情"
          aria-label="关闭详情"
        >
          ✕
        </button>
      </div>

      <div className="detail-stats">
        <div className="stat">
          <div className="k">大小</div>
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
          {rules.length > 0 ? (
            <div className="qa-evidence">
              {rules.map((rule) => (
                <div key={rule.id} className="ev">
                  <span className="src">规则 {rule.id} · {rule.source}</span>
                  <span className="txt">{rule.slug}——{rule.description}</span>
                </div>
              ))}
            </div>
          ) : (
            <span className="a muted-inline">无规则命中——由 Provider 自身启发式分类。</span>
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
