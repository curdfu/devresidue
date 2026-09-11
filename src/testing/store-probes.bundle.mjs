// src/stores/scanStore.ts
import { create } from "zustand";

// probe:@/adapters
function getBackend() {
  return globalThis.__probeBackend;
}

// probe:@/adapters/events
function subscribeScan(handlers) {
  globalThis.__probeHandlers = handlers;
  return { ready: Promise.resolve(), close: () => {
  } };
}

// src/stores/scanStore.ts
var subscription = null;
var epochCounter = 0;
function snapshotKey(snap) {
  return snap.generation > 0 ? snap.generation : snap.scanned_at;
}
var useScanStore = create((set, get) => ({
  phase: "idle",
  scanId: null,
  scope: { kind: "default" },
  items: [],
  warnings: [],
  scannedAt: null,
  generation: 0,
  cancelled: false,
  progress: [],
  error: null,
  lastError: null,
  generationNotice: null,
  diskKeyAtStart: 0,
  activeEpoch: 0,
  streamedCount: 0,
  startScan: async (scope) => {
    if (get().phase === "scanning") return;
    let diskKeyAtStart = 0;
    try {
      const before = await getBackend().getScanResults();
      diskKeyAtStart = snapshotKey(before);
    } catch {
      diskKeyAtStart = 0;
    }
    const epoch = ++epochCounter;
    pendingTerminals.clear();
    set({
      phase: "scanning",
      scope,
      scanId: null,
      items: [],
      warnings: [],
      progress: [],
      error: null,
      scannedAt: null,
      cancelled: false,
      streamedCount: 0,
      diskKeyAtStart,
      activeEpoch: epoch
    });
    if (!subscription) {
      subscription = subscribeScan(makeHandlers());
    }
    await subscription.ready;
    try {
      const backend = getBackend();
      const handle = await backend.scan(scope);
      if (useScanStore.getState().activeEpoch !== epoch) return;
      set({ scanId: handle.scanId });
      replayPendingTerminalFor(epoch, handle.scanId);
    } catch (raw) {
      if (useScanStore.getState().activeEpoch !== epoch) return;
      const err = raw;
      set({ phase: "error", error: err, lastError: err });
    }
  },
  cancelScan: async () => {
    const { scanId } = get();
    if (scanId === null) return;
    await getBackend().cancelScan(scanId);
  },
  loadLatest: async () => {
    try {
      const snap = await getBackend().getScanResults();
      const prevKey = snapshotKeyOf(get());
      const nextKey = snapshotKey(snap);
      const items = snap.items;
      const streamedCount = items.length;
      const scannedAt = snap.scanned_at;
      const cancelled = snap.cancelled;
      const generation = snap.generation;
      const advanced = prevKey !== 0 && nextKey !== prevKey;
      if (advanced) {
        onGenerationAdvanced(nextKey);
      }
      set({ items, warnings: snap.warnings, scannedAt, cancelled, generation, streamedCount, error: null });
      return true;
    } catch {
      return false;
    }
  },
  dismissGenerationNotice: () => set({ generationNotice: null }),
  reset: () => {
    epochCounter++;
    pendingTerminals.clear();
    set({
      phase: "idle",
      scanId: null,
      items: [],
      warnings: [],
      progress: [],
      scannedAt: null,
      cancelled: false,
      error: null,
      generationNotice: null,
      streamedCount: 0,
      // Freshness key must not keep the wiped snapshot's generation (the
      // 清空 reset: an empty store IS first-run, generation 0).
      generation: 0
    });
  }
}));
function snapshotKeyOf(s) {
  return s.generation > 0 ? s.generation : s.scannedAt ?? 0;
}
function makeHandlers() {
  return {
    onProgress: (p) => {
      if (rejectStale(p.scan_id)) return;
      useScanStore.setState((s) => ({
        progress: [...s.progress, { provider: p.provider, stage: p.stage }]
      }));
    },
    onItem: (p) => {
      if (rejectStale(p.scan_id)) return;
      useScanStore.setState((s) => ({
        items: [...s.items, p.item],
        streamedCount: s.streamedCount + 1
      }));
    },
    onWarning: (p) => {
      if (rejectStale(p.scan_id)) return;
      useScanStore.setState((s) => ({ warnings: [...s.warnings, p.message] }));
    },
    onDone: (p) => {
      const s = useScanStore.getState();
      if (s.phase !== "scanning") {
        const shown = snapshotKeyOf(s);
        if (p.generation > 0 && shown > 0 && p.generation < shown) {
          return;
        }
        return;
      }
      if (s.scanId === null) {
        parkTerminal({ kind: "done", epoch: s.activeEpoch, scanId: p.scan_id, payload: p });
        return;
      }
      if (p.scan_id !== s.scanId) {
        return;
      }
      void finaliseDone(p);
    },
    onError: (p) => {
      const s = useScanStore.getState();
      if (s.phase !== "scanning") return;
      if (s.scanId !== null) {
        if (p.scan_id === s.scanId) {
          failScan(p.message);
          return;
        }
        return;
      }
      parkTerminal({ kind: "error", epoch: s.activeEpoch, scanId: p.scan_id, message: p.message });
    }
  };
}
function failScan(message) {
  const err = { code: "engine", message };
  useScanStore.setState({ phase: "error", error: err, lastError: err });
}
function replayTerminal(pending) {
  if (useScanStore.getState().activeEpoch !== pending.epoch) return;
  const bound = useScanStore.getState().scanId;
  if (bound === null || bound !== pending.scanId) return;
  if (pending.kind === "error") {
    failScan(pending.message);
    return;
  }
  void finaliseDone(pending.payload);
}
function replayPendingTerminalFor(epoch, boundScanId) {
  const pending = pendingTerminals.get(boundScanId);
  if (pending === void 0 || pending.epoch !== epoch) return;
  pendingTerminals.delete(boundScanId);
  replayTerminal(pending);
}
function rejectStale(eventScanId) {
  const s = useScanStore.getState();
  if (s.phase !== "scanning" && s.phase !== "done" && s.phase !== "cancelled") return true;
  const known = s.scanId;
  if (known === null || known !== eventScanId) return s.phase !== "scanning";
  return false;
}
var pendingTerminals = /* @__PURE__ */ new Map();
function parkTerminal(entry) {
  const existing = pendingTerminals.get(entry.scanId);
  if (existing?.kind === "error" && entry.kind === "done") {
    return;
  }
  pendingTerminals.set(entry.scanId, entry);
}
async function finaliseDone(p) {
  const baseline = useScanStore.getState().diskKeyAtStart;
  const want = Math.max(p.generation, baseline);
  const maxAttempts = 12;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const ok = await useScanStore.getState().loadLatest();
    if (ok) {
      const after = snapshotKeyOf(useScanStore.getState());
      if (after >= want) {
        settleTerminal(p.cancelled);
        return;
      }
      if (after === 0 && p.cancelled) {
        useScanStore.setState({ phase: "cancelled", cancelled: true });
        return;
      }
    }
    await new Promise((r) => setTimeout(r, Math.min(40 * 2 ** (attempt - 1), 500)));
  }
  settleTerminal(p.cancelled);
}
function settleTerminal(cancelled) {
  const phase = useScanStore.getState().phase;
  if (phase === "idle") return;
  const next = cancelled ? "cancelled" : "done";
  useScanStore.setState({ phase: next, cancelled });
}
var generationListeners = [];
function onScanGeneration(listener) {
  generationListeners.push(listener);
}
function onGenerationAdvanced(key) {
  useScanStore.setState({
    generationNotice: "\u5DF2\u6E05\u7A7A\u52FE\u9009\uFF1A\u51FA\u73B0\u4E86\u65B0\u7684\u626B\u63CF\u7ED3\u679C"
  });
  for (const fn of [...generationListeners]) fn(key);
}

// src/stores/selectionStore.ts
import { create as create2 } from "zustand";
var useSelectionStore = create2((set) => ({
  selected: /* @__PURE__ */ new Set(),
  toggle: (id2) => set((s) => {
    const next = new Set(s.selected);
    if (next.has(id2)) next.delete(id2);
    else next.add(id2);
    return { selected: next };
  }),
  setAll: (ids) => set({ selected: new Set(ids) }),
  clear: () => set({ selected: /* @__PURE__ */ new Set() })
}));

