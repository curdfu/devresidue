import { useEffect, useMemo, useState } from "react";
import { AlertTriangle, CheckCircle2, ChevronLeft, ChevronRight, LoaderCircle, Send, ShieldCheck, Sparkles, X } from "lucide-react";
import type {
  AiConfirmResultDto,
  AiFinalCategory,
  AiFinalRisk,
  AiPreparedBatchDto,
  AiProfileDto,
  AiSuggestionDto,
  ScanItemDto,
} from "@/types";
import { AI_FINAL_CATEGORY_OPTIONS, AI_FINAL_RISK_LEVELS } from "@/types";
import { useAiStore } from "@/stores/aiStore";
import { categoryLabel, formatBytes, riskMeta } from "@/utils/format";
import { RiskBadge } from "./common";

type Props = {
  items: ScanItemDto[];
  generation: number;
};

/**
 * Two-step remote-AI flow: prepare a strictly sanitized preview, then ask for
 * explicit metadata-send consent. Suggestions stay local until the user
 * chooses a concrete final risk and confirms selected IDs.
 */
export function AiReviewPanel({ items, generation }: Props) {
  const masterEnabled = useAiStore((state) => state.masterEnabled);
  const profiles = useAiStore((state) => state.profiles);
  const loaded = useAiStore((state) => state.loaded);
  const preparedBatch = useAiStore((state) => state.preparedBatch);
  const suggestions = useAiStore((state) => state.suggestions);
  const finalRisks = useAiStore((state) => state.finalRisks);
  const finalCategories = useAiStore((state) => state.finalCategories);
  const selectedSuggestionIds = useAiStore((state) => state.selectedSuggestionIds);
  const preparing = useAiStore((state) => state.preparing);
  const analyzing = useAiStore((state) => state.analyzing);
  const confirming = useAiStore((state) => state.confirming);
  const confirmationResult = useAiStore((state) => state.confirmationResult);
  const error = useAiStore((state) => state.error);
  const loadProfiles = useAiStore((state) => state.loadProfiles);
  const prepareBatch = useAiStore((state) => state.prepareBatch);
  const analyzePreparedBatch = useAiStore((state) => state.analyzePreparedBatch);
  const setFinalRisk = useAiStore((state) => state.setFinalRisk);
  const setFinalCategory = useAiStore((state) => state.setFinalCategory);
  const toggleSuggestion = useAiStore((state) => state.toggleSuggestion);
  const confirmSelected = useAiStore((state) => state.confirmSelected);
  const cancelAnalysis = useAiStore((state) => state.cancelAnalysis);
  const discardPreparedBatch = useAiStore((state) => state.discardPreparedBatch);
  const dismissError = useAiStore((state) => state.dismissError);

  const [selectedCandidateIds, setSelectedCandidateIds] = useState<Set<number>>(new Set());
  const [confirmLowRisk, setConfirmLowRisk] = useState(false);
  const [includePaths, setIncludePaths] = useState(false);

  useEffect(() => {
    void loadProfiles();
  }, [loadProfiles]);

  useEffect(() => {
    setSelectedCandidateIds(new Set());
    setConfirmLowRisk(false);
    setIncludePaths(false);
  }, [generation]);

  const candidates = useMemo(
    () => items.filter((item) => item.risk === "unknown" || item.risk === "review"),
    [items],
  );
  const byId = useMemo(() => new Map(items.map((item) => [item.id, item])), [items]);
  const activeProfile = profiles.find((profile) => profile.isActive) ?? null;
  const selectedCandidateCount = candidates.filter((item) => selectedCandidateIds.has(item.id)).length;
  const allCandidatesSelected = candidates.length > 0 && selectedCandidateCount === candidates.length;
  const batchProfile = preparedBatch
    ? profiles.find((profile) => profile.id === preparedBatch.profileId) ?? null
    : activeProfile;

  const toggleCandidate = (itemId: number) => {
    setSelectedCandidateIds((current) => {
      const next = new Set(current);
      if (next.has(itemId)) next.delete(itemId);
      else next.add(itemId);
      return next;
    });
  };

  const toggleAllCandidates = () => {
    setSelectedCandidateIds((current) => {
      if (candidates.length > 0 && candidates.every((item) => current.has(item.id))) return new Set();
      return new Set(candidates.map((item) => item.id));
    });
  };

  const requestConfirmation = () => {
    const selectedLowRisk = [...selectedSuggestionIds].some((itemId) => {
      const risk = finalRisks.get(itemId);
      return risk === "safe" || risk === "regenerable-local";
    });
    if (selectedLowRisk) setConfirmLowRisk(true);
    else void confirmSelected();
  };

  return (
    <section className="settings-card ai-review-panel" aria-labelledby="ai-review-heading">
      <div style={{ display: "flex", alignItems: "flex-start", gap: 10 }}>
        <Sparkles size={17} style={{ marginTop: 2, color: "var(--accent)" }} />
        <div>
          <h3 id="ai-review-heading" style={{ marginBottom: 3 }}>联网 AI 批次研判</h3>
          <p style={{ marginBottom: 0 }}>
            仅可选择 Unknown 或 Review 条目。远程服务只会在下一步收到你确认的脱敏元数据；
            建议不会直接创建计划或删除数据。
          </p>
        </div>
      </div>

      {error && (
        <div className="unknown-err" role="alert" style={{ marginTop: 10 }}>
          {error.message}
          <button className="btn small ghost" style={{ marginLeft: 8 }} onClick={dismissError}>
            <X size={11} /> 关闭
          </button>
        </div>
      )}

      {confirmationResult && <ConfirmationNotice result={confirmationResult} />}

      {!loaded ? (
        <p className="ai-advisor-hint"><LoaderCircle size={12} className="spin" /> 正在读取远程 AI 配置…</p>
      ) : !masterEnabled ? (
        <p className="ai-advisor-hint"><ShieldCheck size={12} /> 远程研判总开关处于关闭状态；可在“设置”中启用。</p>
      ) : !activeProfile || !activeProfile.enabled ? (
        <p className="ai-advisor-hint"><ShieldCheck size={12} /> 请先在“设置”中创建并启用一个活动的远程 AI 配置。</p>
      ) : preparedBatch ? (
        <ReviewStage
          batchProfile={batchProfile}
          preparedBatch={preparedBatch}
          suggestions={suggestions}
          finalRisks={finalRisks}
          finalCategories={finalCategories}
          selectedSuggestionIds={selectedSuggestionIds}
          analyzing={analyzing}
          confirming={confirming}
          byId={byId}
          setFinalRisk={setFinalRisk}
          setFinalCategory={setFinalCategory}
          toggleSuggestion={toggleSuggestion}
          onAnalyze={() => void analyzePreparedBatch()}
          onCancel={() => void cancelAnalysis()}
          onBack={() => void discardPreparedBatch()}
          onRequestConfirmation={requestConfirmation}
          lowRiskWarningVisible={confirmLowRisk}
          closeLowRisk={() => setConfirmLowRisk(false)}
          onConfirmLowRisk={() => {
            setConfirmLowRisk(false);
            void confirmSelected();
          }}
        />
      ) : (
        <CandidateStage
          candidates={candidates}
          selectedCandidateIds={selectedCandidateIds}
          toggleCandidate={toggleCandidate}
          allCandidatesSelected={allCandidatesSelected}
          selectedCandidateCount={selectedCandidateCount}
          toggleAllCandidates={toggleAllCandidates}
          includePaths={includePaths}
          setIncludePaths={setIncludePaths}
          activeProfile={activeProfile}
          preparing={preparing}
          onPrepare={() => void prepareBatch(
            activeProfile.id,
            generation,
            candidates.filter((item) => selectedCandidateIds.has(item.id)).map((item) => item.id),
            includePaths,
          )}
        />
      )}
    </section>
  );
}

