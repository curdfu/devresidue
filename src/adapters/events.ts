import type {
  CleanupItemPayload,
  ProgressPayload,
  ScanItemPayload,
  WarningPayload,
  ScanDonePayload,
  ScanErrorPayload,
} from "@/types";
import { SCAN_EVENTS, CLEANUP_EVENTS } from "@/types";
import { getBackend } from "@/adapters";
import { MockBackend } from "@/adapters/mockBackend";

/**
 * Unifies Tauri's `listen` and the mock event bus behind one subscription
 * surface. The scan store calls `subscribeScan` once at startup; cleanup
 * item events are handled by polling the session result (the backend
 * resolves the full session — simpler and identical in both backends).
 */
export interface ScanEventHandlers {
  onProgress?: (p: ProgressPayload) => void;
  onItem?: (p: ScanItemPayload) => void;
  onWarning?: (p: WarningPayload) => void;
  onDone?: (p: ScanDonePayload) => void;
  /** R10: worker failed before a snapshot finalised (scan://error). */
  onError?: (p: ScanErrorPayload) => void;
}

export interface CleanupEventHandlers {
  onItem?: (p: CleanupItemPayload) => void;
}

type Unsubscribe = () => void;

/** A subscription that reports when its listeners are actually armed. */
export interface ScanSubscription {
  /** Resolves once every handler is registered on the event source. */
  ready: Promise<void>;
  /** Detaches all handlers (idempotent). */
  close: Unsubscribe;
}

/**
 * R10a: subscribes and returns a ready barrier. Callers MUST await `ready`
 * before starting work that can emit events, otherwise a fast task can emit
 * `done` before the listeners exist (review case 2).
 */
export function subscribeScan(handlers: ScanEventHandlers): ScanSubscription {
  const backend = getBackend();

  if (backend instanceof MockBackend) {
    const offs: Unsubscribe[] = [];
    if (handlers.onProgress) offs.push(backend.onScanProgress(handlers.onProgress));
    if (handlers.onItem) offs.push(backend.onScanItem(handlers.onItem));
    if (handlers.onWarning) offs.push(backend.onScanWarning(handlers.onWarning));
    if (handlers.onDone) offs.push(backend.onScanDone(handlers.onDone));
    if (handlers.onError) offs.push(backend.onScanError(handlers.onError));
    return { ready: Promise.resolve(), close: () => offs.forEach((off) => off()) };
  }

  // Tauri path: dynamic import keeps browser dev from loading the API.
  let closed = false;
  const realOffs: Unsubscribe[] = [];

  const ready = (async () => {
    const { listen } = await import("@tauri-apps/api/event");
    const add = async (event: string, fn: (p: unknown) => void) => {
      const off = await listen(event, (e) => fn(e.payload));
      realOffs.push(off);
    };
    if (handlers.onProgress)
      await add(SCAN_EVENTS.progress, (p) => handlers.onProgress?.(p as ProgressPayload));
    if (handlers.onItem)
      await add(SCAN_EVENTS.item, (p) => handlers.onItem?.(p as ScanItemPayload));
    if (handlers.onWarning)
      await add(SCAN_EVENTS.warning, (p) => handlers.onWarning?.(p as WarningPayload));
    if (handlers.onDone)
      await add(SCAN_EVENTS.done, (p) => handlers.onDone?.(p as ScanDonePayload));
    if (handlers.onError)
      await add(SCAN_EVENTS.error, (p) => handlers.onError?.(p as ScanErrorPayload));
    // If close() raced the wiring, detach immediately.
    if (closed) realOffs.forEach((off) => off());
  })();

  return {
    ready,
    close: () => {
      closed = true;
      realOffs.forEach((off) => off());
    },
  };
}

export function subscribeCleanup(handlers: CleanupEventHandlers): Unsubscribe {
  const backend = getBackend();

  // The mock streams nothing (execute resolves after its internal delays);
  // Tauri emits cleanup://item — used purely for live progress feel.
  if (backend instanceof MockBackend) return () => undefined;

  let cancelled = false;
  const realOffs: Unsubscribe[] = [];
  void (async () => {
    const { listen } = await import("@tauri-apps/api/event");
    const off = await listen(CLEANUP_EVENTS.item, (e) =>
      handlers.onItem?.(e.payload as CleanupItemPayload),
    );
    realOffs.push(off);
    if (cancelled) off();
  })();
  return () => {
    cancelled = true;
    realOffs.forEach((off) => off());
  };
}
