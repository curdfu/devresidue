import { create } from "zustand";
import type {
  CommandError,
  ScanHandleDto,
  ScanItemDto,
  ScanScope,
  ScanSnapshotDto,
} from "@/types";
import { getBackend } from "@/adapters";
import { subscribeScan, type ScanSubscription } from "@/adapters/events";

export type ScanPhase = "idle" | "scanning" | "done" | "cancelled" | "error";

export interface ProviderProgress {
  provider: string;
  /** started | done | project */
  stage: string;
}

interface ScanState {
  phase: ScanPhase;
  scanId: number | null;
  scope: ScanScope;
  items: ScanItemDto[];
  warnings: string[];
  scannedAt: number | null;
  /** Current snapshot generation (R04). */
  generation: number;
  cancelled: boolean;
  progress: ProviderProgress[];
  error: CommandError | null;
  lastError: CommandError | null;
  /** One-shot notice when a new generation cleared dependent state (R04). */
  generationNotice: string | null;

  /**
   * R10c: the snapshot key (generation, else scanned_at) that was on disk
   * when this scan was *initiated*. The done-handler only settles when the
   * fetched snapshot's key advances past it — otherwise it has read the
   * previous scan's still-authoritative result and must retry.
   */
  diskKeyAtStart: number;

  /**
   * F10 (R10): the epoch of the scan currently being driven. `startScan`
   * increments a monotonic counter and records it here; the command
   * response and every terminal event carry the epoch they belong to, so a
   * stale response or event from an earlier aborted attempt cannot mutate
   * the active scan's state.
   */
  activeEpoch: number;

  /** Live count of streamed items during a scan. */
  streamedCount: number;

  startScan: (scope: ScanScope) => Promise<void>;
  cancelScan: () => Promise<void>;
  loadLatest: () => Promise<boolean>;
  dismissGenerationNotice: () => void;
  reset: () => void;
}

let subscription: ScanSubscription | null = null;

/** Monotonic scan epoch (F10): incremented on every startScan call. */
let epochCounter = 0;

/**
 * High-volume item events are published in short batches. A real scan can
 * discover many entries in a single event-loop turn; copying the complete
 * items array for every entry makes the renderer compete with the scanner.
 */
const ITEM_FLUSH_INTERVAL_MS = 50;
let pendingItems: ScanItemDto[] = [];
let pendingItemsEpoch: number | null = null;
let itemFlushTimer: ReturnType<typeof setTimeout> | null = null;

function clearPendingItems(): void {
  if (itemFlushTimer !== null) {
    clearTimeout(itemFlushTimer);
    itemFlushTimer = null;
  }
  pendingItems = [];
  pendingItemsEpoch = null;
}

function flushPendingItems(epoch: number): void {
  if (itemFlushTimer !== null) {
    clearTimeout(itemFlushTimer);
    itemFlushTimer = null;
  }
  if (pendingItems.length === 0 || pendingItemsEpoch !== epoch) return;

  const batch = pendingItems;
  pendingItems = [];
  pendingItemsEpoch = null;
  const current = useScanStore.getState();
  if (current.activeEpoch !== epoch || current.phase !== "scanning") return;

  useScanStore.setState((s) => ({
    items: [...s.items, ...batch],
    streamedCount: s.streamedCount + batch.length,
  }));
}

function scheduleItemFlush(epoch: number): void {
  if (itemFlushTimer !== null) return;
  itemFlushTimer = setTimeout(() => {
    itemFlushTimer = null;
    flushPendingItems(epoch);
  }, ITEM_FLUSH_INTERVAL_MS);
}

/** Snapshot freshness key: generation when present, else scanned_at. */
function snapshotKey(snap: ScanSnapshotDto): number {
  return snap.generation > 0 ? snap.generation : snap.scanned_at;
}