// src/stores/cleanupStore.ts
import { create as create3 } from "zustand";
var CLEAR = {
  step: "selecting",
  plan: null,
  dryRunSession: null,
  finalSession: null,
  liveItems: [],
  error: null,
  busy: false
};
var useCleanupStore = create3((set, get) => ({
  ...CLEAR,
  policy: "default",
  createPlan: async (itemIds, policy) => {
    if (useScanStore.getState().phase !== "done") return;
    set({ busy: true, error: null, step: "planning" });
    try {
      const scanGeneration = useScanStore.getState().generation;
      const plan = await getBackend().createCleanupPlan(itemIds, policy, scanGeneration);
      set({ plan, step: "plan-review", busy: false, policy });
    } catch (raw) {
      set({ step: "error", error: raw, busy: false });
    }
  },
  runDryRun: async () => {
    const { plan, policy } = get();
    if (!plan || plan.items.length === 0) return;
    set({ busy: true, error: null, step: "dry-run", dryRunSession: null });
    try {
      const session = await getBackend().executeCleanupPlan(plan.planId, policy, true);
      set({ dryRunSession: session, step: "dry-run-review", busy: false });
    } catch (raw) {
      set({ step: "error", error: raw, busy: false });
    }
  },
  execute: async () => {
    const { plan, policy } = get();
    if (!plan || plan.items.length === 0) return;
    set({ busy: true, error: null, step: "executing", liveItems: [], finalSession: null });
    try {
      const session = await getBackend().executeCleanupPlan(plan.planId, policy, false);
      set({ finalSession: session, step: "session", busy: false });
    } catch (raw) {
      set({ step: "error", error: raw, busy: false });
    }
  },
  setPolicy: (p) => set({ policy: p }),
  close: () => set(CLEAR)
}));

// src/stores/aiStore.ts
import { create as create4 } from "zustand";

// src/adapters/backend.ts
function toCommandError(raw) {
  if (typeof raw === "object" && raw !== null && "code" in raw && "message" in raw) {
    return raw;
  }
  return { code: "engine", message: String(raw) };
}

// src/stores/aiStore.ts
var emptyReviewState = () => ({
  preparedBatch: null,
  suggestions: [],
  finalRisks: /* @__PURE__ */ new Map(),
  selectedSuggestionIds: /* @__PURE__ */ new Set(),
  requiresLowRiskWarning: false,
  preparing: false,
  analyzing: false,
  confirming: false,
  confirmationResult: null
});
function requiresLowRiskWarning(finalRisks) {
  for (const risk of finalRisks.values()) {
    if (risk === "safe" || risk === "regenerable-local") return true;
  }
  return false;
}
function invalidSelection(message) {
  return { code: "invalid-item", message };
}
var useAiStore = create4((set, get) => ({
  masterEnabled: false,
  profiles: [],
  loaded: false,
  profileBusy: false,
  connectionProfileId: null,
  ...emptyReviewState(),
  error: null,
  loadProfiles: async () => {
    try {
      const state = await getBackend().listAiProfiles();
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        loaded: true,
        error: null
      });
    } catch (raw) {
      set({ loaded: true, error: toCommandError(raw) });
    }
  },
  upsertProfile: async (input, apiKey) => {
    set({ profileBusy: true, error: null });
    try {
      const profile = await getBackend().upsertAiProfile(input, apiKey);
      const state = await getBackend().listAiProfiles();
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...emptyReviewState()
      });
      return profile;
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
      return null;
    }
  },
  deleteProfile: async (profileId) => {
    set({ profileBusy: true, error: null });
    try {
      await getBackend().deleteAiProfile(profileId);
      const state = await getBackend().listAiProfiles();
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...emptyReviewState()
      });
      return true;
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
      return false;
    }
  },
  setActiveProfile: async (profileId) => {
    set({ profileBusy: true, error: null });
    try {
      const state = await getBackend().setActiveAiProfile(profileId);
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...emptyReviewState()
      });
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
    }
  },
  setMasterEnabled: async (enabled) => {
    set({ profileBusy: true, error: null });
    try {
      const state = await getBackend().setAiMasterEnabled(enabled);
      set({
        masterEnabled: state.masterEnabled,
        profiles: state.profiles,
        profileBusy: false,
        ...enabled ? {} : emptyReviewState()
      });
    } catch (raw) {
      set({ profileBusy: false, error: toCommandError(raw) });
    }
  },
  testConnection: async (profileId) => {
    set({ connectionProfileId: profileId, error: null });
    try {
      await getBackend().testAiConnection(profileId);
      set({ connectionProfileId: null });
      return true;
    } catch (raw) {
      set({ connectionProfileId: null, error: toCommandError(raw) });
      return false;
    }
  },
  prepareBatch: async (profileId, scanGeneration, itemIds) => {
    if (get().preparing || get().analyzing || get().confirming) return;
    set({ preparing: true, error: null, confirmationResult: null });
    try {
      const batch = await getBackend().prepareAiBatch(profileId, scanGeneration, itemIds);
      set({ ...emptyReviewState(), preparedBatch: batch });
    } catch (raw) {
      set({ preparing: false, error: toCommandError(raw) });
    }
  },
  setPreparedBatch: (batch) => set({ ...emptyReviewState(), preparedBatch: batch, error: null }),
  analyzePreparedBatch: async () => {
    const batch = get().preparedBatch;
    if (!batch || get().analyzing || get().confirming) return;
    set({ analyzing: true, error: null, confirmationResult: null });
    try {
      const suggestions = await getBackend().analyzeAiBatch(batch.batchId);
      set({
        analyzing: false,
        suggestions,
        finalRisks: /* @__PURE__ */ new Map(),
        selectedSuggestionIds: /* @__PURE__ */ new Set(),
        requiresLowRiskWarning: false
      });
    } catch (raw) {
      set({ analyzing: false, error: toCommandError(raw) });
    }
  },
  setFinalRisk: (itemId, risk) => set((state) => {
    const finalRisks = new Map(state.finalRisks);
    finalRisks.set(itemId, risk);
    return { finalRisks, requiresLowRiskWarning: requiresLowRiskWarning(finalRisks) };
  }),
  toggleSuggestion: (itemId) => set((state) => {
    if (!state.finalRisks.has(itemId)) return {};
    const selectedSuggestionIds = new Set(state.selectedSuggestionIds);
    if (selectedSuggestionIds.has(itemId)) selectedSuggestionIds.delete(itemId);
    else selectedSuggestionIds.add(itemId);
    return { selectedSuggestionIds };
  }),
  confirmSelected: async () => {
    const { preparedBatch, finalRisks, selectedSuggestionIds } = get();
    if (!preparedBatch) return;
    const items = [...selectedSuggestionIds].flatMap((itemId) => {
      const finalRisk = finalRisks.get(itemId);
      return finalRisk ? [{ itemId, finalRisk }] : [];
    });
    if (items.length === 0) {
      set({ error: invalidSelection("\u8BF7\u5148\u4E3A\u81F3\u5C11\u4E00\u9879\u5EFA\u8BAE\u9009\u62E9\u6700\u7EC8\u98CE\u9669\u5E76\u52FE\u9009\u786E\u8BA4") });
      return;
    }
    set({ confirming: true, error: null });
    try {
      const confirmationResult = await getBackend().confirmAiBatch(
        preparedBatch.batchId,
        preparedBatch.scanGeneration,
        items
      );
      set({ ...emptyReviewState(), confirmationResult });
    } catch (raw) {
      set({ confirming: false, error: toCommandError(raw) });
    }
  },
  cancelAnalysis: async () => {
    const batch = get().preparedBatch;
    if (!batch) return;
    try {
      await getBackend().cancelAiBatch(batch.batchId);
    } catch {
    } finally {
      set({ analyzing: false });
    }
  },
  resetForNewScan: (_generation) => {
    const batch = get().preparedBatch;
    if (batch) void getBackend().cancelAiBatch(batch.batchId).catch(() => void 0);
    set({ ...emptyReviewState(), error: null });
  },
  dismissError: () => set({ error: null })
}));