function CandidateStage({
  candidates,
  selectedCandidateIds,
  toggleCandidate,
  allCandidatesSelected,
  selectedCandidateCount,
  toggleAllCandidates,
  includePaths,
  setIncludePaths,
  activeProfile,
  preparing,
  onPrepare,
}: {
  candidates: ScanItemDto[];
  selectedCandidateIds: Set<number>;
  toggleCandidate: (itemId: number) => void;
  allCandidatesSelected: boolean;
  selectedCandidateCount: number;
  toggleAllCandidates: () => void;
  includePaths: boolean;
  setIncludePaths: (include: boolean) => void;
  activeProfile: AiProfileDto;
  preparing: boolean;
  onPrepare: () => void;
}) {
  return (
    <div style={{ marginTop: 12 }}>
      <div className="ai-advisor-hint" style={{ marginBottom: 8 }}>
        当前配置：<b>{activeProfile.name}</b> · {activeProfile.model} · {activeProfile.baseUrl}
      </div>
      {candidates.length === 0 ? (
        <p className="ai-advisor-hint">当前扫描没有 Unknown 或 Review 条目可提交研判。</p>
      ) : (
        <>
          <label className="ai-advisor-hint" style={{ display: "flex", alignItems: "center", gap: 7, marginBottom: 7, cursor: "pointer" }}>
            <input type="checkbox" checked={allCandidatesSelected} onChange={toggleAllCandidates} disabled={preparing} />
            {allCandidatesSelected ? "取消全选" : "全选当前可研判条目"}（{candidates.length}）
          </label>
          <div style={{ display: "grid", gap: 5, marginBottom: 10 }}>
            {candidates.map((item) => (
              <label key={item.id} className="root-row" style={{ cursor: "pointer", gap: 8 }}>
                <input
                  type="checkbox"
                  checked={selectedCandidateIds.has(item.id)}
                  onChange={() => toggleCandidate(item.id)}
                />
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ color: "var(--text)" }}>{item.display_name}</div>
                  <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
                    {item.product ?? "未识别产品"} · {formatBytes(item.logical_size)} · <RiskBadge risk={item.risk} />
                  </div>
                </div>
              </label>
            ))}
          </div>
          <label className="confirm-note review" style={{ display: "flex", alignItems: "flex-start", gap: 8, marginBottom: 10, cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={includePaths}
              disabled={preparing}
              onChange={(event) => setIncludePaths(event.target.checked)}
            />
            <span>
              同意本批次同时发送完整本地路径。路径可能包含用户名、项目名或敏感目录名；仍不发送文件内容、API Key、凭据或原始证据。
            </span>
          </label>
          <button type="button" className="btn small primary" disabled={preparing || selectedCandidateCount === 0} onClick={onPrepare}>
            {preparing ? <LoaderCircle size={12} className="spin" /> : <ChevronRight size={12} />}
            查看待发送的{includePaths ? "数据与路径" : "脱敏元数据"}（{selectedCandidateCount}）
          </button>
        </>
      )}
    </div>
  );
}

