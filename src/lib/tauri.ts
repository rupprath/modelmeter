// Typed wrappers around Tauri invoke calls and event listeners.
// Tauri 2 converts Rust snake_case parameter names to camelCase for the JS
// interface, so all multi-word parameter names must be camelCase here.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  Provider,
  KeyValidation,
  Balance,
  UsageSummary,
  SyncStatus,
  ProviderSyncCompleteEvent,
  ProviderKindMeta,
  PlanUsageResult,
  CachedClaudeCodeResult,
} from "./types";

// ── Provider commands ──────────────────────────────────────────────────────

export const listProviders = (): Promise<Provider[]> =>
  invoke("list_providers");

export const listProviderKinds = (): Promise<ProviderKindMeta[]> =>
  invoke("list_provider_kinds");

export const addProvider = (
  providerType: string,
  displayName: string,
  key: string,
): Promise<number> =>
  invoke("add_provider", { providerType, displayName, key });

export const removeProvider = (id: number): Promise<boolean> =>
  invoke("remove_provider", { id });

export const validateProviderKey = (
  providerType: string,
  key: string,
): Promise<KeyValidation> =>
  invoke("validate_provider_key", { providerType, key });

// ── Widget query commands ──────────────────────────────────────────────────

export const getLatestBalance = (providerId: number): Promise<Balance | null> =>
  invoke("get_latest_balance", { providerId });

export const getUsageSummary = (
  providerId: number,
  sinceTs: number,
  untilTs: number,
): Promise<UsageSummary> =>
  invoke("get_usage_summary", { providerId, sinceTs, untilTs });

// ── App config / settings ─────────────────────────────────────────────────

export interface AppSettings {
  sync_interval_seconds: number;
  retention_max_days: number;
  retention_max_size_mb: number;
  theme: string;
  window_height_px: number | null;
}

export const getConfig = (): Promise<AppSettings> =>
  invoke("get_config");

export const setConfig = (args: AppSettings): Promise<void> =>
  invoke("set_config", { args });

// ── Sync commands ──────────────────────────────────────────────────────────

export const triggerSyncAll = (): Promise<void> =>
  invoke("trigger_sync_all");

export const getSyncStatus = (): Promise<SyncStatus> =>
  invoke("get_sync_status");

// ── Claude Code plan usage ─────────────────────────────────────────────────

export const getClaudeCodePlanUsage = (): Promise<PlanUsageResult> =>
  invoke("get_claude_code_plan_usage");

export const getCachedClaudeCodeResult = (): Promise<CachedClaudeCodeResult | null> =>
  invoke("get_cached_claude_code_result");

// ── Event listeners ────────────────────────────────────────────────────────

export const onProviderSyncComplete = (
  handler: (event: ProviderSyncCompleteEvent) => void,
): Promise<UnlistenFn> =>
  listen<ProviderSyncCompleteEvent>("provider-sync-complete", (e) => handler(e.payload));
