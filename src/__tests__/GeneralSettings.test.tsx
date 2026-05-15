import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { GeneralSettings } from "../pages/settings/GeneralSettings";

// ── Mocks ────────────────────────────────────────────────────────────────────

vi.mock("../lib/tauri", () => ({
  getConfig: vi.fn().mockResolvedValue({
    sync_interval_seconds: 900,
    retention_max_days: 90,
    retention_max_size_mb: 1024,
    theme: "system",
    window_height_px: null,
  }),
  setConfig: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    minimize: vi.fn().mockResolvedValue(undefined),
    toggleMaximize: vi.fn().mockResolvedValue(undefined),
    hide: vi.fn().mockResolvedValue(undefined),
    isMaximized: vi.fn().mockResolvedValue(false),
    onResized: vi.fn().mockResolvedValue(() => {}),
    outerSize: vi.fn().mockResolvedValue({ width: 720, height: 820 }),
    setSize: vi.fn().mockResolvedValue(undefined),
  }),
}));

vi.mock("../lib/theme", () => ({
  applyThemePref: vi.fn(),
  emitThemeChanged: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: vi.fn().mockResolvedValue(undefined),
}));

// ── Helpers ───────────────────────────────────────────────────────────────────

function renderPage() {
  return render(
    <MemoryRouter>
      <GeneralSettings syncStatus={null} lastSyncedAt={null} />
    </MemoryRouter>,
  );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("GeneralSettings", () => {
  it("renders without crashing when getConfig resolves with valid settings", async () => {
    renderPage();
    await waitFor(() =>
      expect(screen.getByText(/appearance/i)).toBeInTheDocument(),
    );
  });

  it("renders the three theme buttons (System, Light, Dark)", async () => {
    renderPage();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /system/i })).toBeInTheDocument(),
    );
    expect(screen.getByRole("button", { name: /light/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /dark/i })).toBeInTheDocument();
  });

  it("renders sync interval select", async () => {
    renderPage();
    await waitFor(() =>
      expect(screen.getByText(/sync interval/i)).toBeInTheDocument(),
    );
  });

  it("renders retention settings section", async () => {
    renderPage();
    await waitFor(() =>
      expect(screen.getByText(/data retention/i)).toBeInTheDocument(),
    );
  });
});
