// Batch-3 store probes (review R04/R10): bundles the REAL stores, replacing
// only the I/O layer with inert fakes — the same shape as the review's
// store-probes.mjs, converted from "defect reproductions" to "fix
// verifications". Run from the repo root:
//
//   node src/testing/storeProbes.mjs
//
// Scenarios:
//   1. R04 — re-scan clears the selection when the generation advances.
//   2. R10a/b — a done that fires before the scan command response is still
//      applied (no more permanent `scanning`).
//   3. R10c — done followed by a *late* authoritative snapshot still ends
//      with the NEW results on screen (retry loop).
import { build } from "esbuild";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import assert from "node:assert/strict";

const root = process.cwd();
const outfile = resolve(root, "src/testing/store-probes.bundle.mjs");

await build({
  stdin: {
    contents:
      "export {useScanStore, onScanGeneration} from './src/stores/scanStore';" +
      "export {useSelectionStore} from './src/stores/selectionStore';" +
      "export {wireGenerationLifecycle} from './src/stores/generationLifecycle';",
    resolveDir: root,
    loader: "ts",
  },
  outfile,
  bundle: true,
  format: "esm",
  platform: "node",
  packages: "external",
  plugins: [
    {
      name: "inert-probe-adapters",
      setup(b) {
        b.onResolve({ filter: /^@\/adapters(\/events)?$/ }, (args) => ({
          path: args.path,
          namespace: "probe",
        }));
        b.onLoad({ filter: /.*/, namespace: "probe" }, (args) => ({
          contents: args.path.endsWith("/events")
            ? "export function subscribeScan(handlers) { globalThis.__probeHandlers = handlers; return { ready: Promise.resolve(), close: () => {} }; }"
            : "export function getBackend() { return globalThis.__probeBackend; }",
          loader: "js",
        }));
      },
    },
  ],
});

const { useScanStore: scan, useSelectionStore: selection, wireGenerationLifecycle } =
  await import(pathToFileURL(outfile).href);

const turn = () => new Promise((r) => setImmediate(r));
const old = { id: 1, path: "C:/review/old-selected-cache" };
const replacement = { id: 1, path: "C:/review/new-unselected-cache" };
const snapshot = (items, generation) => ({
  items,
  warnings: [],
  scanned_at: 100 + generation,
  generation,
  cancelled: false,
});

// ---------------------------------------------------------------- scenario 1
// R04: selection must not survive a generation advance.
wireGenerationLifecycle();
scan.setState({ phase: "done", scanId: 1, items: [old], generation: 1, scannedAt: 101 });
selection.getState().setAll([old.id]);
assert.equal(selection.getState().selected.size, 1, "precondition: one selection");

globalThis.__probeBackend = {
  scan: async () => ({ scanId: 2 }),
  getScanResults: async () => snapshot([replacement], 2),
};
await scan.getState().startScan({ kind: "default" });
globalThis.__probeHandlers.onDone({ scan_id: 2, cancelled: false, total: 1, generation: 2 });
for (let i = 0; i < 30 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 60));

assert.equal(scan.getState().items[0].path, replacement.path, "new snapshot loaded");
assert.equal(scan.getState().generation, 2, "generation advanced to 2");
assert.equal(selection.getState().selected.size, 0, "R04: selection cleared on re-scan");
assert.ok(
  scan.getState().generationNotice !== null,
  "R04: one-shot notice surfaced",
);
console.log("PASS 1 (R04): re-scan clears selection + notice shown");

// ---------------------------------------------------------------- scenario 2
// R10a/b: done fires BEFORE the scan command's response resolves.
scan.getState().reset();
// The disk holds generation 2 (scenario 1's result) while the scan runs;
// only once the worker reports done does the new generation 3 appear —
// the real backend's ordering.
let diskTwo = snapshot([], 2);
globalThis.__probeBackend = {
  scan: async () => {
    // The worker finishes before the command response is awaited.
    diskTwo = snapshot([], 3);
    globalThis.__probeHandlers.onDone({
      scan_id: 3,
      cancelled: false,
      total: 0,
      generation: 3,
    });
    return { scanId: 3 };
  },
  getScanResults: async () => diskTwo,
};
await scan.getState().startScan({ kind: "default" });
for (let i = 0; i < 30 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 60));

assert.equal(scan.getState().scanId, 3, "scan id published");
assert.notEqual(scan.getState().phase, "scanning", "R10b: done before response is applied");
assert.equal(scan.getState().phase, "done");
console.log("PASS 2 (R10a/b): early done no longer strands the UI in scanning");

