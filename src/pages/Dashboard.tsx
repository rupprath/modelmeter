import { useEffect, useMemo, useRef, useState } from "react";
import { type Provider, type SyncStatus, type Balance, type PlanUsageResult, type RateLimitWindow } from "../lib/types";
import { getLatestBalance, getUsageSummary, getClaudeCodePlanUsage, getCachedClaudeCodeResult } from "../lib/tauri";
import { relativeTime, timeUntil } from "../lib/time";
import { Money } from "../components/ui/Money";
import { DailyBars } from "../components/ui/DailyBars";

// ── Types ─────────────────────────────────────────────────────────────────

type Period = 7 | 14 | 30 | 90;

interface ProviderData {
  provider: Provider;
  daily: number[];   // oldest → newest, length = max period (90)
  balance: number | null;
}

// ── Helpers ───────────────────────────────────────────────────────────────

function nowSec() {
  return Math.floor(Date.now() / 1000);
}

function dayBoundaries(daysAgo: number): { since: number; until: number } {
  const now = nowSec();
  const DAY = 86400;
  const until = now - daysAgo * DAY;
  const since = until - DAY;
  return { since, until };
}

function formatAge(fetchedAtSec: number): string {
  const elapsed = nowSec() - fetchedAtSec;
  if (elapsed < 60) return "just now";
  if (elapsed < 3600) return `${Math.floor(elapsed / 60)} minutes ago`;
  if (elapsed < 86400) return `${Math.floor(elapsed / 3600)} hours ago`;
  return `${Math.floor(elapsed / 86400)} days ago`;
}

function syncStatusOf(provider: Provider): "ok" | "warn" | "failed" {
  if (provider.last_sync_status === "ok") return "ok";
  if (provider.last_sync_status === "failed") return "failed";
  return "warn";
}

// ── Sub-components ────────────────────────────────────────────────────────

function PeriodTabs({ period, onChange }: { period: Period; onChange: (p: Period) => void }) {
  const options: Period[] = [7, 14, 30, 90];
  return (
    <div className="sv-period-tabs">
      {options.map((p) => (
        <button
          key={p}
          className={`sv-period-tab${p === period ? " active" : ""}`}
          onClick={() => onChange(p)}
        >
          {p}d
        </button>
      ))}
    </div>
  );
}

function SyncStatusPill({ provider }: { provider: Provider }) {
  const status = syncStatusOf(provider);
  const color =
    status === "ok" ? "var(--mm-ok)" :
    status === "warn" ? "var(--mm-warn)" :
    "var(--mm-err)";
  const label = status === "ok" ? "synced" : status === "warn" ? "stale" : "failed";
  return (
    <div className="sv-status">
      <span className="dot" style={{ background: color }} />
      {label}
    </div>
  );
}

function AggregateCard({ series, period }: { series: number[]; period: Period }) {
  const data = series.slice(-period);
  const total = data.reduce((s, v) => s + v, 0);

  return (
    <div className="sv-agg">
      <div className="sv-agg-head">
        <div>
          <div className="sv-agg-title">All API Providers</div>
          <div className="sv-agg-sub">total daily spend</div>
        </div>
        <div className="sv-head-totals">
          <div className="sv-totals-amount">
            <Money value={total} mutedCents={false} />
          </div>
          <div className="sv-totals-period">{period}d total</div>
        </div>
      </div>
      <DailyBars data={data} color="#1a1a1a" height={90} />
    </div>
  );
}

