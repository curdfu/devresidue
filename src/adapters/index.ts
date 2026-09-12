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
  async clearJournal(...args: Parameters<Backend["clearJournal"]>) {
    return (await this.real()).clearJournal(...args);
  }
  async resetScanData(...args: Parameters<Backend["resetScanData"]>) {
    return (await this.real()).resetScanData(...args);
  }
  async pickWorkspaceDirectory(...args: Parameters<Backend["pickWorkspaceDirectory"]>) {
    return (await this.real()).pickWorkspaceDirectory(...args);
  }
  async validateWorkspaceRoots(...args: Parameters<Backend["validateWorkspaceRoots"]>) {
    return (await this.real()).validateWorkspaceRoots(...args);
  }
  async getScanScopePreview(...args: Parameters<Backend["getScanScopePreview"]>) {
    return (await this.real()).getScanScopePreview(...args);
  }
  async getAppDataInfo(...args: Parameters<Backend["getAppDataInfo"]>) {
    return (await this.real()).getAppDataInfo(...args);
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
  async listAiProfiles(...args: Parameters<Backend["listAiProfiles"]>) {
    return (await this.real()).listAiProfiles(...args);
  }
  async upsertAiProfile(...args: Parameters<Backend["upsertAiProfile"]>) {
    return (await this.real()).upsertAiProfile(...args);
  }
  async deleteAiProfile(...args: Parameters<Backend["deleteAiProfile"]>) {
    return (await this.real()).deleteAiProfile(...args);
  }
  async setActiveAiProfile(...args: Parameters<Backend["setActiveAiProfile"]>) {
    return (await this.real()).setActiveAiProfile(...args);
  }
  async setAiMasterEnabled(...args: Parameters<Backend["setAiMasterEnabled"]>) {
    return (await this.real()).setAiMasterEnabled(...args);
  }
  async testAiConnection(...args: Parameters<Backend["testAiConnection"]>) {
    return (await this.real()).testAiConnection(...args);
  }
  async listAiModels(...args: Parameters<Backend["listAiModels"]>) {
    return (await this.real()).listAiModels(...args);
  }
  async prepareAiBatch(...args: Parameters<Backend["prepareAiBatch"]>) {
    return (await this.real()).prepareAiBatch(...args);
  }
  async analyzeAiBatch(...args: Parameters<Backend["analyzeAiBatch"]>) {
    return (await this.real()).analyzeAiBatch(...args);
  }
  async confirmAiBatch(...args: Parameters<Backend["confirmAiBatch"]>) {
    return (await this.real()).confirmAiBatch(...args);
  }
  async cancelAiBatch(...args: Parameters<Backend["cancelAiBatch"]>) {
    return (await this.real()).cancelAiBatch(...args);
  }
  async discardAiBatch(...args: Parameters<Backend["discardAiBatch"]>) {
    return (await this.real()).discardAiBatch(...args);
  }
  async getRules(...args: Parameters<Backend["getRules"]>) {
    return (await this.real()).getRules(...args);
  }
  async validateRules(...args: Parameters<Backend["validateRules"]>) {
    return (await this.real()).validateRules(...args);
  }
  async deleteUserRule(...args: Parameters<Backend["deleteUserRule"]>) {
    return (await this.real()).deleteUserRule(...args);
  }
}

export function getBackend(): Backend {
  if (!instance) instance = detect();
  return instance;
}

export type { Backend };