// ---------------------------------------------------------------- scenario 3
// R10c: done arrives while the authoritative snapshot is still the OLD one;
// the retry loop must end with the NEW results.
scan.getState().reset();
let authoritative = snapshot([old], 4);
let publishedNew = false;
globalThis.__probeBackend = {
  scan: async () => ({ scanId: 5 }),
  getScanResults: async () => {
    // The backend publishes the new generation a moment after `done`.
    if (!publishedNew) {
      publishedNew = true;
      setTimeout(() => {
        authoritative = snapshot([replacement], 5);
      }, 30);
    }
    return authoritative;
  },
};
await scan.getState().startScan({ kind: "default" });
globalThis.__probeHandlers.onItem({ scan_id: 5, item: replacement });
globalThis.__probeHandlers.onDone({
  scan_id: 5,
  cancelled: false,
  total: 1,
  generation: 5,
});
for (let i = 0; i < 40 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 250));

assert.equal(scan.getState().phase, "done", "settled to done");
assert.equal(scan.getState().generation, 5, "generation tracked the late publish");
assert.equal(
  scan.getState().items[0]?.path,
  replacement.path,
  "R10c: late authoritative snapshot replaces the early stale read",
);
console.log("PASS 3 (R10c): late-published snapshot still lands in the UI");

// ---------------------------------------------------------------- scenario 4
// R10 (batch-3 bite-in): scan://error clears `scanning` and surfaces the
// message — a failed scan never looks finished.
scan.getState().reset();
globalThis.__probeBackend = {
  scan: async () => ({ scanId: 6 }),
  getScanResults: async () => snapshot([], 5),
};
await scan.getState().startScan({ kind: "default" });
globalThis.__probeHandlers.onError({
  scan_id: 6,
  message: "rule gate failed: cannot load resources/rules",
});
await turn();
assert.equal(scan.getState().phase, "error", "R10: error terminal applied");
assert.equal(
  scan.getState().error?.message,
  "rule gate failed: cannot load resources/rules",
  "R10: failure message surfaced",
);
console.log("PASS 4 (R10 error): scan://error clears scanning and shows the failure");

// ---------------------------------------------------------------- scenario 5
// Stale-done expiry: a done whose generation is OLDER than what the UI
// already shows must not regress the state.
scan.getState().reset();
scan.setState({ phase: "done", scanId: 7, generation: 7, items: [replacement] });
globalThis.__probeBackend = {
  scan: async () => ({ scanId: 7 }),
  getScanResults: async () => snapshot([replacement], 7),
};
await scan.getState().startScan({ kind: "default" });
globalThis.__probeHandlers.onDone({
  scan_id: 7,
  cancelled: false,
  total: 1,
  generation: 6,
});
for (let i = 0; i < 20 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 60));
assert.equal(scan.getState().phase, "done", "stale done still settles (never hangs)");
assert.equal(scan.getState().generation, 7, "generation never regressed below 7");
console.log("PASS 5 (R10 stale-done): old-generation done cannot regress the UI");

// ---------------------------------------------------------------- scenario 6
// F10 (review R2): a SECOND scan without reset keeps the previous scan's
// ID on the store. The new scan's done arrives BEFORE the command response
// binds scanId=2 — the old ID must not swallow it (the R2 reproduction
// ended with phase=scanning forever).
scan.getState().reset();
scan.setState({ phase: "done", scanId: 1, generation: 1, scannedAt: 101 });
let diskF10a = snapshot([old], 1);
globalThis.__probeBackend = {
  getScanResults: async () => diskF10a,
  scan: async () => {
    // The worker publishes the authoritative snapshot and emits done
    // before the command response resolves with scanId: 2.
    diskF10a = snapshot([replacement], 2);
    globalThis.__probeHandlers.onDone({
      scan_id: 2,
      cancelled: false,
      total: 1,
      generation: 2,
    });
    return { scanId: 2 };
  },
};
await scan.getState().startScan({ kind: "default" });
for (let i = 0; i < 40 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 80));

assert.equal(scan.getState().scanId, 2, "F10: second scan bound its own id");
assert.equal(scan.getState().phase, "done", "F10: early done on 2nd scan terminalises");
assert.equal(
  scan.getState().items[0]?.path,
  replacement.path,
  "F10: 2nd scan results on screen",
);
console.log("PASS 6 (F10 done): consecutive 2nd scan early-done reaches terminal");

// ---------------------------------------------------------------- scenario 7
// F10 (review R2), error flavour: same consecutive-scan window, the new
// scan's scan://error arrives before the response — must land phase=error
// with the message, never stuck scanning.
scan.getState().reset();
scan.setState({ phase: "done", scanId: 1, generation: 2, scannedAt: 102 });
globalThis.__probeBackend = {
  getScanResults: async () => snapshot([replacement], 2),
  scan: async () => {
    globalThis.__probeHandlers.onError({
      scan_id: 2,
      message: "rule gate failed: cannot load resources/rules",
    });
    return { scanId: 2 };
  },
};
await scan.getState().startScan({ kind: "default" });
for (let i = 0; i < 40 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 80));