export const useScanStore = create<ScanState>((set, get) => ({
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
    // R10c: capture the disk snapshot key BEFORE starting, so the done
    // handler can distinguish "the new scan's snapshot" from "the previous
    // scan's snapshot that is still authoritative for a moment".
    let diskKeyAtStart = 0;
    try {
      const before = await getBackend().getScanResults();
      diskKeyAtStart = snapshotKey(before);
    } catch {
      // No snapshot yet (first scan) — any published result is new.
      diskKeyAtStart = 0;
    }

    // F10: a fresh scan opens a new epoch. The old scanId is cleared so it
    // can no longer filter events of the *new* scan (the previous scan is
    // already terminal — it cannot emit anything anymore; under the
    // single-active-scan contract, anything that arrives while we are
    // scanning belongs to THIS attempt).
    const epoch = ++epochCounter;
    // R4-H06: per-id parked terminals of previous epochs are void now —
    // the new epoch owns the terminal state; drop stale entries wholesale.
    clearPendingItems();
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
      activeEpoch: epoch,
    });

    // R10a: arm the listeners BEFORE invoking scan and await the ready
    // barrier — a fast task can otherwise emit `done` before any handler
    // exists (review case 2).
    if (!subscription) {
      subscription = subscribeScan(makeHandlers());
    }
    await subscription.ready;

    try {
      const backend = getBackend();
      const handle: ScanHandleDto = await backend.scan(scope);
      // F10: only the response of the CURRENT epoch may bind the scanId;
      // a stale response from an abandoned attempt is discarded.
      if (useScanStore.getState().activeEpoch !== epoch) return;
      set({ scanId: handle.scanId });
      // R10b/F10 + R4-H06: terminal events that raced ahead of the response
      // were parked per scan id; replay exactly the entry for the bound id.
      replayPendingTerminalFor(epoch, handle.scanId);
    } catch (raw) {
      if (useScanStore.getState().activeEpoch !== epoch) return;
      const err = raw as CommandError;
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
      const snap: ScanSnapshotDto = await getBackend().getScanResults();
      const prevKey = snapshotKeyOf(get());
      const nextKey = snapshotKey(snap);
      const items = snap.items;
      const streamedCount = items.length;
      const scannedAt = snap.scanned_at;
      const cancelled = snap.cancelled;
      const generation = snap.generation;
      // R04: a new generation invalidates every id-keyed UI state —
      // selections, plans and remote AI review state reference ids from the
      // OLD generation and would silently retarget new objects otherwise.
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
    clearPendingItems();
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
      generation: 0,
    });
  },
}));

function snapshotKeyOf(s: ScanState): number {
  return s.generation > 0 ? s.generation : (s.scannedAt ?? 0);
}

// ---- Event handlers (module-scope so the store can swap state freely) ------
//
// Wire field names are snake_case (contract.rs event payloads carry no
// rename_all): p.scan_id, p.message, …

function makeHandlers() {
  return {
    onProgress: (p: { scan_id: number; provider: string; stage: string }) => {
      if (rejectStale(p.scan_id)) return;
      useScanStore.setState((s) => ({
        // Keep the visible activity trail bounded during long scans.
        progress: [...s.progress, { provider: p.provider, stage: p.stage }].slice(-64),
      }));
    },
    onItem: (p: { scan_id: number; item: ScanItemDto }) => {
      if (rejectStale(p.scan_id)) return;
      const epoch = useScanStore.getState().activeEpoch;
      if (pendingItemsEpoch !== epoch) {
        clearPendingItems();
        pendingItemsEpoch = epoch;
      }
      pendingItems.push(p.item);
      scheduleItemFlush(epoch);
    },
    onWarning: (p: { scan_id: number; message: string }) => {
      if (rejectStale(p.scan_id)) return;
      useScanStore.setState((s) => ({ warnings: [...s.warnings, p.message] }));
    },
    onDone: (p: {
      scan_id: number;
      cancelled: boolean;
      total: number;
      generation: number;
    }) => {
      // F10/R10 terminal attribution: while a scan is in flight, ANY done
      // belongs to the active epoch — the previous scan is already terminal
      // and cannot emit again (single-active-scan backend). Late duplicates
      // of a done we already processed (generation older than what's on
      // screen AND we're terminal) are discarded.
      const s = useScanStore.getState();
      if (s.phase !== "scanning") {
        const shown = snapshotKeyOf(s);
        if (p.generation > 0 && shown > 0 && p.generation < shown) {
          return; // late duplicate of an older scan's finish
        }
        return; // terminal already reached for this view
      }
      // Scanning: park-or-finalise. If the command response has not bound
      // the scanId yet, park the event against the active epoch so the
      // response handler replays it (F10 — this is exactly the
      // second-scan-with-stale-previous-id window).
      if (s.scanId === null) {
        // R3-G07 + R4-H06: keep the event's OWN scan id in a per-id slot —
        // the replay compares it against the response-bound id, and a
        // *previous* scan's delayed terminal can neither be attributed to
        // the new scan nor evict the new scan's own parked terminal.
        parkTerminal({ kind: "done", epoch: s.activeEpoch, scanId: p.scan_id, payload: p });
        return;
      }
      if (p.scan_id !== s.scanId) {
        // R3-G07: a KNOWN-but-different id while scanning is not "ours
        // arriving early" — the single-active-scan backend never emits a
        // foreign id for the active scan. This is a delayed terminal of an
        // already-finished scan (its worker emitted before we started but
        // the event was delivered late). Drop it; never park it against the
        // active epoch, never let it terminalise the new scan.
        return;
      }
      flushPendingItems(s.activeEpoch);
      void finaliseDone(p);
    },
    onError: (p: { scan_id: number; message: string }) => {
      // F10/R10: worker failed before a snapshot was finalised — never a
      // done. Same attribution rule as done: while scanning, any error
      // belongs to the active epoch (old scans are terminal and silent).
      const s = useScanStore.getState();
      if (s.phase !== "scanning") return; // stale error from an old scan
      if (s.scanId !== null) {
        if (p.scan_id === s.scanId) {
          failScan(p.message);
          return;
        }
        // R3-G07: known id mismatch = delayed terminal of a previous scan —
        // discard instead of failing the active scan.
        return;
      }
      // Response not bound yet: park for replay keyed by the event's own
      // scan id (R4-H06: per-id slots, error-wins only for the SAME id).
      parkTerminal({ kind: "error", epoch: s.activeEpoch, scanId: p.scan_id, message: p.message });
    },
  };
}