function ReviewStage({
  batchProfile,
  preparedBatch,
  suggestions,
  finalRisks,
  finalCategories,
  selectedSuggestionIds,
  analyzing,
  confirming,
  byId,
  setFinalRisk,
  setFinalCategory,
  toggleSuggestion,
  onAnalyze,
  onCancel,
  onBack,
  onRequestConfirmation,
  lowRiskWarningVisible,
  closeLowRisk,
  onConfirmLowRisk,
}: {
  batchProfile: AiProfileDto | null;
  preparedBatch: AiPreparedBatchDto;
  suggestions: AiSuggestionDto[];
  finalRisks: Map<number, AiFinalRisk>;
  finalCategories: Map<number, AiFinalCategory>;
  selectedSuggestionIds: Set<number>;
  analyzing: boolean;
  confirming: boolean;
  byId: Map<number, ScanItemDto>;
  setFinalRisk: (itemId: number, risk: AiFinalRisk) => void;
  setFinalCategory: (itemId: number, category: AiFinalCategory) => void;
  toggleSuggestion: (itemId: number) => void;
  onAnalyze: () => void;
  onCancel: () => void;
  onBack: () => void;
  onRequestConfirmation: () => void;
  lowRiskWarningVisible: boolean;
  closeLowRisk: () => void;
  onConfirmLowRisk: () => void;
}) {
  if (suggestions.length === 0) {
    return (
      <div style={{ marginTop: 12 }}>
        <div className="ai-advisor-hint" style={{ marginBottom: 8 }}>
          配置：<b>{batchProfile?.name ?? "已删除的配置"}</b> · {batchProfile?.model ?? "—"} · {batchProfile?.baseUrl ?? "—"}
        </div>
        <p style={{ color: "var(--text-mute)", fontSize: 12 }}>
          {preparedBatch.includesPaths
            ? "以下字段及每项完整本地路径就是将发送的全部内容：不含文件内容、API Key、凭据或原始证据。"
            : "以下字段就是将发送的全部内容：不含本地路径、文件内容、API Key、凭据或原始证据。"}
        </p>
        <div style={{ display: "grid", gap: 6, marginBottom: 10 }}>
          {preparedBatch.entries.map((entry) => (
            <div key={entry.itemId} className="root-row" style={{ alignItems: "flex-start" }}>
              <div>
                <div style={{ color: "var(--text)" }}>{entry.displayName}</div>
                <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
                  {entry.zone} · 深度 {entry.relativeDepth} · {entry.sourceKind} · {entry.categoryHint}
                </div>
                <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
                  产品：{entry.productHint ?? "—"} · 大小：{entry.sizeBucket} · 时间：{entry.ageBucket}
                </div>
                <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>信号：{entry.signals.join("、")}</div>
                {preparedBatch.includesPaths && (
                  <div className="mono" style={{ color: "var(--text-mute)", fontSize: 11.5, overflowWrap: "anywhere" }}>
                    路径：{byId.get(entry.itemId)?.path ?? "当前扫描条目不可用"}
                  </div>
                )}
              </div>
            </div>
          ))}
        </div>
        <div className="confirm-note review" style={{ marginBottom: 10 }}>
          <AlertTriangle size={14} /> 我确认仅向选定服务发送以上脱敏元数据，用于获得非权威分类建议。
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="btn small ghost" disabled={analyzing} onClick={onBack}>
            <ChevronLeft size={12} /> 返回重选
          </button>
          <button type="button" className="btn small primary" disabled={analyzing} onClick={onAnalyze}>
            {analyzing ? <LoaderCircle size={12} className="spin" /> : <Send size={12} />}
            发送脱敏元数据并分析
          </button>
          {analyzing && <button type="button" className="btn small ghost" onClick={onCancel}>取消分析</button>}
        </div>
      </div>
    );
  }

  return (
    <div style={{ marginTop: 12 }}>
      <p className="ai-advisor-hint">
        以下是非权威建议。每行必须选择最终风险和最终类别后才能勾选；默认均不勾选。
      </p>
      <div
        className="ai-review-suggestion-list"
        role="region"
        aria-label="联网 AI 建议列表"
      >
        {suggestions.map((suggestion) => {
          const item = byId.get(suggestion.itemId);
          const finalRisk = finalRisks.get(suggestion.itemId);
          const finalCategory = finalCategories.get(suggestion.itemId);
          const checked = selectedSuggestionIds.has(suggestion.itemId);
          return (
            <div key={suggestion.itemId} className="root-row" style={{ alignItems: "flex-start", gap: 9 }}>
              <input
                type="checkbox"
                checked={checked}
                disabled={!finalRisk || !finalCategory || confirming}
                onChange={() => toggleSuggestion(suggestion.itemId)}
                aria-label={`确认 ${item?.display_name ?? `条目 ${suggestion.itemId}`}`}
              />
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ color: "var(--text)" }}>{item?.display_name ?? `条目 ${suggestion.itemId}`}</div>
                <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
                  {item ? `${productIdentification(item, suggestion)} · ${formatBytes(item.logical_size)}` : "本地条目摘要不可用"}
                </div>
                <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
                  建议风险：<RiskBadge risk={suggestion.suggestedRisk} /> · 置信度 {Math.round(suggestion.confidence * 100)}%
                </div>
                <p style={{ margin: "5px 0 0", color: "var(--text-mute)", fontSize: 12 }}>{suggestion.reason}</p>
              </div>
              <div style={{ display: "grid", gap: 7, minWidth: 142 }}>
                <label style={{ display: "grid", gap: 4, color: "var(--text-mute)", fontSize: 11.5 }}>
                  最终风险
                  <select
                    value={finalRisk ?? ""}
                    disabled={confirming}
                    onChange={(event) => {
                      if (isAiFinalRisk(event.target.value)) setFinalRisk(suggestion.itemId, event.target.value);
                    }}
                  >
                    <option value="">请选择</option>
                    {suggestion.finalRiskOptions.map((risk) => (
                      <option key={risk} value={risk}>{riskMeta(risk).label}</option>
                    ))}
                  </select>
                </label>
                <label style={{ display: "grid", gap: 4, color: "var(--text-mute)", fontSize: 11.5 }}>
                  最终类别
                  <select
                    value={finalCategory ?? ""}
                    disabled={confirming}
                    onChange={(event) => {
                      if (isAiFinalCategory(event.target.value)) setFinalCategory(suggestion.itemId, event.target.value);
                    }}
                  >
                    <option value="">请选择</option>
                    {AI_FINAL_CATEGORY_OPTIONS.map((category) => (
                      <option key={category} value={category}>{categoryLabel(category)}</option>
                    ))}
                  </select>
                </label>
              </div>
            </div>
          );
        })}
      </div>
      <button className="btn small primary" disabled={confirming || selectedSuggestionIds.size === 0} onClick={onRequestConfirmation}>
        {confirming ? <LoaderCircle size={12} className="spin" /> : <CheckCircle2 size={12} />}
        确认选中的本地分类规则（{selectedSuggestionIds.size}）
      </button>

      {lowRiskWarningVisible && (
        <div className="confirm-note protected" style={{ marginTop: 10, flexDirection: "column", alignItems: "flex-start", gap: 8 }}>
          <div style={{ display: "flex", gap: 8 }}>
            <AlertTriangle size={15} style={{ flex: "none", marginTop: 2 }} />
            <span>
              <b>确认低风险分类？</b> 后续 <code>clean --safe</code> 可能会选中标为“安全”或“本地可重建”的条目，
              但仍必须经过清理计划、人工确认和 TOCTOU 重新验证；此操作本身不会删除数据。
            </span>
          </div>
          <div style={{ display: "flex", gap: 8, alignSelf: "flex-end" }}>
            <button className="btn small ghost" onClick={closeLowRisk}>返回检查</button>
            <button className="btn small primary" onClick={onConfirmLowRisk}>确认创建本地规则</button>
          </div>
        </div>
      )}
    </div>
  );
}