function ProviderCard({
  pd,
  period,
}: {
  pd: ProviderData;
  period: Period;
}) {
  const { provider, daily, balance } = pd;
  const short = provider.provider_type[0]?.toUpperCase() ?? "?";
  const data = daily.slice(-period);
  const total = data.reduce((s, v) => s + v, 0);
  const avgDay = total / period;
  const last24h = data[data.length - 1] ?? 0;
  const tint = `var(--mm-prov-${provider.provider_type})`;

  return (
    <div className="sv-card">
      <div className="sv-card-head">
        <span
          className={`mm-prov-mark ${provider.provider_type}`}
          style={{ width: 18, height: 18, fontSize: 10, borderRadius: 4 }}
        >
          {short}
        </span>
        <div style={{ minWidth: 0 }}>
          <div className="sv-card-name">{provider.display_name}</div>
          <div className="sv-card-sub">last {period} days</div>
        </div>
        <div className="sv-head-totals">
          <div className="sv-totals-amount">
            <Money value={total} mutedCents={false} />
          </div>
          <div className="sv-totals-period">{period}d total</div>
        </div>
      </div>

      <DailyBars data={data} color={tint} height={84} />

      <div className="sv-card-foot">
        <div className="sv-stat">
          <div className="sv-stat-label">Balance</div>
          <div className="sv-stat-val">
            {balance != null ? <Money value={balance} /> : <span style={{ color: "var(--mm-text-4)" }}>—</span>}
          </div>
        </div>
        <div className="sv-stat">
          <div className="sv-stat-label">Avg / day</div>
          <div className="sv-stat-val"><Money value={avgDay} /></div>
        </div>
        <div className="sv-stat">
          <div className="sv-stat-label">Last 24 h</div>
          <div className="sv-stat-val"><Money value={last24h} /></div>
        </div>
        <SyncStatusPill provider={provider} />
      </div>
    </div>
  );
}

// ── Claude Code card ──────────────────────────────────────────────────────

function RateLimitBar({ window: w }: { window: RateLimitWindow }) {
  const pct = Math.min(Math.max(w.percent_used, 0), 100);
  const resetLabel = w.resets_at ? `Resets in ${timeUntil(w.resets_at)}` : "Reset time unknown";
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
        <span style={{ color: "var(--mm-text-3)" }}>{w.window_label}</span>
        <span style={{ fontWeight: 600, color: pct >= 80 ? "var(--mm-err)" : "var(--mm-text)" }}>
          {Math.round(pct)}%
        </span>
      </div>
      <div style={{ height: 6, borderRadius: 999, background: "var(--mm-border)", overflow: "hidden" }}>
        <div
          style={{
            height: "100%",
            width: `${pct}%`,
            background: pct >= 80 ? "var(--mm-err)" : "#cc785c",
            borderRadius: 999,
            transition: "width 0.4s ease",
          }}
        />
      </div>
      <div style={{ fontSize: 10.5, color: "var(--mm-text-4)" }}>{resetLabel}</div>
    </div>
  );
}

type OkResult = Extract<PlanUsageResult, { status: "ok" }>;

const CLAUDE_MIN_FETCH_INTERVAL = 5 * 60; // seconds — avoids back-to-back calls on startup

