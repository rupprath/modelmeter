#![forbid(unsafe_code)]

//! Tauri commands specific to the ElevenLabs provider.
//!
//! `get_elevenlabs_state` — returns a single `ElevenLabsStateDto` for the
//! dashboard card. Subscription snapshot (character_count, character_limit,
//! reset timestamp, overage) is fetched live from the API on every call.
//! Daily credit history is read from `usage_records` via
//! `crud::get_daily_credits` over the supplied window.
//!
//! Why a custom command instead of `get_usage_summary` + `get_latest_balance`?
//! ElevenLabs' native unit is credits, not dollars, so the standard
//! `total_cost_usd` aggregation does not apply. Subscription state lives in
//! `usage_records.provider_metadata` (credits) and on the live API
//! (character quota); neither maps cleanly to the dollar-denominated `balances`
//! table.

use serde::Serialize;
use tauri::State;

use modelmeter_core::{crud, providers::elevenlabs::ElevenLabsProvider};

use crate::{error::CommandResult, state::AppState};

#[derive(Debug, Serialize)]
pub struct DayCreditsDto {
    pub bucket_start: i64,
    pub credits: i64,
}

impl From<crud::DayCredits> for DayCreditsDto {
    fn from(d: crud::DayCredits) -> Self {
        Self { bucket_start: d.bucket_start, credits: d.credits }
    }
}

#[derive(Debug, Serialize)]
pub struct ElevenLabsStateDto {
    // Subscription snapshot (live).
    pub tier: String,
    pub status: String,
    pub character_count: i64,
    pub character_limit: i64,
    pub next_reset_unix: i64,
    pub current_overage_usd: f64,
    pub currency: String,
    pub fetched_at: i64,
    // Daily credit history (from DB).
    pub daily_credits: Vec<DayCreditsDto>,
}

/// Returns the ElevenLabs dashboard card data: live subscription snapshot +
/// daily credit history from the local DB over `[since_ts, until_ts)`.
#[tauri::command]
pub async fn get_elevenlabs_state(
    state: State<'_, AppState>,
    provider_id: i64,
    since_ts: i64,
    until_ts: i64,
) -> CommandResult<ElevenLabsStateDto> {
    let accessor = state.secrets.accessor_for("elevenlabs");
    let provider = ElevenLabsProvider::new(accessor);

    let snapshot = provider
        .fetch_subscription_state()
        .await
        .map_err(|e| crate::error::CommandError::new(format!("elevenlabs subscription: {e}")))?;

    let daily = state
        .db
        .with_conn(move |c| crud::get_daily_credits(c, provider_id, since_ts, until_ts))?;

    Ok(ElevenLabsStateDto {
        tier: snapshot.tier,
        status: snapshot.status,
        character_count: snapshot.character_count,
        character_limit: snapshot.character_limit,
        next_reset_unix: snapshot.next_reset_unix,
        current_overage_usd: snapshot.current_overage_usd,
        currency: snapshot.currency,
        fetched_at: snapshot.fetched_at,
        daily_credits: daily.into_iter().map(Into::into).collect(),
    })
}
