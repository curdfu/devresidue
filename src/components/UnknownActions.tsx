import { useState } from "react";
import {
  FolderOpen,
  EyeOff,
  Lock,
  ShieldCheck,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import type { RiskLevel, ScanItemDto } from "@/types";
import { getBackend } from "@/adapters";
import { useUnknownWorkflowStore } from "@/stores/unknownWorkflowStore";
import { useAppSettingsStore } from "@/stores/appSettingsStore";
import { riskMeta } from "@/utils/format";
import { RiskBadge } from "./common";

/**
 * The Unknown item's disposition toolkit (SPEC §25): Open Folder / Ignore /
 * Protect / AI Analyze. Destructive-ish decisions route through a confirm
 * dialog that spells out what the choice means.
 *
 * Layout: action rail above the six-question block inside the Detail Panel
 * — actions live *with* the item context, not in table rows (keeps the
 * eight-column density intact and gives each action room to explain
 * itself).
 */
export function UnknownActions({ item }: { item: ScanItemDto }) {
  const analyzerEnabled = useAppSettingsStore((s) => s.analyzerEnabled);
  const analyze = useUnknownWorkflowStore((s) => s.analyze);
  const analyzing = useUnknownWorkflowStore((s) => s.analyzing.has(item.id));
  const applying = useUnknownWorkflowStore((s) => s.applying.has(item.id));

  const [confirming, setConfirming] = useState<null | "ignore" | "protect">(null);
  const [openErr, setOpenErr] = useState<string | null>(null);

  const openFolder = async () => {
    setOpenErr(null);
    try {
      await getBackend().openFolder(item.id);
    } catch (e) {
      setOpenErr(e instanceof Error ? e.message : "无法打开文件夹");
    }
  };

  const busy = analyzing || applying;

  return (
    <div className="unknown-actions">
      <div className="action-rail">
        <button className="btn small" onClick={() => void openFolder()} disabled={busy}>
          <FolderOpen size={12} /> 打开文件夹
        </button>
        <button
          className="btn small"
          onClick={() => setConfirming("ignore")}
          disabled={busy}
          title="之后的扫描不再报告此文件夹"
        >
          <EyeOff size={12} /> 忽略
        </button>
        <button
          className="btn small"
          onClick={() => setConfirming("protect")}
          disabled={busy}
          title="永不清理，始终受保护"
        >
          <ShieldCheck size={12} /> 保护
        </button>
        <button
          className="btn small"
          onClick={() => void analyze(item.id)}
          disabled={busy}
          title={
            analyzerEnabled
              ? "分析元数据，给出分类建议"
              : "请先在设置中启用 AI 分析"
          }
        >
          {analyzerEnabled ? <Sparkles size={12} /> : <Lock size={12} />}
          {analyzing ? "分析中…" : "AI 分析"}
        </button>
      </div>

      {openErr && <div className="unknown-err">{openErr}</div>}

      {!analyzerEnabled && (
        <p className="analyzer-hint">
          <Lock size={11} /> AI 分析未启用。分析仅提供建议——绝不删除数据；可在设置中开启。
        </p>
      )}

      {confirming && (
        <DispositionConfirm
          item={item}
          kind={confirming}
          onCancel={() => setConfirming(null)}
        />
      )}
    </div>
  );
}

function DispositionConfirm({
  item,
  kind,
  onCancel,
}: {
  item: ScanItemDto;
  /** UI state word; maps onto the wire disposition ("ignore"/"protect"). */
  kind: "ignore" | "protect";
  onCancel: () => void;
}) {
  const setDisposition = useUnknownWorkflowStore((s) => s.setDisposition);
  const applying = useUnknownWorkflowStore((s) => s.applying.has(item.id));
  const disposition = kind === "protect" ? "protect" : "ignore";

  return (
    <div className="confirm-note protected" style={{ flexDirection: "column", gap: 8 }}>
      <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
        {kind === "protect" ? (
          <ShieldCheck size={15} style={{ marginTop: 1, flex: "none" }} />
        ) : (
          <EyeOff size={15} style={{ marginTop: 1, flex: "none" }} />
        )}
        <span>
          {kind === "protect" ? (
            <>
              <b>将此文件夹设为受保护？</b>
              它会移入「受保护」类别——任何清理中 DevResidue 都不会把它列入删除计划。之后只有你能撤销此设置。
            </>
          ) : (
            <>
              <b>忽略此文件夹？</b>
              之后的扫描将不再报告它。不会删除任何数据——文件保持原位，只是不再出现在报告中。
            </>
          )}
        </span>
      </div>
      <div className="mono-path">{item.path}</div>
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", width: "100%" }}>
        <button className="btn small ghost" onClick={onCancel}>
          <X size={11} /> 取消
        </button>
        <button
          className={`btn small ${kind === "protect" ? "primary" : ""}`}
          disabled={applying}
          onClick={() => void setDisposition(item.id, disposition)}
        >
          {applying ? "应用中…" : kind === "protect" ? "设为受保护" : "确认忽略"}
        </button>
      </div>
    </div>
  );
}

/**
 * The analyzer's inline suggestion card. Visually a *proposal*, never a
 * fact: dashed border, italic "Suggested" markers, and an explicit accept
 * action. Accepting creates a user rule carrying the suggested risk
 * (SPEC §26 — the AI suggests, the user commits) via
 * `createRuleFromSuggestion(itemId, suggestedRisk)`.
 */
export function SuggestionCard({ item }: { item: ScanItemDto }) {
  const suggestion = useUnknownWorkflowStore((s) => s.suggestions.get(item.id));
  const accepting = useUnknownWorkflowStore((s) => s.applying.has(item.id));
  const createRule = useUnknownWorkflowStore((s) => s.createRuleFromSuggestion);
  const clear = useUnknownWorkflowStore((s) => s.clearSuggestion);
  const [confirming, setConfirming] = useState(false);

  if (!suggestion) return null;
  const risk = normalizeSuggestionRisk(suggestion.suggestedRisk);
  const meta = riskMeta(risk);
  const confidencePct = Math.round(suggestion.confidence * 100);

  return (
    <div className="suggestion-card">
      <div className="suggestion-head">
        <Sparkles size={13} />
        <span className="suggestion-title">AI 分析建议</span>
        <span className="suggested-tag">建议——尚未应用</span>
        <button className="detail-close" onClick={() => clear(item.id)} title="关闭">
          <X size={12} />
        </button>
      </div>

      <div className="suggestion-grid">
        <div className="sg">
          <div className="k">产品猜测</div>
          <div className="v">
            <i>{suggestion.productGuess ?? "—"}</i>
          </div>
        </div>
        <div className="sg">
          <div className="k">置信度</div>
          <div className="v">
            {confidencePct}%
            <i className="conf-bar">
              <i style={{ width: `${confidencePct}%` }} />
            </i>
          </div>
        </div>
        <div className="sg">
          <div className="k">建议分类</div>
          <div className="v">
            <RiskBadge risk={risk} />
          </div>
        </div>
        <div className="sg">
          <div className="k">类别</div>
          <div className="v">
            <i>{suggestion.category}</i>
          </div>
        </div>
      </div>

      <p className="suggestion-text">{suggestion.explanation}</p>

      <div className="suggestion-rule">
        <span className="k">将创建规则</span>
        <span className="mono">{suggestion.path}</span>
        <span className={`badge ${meta.className}`}>{meta.label}</span>
      </div>

      {confirming ? (
        <div className="confirm-note review" style={{ flexDirection: "column", gap: 8 }}>
          <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
            <Sparkles size={15} style={{ marginTop: 1, flex: "none" }} />
            <span>
              <b>创建此规则？</b>
              建议的风险级别 <b>{meta.label}</b> 将写入你的用户规则——之后的扫描会把此文件夹归为
              <b>{meta.label}</b>。之后可以随时修改或删除该规则。
            </span>
          </div>
          <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", width: "100%" }}>
            <button className="btn small ghost" onClick={() => setConfirming(false)}>
              <X size={11} /> 取消
            </button>
            <button
              className="btn small primary"
              disabled={accepting}
              onClick={() => void createRule(item.id)}
            >
              {accepting ? "创建规则中…" : "创建规则"}
            </button>
          </div>
        </div>
      ) : (
        <div className="suggestion-foot">
          <span className="hint">
            <Trash2 size={11} /> AI 分析绝不删除数据——采纳建议仅创建分类规则。
          </span>
          <button
            className="btn small primary"
            onClick={() => setConfirming(true)}
          >
            按建议创建规则
          </button>
        </div>
      )}
    </div>
  );
}

/** Clamps the wire risk label to the UI's RiskLevel vocabulary. */
function normalizeSuggestionRisk(risk: string): RiskLevel {
  const known: RiskLevel[] = [
    "safe",
    "regenerable-local",
    "regenerable-download",
    "review",
    "protected",
    "unknown",
  ];
  return (known as string[]).includes(risk) ? (risk as RiskLevel) : "unknown";
}