function ClaudeCodeCard({
  provider,
  syncLastTickAt,
  extraUsageBalance,
}: {
  provider: Provider;
  syncLastTickAt: number | null;
  extraUsageBalance: number | null;
}) {
  const [result, setResult] = useState<PlanUsageResult | null>(null);
  const lastOk = useRef<OkResult | null>(null);
  const lastFetchAt = useRef<number>(0);
  const cachedFetchedAt = useRef<number | null>(null);

  // Guard is inside doFetch so it applies regardless of what triggers the call —
  // including React StrictMode's double-invoke of mount effects.
  const doFetch = () => {
    if (nowSec() - lastFetchAt.current < CLAUDE_MIN_FETCH_INTERVAL) return;
    lastFetchAt.current = nowSec();
    getClaudeCodePlanUsage().then((r) => {
      if (r.status === "ok") {
        lastOk.current = r;
        cachedFetchedAt.current = nowSec();
      }
      setResult(r);
    }).catch(() =>
      setResult({ status: "error", message: "Failed to fetch usage" }),
    );
  };

  // On mount: seed lastOk from the DB cache so a rate-limited first fetch has
  // a value to fall back to, then kick off the live fetch.
  useEffect(() => {
    getCachedClaudeCodeResult().then((cached) => {
      if (cached && cached.result.status === "ok") {
        lastOk.current = cached.result;
        cachedFetchedAt.current = cached.fetched_at;
      }
    }).finally(() => { doFetch(); });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { if (syncLastTickAt) doFetch(); }, [syncLastTickAt]); // eslint-disable-line react-hooks/exhaustive-deps

  // When rate-limited, fall back to the last known-good result if available.
  const display = (result?.status === "rate_limited" && lastOk.current)
    ? lastOk.current
    : result;

  const isStale = display?.status === "ok" && result?.status === "rate_limited";

  const planLabel =
    display?.status === "ok"
      ? display.subscription_type
          .replace(/^claude[\s_-]?/i, "Claude ")
          .replace(/\b\w/g, (c: string) => c.toUpperCase())
      : null;

  return (
    <div className="sv-card">
      <div className="sv-card-head">
        <span
          className="mm-prov-mark claude_code"
          style={{ width: 18, height: 18, fontSize: 10, borderRadius: 4 }}
        >
          C
        </span>
        <div style={{ minWidth: 0 }}>
          <div className="sv-card-name">{provider.display_name}</div>
          <div className="sv-card-sub">Claude Code plan usage</div>
        </div>
        {planLabel && (
          <div style={{ marginLeft: "auto", fontSize: 11, color: "var(--mm-text-3)", whiteSpace: "nowrap" }}>
            {planLabel}
          </div>
        )}
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 12, padding: "4px 0" }}>
        {display === null && (
          <div style={{ fontSize: 11, color: "var(--mm-text-4)", textAlign: "center", padding: "12px 0" }}>
            Loading…
          </div>
        )}
        {display?.status === "ok" && (
          <>
            <RateLimitBar window={display.session} />
            <RateLimitBar window={display.weekly} />
            {isStale && (
              <div style={{ fontSize: 10.5, color: "var(--mm-text-4)", textAlign: "center" }}>
                Showing cached data
                {cachedFetchedAt.current ? ` from ${formatAge(cachedFetchedAt.current)}` : ""}
                {" "}— refresh limit reached
              </div>
            )}
          </>
        )}
        {extraUsageBalance != null && (
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", paddingTop: 8, borderTop: "1px solid var(--mm-border)" }}>
            <span style={{ fontSize: 11, color: "var(--mm-text-3)" }}>Extra Usage Balance</span>
            <Money value={extraUsageBalance} />
          </div>
        )}
        {display?.status === "auth_expired" && (
          <div style={{ fontSize: 11, color: "var(--mm-warn)", textAlign: "center", padding: "8px 0" }}>
            Session expired — re-authenticate by opening Claude Code
          </div>
        )}
        {display?.status === "no_credentials" && (
          <div style={{ fontSize: 11, color: "var(--mm-text-4)", textAlign: "center", padding: "8px 0" }}>
            No credentials found — open Claude Code and sign in
          </div>
        )}
        {display?.status === "rate_limited" && (
          <div style={{ fontSize: 11, color: "var(--mm-text-3)", textAlign: "center", padding: "8px 0" }}>
            Couldn't load usage data yet — rate limited by Claude.ai
          </div>
        )}
        {display?.status === "error" && (
          <div style={{ fontSize: 11, color: "var(--mm-err)", textAlign: "center", padding: "8px 0" }}>
            {display.message}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Data fetching ─────────────────────────────────────────────────────────

const MAX_DAYS = 90;

async function fetchProviderData(provider: Provider): Promise<ProviderData> {
  const dailyPromises = Array.from({ length: MAX_DAYS }, (_, i) => {
    const { since, until } = dayBoundaries(MAX_DAYS - 1 - i);
    return getUsageSummary(provider.id, since, until).then((s) => s?.total_cost_usd ?? 0);
  });

  const [dailyValues, balanceResult] = await Promise.all([
    Promise.all(dailyPromises),
    getLatestBalance(provider.id).catch(() => null),
  ]);

  const balance = (balanceResult as Balance | null)?.amount_usd ?? null;

  return { provider, daily: dailyValues, balance };
}

// ── Summary View header sync label ────────────────────────────────────────

function globalSyncLabel(syncStatus: SyncStatus | null): string {
  if (!syncStatus) return "unknown";
  const ts = syncStatus.last_tick_at;
  if (!ts) return "never synced";
  return `synced ${relativeTime(ts)}`;
}

// ── Dashboard (Summary View) ──────────────────────────────────────────────

interface Props {
  providers: Provider[];
  syncStatus: SyncStatus | null;
}

export function Dashboard({ providers, syncStatus }: Props) {
  const [period, setPeriod] = useState<Period>(30);
  const [providerData, setProviderData] = useState<ProviderData[]>([]);
  const [loading, setLoading] = useState(true);
  // Split: claude_code is shown in its own card; everything else uses the regular data pipeline.
  const claudeProvider = providers.find((p) => p.provider_type === "claude_code") ?? null;
  const apiProviders = providers.filter((p) => p.provider_type !== "claude_code");

  useEffect(() => {
    if (apiProviders.length === 0) {
      setProviderData([]);
      setLoading(false);
      return;
    }

    setLoading(true);
    Promise.all(apiProviders.map(fetchProviderData))
      .then(setProviderData)
      .catch(() => setProviderData([]))
      .finally(() => setLoading(false));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providers, syncStatus?.last_tick_at]);

  const aggregateSeries = useMemo(() => {
    if (providerData.length === 0) return Array(MAX_DAYS).fill(0) as number[];
    return Array.from({ length: MAX_DAYS }, (_, i) =>
      providerData.reduce((sum, pd) => sum + (pd.daily[i] ?? 0), 0),
    );
  }, [providerData]);

  const syncLabel = globalSyncLabel(syncStatus);
  const providerCount = providers.length;
  const apiProviderCount = apiProviders.length;

  // ── Empty state ───────────────────────────────────────────────────────

  if (providerCount === 0) {
    return (
      <div className="sv-feed" style={{ alignItems: "center", justifyContent: "center", display: "flex", flexDirection: "column", gap: 16 }}>
        <div style={{ textAlign: "center" }}>
          <div style={{ fontWeight: 600, marginBottom: 6 }}>No providers configured</div>
          <div style={{ fontSize: 12, color: "var(--mm-text-3)", maxWidth: 260 }}>
            Add your API keys to start tracking usage and spend across providers.
          </div>
        </div>
      </div>
    );
  }

  // ── Single render tree (keeps ClaudeCodeCard mounted across loading state changes) ──

  return (
    <div className="sv-feed">
      {/* Title row — only shown once data is ready */}
      {!loading && apiProviderCount > 0 && (
        <div style={{ display: "flex", alignItems: "center", marginBottom: 10, padding: "0 4px" }}>
          <div>
            <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: "-0.01em" }}>Overview</div>
            <div style={{ fontSize: 10.5, color: "var(--mm-text-3)", marginTop: 1, display: "flex", alignItems: "center", gap: 6 }}>
              <span style={{ width: 6, height: 6, borderRadius: 999, background: "var(--mm-ok)", display: "inline-block" }} />
              {syncLabel} · {providerCount} provider{providerCount !== 1 ? "s" : ""}
            </div>
          </div>
          <div style={{ marginLeft: "auto" }}>
            <PeriodTabs period={period} onChange={setPeriod} />
          </div>
        </div>
      )}

      {/* Claude Code card — always in the same tree position so it never re-mounts */}
      {claudeProvider && (
        <ClaudeCodeCard
          provider={claudeProvider}
          syncLastTickAt={syncStatus?.last_tick_at ?? null}
          extraUsageBalance={providerData.find((pd) => pd.provider.provider_type === "anthropic")?.balance ?? null}
        />
      )}

      {/* Aggregate card (only when API provider data is ready) */}
      {!loading && apiProviderCount > 0 && <AggregateCard series={aggregateSeries} period={period} />}

      {/* API provider section heading */}
      {!loading && apiProviderCount > 0 && (
        <div className="sv-feed-head">
          <h3>Providers</h3>
          <span className="count">{apiProviderCount} configured</span>
        </div>
      )}

      {/* Loading skeletons or populated cards */}
      {loading && apiProviderCount > 0
        ? apiProviders.map((p) => (
            <div key={p.id} className="sv-card" style={{ height: 196, animation: "mm-pulse 1.2s ease-in-out infinite" }} />
          ))
        : providerData.map((pd) => (
            <ProviderCard key={pd.provider.id} pd={pd} period={period} />
          ))
      }
    </div>
  );
}