/** Applies the failure terminal (used by both the live and replay paths). */
function failScan(message: string): void {
  flushPendingItems(useScanStore.getState().activeEpoch);
  const err: CommandError = { code: "engine", message };
  useScanStore.setState({ phase: "error", error: err, lastError: err });
}

/** Replays a parked terminal event for the epoch it was recorded against. */
function replayTerminal(pending: PendingTerminal): void {
  // Belt-and-braces: the store must still be in this epoch's scan.
  if (useScanStore.getState().activeEpoch !== pending.epoch) return;
  // R3-G07: exact scan-id match required — the parked event belongs to the
  // active scan only when the response-bound id equals the event's own id.
  // A delayed terminal of a *previous* scan (parked while the response was
  // pending) is discarded here instead of being replayed onto the new scan.
  const bound = useScanStore.getState().scanId;
  if (bound === null || bound !== pending.scanId) return;
  if (pending.kind === "error") {
    failScan(pending.message);
    return;
  }
  flushPendingItems(pending.epoch);
  void finaliseDone(pending.payload);
}

/**
 * Takes and replays the parked terminal matching the response-bound scan id
 * for the given epoch (R4-H06: per-id slots — only the entry whose OWN scan
 * id equals the bound id and whose epoch matches is consumed; every other
 * parked entry stays for its scan / gets discarded when its epoch passes).
 */
function replayPendingTerminalFor(epoch: number, boundScanId: number): void {
  const pending = pendingTerminals.get(boundScanId);
  if (pending === undefined || pending.epoch !== epoch) return;
  pendingTerminals.delete(boundScanId);
  replayTerminal(pending);
}

/** True when the event belongs to a scan we are no longer in. */
function rejectStale(eventScanId: number): boolean {
  const s = useScanStore.getState();
  if (s.phase !== "scanning" && s.phase !== "done" && s.phase !== "cancelled") return true;
  const known = s.scanId;
  // While scanning: unknown id means the response hasn't landed yet —
  // accept (single active scan) rather than drop. A KNOWN-but-different id
  // during scanning is likewise the new scan's event arriving before the
  // response rebinding (F10): accept, the epoch guards the rest.
  if (known === null || known !== eventScanId) return s.phase !== "scanning";
  return false;
}

/**
 * Terminal events that raced ahead of the scan command's response (F10).
 * R4-H06: keyed by **scan id** (not a single overwrite-slot) — the new
 * scan's own terminal can never be evicted by a delayed terminal of a
 * *previous* scan arriving while the response is still pending. Each entry
 * also carries the epoch it was parked against. The error kind wins over a
 * parked done for the SAME scan id (a failed scan never finishes).
 */
