import { useEffect, useState } from "react";
import { Toolbar } from "../../components/layout/Toolbar";
import { getConfig, setConfig, type AppSettings } from "../../lib/tauri";
import { applyThemePref, emitThemeChanged, type ThemePref } from "../../lib/theme";

// ── Helpers ───────────────────────────────────────────────────────────────

const INTERVAL_PRESETS = [
  { label: "1 minute",   value: 60 },
  { label: "5 minutes",  value: 300 },
  { label: "15 minutes", value: 900 },
  { label: "30 minutes", value: 1800 },
  { label: "1 hour",     value: 3600 },
  { label: "6 hours",    value: 21600 },
  { label: "24 hours",   value: 86400 },
];

function SectionHeader({ title }: { title: string }) {
  return (
    <div
      style={{
        fontSize: 11,
        fontWeight: 600,
        letterSpacing: "0.06em",
        textTransform: "uppercase",
        color: "var(--mm-text-4)",
        padding: "20px 0 8px",
        borderBottom: "1px solid var(--mm-divider)",
        marginBottom: 12,
      }}
    >
      {title}
    </div>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 16, padding: "8px 0" }}>
      <div style={{ flex: 1 }}>
        <div style={{ fontSize: 12, fontWeight: 500, color: "var(--mm-text)" }}>{label}</div>
        {hint && <div style={{ fontSize: 11, color: "var(--mm-text-4)", marginTop: 2 }}>{hint}</div>}
      </div>
      {children}
    </div>
  );
}

// ── Component ─────────────────────────────────────────────────────────────

export function GeneralSettings() {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    getConfig().then((cfg) => {
      setSettings(cfg);
    }).catch(() => {});
  }, []);

  const save = async (next: AppSettings) => {
    setSaving(true);
    setSaved(false);
    try {
      await setConfig(next);
      setSettings(next);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } finally {
      setSaving(false);
    }
  };

  const handleTheme = async (pref: ThemePref) => {
    if (!settings) return;
    const next = { ...settings, theme: pref };
    applyThemePref(pref);
    emitThemeChanged(pref).catch(() => {});
    await save(next);
  };

  const handleInterval = (value: number) => {
    if (!settings) return;
    save({ ...settings, sync_interval_seconds: value });
  };

  const handleRetentionDays = (value: number) => {
    if (!settings) return;
    save({ ...settings, retention_max_days: value });
  };

  const handleRetentionMb = (value: number) => {
    if (!settings) return;
    save({ ...settings, retention_max_size_mb: value });
  };

  if (!settings) return null;

  const theme = (settings.theme as ThemePref) ?? "system";

  return (
    <>
      <Toolbar
        title="Settings"
        syncStatus={null}
        right={
          saved ? (
            <span style={{ fontSize: 11, color: "var(--mm-ok)" }}>Saved</span>
          ) : saving ? (
            <span style={{ fontSize: 11, color: "var(--mm-text-4)" }}>Saving…</span>
          ) : null
        }
      />

      <div style={{ flex: 1, overflow: "auto", padding: "4px 24px 24px" }}>
        {/* Appearance */}
        <SectionHeader title="Appearance" />
        <Row label="Theme" hint="Applies immediately to all open windows">
          <div style={{ display: "flex", gap: 4 }}>
            {(["system", "light", "dark"] as ThemePref[]).map((t) => (
              <button
                key={t}
                className={`mm-btn ${theme === t ? "primary" : "ghost"}`}
                style={{ height: 26, fontSize: 11 }}
                onClick={() => handleTheme(t)}
              >
                {t.charAt(0).toUpperCase() + t.slice(1)}
              </button>
            ))}
          </div>
        </Row>
        {/* Sync */}
        <SectionHeader title="Sync" />
        <Row label="Sync interval" hint="How often to fetch data from providers">
          <select
            className="mm-input"
            style={{ width: 150 }}
            value={settings.sync_interval_seconds}
            onChange={(e) => handleInterval(Number(e.target.value))}
          >
            {INTERVAL_PRESETS.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label}
              </option>
            ))}
          </select>
        </Row>

        {/* Retention */}
        <SectionHeader title="Data retention" />
        <Row
          label="Keep history for"
          hint="Usage records older than this are pruned automatically"
        >
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <input
              className="mm-input"
              type="number"
              min={1}
              max={3650}
              style={{ width: 80 }}
              value={settings.retention_max_days}
              onChange={(e) => {
                const v = Number(e.target.value);
                if (v >= 1 && v <= 3650) setSettings((s) => s ? { ...s, retention_max_days: v } : s);
              }}
              onBlur={() => handleRetentionDays(settings.retention_max_days)}
            />
            <span style={{ fontSize: 12, color: "var(--mm-text-3)" }}>days</span>
          </div>
        </Row>
        <Row
          label="Database size limit"
          hint="Oldest records are pruned when the database exceeds this size"
        >
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <input
              className="mm-input"
              type="number"
              min={100}
              max={102400}
              style={{ width: 80 }}
              value={settings.retention_max_size_mb}
              onChange={(e) => {
                const v = Number(e.target.value);
                if (v >= 100 && v <= 102400) setSettings((s) => s ? { ...s, retention_max_size_mb: v } : s);
              }}
              onBlur={() => handleRetentionMb(settings.retention_max_size_mb)}
            />
            <span style={{ fontSize: 12, color: "var(--mm-text-3)" }}>MB</span>
          </div>
        </Row>
      </div>
    </>
  );
}
