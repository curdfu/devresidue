import type { Backend } from "./backend";
import { MockBackend } from "./mockBackend";

/**
 * Backend resolution (startup, once):
 *
 *  1. `VITE_DEVRESIDUE_MOCK=1` → always mock (pure-browser dev).
 *  2. Tauri internals present  → TauriBackend.
 *  3. Anything else (plain browser, backend down) → mock fallback.
 *
 * The UI code never branches on the kind — both satisfy the same port.
 */

let instance: Backend | null = null;

function detect(): Backend {
  const forced = import.meta.env.VITE_DEVRESIDUE_MOCK === "1";
  const hasTauri =
    typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
  if (!forced && hasTauri) {
    return new TauriBackendLazy();
  }
  console.info("[devresidue] using MockBackend (no Tauri internals detected)");
  return new MockBackend();
}

/** Defers importing the real backend until first use (keeps browser dev light). */
class TauriBackendLazy implements Backend {
  readonly kind = "tauri" as const;
  private inner: Backend | null = null;

  private async real(): Promise<Backend> {
    if (!this.inner) {
      const { TauriBackend } = await import("./tauriBackend");
      this.inner = new TauriBackend();
    }
    return this.inner;
  }

  async scan(...args: Parameters<Backend["scan"]>) {
    return (await this.real()).scan(...args);
  }
  async cancelScan(...args: Parameters<Backend["cancelScan"]>) {
    return (await this.real()).cancelScan(...args);
  }
  async getScanResults(...args: Parameters<Backend["getScanResults"]>) {
    return (await this.real()).getScanResults(...args);
  }
  async createCleanupPlan(...args: Parameters<Backend["createCleanupPlan"]>) {
    return (await this.real()).createCleanupPlan(...args);
  }
  async executeCleanupPlan(...args: Parameters<Backend["executeCleanupPlan"]>) {
    return (await this.real()).executeCleanupPlan(...args);
  }
  async getJournal(...args: Parameters<Backend["getJournal"]>) {
    return (await this.real()).getJournal(...args);
  }
  async clearAllData(...args: Parameters<Backend["clearAllData"]>) {
    return (await this.real()).clearAllData(...args);
  }
  async openFolder(...args: Parameters<Backend["openFolder"]>) {
    return (await this.real()).openFolder(...args);
  }
  async setDisposition(...args: Parameters<Backend["setDisposition"]>) {
    return (await this.real()).setDisposition(...args);
  }
  async getSettings(...args: Parameters<Backend["getSettings"]>) {
    return (await this.real()).getSettings(...args);
  }
  async setAnalyzerEnabled(...args: Parameters<Backend["setAnalyzerEnabled"]>) {
    return (await this.real()).setAnalyzerEnabled(...args);
  }
  async analyzeItem(...args: Parameters<Backend["analyzeItem"]>) {
    return (await this.real()).analyzeItem(...args);
  }
  async createRuleFromSuggestion(...args: Parameters<Backend["createRuleFromSuggestion"]>) {
    return (await this.real()).createRuleFromSuggestion(...args);
  }
  async getRules(...args: Parameters<Backend["getRules"]>) {
    return (await this.real()).getRules(...args);
  }
  async validateRules(...args: Parameters<Backend["validateRules"]>) {
    return (await this.real()).validateRules(...args);
  }
}

export function getBackend(): Backend {
  if (!instance) instance = detect();
  return instance;
}

export type { Backend };
