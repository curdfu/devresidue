import { useEffect, useRef, useState } from "react";
import { CheckCircle2, KeyRound, LoaderCircle, Plus, RefreshCw, Trash2, Wifi } from "lucide-react";
import type { AiApiProtocol, AiProfileDto, AiProfileInput, StructuredOutputMode } from "@/types";
import { useAiStore } from "@/stores/aiStore";

const DEFAULT_FORM = {
  name: "",
  baseUrl: "https://api.openai.com/v1",
  model: "gpt-4.1-mini",
  apiProtocol: "openai-compatible" as AiApiProtocol,
  structuredOutput: "auto" as StructuredOutputMode,
  timeoutSecs: 45,
  enabled: true,
};

/**
 * Opt-in remote-AI profile management. The Key input below is deliberately
 * uncontrolled: it never enters React state, Zustand or localStorage.
 */
export function AiProfilePanel() {
  const masterEnabled = useAiStore((state) => state.masterEnabled);
  const profiles = useAiStore((state) => state.profiles);
  const loaded = useAiStore((state) => state.loaded);
  const profileBusy = useAiStore((state) => state.profileBusy);
  const connectionProfileId = useAiStore((state) => state.connectionProfileId);
  const modelProfileId = useAiStore((state) => state.modelProfileId);
  const modelsLoading = useAiStore((state) => state.modelsLoading);
  const availableModels = useAiStore((state) => state.availableModels);
  const error = useAiStore((state) => state.error);
  const loadProfiles = useAiStore((state) => state.loadProfiles);
  const setMasterEnabled = useAiStore((state) => state.setMasterEnabled);
  const setActiveProfile = useAiStore((state) => state.setActiveProfile);
  const upsertProfile = useAiStore((state) => state.upsertProfile);
  const deleteProfile = useAiStore((state) => state.deleteProfile);
  const testConnection = useAiStore((state) => state.testConnection);
  const loadModels = useAiStore((state) => state.loadModels);
  const dismissError = useAiStore((state) => state.dismissError);

  const [editing, setEditing] = useState<string | null>(null);
  const [form, setForm] = useState<AiProfileInput>({ profileId: null, ...DEFAULT_FORM });
  const [connectionNote, setConnectionNote] = useState<string | null>(null);
  const apiKeyRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    void loadProfiles();
  }, [loadProfiles]);

  const activeProfile = profiles.find((profile) => profile.isActive) ?? null;

  const beginCreate = () => {
    setEditing(null);
    setForm({ profileId: null, ...DEFAULT_FORM });
    setConnectionNote(null);
    if (apiKeyRef.current) apiKeyRef.current.value = "";
  };

  const beginEdit = (profile: AiProfileDto) => {
    setEditing(profile.id);
    setForm({
      profileId: profile.id,
      name: profile.name,
      baseUrl: profile.baseUrl,
      model: profile.model,
      apiProtocol: profile.apiProtocol,
      structuredOutput: profile.structuredOutput,
      timeoutSecs: 45,
      enabled: profile.enabled,
    });
    setConnectionNote(null);
    if (apiKeyRef.current) apiKeyRef.current.value = "";
  };

  const submit = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    dismissError();
    setConnectionNote(null);
    // This local value exists only through the bridge call. It is never put in
    // a state setter, URL, toast, error message or persistence payload.
    const apiKey = apiKeyRef.current?.value ?? "";
    try {
      const saved = await upsertProfile(form, apiKey);
      if (saved) {
        beginEdit(saved);
        setConnectionNote("配置已保存；密钥输入已清空。");
      }
    } finally {
      if (apiKeyRef.current) apiKeyRef.current.value = "";
    }
  };

  const test = async (profileId: string) => {
    dismissError();
    setConnectionNote(null);
    if (await testConnection(profileId)) setConnectionNote("连接检查通过；未发送扫描元数据。");
  };

  const fetchModels = async (profileId: string) => {
    dismissError();
    setConnectionNote(null);
    const models = await loadModels(profileId);
    if (models) {
      setConnectionNote(models.length > 0 ? `已获取 ${models.length} 个模型；可在下方“模型名称”中选择。` : "服务未返回可用模型 ID；可手动填写模型名称。");
    }
  };

  return (
    <section className="settings-card" aria-labelledby="remote-ai-heading">
      <h3 id="remote-ai-heading">联网 AI 研判</h3>
      <p>
        可选的 OpenAI-compatible 服务，仅在你确认后发送已展示的脱敏元数据。它不读取文件内容、
        不创建清理计划，也不能删除任何数据。
      </p>

      <div className="ai-advisor-toggle-row">
        <label className="toggle">
          <input
            type="checkbox"
            checked={masterEnabled}
            disabled={profileBusy}
            onChange={(event) => void setMasterEnabled(event.target.checked)}
          />
          <span className="toggle-track"><span className="toggle-thumb" /></span>
        </label>
        <div>
          <div style={{ color: "var(--text)", fontWeight: 500 }}>
            {masterEnabled ? "远程研判已开启" : "远程研判已关闭"}
          </div>
          <div style={{ color: "var(--text-mute)", fontSize: 11.5 }}>
            这是联网 AI 研判的总开关
          </div>
        </div>
      </div>

      <p style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
        <KeyRound size={13} style={{ marginTop: 2, flex: "none", color: "var(--risk-review)" }} />
        <span>
          Key 仅在提交配置时写入当前用户的系统环境变量；不会返回到界面、不会存入浏览器存储，
          同一 Windows 用户下的进程可读取该环境变量。
        </span>
      </p>

      {error && (
        <div className="unknown-err" role="alert" style={{ marginBottom: 10 }}>
          {error.message}
        </div>
      )}
      {connectionNote && (
        <div className="confirm-note review" style={{ marginBottom: 10 }}>
          <CheckCircle2 size={14} /> {connectionNote}
        </div>
      )}

      <div style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr) auto", gap: 8, marginBottom: 12 }}>
        <label style={{ display: "grid", gap: 4, color: "var(--text-mute)", fontSize: 12 }}>
          活动配置
          <select
            value={activeProfile?.id ?? ""}
            disabled={profileBusy || profiles.length === 0}
            onChange={(event) => void setActiveProfile(event.target.value || null)}
          >
            <option value="">未选择</option>
            {profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>
                {profile.name}{profile.enabled ? "" : "（已停用）"}
              </option>
            ))}
          </select>
        </label>
        <button className="btn small" type="button" onClick={beginCreate} disabled={profileBusy}>
          <Plus size={12} /> 新建
        </button>
      </div>

      {!loaded ? (
        <div className="ai-advisor-hint"><LoaderCircle size={12} className="spin" /> 正在读取配置…</div>
      ) : profiles.length === 0 ? (
        <div className="ai-advisor-hint">尚无远程 AI 配置。新建时将要求输入一次 Key。</div>
      ) : (
        <div style={{ display: "grid", gap: 6, marginBottom: 12 }}>
          {profiles.map((profile) => (
            <div key={profile.id} className="root-row" style={{ alignItems: "center", gap: 8 }}>
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ color: "var(--text)" }}>
                  {profile.name}{profile.isActive ? " · 活动" : ""}{!profile.enabled ? " · 已停用" : ""}
                </div>
                <div className="mono" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                  {profile.model} · {profile.baseUrl}
                </div>
              </div>
              <button className="btn small ghost" type="button" onClick={() => beginEdit(profile)} disabled={profileBusy}>
                编辑
              </button>
              <button
                className="btn small ghost"
                type="button"
                title="仅测试认证和 /models 连通性，不发送扫描数据"
                onClick={() => void test(profile.id)}
                disabled={profileBusy || !masterEnabled || !profile.enabled || connectionProfileId !== null}
              >
                {connectionProfileId === profile.id ? <LoaderCircle size={12} className="spin" /> : <Wifi size={12} />}
                测试
              </button>
              <button
                className="btn small ghost"
                type="button"
                title="从此配置的 /models 获取模型列表；不发送扫描数据，远程研判总开关关闭时也可使用"
                onClick={() => {
                  beginEdit(profile);
                  void fetchModels(profile.id);
                }}
                disabled={profileBusy || !profile.enabled || modelsLoading}
              >
                {modelsLoading && modelProfileId === profile.id ? <LoaderCircle size={12} className="spin" /> : <RefreshCw size={12} />}
                获取模型
              </button>
              <button
                className="btn small ghost"
                type="button"
                title="删除此配置与对应用户环境变量中的 Key"
                onClick={() => void deleteProfile(profile.id)}
                disabled={profileBusy}
              >
                <Trash2 size={12} /> 删除
              </button>
            </div>
          ))}
        </div>
      )}

      <form onSubmit={(event) => void submit(event)} style={{ display: "grid", gap: 8 }}>
        <div style={{ color: "var(--text)", fontWeight: 500 }}>
          {editing ? "编辑配置" : "新建配置"}
        </div>
        <div className="root-add" style={{ display: "grid", gridTemplateColumns: "minmax(10rem, .8fr) minmax(14rem, 1.2fr)", gap: 8 }}>
          <input
            value={form.name}
            placeholder="配置名称"
            onChange={(event) => setForm((current) => ({ ...current, name: event.target.value }))}
            required
          />
          <input
            value={form.baseUrl}
            placeholder="https://api.example.com/v1"
            inputMode="url"
            onChange={(event) => setForm((current) => ({ ...current, baseUrl: event.target.value }))}
            required
          />
          <input
            value={form.model}
            placeholder="模型名称"
            list={editing && modelProfileId === editing ? "remote-ai-model-options" : undefined}
            onChange={(event) => setForm((current) => ({ ...current, model: event.target.value }))}
            required
          />
          {editing && modelProfileId === editing && (
            <datalist id="remote-ai-model-options">
              {availableModels.map((model) => <option key={model} value={model} />)}
            </datalist>
          )}
          <input
            ref={apiKeyRef}
            type="password"
            name="remote-ai-api-key"
            autoComplete="new-password"
            placeholder={editing ? "重新输入 Key 以更新配置" : "API Key（仅本次提交）"}
            required
          />
        </div>
        <div className="ai-advisor-hint">
          为保护 API Key，提交后无论成功或失败都会清空输入；保存失败时请修正配置后重新输入。
        </div>
        <div style={{ display: "flex", flexWrap: "wrap", gap: 10, alignItems: "center" }}>
          <label style={{ color: "var(--text-mute)", fontSize: 12 }}>
            超时（秒）
            <input
              style={{ width: 76, marginLeft: 6 }}
              type="number"
              min={1}
              max={600}
              value={form.timeoutSecs}
              onChange={(event) =>
                setForm((current) => ({ ...current, timeoutSecs: Number(event.target.value) || 1 }))
              }
            />
          </label>
          <label style={{ color: "var(--text-mute)", fontSize: 12 }}>
            接口模式
            <select
              style={{ marginLeft: 6 }}
              value={form.apiProtocol}
              onChange={(event) =>
                setForm((current) => ({ ...current, apiProtocol: event.target.value as AiApiProtocol }))
              }
            >
              <option value="openai-compatible">OpenAI Compatible</option>
              <option value="openai-responses">OpenAI Responses</option>
            </select>
          </label>
          <label style={{ color: "var(--text-mute)", fontSize: 12 }}>
            结构化输出
            <select
              style={{ marginLeft: 6 }}
              value={form.structuredOutput}
              onChange={(event) =>
                setForm((current) => ({
                  ...current,
                  structuredOutput: event.target.value as StructuredOutputMode,
                }))
              }
            >
              <option value="auto">自动</option>
              <option value="json-schema">JSON Schema</option>
              <option value="json-object">JSON Object</option>
            </select>
          </label>
          <label style={{ color: "var(--text-mute)", fontSize: 12 }}>
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(event) => setForm((current) => ({ ...current, enabled: event.target.checked }))}
            />{" "}
            此配置可用
          </label>
          <button className="btn small primary" type="submit" disabled={profileBusy}>
            {profileBusy ? <LoaderCircle size={12} className="spin" /> : <RefreshCw size={12} />}
            保存配置
          </button>
          {editing && (
            <button
              className="btn small ghost"
              type="button"
              onClick={() => void fetchModels(editing)}
              disabled={profileBusy || modelsLoading}
              title="仅请求 /models，不发送扫描数据"
            >
              {modelsLoading && modelProfileId === editing ? <LoaderCircle size={12} className="spin" /> : <RefreshCw size={12} />}
              获取模型列表
            </button>
          )}
          {editing && (
            <button className="btn small ghost" type="button" onClick={beginCreate} disabled={profileBusy}>
              取消编辑
            </button>
          )}
        </div>
      </form>
    </section>
  );
}