// src/stores/generationLifecycle.ts
var wired = false;
function wireGenerationLifecycle() {
  if (wired) return;
  wired = true;
  onScanGeneration((key) => {
    useSelectionStore.getState().clear();
    const cleanup = useCleanupStore.getState();
    if (cleanup.step !== "selecting") {
      cleanup.close();
    }
    useAiStore.getState().resetForNewScan(key);
    if (key < 0) {
      console.warn("[devresidue] unexpected generation key", key);
    }
  });
}

// src/data/rules.ts
var RULES = [
  {
    id: 2,
    slug: "builtin-protected/ssh",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "SSH keys, config and known_hosts",
    match: "exact: %USERPROFILE%/.ssh",
    valid: true
  },
  {
    id: 3,
    slug: "builtin-protected/gnupg",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "GPG keyrings and trust database",
    match: "exact: %USERPROFILE%/.gnupg",
    valid: true
  },
  {
    id: 4,
    slug: "builtin-protected/aws",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "AWS credentials and config",
    match: "exact: %USERPROFILE%/.aws",
    valid: true
  },
  {
    id: 5,
    slug: "builtin-detection/node-project-node-modules",
    source: "builtin-detection",
    risk: "regenerable-download",
    category: "dependency",
    description: "npm project dependency tree, re-downloadable via npm ci",
    match: "glob: %USERPROFILE%/repos/*/node_modules + parent_marker: package.json",
    valid: true
  },
  {
    id: 12,
    slug: "builtin-detection/claude-shell-snapshots-cache",
    source: "builtin-detection",
    risk: "safe",
    category: "ai-agent",
    description: "Claude Code shell-snapshot cache (regenerated on demand)",
    match: "exact: %USERPROFILE%/.claude/shell-snapshots",
    valid: true
  },
  {
    id: 13,
    slug: "builtin-detection/claude-session-history",
    source: "builtin-detection",
    risk: "review",
    category: "session",
    description: "Claude Code per-project session transcripts (review before cleanup)",
    match: "glob: %USERPROFILE%/.claude/projects/*",
    valid: true
  },
  {
    id: 14,
    slug: "builtin-detection/claude-project-trash-cache",
    source: "builtin-detection",
    risk: "safe",
    category: "temporary",
    description: "Claude Code trash/interrupt caches under session projects",
    match: "glob: %USERPROFILE%/.claude/projects/*/.trash/**",
    valid: true
  },
  {
    id: 20,
    slug: "builtin-detection/codex-temp",
    source: "builtin-detection",
    risk: "safe",
    category: "temporary",
    description: "Codex temporary working files (regenerated on demand)",
    match: "exact: %USERPROFILE%/.codex/.tmp",
    valid: true
  },
  {
    id: 21,
    slug: "builtin-detection/codex-session-history",
    source: "builtin-detection",
    risk: "review",
    category: "session",
    description: "Codex conversation session history (review before cleanup)",
    match: "glob: %USERPROFILE%/.codex/sessions/**",
    valid: true
  },
  {
    id: 22,
    slug: "builtin-detection/codex-computer-use-cache",
    source: "builtin-detection",
    risk: "review",
    category: "session",
    description: "Codex computer-use screen captures \u2014 may contain sensitive content",
    match: "glob: %USERPROFILE%/.codex/cache/computer-use/**",
    valid: true
  },
  {
    id: 31,
    slug: "builtin-protected/opencode-config",
    source: "builtin-protected",
    risk: "protected",
    category: "credential",
    description: "OpenCode config directory (auth, credentials, user config)",
    match: "glob: %USERPROFILE%/.config/opencode/**",
    valid: true
  },
  {
    id: 33,
    slug: "builtin-detection/claude-agent-cache-narrowed",
    source: "builtin-detection",
    risk: "safe",
    category: "ai-agent",
    description: "Cache sub-layout of the agent dir, narrowed by include",
    match: "glob: %USERPROFILE%/.claude/* + include: shell-snapshots/**",
    valid: true
  }
];
var DEMO_INVALID_USER_RULE = {
  ruleId: "user/protect-all-of-c",
  source: null,
  risk: null,
  category: null,
  description: null,
  valid: false,
  issues: ["match path C:\\ resolves to a drive root \u2014 rejected by path-bound validation"]
};
function mockRuleDtos() {
  return [
    ...RULES.map((r) => ({
      ruleId: r.slug,
      source: r.source,
      risk: r.risk,
      category: r.category,
      description: r.description,
      valid: r.valid,
      issues: []
    })),
    DEMO_INVALID_USER_RULE
  ];
}
function mockRulesValidation() {
  const rules = mockRuleDtos();
  const loaded = rules.filter((r) => r.valid).length;
  return {
    total: loaded,
    errors: 1,
    warnings: 1,
    issues: [
      {
        ruleId: "user/protect-all-of-c",
        message: "match path C:\\ resolves to a drive root \u2014 rejected by path-bound validation",
        severity: "error"
      },
      {
        ruleId: "builtin-detection/claude-session-history",
        message: "exclude pattern **/.trash/** is shadowed by claude-project-trash-cache at depth 1",
        severity: "warning"
      }
    ]
  };
}

