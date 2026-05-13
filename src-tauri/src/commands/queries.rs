#![forbid(unsafe_code)]

//! Tauri commands for widget query primitives and layout persistence.

use serde::Serialize;
use tauri::State;

use modelmeter_core::crud;

use crate::{
    error::CommandResult,
    state::AppState,
};

// ---------------------------------------------------------------------------
// Data-transfer objects
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct BalanceDto {
    pub id: i64,
    pub provider_id: i64,
    pub amount_usd: Option<f64>,
    pub shape: String,
    pub note: Option<String>,
    pub as_of: i64,
    pub fetched_at: i64,
}

impl From<crud::BalanceRow> for BalanceDto {
    fn from(r: crud::BalanceRow) -> Self {
        Self {
            id: r.id,
            provider_id: r.provider_id,
            amount_usd: r.amount_usd,
            shape: r.shape.as_str().to_owned(),
            note: r.note,
            as_of: r.as_of,
            fetched_at: r.fetched_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UsageSummaryDto {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_creation_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cost_usd: f64,
    pub total_request_count: i64,
}

impl From<crud::UsageSummary> for UsageSummaryDto {
    fn from(s: crud::UsageSummary) -> Self {
        Self {
            total_input_tokens: s.total_input_tokens,
            total_output_tokens: s.total_output_tokens,
            total_cache_creation_tokens: s.total_cache_creation_tokens,
            total_cache_read_tokens: s.total_cache_read_tokens,
            total_cost_usd: s.total_cost_usd,
            total_request_count: s.total_request_count,
        }
    }
}

// ---------------------------------------------------------------------------
// Query commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_latest_balance(
    state: State<'_, AppState>,
    provider_id: i64,
) -> CommandResult<Option<BalanceDto>> {
    let row = state.db.with_conn(move |c| crud::get_latest_balance(c, provider_id))?;
    Ok(row.map(Into::into))
}

#[tauri::command]
pub fn get_usage_summary(
    state: State<'_, AppState>,
    provider_id: i64,
    since_ts: i64,
    until_ts: i64,
) -> CommandResult<UsageSummaryDto> {
    let summary =
        state.db.with_conn(move |c| crud::get_usage_summary(c, provider_id, since_ts, until_ts))?;
    Ok(summary.into())
}
