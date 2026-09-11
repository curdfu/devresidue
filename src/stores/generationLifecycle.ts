import { onScanGeneration } from "@/stores/scanStore";
import { useSelectionStore } from "@/stores/selectionStore";
import { useCleanupStore } from "@/stores/cleanupStore";
import { useAiStore } from "@/stores/aiStore";

/**
 * R04 wiring: when a new snapshot generation lands (re-scan finished), every
 * id-keyed UI state from the previous generation is invalidated — ids are
 * only unique *within* a scan, so stale selections/plans/suggestions would
 * silently retarget whatever the new scan assigned those ids.
 *
 * Called once at app startup (App.tsx). Clearing is the intended safety
 * semantic (review R04): a re-scan means the user re-selects.
 */
let wired = false;

export function wireGenerationLifecycle(): void {
  if (wired) return;
  wired = true;

  onScanGeneration((key) => {
    // Clear user selections — the top-priority retarget hazard.
    useSelectionStore.getState().clear();

    // A plan under construction references old-generation item ids; the
    // backend would reject them — clear proactively so the flow restarts.
    const cleanup = useCleanupStore.getState();
    if (cleanup.step !== "selecting") {
      cleanup.close();
    }

    // Remote-AI batches bind item ids to one HMAC-verified snapshot. They
    // must never survive a generation advance, even if the numeric ids are
    // reused by a later scan.
    useAiStore.getState().resetForNewScan(key);

    // Keep the key referenced for logging symmetry with the store's notice.
    if (key < 0) {
      // unreachable; generation keys are monotonically positive
      console.warn("[devresidue] unexpected generation key", key);
    }
  });
}