// src/adapters/mockBackend.ts
var DAY = 86400;
var now = () => Math.floor(Date.now() / 1e3);
var daysAgo = (d) => now() - d * DAY;
var GiB = 1024 ** 3;
var MiB = 1024 ** 2;
var KiB = 1024;
var nextId = 1;
var id = () => nextId++;
function item(base) {
  return { id: id(), ...base };
}
function defaultItems() {
  return [
    // ---- Dev cache / package managers (download class) ----
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\npm-cache",
      display_name: "npm cache",
      product: "npm",
      category: "package-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 22.2 * GiB,
      file_count: 284310,
      last_modified: daysAgo(2),
      explanation: "npm package download cache. Deleting frees 22.2 GiB; future installs re-download every package from the registry. Cleaned via the tool-native command (npm cache clean --force), not a raw delete.",
      cleanup_action: {
        kind: "external-command",
        command: {
          executable: "npm",
          args: ["cache", "clean", "--force"],
          working_directory: null,
          timeout_secs: 300
        }
      },
      evidence: [
        {
          rule_id: null,
          source: "tool-reported-path",
          detail: "npm config get cache reported this location"
        },
        {
          rule_id: null,
          source: "developer-cache-provider",
          detail: "npm provider measured cache size on disk"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\pip\\cache",
      display_name: "pip HTTP cache",
      product: "pip",
      category: "developer-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 1.9 * GiB,
      file_count: 61204,
      last_modified: daysAgo(6),
      explanation: "pip wheel/HTTP download cache. Safe to remove; packages re-download on next pip install. 1.9 GiB reclaimable.",
      cleanup_action: { kind: "direct-delete" },
      evidence: [
        {
          rule_id: null,
          source: "tool-reported-path",
          detail: "pip cache dir reported this location"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.cargo\\registry",
      display_name: "Cargo registry cache",
      product: "cargo",
      category: "developer-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 4.3 * GiB,
      file_count: 96118,
      last_modified: daysAgo(9),
      explanation: "Downloaded .crate files and extracted sources for every dependency cargo ever resolved. Deleting frees 4.3 GiB; the next build of each project re-downloads its crates.",
      cleanup_action: { kind: "direct-delete" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "cargo registry layout under .cargo/registry"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\uv\\cache",
      display_name: "uv cache",
      product: "uv",
      category: "developer-cache",
      risk: "regenerable-download",
      source: "developer-cache-provider",
      logical_size: 2.8 * GiB,
      file_count: 48922,
      last_modified: daysAgo(1),
      explanation: "uv package cache. Regenerable by re-download; 2.8 GiB. uv cache clean is the tool-native path.",
      cleanup_action: {
        kind: "external-command",
        command: {
          executable: "uv",
          args: ["cache", "clean"],
          working_directory: null,
          timeout_secs: 120
        }
      },
      evidence: [
        {
          rule_id: null,
          source: "tool-reported-path",
          detail: "uv cache directory reported by uv"
        }
      ]
    }),
    // ---- AI agents (split per SPEC §9) ----
    item({
      path: "C:\\Users\\demo\\.claude\\shell-snapshots",
      display_name: "Claude Code shell-snapshots",
      product: "Claude Code",
      category: "ai-agent",
      risk: "safe",
      source: "agent-provider",
      logical_size: 384 * MiB,
      file_count: 91,
      last_modified: daysAgo(12),
      explanation: "Shell environment snapshots Claude Code takes at session start. Pure cache, regenerated on demand; deleting has no lasting effect.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: 12,
          source: "path-layout",
          detail: "matched builtin-detection/claude-shell-snapshots-cache (rule 12)"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.claude\\projects\\demo-app",
      display_name: "Claude Code session history (demo-app)",
      product: "Claude Code",
      category: "session",
      risk: "review",
      source: "agent-provider",
      logical_size: 16384e3,
      file_count: 248,
      last_modified: daysAgo(60),
      explanation: "Transcripts of every Claude Code conversation for demo-app. May contain context you want to keep (decisions, hard-won fixes). Review before deleting.",
      cleanup_action: {
        kind: "defer",
        reason: "Review risk class; user confirmation required before cleanup"
      },
      evidence: [
        {
          rule_id: 13,
          source: "path-layout",
          detail: "matched builtin-detection/claude-session-history (rule 13)"
        },
        {
          rule_id: null,
          source: "agent-provider",
          detail: "claude provider session transcript directory"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.codex\\sessions",
      display_name: "Codex CLI session history",
      product: "Codex CLI",
      category: "session",
      risk: "review",
      source: "agent-provider",
      logical_size: 67.7 * MiB,
      file_count: 1204,
      last_modified: daysAgo(3),
      explanation: "Rolling session files for Codex CLI conversations. 67.7 MiB. Contains the actual conversation history \u2014 treat as user data until reviewed.",
      cleanup_action: {
        kind: "defer",
        reason: "Review risk class; user confirmation required before cleanup"
      },
      evidence: [
        {
          rule_id: 21,
          source: "path-layout",
          detail: "matched builtin-detection/codex-session-history (rule 21)"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.codex\\.tmp",
      display_name: "Codex CLI temp files",
      product: "Codex CLI",
      category: "temporary",
      risk: "safe",
      source: "agent-provider",
      logical_size: 18.4 * MiB,
      file_count: 37,
      last_modified: daysAgo(0.5),
      explanation: "Scratch working files Codex CLI regenerates per run. No persistent value; 18 MiB.",
      cleanup_action: { kind: "direct-delete" },
      evidence: [
        {
          rule_id: 20,
          source: "path-layout",
          detail: "matched builtin-detection/codex-temp (rule 20)"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Roaming\\opencode\\opencode.db",
      display_name: "OpenCode session database",
      product: "OpenCode",
      category: "session",
      risk: "review",
      source: "agent-provider",
      logical_size: 1.3 * GiB,
      file_count: 1,
      last_modified: daysAgo(1),
      explanation: "Single SQLite database holding OpenCode sessions and messages. 1.3 GiB of conversation history \u2014 deleting it is irreversible. Review first.",
      cleanup_action: {
        kind: "defer",
        reason: "Review risk class; user confirmation required before cleanup"
      },
      evidence: [
        {
          rule_id: 31,
          source: "path-layout",
          detail: "opencode session store detected by file name"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.claude\\projects\\demo-app\\.trash",
      display_name: "Claude Code trash cache (demo-app)",
      product: "Claude Code",
      category: "temporary",
      risk: "safe",
      source: "agent-provider",
      logical_size: 92 * MiB,
      file_count: 214,
      last_modified: daysAgo(40),
      explanation: "Claude Code trash/interrupt cache for one project. Safe to clean.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: 14,
          source: "path-layout",
          detail: "matched builtin-detection/claude-project-trash-cache (rule 14)"
        }
      ]
    }),
    // ---- Projects (kondo) ----
    item({
      path: "D:\\Code\\dev-cleaner\\target",
      display_name: "Rust build artifacts (dev-cleaner)",
      product: "Rust",
      category: "build-artifact",
      risk: "regenerable-local",
      source: "kondo",
      logical_size: 3.1 * GiB,
      file_count: 19204,
      last_modified: daysAgo(21),
      explanation: "Rust incremental build output. 3.1 GiB, rebuilt locally by cargo build \u2014 no downloads involved. Moving to the recycle bin is fully reversible.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: null,
          source: "kondo-project-type",
          detail: "kondo detected a Rust project (Cargo.toml) at D:\\Code\\dev-cleaner"
        }
      ]
    }),
    item({
      path: "D:\\Code\\demo-app\\node_modules",
      display_name: "node_modules (demo-app)",
      product: "npm",
      category: "dependency",
      risk: "regenerable-download",
      source: "kondo",
      logical_size: 1.4 * GiB,
      file_count: 210433,
      last_modified: daysAgo(30),
      explanation: "Installed dependencies for demo-app. Restorable with npm ci; the next install re-downloads all packages from the registry.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: 5,
          source: "kondo-project-type",
          detail: "node project node_modules, parent holds package.json (rule 5)"
        }
      ]
    }),
    item({
      path: "D:\\Code\\demo-app\\build",
      display_name: "CMake build directory (demo-app)",
      product: "CMake",
      category: "build-artifact",
      risk: "regenerable-local",
      source: "kondo",
      logical_size: 640 * MiB,
      file_count: 3921,
      last_modified: daysAgo(45),
      explanation: "CMake/Ninja build output. Rebuilt locally from sources; deleting costs only the next compile.",
      cleanup_action: { kind: "recycle-bin" },
      evidence: [
        {
          rule_id: null,
          source: "kondo-project-type",
          detail: "kondo detected a CMake project at D:\\Code\\demo-app"
        }
      ]
    }),
    // ---- Protected ----
    item({
      path: "C:\\Users\\demo\\.ssh",
      display_name: "SSH keys & config",
      product: "OpenSSH",
      category: "credential",
      risk: "protected",
      source: "rule",
      logical_size: 24576,
      file_count: 6,
      last_modified: daysAgo(400),
      explanation: "SSH private keys, config and known_hosts. Permanently protected by a built-in rule; DevResidue will never plan it for deletion.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: 2,
          source: "builtin-protected",
          detail: "builtin-protected/ssh (rule 2): user-profile/.ssh"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.config\\opencode",
      display_name: "OpenCode config directory",
      product: "OpenCode",
      category: "credential",
      risk: "protected",
      source: "rule",
      logical_size: 132 * KiB,
      file_count: 4,
      last_modified: daysAgo(120),
      explanation: "OpenCode auth.json, credentials and user config. Protected by built-in rule builtin-protected/opencode-config; never cleanable.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: 33,
          source: "builtin-protected",
          detail: "builtin-protected/opencode-config (rule 33)"
        }
      ]
    }),
    // ---- Unknown (SPEC §25: unrecognized dev-shaped data, never auto-deleted) ----
    item({
      path: "C:\\Users\\demo\\AppData\\Roaming\\some-tool\\data",
      display_name: "Unrecognised developer data (some-tool)",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 512 * MiB,
      file_count: 8004,
      last_modified: daysAgo(70),
      explanation: "Found under AppData with developer-tool-like layout, but no rule or provider recognises it. Classified UNKNOWN \u2014 DevResidue never auto-deletes unknown data.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "suspected dev-data layout, no matching rule"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.qagent",
      display_name: ".qagent",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 128 * MiB,
      file_count: 942,
      last_modified: daysAgo(14),
      explanation: "Dot-directory in the user profile with an agent-like name, but not on the known-agent list. Classified UNKNOWN until reviewed.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "profile dot-directory, agent-shaped name, no matching rule"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\.tigerproxy",
      display_name: ".tigerproxy",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 34 * MiB,
      file_count: 61,
      last_modified: daysAgo(190),
      explanation: "Profile dot-directory that looks like tool state, untouched for six months. No rule recognises it \u2014 needs a human decision.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "profile dot-directory, no matching rule"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Roaming\\RustDeskTool\\sessions",
      display_name: "RustDeskTool sessions",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 240 * MiB,
      file_count: 2310,
      last_modified: daysAgo(9),
      explanation: "AppData sub-directory with a session-store layout. The vendor is not in the rule set, so the data stays UNKNOWN \u2014 review before any action.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "AppData session-store layout, unknown vendor"
        }
      ]
    }),
    item({
      path: "C:\\Users\\demo\\AppData\\Local\\Temp\\build-cache-v3",
      display_name: "build-cache-v3 (Temp)",
      product: null,
      category: "unknown",
      risk: "unknown",
      source: "rule",
      logical_size: 780 * MiB,
      file_count: 41066,
      last_modified: daysAgo(3),
      explanation: "A build-cache-shaped directory under Temp, but no owning product could be identified. UNKNOWN \u2014 decide manually.",
      cleanup_action: { kind: "none" },
      evidence: [
        {
          rule_id: null,
          source: "path-layout",
          detail: "Temp build-cache layout, no owning product identified"
        }
      ]
    })
  ];
}
var PROVIDER_SLUGS = {
  "dev-cache": ["npm", "bun", "pip", "uv", "cargo", "nuget"],
  agents: ["codex", "claude-code", "opencode", "cursor", "windsurf"],
  projects: ["kondo"],
  unknown: ["unknown-dev-data"],
  default: [
    "npm",
    "bun",
    "pip",
    "uv",
    "cargo",
    "nuget",
    "kondo",
    "codex",
    "claude-code",
    "opencode",
    "cursor",
    "windsurf",
    "unknown-dev-data"
  ]
};
var MockBackend = class {
  kind = "mock";
  snapshot = null;
  journal = [];
  nextScanId = 1;
  nextPlanId = 1;
  nextSessionId = 1;
  /** R04: snapshot generation counter (mirrors scan_store::next_generation). */
  nextGeneration = 0;
  active = null;
  /** Plans by id, so execution targets exactly what the UI planned. */
  plans = /* @__PURE__ */ new Map();
  // ---- M4 mock state -------------------------------------------------------
  /** Dispositions keyed by *path* (stable across scans, unlike item ids). */
  dispositions = /* @__PURE__ */ new Map();
  persistedLoaded = false;
  // Remote-AI mock state is process-local except the non-secret profile
  // metadata written by persistAiProfiles(). No API Key field exists here.
  aiMasterEnabled = false;
  aiProfiles = [];
  activeAiProfileId = null;
  aiBatches = /* @__PURE__ */ new Map();
  activeAiBatchId = null;
  nextAiBatchId = 1;
  constructor() {
    this.loadPersisted();
    this.seedJournal();
  }
  loadPersisted() {
    if (this.persistedLoaded) return;
    this.persistedLoaded = true;
    try {
      const raw = localStorage.getItem("devresidue.dispositions.v1");
      if (raw) {
        const parsed = JSON.parse(raw);
        for (const [path, d] of Object.entries(parsed)) {
          if (d === "ignore" || d === "protect") this.dispositions.set(path, d);
          else if (d === "ignored") this.dispositions.set(path, "ignore");
          else if (d === "protected") this.dispositions.set(path, "protect");
        }
      }
      const rawAi = localStorage.getItem("devresidue.ai-profiles.v1");
      if (rawAi) {
        const parsed = JSON.parse(rawAi);
        if (typeof parsed.masterEnabled === "boolean") {
          this.aiMasterEnabled = parsed.masterEnabled;
        }
        if (typeof parsed.activeProfileId === "string" || parsed.activeProfileId === null) {
          this.activeAiProfileId = parsed.activeProfileId;
        }
        if (Array.isArray(parsed.profiles)) {
          this.aiProfiles = parsed.profiles.flatMap(readStoredMockAiProfile);
        }
      }
    } catch {
    }
  }
  persistDispositions() {
    try {
      const obj = {};
      for (const [path, d] of this.dispositions) obj[path] = d;
      localStorage.setItem("devresidue.dispositions.v1", JSON.stringify(obj));
    } catch {
    }
  }
  /** Deliberately serializes a non-secret allowlist rather than request input. */
  persistAiProfiles() {
    try {
      const payload = {
        masterEnabled: this.aiMasterEnabled,
        activeProfileId: this.activeAiProfileId,
        profiles: this.aiProfiles.map((profile) => ({
          id: profile.id,
          name: profile.name,
          baseUrl: profile.baseUrl,
          model: profile.model,
          enabled: profile.enabled
        }))
      };
      localStorage.setItem("devresidue.ai-profiles.v1", JSON.stringify(payload));
    } catch {
    }
  }
  /** Journal seed: two historical sessions with realistic mixed outcomes. */
  seedJournal() {
    const t0 = daysAgo(6);
    const t1 = daysAgo(1);
    let minute = 0;
    const push = (sessionId, timeSecs, product, path, action, estimatedSize, result, error) => {
      this.journal.push({
        sessionId,
        timeSecs,
        phase: "result",
        product,
        rule: null,
        provider: null,
        path,
        action,
        estimatedSize,
        result,
        error
      });
    };
    const s1 = [
      ["npm", "C:\\Users\\demo\\AppData\\Local\\npm-cache", "execute", 1258291200, "success", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\shell-snapshots", "recycle", 384 * MiB, "success", null],
      ["Rust", "D:\\Code\\dev-cleaner\\target", "recycle", 3.1 * GiB, "success", null],
      ["Codex CLI", "C:\\Users\\demo\\.codex\\.tmp", "delete", 18.4 * MiB, "success", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\projects\\demo-app\\.trash", "recycle", 92 * MiB, "success", null],
      ["OpenSSH", "C:\\Users\\demo\\.ssh", "recycle", 24576, "skipped", "deny:protected-risk"],
      ["CMake", "D:\\Code\\demo-app\\build", "recycle", 640 * MiB, "success", null],
      ["pip", "C:\\Users\\demo\\AppData\\Local\\pip\\cache", "delete", 1.9 * GiB, "success", null],
      ["uv", "C:\\Users\\demo\\AppData\\Local\\uv\\cache", "execute", 2.8 * GiB, "failed", "process exited with code 1: uv cache clean (timeout 120s)"],
      ["cargo", "C:\\Users\\demo\\.cargo\\registry", "delete", 4.3 * GiB, "success", null],
      ["npm", "D:\\Code\\demo-app\\node_modules", "recycle", 1.4 * GiB, "success", null]
    ];
    for (const [product, path, action, size, result, error] of s1) {
      push(1, t0 + minute * 60, product, path, action, size, result, error);
      minute += 1;
    }
    minute = 0;
    const s2 = [
      ["npm", "C:\\Users\\demo\\AppData\\Local\\npm-cache", "execute", 22.2 * GiB, "dry-run", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\shell-snapshots", "recycle", 384 * MiB, "success", null],
      ["Claude Code", "C:\\Users\\demo\\.claude\\projects\\demo-app", "recycle", 16384e3, "skipped", "deny:requires review confirmation"],
      ["OpenCode", "C:\\Users\\demo\\AppData\\Roaming\\opencode\\opencode.db", "delete", 1.3 * GiB, "skipped", "deny:unknown-risk"]
    ];
    for (const [product, path, action, size, result, error] of s2) {
      push(2, t1 + minute * 60, product, path, action, size, result, error);
      minute += 1;
    }
  }
  // Event subscribers (mock event bus). Payload field names mirror the
  // backend's snake_case event wire exactly (contract.rs EV_* structs have
  // no rename_all) — a mock that drifts masks contract breaks.
  scanProgressSubs = [];
  scanItemSubs = [];
  scanWarningSubs = [];
  scanDoneSubs = [];
  scanErrorSubs = [];
  onScanProgress(fn) {
    this.scanProgressSubs.push(fn);
    return () => this.drop(this.scanProgressSubs, fn);
  }
  onScanItem(fn) {
    this.scanItemSubs.push(fn);
    return () => this.drop(this.scanItemSubs, fn);
  }
  onScanWarning(fn) {
    this.scanWarningSubs.push(fn);
    return () => this.drop(this.scanWarningSubs, fn);
  }
  onScanDone(fn) {
    this.scanDoneSubs.push(fn);
    return () => this.drop(this.scanDoneSubs, fn);
  }
  onScanError(fn) {
    this.scanErrorSubs.push(fn);
    return () => this.drop(this.scanErrorSubs, fn);
  }
  drop(list, value) {
    const i = list.indexOf(value);
    if (i >= 0) list.splice(i, 1);
  }
  emitProgress(scanId, provider, stage) {
    for (const fn of [...this.scanProgressSubs]) fn({ scan_id: scanId, provider, stage });
  }
  emitItem(scanId, item2) {
    for (const fn of [...this.scanItemSubs]) fn({ scan_id: scanId, item: item2 });
  }
  emitWarning(scanId, message) {
    for (const fn of [...this.scanWarningSubs]) fn({ scan_id: scanId, message });
  }
  emitDone(scanId, cancelled, total, generation) {
    for (const fn of [...this.scanDoneSubs])
      fn({ scan_id: scanId, cancelled, total, generation });
  }
  emitError(scanId, message) {
    for (const fn of [...this.scanErrorSubs]) fn({ scan_id: scanId, message });
  }
  async scan(scope) {
    if (this.active) {
      throw toCommandError({
        code: "partial-scan",
        message: "a scan is already running"
      });
    }
    const scanId = this.nextScanId++;
    this.active = { resolve: null, scope, cancel: false };
    if (localStorage.getItem("devresidue.demoEarlyTerminal") === "1") {
      this.snapshot = {
        scanned_at: now(),
        generation: ++this.nextGeneration,
        mode: { kind: "real", workspace_roots: ["D:\\Code"] },
        items: defaultItems().filter((it) => {
          switch (scope.kind) {
            case "agents":
              return it.source === "agent-provider" || it.source === "rule";
            case "dev-cache":
              return it.source === "developer-cache-provider";
            case "projects":
              return it.source === "kondo";
            case "unknown":
              return it.risk === "unknown";
            default:
              return true;
          }
        }),
        warnings: [],
        cancelled: false
      };
      this.emitDone(
        scanId,
        false,
        this.snapshot.items.length,
        this.snapshot.generation
      );
      this.active = null;
      return { scanId };
    }
    void this.runScan(scanId, scope);
    return { scanId };
  }
  async runScan(scanId, scope) {
    const slugs = PROVIDER_SLUGS[scope.kind] ?? [];
    const all = defaultItems();
    const keep = (it) => {
      switch (scope.kind) {
        case "agents":
          return it.source === "agent-provider" || it.source === "rule";
        case "dev-cache":
          return it.source === "developer-cache-provider";
        case "projects":
          return it.source === "kondo";
        case "unknown":
          return it.risk === "unknown";
        case "default":
        default:
          return true;
      }
    };
    const items = all.filter(keep);
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await sleep(250);
    if (localStorage.getItem("devresidue.demoScanFailure") === "1") {
      this.emitError(scanId, "rule gate failed: cannot load resources/rules (demo failure)");
      this.active = null;
      return;
    }
    let cursor = 0;
    for (const slug of slugs) {
      if (this.active?.cancel) break;
      this.emitProgress(scanId, slug, "started");
      await sleep(200 + Math.random() * 250);
      if (this.active?.cancel) break;
      const bucket = Math.ceil(items.length / slugs.length);
      const slice = items.slice(cursor, cursor + bucket);
      cursor += bucket;
      for (const it of slice) {
        if (this.active?.cancel) break;
        this.emitItem(scanId, it);
        await sleep(90 + Math.random() * 120);
      }
      this.emitProgress(scanId, slug, "done");
      if (slug === "kondo") this.emitProgress(scanId, "kondo", "project");
    }
    const cancelled = this.active?.cancel ?? false;
    if (!cancelled && scope.kind === "default") {
      this.emitWarning(
        scanId,
        "go: GOPATH not set; scanned the known default %LOCALAPPDATA%\\go-build instead"
      );
      this.emitWarning(
        scanId,
        "gradle: GRADLE_USER_HOME not set; skipped (no tool-reported cache root)"
      );
    }
    const effective = (cancelled ? items.slice(0, Math.ceil(items.length / 2)) : items).filter((it) => it.risk !== "unknown" || !this.dispositions.has(it.path)).map((it) => {
      if (it.risk === "unknown" && this.dispositions.get(it.path) === "protect") {
        return {
          ...it,
          risk: "protected",
          category: "credential",
          explanation: "Marked protected by your disposition \u2014 DevResidue will never plan it for deletion.",
          cleanup_action: { kind: "none" },
          evidence: [
            ...it.evidence,
            {
              rule_id: null,
              source: "user-disposition",
              detail: "protected via Unknown page disposition"
            }
          ]
        };
      }
      return it;
    });
    this.snapshot = {
      scanned_at: now(),
      generation: ++this.nextGeneration,
      mode: { kind: "real", workspace_roots: ["D:\\Code"] },
      items: effective,
      warnings: cancelled ? [] : [
        "go: GOPATH not set; scanned the known default %LOCALAPPDATA%\\go-build instead",
        "gradle: GRADLE_USER_HOME not set; skipped (no tool-reported cache root)"
      ],
      cancelled
    };
    const kept = items.filter((_, i) => i < cursor || !cancelled);
    this.emitDone(scanId, cancelled, kept.length, this.snapshot?.generation ?? 0);
    this.active = null;
  }
  async cancelScan(scanId) {
    void scanId;
    if (this.active) {
      this.active.cancel = true;
      return true;
    }
    return false;
  }
  async getScanResults() {
    if (!this.snapshot) {
      throw toCommandError({
        code: "scan-not-found",
        message: "no scan result yet \u2014 run a scan first"
      });
    }
    return this.snapshot;
  }
  async createCleanupPlan(itemIds, policy, scanGeneration) {
    const snap = await this.getScanResults();
    if (scanGeneration != null && scanGeneration !== snap.generation) {
      throw toCommandError({
        code: "invalid-item",
        message: `selection-generation-mismatch: the selection was made against scan generation ${scanGeneration} but the latest scan is generation ${snap.generation}; re-scan and re-select`
      });
    }
    const byId = new Map(snap.items.map((it) => [it.id, it]));
    const planId = this.nextPlanId++;
    this.plans.set(planId, { itemIds, policy, scanGeneration });
    const planned = [];
    const skipped = [];
    for (const rawId of itemIds) {
      const it = byId.get(rawId);
      if (!it) {
        skipped.push({
          scanItemId: rawId,
          path: "(unknown)",
          reason: "skip: unknown selection"
        });
        continue;
      }
      if (it.risk === "protected") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: protected-risk"
        });
        continue;
      }
      if (it.risk === "unknown") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: unknown-risk"
        });
        continue;
      }
      if (it.cleanup_action.kind === "none") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: action none"
        });
        continue;
      }
      if (it.cleanup_action.kind === "defer") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: `skip: deferred (${it.cleanup_action.reason})`
        });
        continue;
      }
      const confirmation = it.risk === "regenerable-download" ? "redownload" : it.risk === "review" ? "review" : "none";
      if (confirmation === "redownload" && policy === "default") {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: requires redownload confirmation"
        });
        continue;
      }
      if (confirmation === "review" && (policy === "default" || policy === "redownload")) {
        skipped.push({
          scanItemId: it.id,
          path: it.path,
          reason: "skip: requires review confirmation"
        });
        continue;
      }
      planned.push({
        scanItemId: it.id,
        action: it.cleanup_action.kind === "external-command" ? "execute" : it.cleanup_action.kind === "direct-delete" ? "delete" : "recycle",
        confirmation,
        estimatedSize: it.logical_size,
        path: it.path
      });
    }
    return {
      planId,
      dryRun: false,
      totalEstimatedBytes: planned.reduce((s, p) => s + p.estimatedSize, 0),
      items: planned,
      skipped
    };
  }
  async executeCleanupPlan(planId, policy, dryRun) {
    const snap = await this.getScanResults();
    const byId = new Map(snap.items.map((it) => [it.id, it]));
    const stored = this.plans.get(planId);
    if (!stored) {
      throw toCommandError({
        code: "plan-not-found",
        message: `cannot load plan ${planId}: unknown plan id`
      });
    }
    const plan = await this.createCleanupPlan(stored.itemIds, policy, stored.scanGeneration);
    const sessionId = this.nextSessionId++;
    const results = [];
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    for (const p of plan.items) {
      const it = byId.get(p.scanItemId);
      const status = dryRun ? "would" : "ok";
      const action = p.action;
      results.push({
        scanItemId: p.scanItemId,
        path: p.path,
        product: it?.product ?? null,
        action,
        estimatedSize: p.estimatedSize,
        status,
        detail: dryRun ? `would ${action}` : null
      });
      await sleep(140 + Math.random() * 120);
    }
    const okItems = results.filter((r) => r.status === "ok" || r.status === "would");
    const session = {
      sessionId,
      planId,
      dryRun,
      totals: {
        plannedBytes: plan.totalEstimatedBytes,
        completedBytes: okItems.reduce((s, r) => s + r.estimatedSize, 0),
        succeeded: dryRun ? 0 : okItems.length,
        skipped: plan.skipped.length,
        failed: 0
      },
      items: results,
      journalDegraded: false
    };
    if (!dryRun) {
      this.appendJournal(session);
      const removed = new Set(plan.items.map((p) => p.scanItemId));
      if (this.snapshot) {
        this.snapshot = {
          ...this.snapshot,
          items: this.snapshot.items.filter((it) => !removed.has(it.id))
        };
      }
    }
    return session;
  }
  appendJournal(session) {
    for (const r of session.items) {
      this.journal.push({
        sessionId: session.sessionId,
        timeSecs: now(),
        phase: "result",
        product: r.product,
        rule: null,
        provider: null,
        path: r.path,
        action: r.action,
        estimatedSize: r.estimatedSize,
        result: r.status,
        error: null
      });
    }
  }
  async getJournal(lastN) {
    const limit = lastN ?? 50;
    return this.journal.slice(-limit).reverse();
  }
  async clearAllData() {
    const cleared = this.journal.length;
    this.journal = [];
    this.snapshot = null;
    this.plans.clear();
    this.nextPlanId = 1;
    this.nextSessionId = 1;
    this.nextGeneration = 0;
    return cleared;
  }
  // ---- M4 commands (wire-aligned with contract.rs) ----------------------------
  async openFolder(itemId) {
    const snap = await this.getScanResults();
    if (!snap.items.some((it) => it.id === itemId)) {
      throw toCommandError({
        code: "invalid-item",
        message: `item id ${itemId} does not belong to the latest scan`
      });
    }
  }
  async setDisposition(itemId, disposition) {
    const snap = await this.getScanResults();
    const it = snap.items.find((i) => i.id === itemId);
    if (!it) {
      throw toCommandError({
        code: "invalid-item",
        message: `item id ${itemId} does not belong to the latest scan`
      });
    }
    if (it.risk !== "unknown") {
      throw toCommandError({
        code: "invalid-item",
        message: "dispositions apply to unknown-risk items only"
      });
    }
    this.dispositions.set(it.path, disposition);
    this.persistDispositions();
    if (disposition === "ignore") {
      this.removeSnapshotItem(itemId);
    } else {
      this.applyDispositionToSnapshot(itemId, "protected", "protect");
    }
    return {
      itemId,
      ruleId: `user-disposition/${slugOf(it.path)}`,
      path: it.path,
      effect: disposition === "ignore" ? "ignored; will not appear in future scans" : "protected; future scans list it as Protected"
    };
  }
  async listAiProfiles() {
    this.loadPersisted();
    return this.aiProfileState();
  }
  async upsertAiProfile(input, apiKey) {
    void apiKey;
    const name = input.name.trim();
    const baseUrl = input.baseUrl.trim();
    const model = input.model.trim();
    if (!name || !baseUrl || !model || !Number.isSafeInteger(input.timeoutSecs) || input.timeoutSecs < 1) {
      throw aiMockError("ai-not-configured", "\u8FDC\u7A0B AI \u914D\u7F6E\u4E0D\u5B8C\u6574");
    }
    const id2 = input.profileId ?? `mock-ai-${Date.now()}-${this.aiProfiles.length + 1}`;
    const profile = {
      id: id2,
      name,
      baseUrl,
      model,
      enabled: input.enabled
    };
    const index = this.aiProfiles.findIndex((candidate) => candidate.id === id2);
    if (index >= 0) this.aiProfiles[index] = profile;
    else this.aiProfiles.push(profile);
    if (this.activeAiProfileId === null) this.activeAiProfileId = id2;
    this.clearAiBatches();
    this.persistAiProfiles();
    return this.profileDto(profile);
  }
  async deleteAiProfile(profileId) {
    const before = this.aiProfiles.length;
    this.aiProfiles = this.aiProfiles.filter((profile) => profile.id !== profileId);
    if (this.aiProfiles.length === before) {
      throw aiMockError("ai-not-configured", "\u8FDC\u7A0B AI \u914D\u7F6E\u4E0D\u53EF\u7528");
    }
    if (this.activeAiProfileId === profileId) this.activeAiProfileId = null;
    this.clearAiBatches();
    this.persistAiProfiles();
  }
  async setActiveAiProfile(profileId) {
    if (profileId !== null && !this.aiProfiles.some((profile) => profile.id === profileId)) {
      throw aiMockError("ai-not-configured", "\u8FDC\u7A0B AI \u914D\u7F6E\u4E0D\u53EF\u7528");
    }
    this.activeAiProfileId = profileId;
    this.clearAiBatches();
    this.persistAiProfiles();
    return this.aiProfileState();
  }
  async setAiMasterEnabled(enabled) {
    this.aiMasterEnabled = enabled;
    if (!enabled) this.clearAiBatches();
    this.persistAiProfiles();
    return this.aiProfileState();
  }
  async testAiConnection(profileId) {
    this.requireReadyAiProfile(profileId);
  }
  async prepareAiBatch(profileId, scanGeneration, itemIds) {
    this.requireReadyAiProfile(profileId);
    const snapshot = await this.getScanResults();
    if (snapshot.generation !== scanGeneration) {
      throw aiMockError("ai-batch-expired", "\u8FDC\u7A0B AI \u6279\u6B21\u4E0D\u518D\u5339\u914D\u5F53\u524D\u626B\u63CF");
    }
    if (itemIds.length === 0 || new Set(itemIds).size !== itemIds.length) {
      throw toCommandError({ code: "invalid-item", message: "\u8BF7\u9009\u62E9\u4E0D\u91CD\u590D\u7684\u53EF\u7814\u5224\u6761\u76EE" });
    }
    const byId = new Map(snapshot.items.map((item2) => [item2.id, item2]));
    const selected = itemIds.map((id2) => byId.get(id2));
    if (selected.some(
      (item2) => item2 === void 0 || item2.risk !== "unknown" && item2.risk !== "review"
    )) {
      throw toCommandError({
        code: "invalid-item",
        message: "\u4EC5 Unknown \u6216 Review \u6761\u76EE\u53EF\u8FDB\u5165\u8FDC\u7A0B AI \u7814\u5224"
      });
    }
    const entries = selected.map((item2) => preparedMockEntry(item2));
    const batch = {
      batchId: `mock-ai-batch-${this.nextAiBatchId++}`,
      scanGeneration,
      profileId,
      entries
    };
    this.aiBatches.set(batch.batchId, {
      batch,
      suggestions: entries.map(mockAiSuggestion)
    });
    return batch;
  }
  async analyzeAiBatch(batchId) {
    const batch = this.aiBatches.get(batchId);
    if (!batch) throw aiMockError("ai-batch-expired", "\u8FDC\u7A0B AI \u6279\u6B21\u4E0D\u518D\u5339\u914D\u5F53\u524D\u626B\u63CF");
    if (this.activeAiBatchId !== null && this.activeAiBatchId !== batchId) {
      throw aiMockError("ai-batch-in-progress", "\u5DF2\u6709\u8FDC\u7A0B AI \u7814\u5224\u6B63\u5728\u8FDB\u884C");
    }
    this.activeAiBatchId = batchId;
    try {
      return batch.suggestions.map((suggestion) => ({ ...suggestion }));
    } finally {
      this.activeAiBatchId = null;
    }
  }
  async confirmAiBatch(batchId, scanGeneration, items) {
    const stored = this.aiBatches.get(batchId);
    if (!stored || stored.batch.scanGeneration !== scanGeneration) {
      throw aiMockError("ai-batch-expired", "\u8FDC\u7A0B AI \u6279\u6B21\u4E0D\u518D\u5339\u914D\u5F53\u524D\u626B\u63CF");
    }
    if (items.length === 0 || new Set(items.map((item2) => item2.itemId)).size !== items.length) {
      throw toCommandError({ code: "invalid-item", message: "\u8BF7\u9009\u62E9\u4E0D\u91CD\u590D\u7684\u5EFA\u8BAE\u540E\u518D\u786E\u8BA4" });
    }
    const eligible = new Set(stored.suggestions.map((suggestion) => suggestion.itemId));
    if (items.some((item2) => !eligible.has(item2.itemId) || !isAiFinalRisk(item2.finalRisk))) {
      throw toCommandError({ code: "invalid-item", message: "\u786E\u8BA4\u5185\u5BB9\u4E0D\u5C5E\u4E8E\u5F53\u524D\u8FDC\u7A0B AI \u6279\u6B21" });
    }
    const latest = await this.getScanResults();
    if (latest.generation !== scanGeneration) {
      throw aiMockError("ai-batch-expired", "\u8FDC\u7A0B AI \u6279\u6B21\u4E0D\u518D\u5339\u914D\u5F53\u524D\u626B\u63CF");
    }
    const finalRisks = new Map(items.map((item2) => [item2.itemId, item2.finalRisk]));
    this.snapshot = {
      ...latest,
      items: latest.items.map((item2) => {
        const finalRisk = finalRisks.get(item2.id);
        if (!finalRisk) return item2;
        return {
          ...item2,
          risk: finalRisk,
          category: riskCategory(finalRisk),
          // Like the real Core transaction, mock confirmation classifies but
          // never builds or executes a cleanup plan.
          cleanup_action: { kind: "none" },
          explanation: "\u7531\u4F60\u5BA1\u9605\u8FDC\u7A0B AI \u5EFA\u8BAE\u540E\u521B\u5EFA\u7684\u672C\u5730\u5206\u7C7B\u89C4\u5219\u3002",
          evidence: [
            ...item2.evidence,
            { rule_id: null, source: "ai-advisor", detail: "user-confirmed fixture classification" }
          ]
        };
      })
    };
    this.aiBatches.delete(batchId);
    return { confirmedCount: items.length, auditWarning: false };
  }
  async cancelAiBatch(batchId) {
    if (this.activeAiBatchId !== batchId) return false;
    this.activeAiBatchId = null;
    return true;
  }
  async getRules() {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await sleep(120);
    return mockRuleDtos();
  }
  async validateRules() {
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    await sleep(180);
    return mockRulesValidation();
  }
  aiProfileState() {
    return {
      masterEnabled: this.aiMasterEnabled,
      profiles: this.aiProfiles.map((profile) => this.profileDto(profile))
    };
  }
  profileDto(profile) {
    return {
      id: profile.id,
      name: profile.name,
      baseUrl: profile.baseUrl,
      model: profile.model,
      enabled: profile.enabled,
      isActive: profile.id === this.activeAiProfileId
    };
  }
  requireReadyAiProfile(profileId) {
    if (!this.aiMasterEnabled) {
      throw aiMockError("ai-disabled", "\u8FDC\u7A0B AI \u5DF2\u5173\u95ED");
    }
    const profile = this.aiProfiles.find((candidate) => candidate.id === profileId);
    if (!profile || !profile.enabled || this.activeAiProfileId !== profileId) {
      throw aiMockError("ai-not-configured", "\u8FDC\u7A0B AI \u914D\u7F6E\u4E0D\u53EF\u7528");
    }
    return profile;
  }
  clearAiBatches() {
    this.activeAiBatchId = null;
    this.aiBatches.clear();
  }
  /** Removes an ignored item from the live snapshot (mirrors the next scan). */
  removeSnapshotItem(itemId) {
    if (!this.snapshot) return;
    this.snapshot = {
      ...this.snapshot,
      items: this.snapshot.items.filter((x) => x.id !== itemId)
    };
  }
  /**
   * Applies a decision to the live snapshot so the UI reflects it
   * immediately (without a re-scan). `ignored` removes the item entirely;
   * any other risk re-classifies it (fixed flip or accepted suggestion).
   */
  applyDispositionToSnapshot(itemId, risk, evidenceKind) {
    if (!this.snapshot) return;
    if (risk === "unknown") return;
    const detail = evidenceKind === "rule" ? `classified via accepted remote AI review (risk: ${risk})` : risk === "protected" ? "protected via Unknown page disposition" : `classified via disposition (risk: ${risk})`;
    this.snapshot = {
      ...this.snapshot,
      items: this.snapshot.items.map(
        (x) => x.id === itemId ? {
          ...x,
          risk,
          category: riskCategory(risk),
          explanation: risk === "protected" ? "Marked protected by your disposition \u2014 DevResidue will never plan it for deletion." : `Re-classified from an accepted suggestion (${risk}); classified by a user rule.`,
          cleanup_action: { kind: "none" },
          evidence: [...x.evidence, {
            rule_id: null,
            source: "user-disposition",
            detail
          }]
        } : x
      )
    };
  }
};
function readStoredMockAiProfile(value) {
  if (typeof value !== "object" || value === null) return [];
  const candidate = value;
  if (typeof candidate.id !== "string" || typeof candidate.name !== "string" || typeof candidate.baseUrl !== "string" || typeof candidate.model !== "string" || typeof candidate.enabled !== "boolean") {
    return [];
  }
  return [
    {
      id: candidate.id,
      name: candidate.name,
      baseUrl: candidate.baseUrl,
      model: candidate.model,
      enabled: candidate.enabled
    }
  ];
}
function aiMockError(code, message) {
  return toCommandError({ code, message });
}
function preparedMockEntry(item2) {
  const ageDays = item2.last_modified === null ? null : (now() - item2.last_modified) / DAY;
  return {
    itemId: item2.id,
    zone: "user-data",
    relativeDepth: 2,
    displayName: item2.display_name,
    sourceKind: item2.source,
    categoryHint: item2.category,
    productHint: item2.product,
    sizeBucket: sizeBucket(item2.logical_size),
    ageBucket: ageBucket(ageDays),
    signals: ["local-fixture", item2.risk === "review" ? "review-risk" : "unknown-risk"]
  };
}
function mockAiSuggestion(entry) {
  const suggestedRisk = entry.categoryHint === "session" || entry.signals.includes("review-risk") ? "review" : "safe";
  return {
    itemId: entry.itemId,
    suggestedRisk,
    confidence: suggestedRisk === "review" ? 0.68 : 0.61,
    reason: suggestedRisk === "review" ? "\u6F14\u793A\u5939\u5177\uFF1A\u4F1A\u8BDD\u6216\u65E2\u6709 Review \u4FE1\u53F7\u9700\u4FDD\u5B88\u5904\u7406\uFF0C\u8BF7\u9010\u9879\u51B3\u5B9A\u6700\u7EC8\u98CE\u9669\u3002" : "\u6F14\u793A\u5939\u5177\uFF1A\u4EC5\u6839\u636E\u5DF2\u5C55\u793A\u7684\u8131\u654F\u5143\u6570\u636E\u7ED9\u51FA\u4F4E\u7F6E\u4FE1\u5EA6\u5EFA\u8BAE\uFF0C\u8BF7\u9010\u9879\u51B3\u5B9A\u6700\u7EC8\u98CE\u9669\u3002",
    productGuess: entry.productHint,
    finalRiskOptions: [
      "safe",
      "regenerable-local",
      "regenerable-download",
      "review",
      "protected"
    ]
  };
}
function isAiFinalRisk(value) {
  return value === "safe" || value === "regenerable-local" || value === "regenerable-download" || value === "review" || value === "protected";
}
function sizeBucket(size) {
  if (size < MiB) return "under-1MiB";
  if (size < 100 * MiB) return "1MiB-100MiB";
  if (size < GiB) return "100MiB-1GiB";
  return "over-1GiB";
}
function ageBucket(days) {
  if (days === null) return "unknown";
  if (days < 7) return "under-7d";
  if (days < 30) return "7d-30d";
  if (days < 90) return "30d-90d";
  return "over-90d";
}
function slugOf(path) {
  return path.toLowerCase().replace(/[\\/]+/g, "-").replace(/[^a-z0-9-]/g, "").replace(/^-+|-+$/g, "");
}
function riskCategory(risk) {
  switch (risk) {
    case "safe":
      return "temporary";
    case "regenerable-local":
      return "build-artifact";
    case "regenerable-download":
      return "dependency";
    case "review":
      return "session";
    case "protected":
      return "credential";
    case "unknown":
      return "unknown";
  }
}
export {
  MockBackend,
  onScanGeneration,
  useAiStore,
  useScanStore,
  useSelectionStore,
  wireGenerationLifecycle
};