assert.equal(scan.getState().scanId, 2, "F10: second scan bound its own id");
assert.equal(scan.getState().phase, "error", "F10: early error on 2nd scan terminalises");
assert.equal(
  scan.getState().error?.message,
  "rule gate failed: cannot load resources/rules",
  "F10: 2nd-scan failure message surfaced",
);
console.log("PASS 7 (F10 error): consecutive 2nd scan early-error reaches terminal");

// ---------------------------------------------------------------- scenario 8
// R3-G07 (done flavour): a delayed terminal of the PREVIOUS scan arrives
// while the new scan's command response is still pending. The stale event
// must be parked with its OWN scan_id, and the replay must drop it when the
// response binds a different id — the new scan keeps scanning (the review's
// probe ended with phase=done on the wrong id).
scan.getState().reset();
scan.setState({ phase: "done", scanId: 41, items: [old], generation: 7, scannedAt: 107 });
let diskG07 = snapshot([old], 7);
let resolveG07;
globalThis.__probeBackend = {
  getScanResults: async () => diskG07,
  scan: () =>
    new Promise((r) => {
      resolveG07 = r;
    }),
};
{
  const startP = scan.getState().startScan({ kind: "default" });
  // Wait until the command has been issued (phase scanning, id unbound).
  for (let i = 0; i < 20 && scan.getState().scanId !== null; i++) await turn();
  assert.equal(scan.getState().phase, "scanning");
  assert.equal(scan.getState().scanId, null);
  // Delayed delivery of the PREVIOUS scan's already-emitted terminal event.
  globalThis.__probeHandlers.onDone({ scan_id: 41, cancelled: false, total: 1, generation: 7 });
  // The new command response arrives, but its worker has NOT completed and
  // disk is still gen 7.
  resolveG07({ scanId: 42 });
  await startP;
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(scan.getState().scanId, 42, "response bound its own id");
  assert.equal(
    scan.getState().phase,
    "scanning",
    "R3-G07: stale scan 41 done must not terminalise active scan 42",
  );
  // Cleanup: let the worker finish (gen 8) and settle normally.
  diskG07 = snapshot([replacement], 8);
  globalThis.__probeHandlers.onDone({ scan_id: 42, cancelled: false, total: 1, generation: 8 });
  for (let i = 0; i < 40 && scan.getState().phase === "scanning"; i++) await turn();
  await new Promise((r) => setTimeout(r, 60));
  assert.notEqual(scan.getState().phase, "scanning", "the real done settles afterwards");
  console.log("PASS 8 (R3-G07 done): delayed previous-scan done discarded on id mismatch");
}

// ---------------------------------------------------------------- scenario 9
// R3-G07 (error flavour): same window, a delayed scan://error of the
// previous scan must not fail the active scan.
scan.getState().reset();
scan.setState({ phase: "done", scanId: 51, items: [old], generation: 9, scannedAt: 109 });
let diskG07e = snapshot([old], 9);
let resolveG07e;
globalThis.__probeBackend = {
  getScanResults: async () => diskG07e,
  scan: () =>
    new Promise((r) => {
      resolveG07e = r;
    }),
};
{
  const startP = scan.getState().startScan({ kind: "default" });
  for (let i = 0; i < 20 && scan.getState().scanId !== null; i++) await turn();
  assert.equal(scan.getState().phase, "scanning");
  assert.equal(scan.getState().scanId, null);
  globalThis.__probeHandlers.onError({
    scan_id: 51,
    message: "delayed error from previous scan",
  });
  resolveG07e({ scanId: 52 });
  await startP;
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(scan.getState().scanId, 52, "response bound its own id");
  assert.equal(
    scan.getState().phase,
    "scanning",
    "R3-G07: stale scan 51 error must not fail active scan 52",
  );
  assert.equal(scan.getState().error, null, "no error state leaked from the stale event");
  // Cleanup: finish normally.
  diskG07e = snapshot([replacement], 10);
  globalThis.__probeHandlers.onDone({ scan_id: 52, cancelled: false, total: 1, generation: 10 });
  for (let i = 0; i < 40 && scan.getState().phase === "scanning"; i++) await turn();
  await new Promise((r) => setTimeout(r, 60));
  assert.notEqual(scan.getState().phase, "scanning", "the real done settles afterwards");
  console.log("PASS 9 (R3-G07 error): delayed previous-scan error discarded on id mismatch");
}