type PendingTerminal =
  | {
      kind: "done";
      epoch: number;
      scanId: number;
      payload: { cancelled: boolean; generation: number };
    }
  | { kind: "error"; epoch: number; scanId: number; message: string };

/** R4-H06: parked terminals by scan id (the single-slot map evicted new scans). */
let pendingTerminals = new Map<number, PendingTerminal>();

/** Parks a terminal against its OWN scan id (never evicting another scan's). */
function parkTerminal(entry: PendingTerminal): void {
  // Same-scan replacement semantics: an error replaces a done (a failed scan
  // never finishes); a done does NOT replace a parked error.
  const existing = pendingTerminals.get(entry.scanId);
  if (existing?.kind === "error" && entry.kind === "done") {
    return;
  }
  pendingTerminals.set(entry.scanId, entry);
}

/**
 * Finalises a scan: pulls the authoritative snapshot and only settles when
 * the persisted snapshot is NEWER than the done event's generation (and the
 * disk state at scan start — R10c; the backend guarantees finish_scan
 * precedes the done emit, so this is an expiry check: a done carrying an
 * older generation than what we already show is stale and gets discarded).
 * Retries with backoff while the snapshot lags, then gives up with whatever
 * it has so the UI never hangs in `scanning`.
 */
async function finaliseDone(p: {
  cancelled: boolean;
  generation: number;
}): Promise<void> {
  // Disk state at scan start (R10c): a read below this is the previous
  // scan's still-authoritative result. The target is the max of that and
  // the generation the done event claims (a stale done's generation is
  // lower — the disk's newer value wins, so the UI never regresses).
  // R3-G07: finaliseDone runs without the triggering event's epoch — the
  // attribution was already settled by the caller (exact scan-id match at
  // onDone / replay). This body only settles data state; it never flips a
  // phase unless the store is still in the scan it was called for.
  const baseline = useScanStore.getState().diskKeyAtStart;
  const want = Math.max(p.generation, baseline);
  const maxAttempts = 12;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const ok = await useScanStore.getState().loadLatest();
    if (ok) {
      const after = snapshotKeyOf(useScanStore.getState());
      // Authoritative-enough: the disk holds at least the generation the
      // done event refers to (the backend guarantees finish_scan precedes
      // the emit, so a smaller value means we read the previous scan's
      // still-authoritative result and should retry).
      if (after >= want) {
        settleTerminal(p.cancelled);
        return;
      }
      // Never-persisted snapshot (generation 0 / scanned_at 0): treat an
      // empty result as terminal for cancelled scans so cancel still lands.
      if (after === 0 && p.cancelled) {
        useScanStore.setState({ phase: "cancelled", cancelled: true });
        return;
      }
    }
    // Authoritative state not published yet — brief backoff, then retry.
    await new Promise((r) => setTimeout(r, Math.min(40 * 2 ** (attempt - 1), 500)));
  }
  // Stale after retries (disk write failure / backend bug): settle with what
  // we have so the UI is never stuck in `scanning` (review case 3's hang).
  settleTerminal(p.cancelled);
}

/**
 * R3-G07: writes the terminal phase only when the store still expects it —
 * while scanning (the active scan's own finalisation) or already terminal
 * (idempotent re-settle). A store that went back to `idle` (reset) is left
 * alone.
 */
function settleTerminal(cancelled: boolean): void {
  const phase = useScanStore.getState().phase;
  if (phase === "idle") return; // reset while we waited: leave it be
  const next: ScanPhase = cancelled ? "cancelled" : "done";
  useScanStore.setState({ phase: next, cancelled });
}

/**
 * R04 fan-out: a new generation invalidates id-keyed dependent state. The
 * module-scope hook list keeps the stores decoupled from the scan store.
 */
type GenerationListener = (key: number) => void;
const generationListeners: GenerationListener[] = [];

export function onScanGeneration(listener: GenerationListener): void {
  generationListeners.push(listener);
}

function onGenerationAdvanced(key: number): void {
  useScanStore.setState({
    generationNotice: "已清空勾选：出现了新的扫描结果",
  });
  for (const fn of [...generationListeners]) fn(key);
}

/** Test seam: clears parked terminal state (store-probe form). */
export const __resetForTests = (): void => {
  pendingTerminals.clear();
};
