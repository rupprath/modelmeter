import { useEffect, useMemo, useRef, useState } from "react";
import { type Provider, type SyncStatus, type Balance, type PlanUsageResult, type RateLimitWindow, type MonthlySpend } from "../lib/types";
import { getLatestBalance, getUsageSummary, getClaudeCodePlanUsage, getCachedClaudeCodeResult, getXaiMonthlyHistory } from "../lib/tauri";
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
            <Money value={total} />
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
            <Money value={total} />
          </div>
          <div className="sv-totals-period">{period}d total</div>
        </div>
      </div>

      <DailyBars data={data} color={tint} height={84} />

      {balance != null && provider.provider_type !== "anthropic" && provider.provider_type !== "openai" && (
        <div className="sv-balance">
          <span className="sv-balance-label">Remaining balance</span>
          <span className="sv-balance-val"><Money value={balance} /></span>
        </div>
      )}

      <div className="sv-card-foot">
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

// ── x.ai card (monthly bars + balance, period-aligned) ──────────────────
//
// x.ai's billing API is monthly-granular (one invoice per calendar month). The
// card follows the dashboard's period tab:
//   • 7d / 14d / 30d → current month only (1 bar)
//   • 90d            → last 3 calendar months (3 bars, oldest→newest)
// Months with no invoice are backfilled with $0 so the window always shows
// the same number of bars regardless of activity.

const MONTH_NAMES = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

