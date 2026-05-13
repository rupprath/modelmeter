#![forbid(unsafe_code)]

//! Tauri commands for Claude Code plan-usage tracking.
//!
//! `get_claude_code_plan_usage` — reads OAuth credentials and calls the
//!     claude.ai rate-limit API to return live session/weekly usage.

use serde::Serialize;
use tauri::{Manager, State};

use modelmeter_core::crud;
use modelmeter_core::providers::claude_code::{fetch_plan_usage, read_claude_credentials};
use modelmeter_core::providers::unix_now;

use crate::{error::CommandResult, state::AppState};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RateLimitWindowDto {
    pub percent_used: f32,
    pub window_label: String,
    pub resets_in_seconds: i64,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct CachedPlanUsageResult {
    pub result: serde_json::Value,
    pub fetched_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanUsageResult {
    Ok {
        session: RateLimitWindowDto,
        weekly: RateLimitWindowDto,
        subscription_type: String,
    },
    NoCredentials,
    AuthExpired,
    RateLimited,
    Error {
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Reads OAuth credentials from `~/.claude/.credentials.json` and calls the
/// claude.ai rate-limit endpoint. Returns a tagged result the frontend can
/// pattern-match on.
///
/// Note: the underlying API rate-limits aggressively. Callers should fetch
/// on demand (e.g. on Dashboard mount + manual refresh) rather than polling.
#[tauri::command]
pub async fn get_claude_code_plan_usage(app: tauri::AppHandle) -> PlanUsageResult {
    // Extract db before any await points so the State borrow doesn't cross them.
    let db = app.state::<AppState>().db.clone();

    // 1. Read credentials.
    let creds = match tokio::task::spawn_blocking(read_claude_credentials).await {
        Ok(Ok(c)) => c,
        Ok(Err(msg)) => {
            if msg.contains("not found") || msg.contains("sign in") {
                return PlanUsageResult::NoCredentials;
            }
            return PlanUsageResult::Error { message: msg };
        }
        Err(e) => return PlanUsageResult::Error { message: format!("task panicked: {e}") },
    };

    // 2. Check token expiry.
    let now_ms = unix_now() * 1000;
    if creds.expires_at_ms > 0 && creds.expires_at_ms < now_ms {
        return PlanUsageResult::AuthExpired;
    }

    let subscription_type = creds.subscription_type.clone();

    // 3. Fetch live usage.
    match fetch_plan_usage(creds.access_token.as_str()).await {
        Ok(mut usage) => {
            usage.subscription_type = subscription_type.clone();
            let now = unix_now();
            let result = PlanUsageResult::Ok {
                session: window_to_dto(usage.five_hour, "5-hour session", now),
                weekly: window_to_dto(usage.seven_day, "7-day rolling", now),
                subscription_type,
            };
            // Persist for cross-restart stale cache.
            if let Ok(json) = serde_json::to_string(&result) {
                tokio::task::spawn_blocking(move || {
                    let _ = db.with_conn(|c| crud::set_cached_claude_code_result(c, &json, now));
                });
            }
            result
        }
        Err(msg) if msg == "rate_limited" => PlanUsageResult::RateLimited,
        Err(msg) => PlanUsageResult::Error { message: msg },
    }
}

#[tauri::command]
pub fn get_cached_claude_code_result(
    state: State<'_, AppState>,
) -> CommandResult<Option<CachedPlanUsageResult>> {
    let row = state.db.with_conn(|c| crud::get_cached_claude_code_result(c))?;
    Ok(row.and_then(|(blob, fetched_at)| {
        serde_json::from_str::<serde_json::Value>(&blob)
            .ok()
            .map(|result| CachedPlanUsageResult { result, fetched_at })
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn window_to_dto(
    w: modelmeter_core::providers::claude_code::WindowUsage,
    label: &str,
    now: i64,
) -> RateLimitWindowDto {
    let resets_in_seconds = w
        .resets_at
        .map(|t| (t - now).max(0))
        .unwrap_or(0);
    RateLimitWindowDto {
        percent_used: w.utilization,
        window_label: label.to_string(),
        resets_in_seconds,
        resets_at: w.resets_at,
    }
}

