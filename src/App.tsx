import { useEffect, useState } from "react";
import { MemoryRouter, Routes, Route, Navigate } from "react-router-dom";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";

import { AppShell } from "./components/layout/AppShell";
import { TitleBar } from "./components/layout/TitleBar";
import { Dashboard } from "./pages/Dashboard";
import { Providers } from "./pages/Providers";
import { GeneralSettings } from "./pages/settings/GeneralSettings";

import { listProviders, listProviderKinds, getSyncStatus, triggerSyncAll, onProviderSyncComplete, getConfig } from "./lib/tauri";
import { type Provider, type ProviderKindMeta, type SyncStatus } from "./lib/types";
import { applyThemePref, onThemeChanged, type ThemePref } from "./lib/theme";

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return Promise.race([
    p,
    new Promise<T>((_, reject) =>
      setTimeout(() => reject(new Error("timeout")), ms),
    ),
  ]);
}

function AppRoutes() {
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [providerKinds, setProviderKinds] = useState<ProviderKindMeta[]>([]);

  const refreshProviders = () => {
    listProviders().then(setProviders).catch(() => {});
    getSyncStatus().then(setSyncStatus).catch(() => {});
  };

  useEffect(() => {
    withTimeout(listProviders(), 5000).then(setProviders).catch(() => setProviders([]));
    listProviderKinds().then(setProviderKinds).catch(() => {});
    getSyncStatus().then(setSyncStatus).catch(() => {});

    const unsub = onProviderSyncComplete(refreshProviders);

    return () => {
      unsub.then((fn) => fn());
    };
  }, []);

  const handleRefresh = () => {
    triggerSyncAll().catch(() => {});
    getSyncStatus().then(setSyncStatus).catch(() => {});
  };

  if (providers === null) {
    return (
      <div style={{ width: "100vw", height: "100vh", background: "var(--mm-bg)" }}>
        <TitleBar />
      </div>
    );
  }

  const hasProviders = providers.length > 0;
  const lastSyncedAt = providers.reduce<number>((max, p) => Math.max(max, p.last_sync_succeeded_at ?? 0), 0) || null;

  return (
    <Routes>
      <Route
        path="/"
        element={
          <AppShell onRefresh={handleRefresh}>
            <Dashboard providers={providers} syncStatus={syncStatus} />
          </AppShell>
        }
      />
      <Route
        path="/providers"
        element={
          <AppShell onRefresh={handleRefresh}>
            <Providers
              providers={providers}
              providerKinds={providerKinds}
              syncStatus={syncStatus}
              lastSyncedAt={lastSyncedAt}
              onProvidersChanged={refreshProviders}
            />
          </AppShell>
        }
      />
      <Route
        path="/settings"
        element={
          <AppShell onRefresh={handleRefresh}>
            <GeneralSettings syncStatus={syncStatus} lastSyncedAt={lastSyncedAt} />
          </AppShell>
        }
      />
      <Route path="*" element={<Navigate to={hasProviders ? "/" : "/providers"} replace />} />
    </Routes>
  );
}

export default function App() {
  useEffect(() => {
    getConfig()
      .then(async (cfg) => {
        applyThemePref((cfg.theme as ThemePref) ?? "system");

        if (cfg.window_height_px != null) {
          const win = getCurrentWindow();
          const outer = await win.outerSize();
          await win.setSize(new LogicalSize(outer.width, cfg.window_height_px));
        }
      })
      .catch(() => applyThemePref("system"));

    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onMqChange = () => {
      getConfig()
        .then((cfg) => {
          if ((cfg.theme ?? "system") === "system") applyThemePref("system");
        })
        .catch(() => {});
    };
    mq.addEventListener("change", onMqChange);

    const unsub = onThemeChanged(applyThemePref);

    return () => {
      mq.removeEventListener("change", onMqChange);
      unsub.then((fn) => fn());
    };
  }, []);

  return (
    <MemoryRouter>
      <AppRoutes />
    </MemoryRouter>
  );
}
