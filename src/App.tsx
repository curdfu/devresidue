import { useEffect } from "react";
import { useUiStore, systemTheme } from "@/stores/uiStore";
import { useScanStore } from "@/stores/scanStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useAppSettingsStore } from "@/stores/appSettingsStore";
import { backendKind } from "@/stores/settingsStore";
import { wireGenerationLifecycle } from "@/stores/generationLifecycle";
import { AppShell } from "@/components/AppShell";
import { CleanupFlow } from "@/components/CleanupFlow";

export function App() {
  const theme = useUiStore((s) => s.theme);
  const loadLatest = useScanStore((s) => s.loadLatest);
  const loadSettings = useSettingsStore((s) => s.load);
  const loadAppSettings = useAppSettingsStore((s) => s.load);

  // Theme attribute on <html> drives all CSS custom properties. "system"
  // resolves against the OS preference and FOLLOWS it live (no reload needed
  // when the user flips Windows dark/light).
  useEffect(() => {
    if (theme !== "system") {
      document.documentElement.dataset.theme = theme;
      return;
    }
    const apply = () => {
      document.documentElement.dataset.theme = systemTheme();
    };
    apply();
    const media = window.matchMedia?.("(prefers-color-scheme: light)");
    media?.addEventListener?.("change", apply);
    return () => media?.removeEventListener?.("change", apply);
  }, [theme]);

  // R04: selection/plan/suggestion invalidation on generation change (once).
  useEffect(() => {
    wireGenerationLifecycle();
  }, []);

  // Restore persisted settings (roots + analyzer toggle) and the last scan
  // snapshot (if any).
  useEffect(() => {
    loadSettings();
    void loadAppSettings();
    void loadLatest();
  }, [loadSettings, loadAppSettings, loadLatest]);

  return (
    <>
      <AppShell />
      <CleanupFlow />
    </>
  );
}

export const BACKEND_KIND = backendKind();
