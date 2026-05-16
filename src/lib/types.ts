// Core application types — mirror the Rust DTOs from src-tauri/src/commands/.

export type ProviderKind = string;

// ── Provider catalog (fetched from backend via list_provider_kinds) ────────

export interface ProviderKindMeta {
  slug: string;
  display_name: string;
  short: string;
  color: string;
  key_docs_url: string | null;
  key_label: string;
  key_is_secret: boolean;
  /** Whether the user must supply a credential to add this provider. */
  key_required: boolean;
  /** Label for the optional second input some providers need (e.g. x.ai's Team ID). */
  aux_field_label: string | null;
  /** Hint shown below the aux input describing where to find the value. */
  aux_field_hint: string | null;
}

// ── Claude Code plan usage ─────────────────────────────────────────────────

export interface RateLimitWindow {
  percent_used: number;
  window_label: string;    // "5hr" or "7 day"
  resets_in_seconds: number;
  resets_at: number | null;
}

export type PlanUsageResult =
  | { status: "ok"; session: RateLimitWindow; weekly: RateLimitWindow; subscription_type: string }
  | { status: "no_credentials" }
  | { status: "auth_expired" }
  | { status: "rate_limited" }
  | { status: "error"; message: string };

export interface CachedClaudeCodeResult {
  result: PlanUsageResult;
  fetched_at: number; // unix seconds
}

// ── Provider ───────────────────────────────────────────────────────────────

export interface Provider {
  id: number;
  provider_type: ProviderKind;
  display_name: string;
  last_sync_attempted_at: number | null;
  last_sync_succeeded_at: number | null;
  last_sync_status: string; // "ok" | "failed" | "never"
  created_at: number;
  /** Optional auxiliary identifier (currently only x.ai's team UUID). */
  team_id: string | null;
}

// Discriminated union matching the Rust #[serde(tag = "status")] enum.
export type KeyValidation =
  | { status: "valid" }
  | { status: "invalid"; reason: string }
  | { status: "insufficient_permission"; hint: string };

// ── Query results ──────────────────────────────────────────────────────────

export interface Balance {
  id: number;
  provider_id: number;
  amount_usd: number | null;
  shape: "remaining_credit" | "spend_against_cap" | "spend_this_period" | "unknown";
  note: string | null;
  as_of: number;
  fetched_at: number;
}

export interface UsageSummary {
  total_input_tokens: number;
  total_output_tokens: number;
  total_cache_creation_tokens: number;
  total_cache_read_tokens: number;
  total_cost_usd: number;
  total_request_count: number;
}

export interface MonthlySpend {
  year: number;
  month: number; // 1-12
  amount_usd: number;
}

// ── ElevenLabs (credit-denominated, plan-based provider) ───────────────────

export interface DayCredits {
  bucket_start: number; // unix UTC seconds at 00:00 of the day
  credits: number;
}

export interface ElevenLabsState {
  tier: string;                  // "payg", "starter", "creator", ...
  status: string;                // "active", "inactive", ...
  character_count: number;       // credits consumed this period
  character_limit: number;       // credits allotted this period
  next_reset_unix: number;       // unix UTC seconds when the quota refreshes
  current_overage_usd: number;   // dollars billed above the included quota
  currency: string;              // "usd"
  fetched_at: number;            // unix UTC seconds when the snapshot was taken
  daily_credits: DayCredits[];   // oldest-first; zero days omitted
}

// ── Sync ───────────────────────────────────────────────────────────────────

export type WorkerState =
  | "idle"
  | "running"
  | "waiting_to_retry"
  | "succeeded"
  | "failed";

export interface ProviderSyncStateDto {
  state: WorkerState;
  reason?: string | null;
}

export type SyncIndicator = "green" | "amber" | "spinner" | "grey";

export interface SyncStatus {
  paused: boolean;
  last_tick_at: number | null;
  indicator: SyncIndicator;
  providers: Record<number, ProviderSyncStateDto>;
}

export interface ProviderSyncCompleteEvent {
  provider_id: number;
  status: "succeeded" | "failed";
  reason: string | null;
  timestamp: number;
}