// ---------------------------------------------------------------- scenario 10
// R3-G07 (park replay): the new scan's OWN done arriving before the response
// is still applied — the parked event carries the same id the response will
// bind, so the replay must terminalise (regression guard for the fix).
scan.getState().reset();
scan.setState({ phase: "done", scanId: 61, generation: 11, scannedAt: 111 });
let diskG07r = snapshot([old], 11);
globalThis.__probeBackend = {
  getScanResults: async () => diskG07r,
  scan: async () => {
    diskG07r = snapshot([replacement], 12);
    globalThis.__probeHandlers.onDone({
      scan_id: 62,
      cancelled: false,
      total: 1,
      generation: 12,
    });
    return { scanId: 62 };
  },
};
await scan.getState().startScan({ kind: "default" });
for (let i = 0; i < 40 && scan.getState().phase === "scanning"; i++) await turn();
await new Promise((r) => setTimeout(r, 80));
assert.equal(scan.getState().scanId, 62, "same-id early done still binds");
assert.equal(scan.getState().phase, "done", "same-id early done still terminalises (no overfix)");
assert.equal(
  scan.getState().items[0]?.path,
  replacement.path,
  "same-id early done still loads the new snapshot",
);
console.log("PASS 10 (R3-G07 replay): same-id early done is still applied — fix not over-broad");

// ---------------------------------------------------------------- scenario 11
// R4-H06: the NEW scan's done arrives early (parked per id 42), then a
// delayed OLD scan 41 done arrives BEFORE the response — it must NOT evict
// 42's parked terminal. The response binds 42 → the 42 entry is replayed →
// the UI settles on the new scan's results.
scan.getState().reset();
scan.setState({ phase: "done", scanId: 41, items: [old], generation: 7, scannedAt: 107 });
let diskR4 = snapshot([old], 7);
let resolveR4;
globalThis.__probeBackend = {
  getScanResults: async () => diskR4,
  scan: () =>
    new Promise((r) => {
      resolveR4 = r;
    }),
};
{
  const startP = scan.getState().startScan({ kind: "default" });
  for (let i = 0; i < 20 && scan.getState().scanId !== null; i++) await turn();
  assert.equal(scan.getState().phase, "scanning");
  assert.equal(scan.getState().scanId, null);
  // New scan 42 completes and its terminal arrives first (parked per id 42).
  diskR4 = snapshot([{ id: 2, path: "C:/review/new" }], 8);
  globalThis.__probeHandlers.onDone({ scan_id: 42, cancelled: false, total: 1, generation: 8 });
  // Delayed OLD scan 41 terminal then arrives — must not evict 42's entry.
  globalThis.__probeHandlers.onDone({ scan_id: 41, cancelled: false, total: 1, generation: 7 });
  resolveR4({ scanId: 42 });
  await startP;
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(scan.getState().scanId, 42, "response bound its own id");
  assert.equal(
    scan.getState().phase,
    "done",
    "R4-H06: new scan 42 terminal must survive delayed scan 41 terminal",
  );
  assert.equal(scan.getState().generation, 8, "the NEW generation is on screen");
  assert.equal(
    scan.getState().items[0]?.path,
    "C:/review/new",
    "R4-H06: the new scan's results are shown",
  );
  console.log("PASS 11 (R4-H06): old terminal cannot evict the new scan's parked terminal");
}

// ---------------------------------------------------------------- scenario 12
// R4-H06 error flavour: new error (id 42) parked first, delayed old done
// (id 41) arrives, response binds 42 → the error replays; the old done was
// never allowed to overwrite it.
scan.getState().reset();
scan.setState({ phase: "done", scanId: 51, items: [old], generation: 9, scannedAt: 109 });
let resolveR4e;
globalThis.__probeBackend = {
  getScanResults: async () => snapshot([old], 9),
  scan: () =>
    new Promise((r) => {
      resolveR4e = r;
    }),
};
{
  const startP = scan.getState().startScan({ kind: "default" });
  for (let i = 0; i < 20 && scan.getState().scanId !== null; i++) await turn();
  assert.equal(scan.getState().scanId, null);
  globalThis.__probeHandlers.onError({ scan_id: 52, message: "new scan failed" });
  globalThis.__probeHandlers.onDone({ scan_id: 51, cancelled: false, total: 1, generation: 9 });
  resolveR4e({ scanId: 52 });
  await startP;
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(scan.getState().scanId, 52, "response bound its own id");
  assert.equal(
    scan.getState().phase,
    "error",
    "R4-H06: the new scan's parked error survives the old done",
  );
  assert.equal(
    scan.getState().error?.message,
    "new scan failed",
    "R4-H06: the new scan's failure message surfaces",
  );
  console.log("PASS 12 (R4-H06): old done cannot evict the new scan's parked error");
}

console.log("12/12 batch-3 store-probe verifications passed (fixes, not defects).");