function ConfirmationNotice({ result }: { result: AiConfirmResultDto }) {
  return (
    <div className="confirm-note review" style={{ marginTop: 10, flexDirection: "column", alignItems: "flex-start" }}>
      <div><CheckCircle2 size={15} /> 已确认 {result.confirmedCount} 条本地分类规则。</div>
      <span>这不会创建清理计划或删除数据；后续仍需经过既有的计划、确认与 TOCTOU 复核。</span>
      {result.auditWarning && <span>审计记录不可用，但规则事务已完成。</span>}
    </div>
  );
}

function isAiFinalRisk(value: string): value is AiFinalRisk {
  return (AI_FINAL_RISK_LEVELS as readonly string[]).includes(value);
}

function isAiFinalCategory(value: string): value is AiFinalCategory {
  return (AI_FINAL_CATEGORY_OPTIONS as readonly string[]).includes(value);
}

function productIdentification(item: ScanItemDto, suggestion: AiSuggestionDto): string {
  if (suggestion.productGuess) {
    return item.product
      ? `产品识别：${suggestion.productGuess}（AI 研判；本地：${item.product}）`
      : `产品识别：${suggestion.productGuess}（AI 研判）`;
  }

  return item.product ? `产品识别：${item.product}（本地识别）` : "产品识别：未识别";
}