function XaiCard({
  provider,
  balance,
  history,
  period,
}: {
  provider: Provider;
  balance: number | null;
  history: MonthlySpend[];
  period: Period;
}) {
  const monthsBack = monthsBackForPeriod(period);
  const months = backfilledMonths(history, monthsBack);
  const data = months.map((m) => m.amount_usd);
  const labels = months.map((m) => `${MONTH_NAMES[m.month - 1]} ${String(m.year).slice(-2)}`);
  const total = data.reduce((s, v) => s + v, 0);
  const lastMonth = data[data.length - 1] ?? 0;
  const tint = `var(--mm-prov-${provider.provider_type})`;
  const periodLabel = monthsBack === 1 ? "this month" : `last ${monthsBack} months`;

  return (
    <div className="sv-card">
      <div className="sv-card-head">
        <span
          className={`mm-prov-mark ${provider.provider_type}`}
          style={{ width: 18, height: 18, fontSize: 10, borderRadius: 4 }}
        >
          X
        </span>
        <div style={{ minWidth: 0 }}>
          <div className="sv-card-name">{provider.display_name}</div>
          <div className="sv-card-sub">monthly billing · {periodLabel}</div>
        </div>
        <div className="sv-head-totals">
          <div className="sv-totals-amount"><Money value={total} /></div>
          <div className="sv-totals-period">{monthsBack}m total</div>
        </div>
      </div>

      <DailyBars data={data} labels={labels} color={tint} height={84} />

      <div className="sv-card-foot">
        <div className="sv-stat">
          <div className="sv-stat-label">This month</div>
          <div className="sv-stat-val"><Money value={lastMonth} /></div>
        </div>
        <div className="sv-stat">
          <div className="sv-stat-label">{monthsBack}m total</div>
          <div className="sv-stat-val"><Money value={total} /></div>
        </div>
        {balance != null && (
          <div className="sv-stat">
            <div className="sv-stat-label">Remaining</div>
            <div className="sv-stat-val"><Money value={balance} /></div>
          </div>
        )}
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
            Claude Code access token expired — run any Claude Code request to refresh it. ModelMeter will pick up the new credentials on the next sync.
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

// Maps a daily-array index to the calendar (year, month) that day falls in.
// Index 0 = oldest day, index n-1 = today.
function dayIndexToYearMonth(i: number, n: number): { year: number; month: number } {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() - (n - 1 - i));
  return { year: d.getFullYear(), month: d.getMonth() + 1 };
}

// Spreads each month's total evenly across the days of that month that fall
// within the daily array's window. Months with no array coverage are skipped.
// The result is the xai contribution to the daily aggregate.
function proratedXaiDaily(history: MonthlySpend[], n: number): number[] {
  const dayYM = Array.from({ length: n }, (_, i) => dayIndexToYearMonth(i, n));
  const out: number[] = Array(n).fill(0);
  for (const m of history) {
    const indices: number[] = [];
    for (let i = 0; i < n; i++) {
      if (dayYM[i].year === m.year && dayYM[i].month === m.month) indices.push(i);
    }
    if (indices.length === 0) continue;
    const perDay = m.amount_usd / indices.length;
    for (const i of indices) out[i] += perDay;
  }
  return out;
}

// Returns the most recent `monthsBack` calendar months, oldest-to-newest,
// filling in $0 for months not present in `history`.
function backfilledMonths(history: MonthlySpend[], monthsBack: number): MonthlySpend[] {
  const today = new Date();
  const result: MonthlySpend[] = [];
  for (let i = monthsBack - 1; i >= 0; i--) {
    const d = new Date(today.getFullYear(), today.getMonth() - i, 1);
    const year = d.getFullYear();
    const month = d.getMonth() + 1;
    const found = history.find((m) => m.year === year && m.month === month);
    result.push({ year, month, amount_usd: found?.amount_usd ?? 0 });
  }
  return result;
}

// How many months of monthly data the xai card shows for each period tab.
// 7/14/30 → current month only; 90 → last 3 calendar months.
function monthsBackForPeriod(period: Period): number {
  return period >= 90 ? 3 : 1;
}

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

function globalSyncLabel(providers: Provider[], syncStatus: SyncStatus | null): string {
  if (!syncStatus || syncStatus.indicator === "spinner") return "syncing…";
  const maxTs = providers.reduce<number>((max, p) => Math.max(max, p.last_sync_succeeded_at ?? 0), 0);
  if (syncStatus.indicator === "amber") return maxTs ? `stale · synced ${relativeTime(maxTs)}` : "stale";
  if (!maxTs) return "never synced";
  return `synced ${relativeTime(maxTs)}`;
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
  const [xaiHistory, setXaiHistory] = useState<MonthlySpend[]>([]);
  const [, setTimeTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTimeTick((t) => t + 1), 60_000);
    return () => clearInterval(id);
  }, []);
  // Split: claude_code is shown in its own card; everything else uses the regular data pipeline.
  const claudeProvider = providers.find((p) => p.provider_type === "claude_code") ?? null;
  const apiProviders = providers.filter((p) => p.provider_type !== "claude_code");
  const xaiProvider = apiProviders.find((p) => p.provider_type === "xai") ?? null;

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

  // Fetch xai monthly history once per sync tick so we can both (a) render the
  // xai card and (b) prorate it into the daily aggregate.
  useEffect(() => {
    if (!xaiProvider) { setXaiHistory([]); return; }
    getXaiMonthlyHistory(xaiProvider.id)
      .then(setXaiHistory)
      .catch(() => setXaiHistory([]));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [xaiProvider?.id, syncStatus?.last_tick_at]);

  const aggregateSeries = useMemo(() => {
    if (providerData.length === 0 && xaiHistory.length === 0) {
      return Array(MAX_DAYS).fill(0) as number[];
    }
    const dailySum = Array.from({ length: MAX_DAYS }, (_, i) =>
      providerData.reduce((sum, pd) => sum + (pd.daily[i] ?? 0), 0),
    );
    // xai has no daily granularity; spread each month's total evenly across
    // its days within the array's window so the aggregate reflects xai spend.
    const xaiDaily = proratedXaiDaily(xaiHistory, MAX_DAYS);
    return dailySum.map((v, i) => v + xaiDaily[i]);
  }, [providerData, xaiHistory]);

  const syncLabel = globalSyncLabel(providers, syncStatus);
  const dotColor = (!syncStatus || syncStatus.indicator === "spinner")
    ? "var(--mm-text-3)"
    : syncStatus.indicator === "amber"
      ? "var(--mm-warn)"
      : "var(--mm-ok)";
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
              <span style={{ width: 6, height: 6, borderRadius: 999, background: dotColor, display: "inline-block" }} />
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
        : providerData.map((pd) =>
            pd.provider.provider_type === "xai" ? (
              <XaiCard
                key={pd.provider.id}
                provider={pd.provider}
                balance={pd.balance}
                history={xaiHistory}
                period={period}
              />
            ) : (
              <ProviderCard key={pd.provider.id} pd={pd} period={period} />
            ),
          )
      }
    </div>
  );
}
