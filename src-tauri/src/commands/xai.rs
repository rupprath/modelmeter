#![forbid(unsafe_code)]

//! Tauri commands specific to the x.ai (Grok) provider.
//!
//! `get_xai_monthly_history` — fetches the invoice list and returns one entry
//! per month with the total spend. x.ai has no daily-granularity endpoint, so
//! the dashboard's x.ai card uses this for a "Recent monthly spend" display.

use serde::Serialize;
use tauri::State;

use modelmeter_core::{
    crud,
    providers::xai::{MonthlySpend, XaiProvider},
};

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

const MAX_MONTHS: usize = 12;

#[derive(Debug, Serialize)]
pub struct MonthlySpendDto {
    pub year: i32,
    pub month: u32,
    pub amount_usd: f64,
}

impl From<MonthlySpend> for MonthlySpendDto {
    fn from(m: MonthlySpend) -> Self {
        Self {
            year: m.year,
            month: m.month,
            amount_usd: m.amount_usd,
        }
    }
}

/// Returns the last 12 months of x.ai invoice totals for the given provider.
/// Calls the live x.ai management API on each invocation — there's no caching
/// at this layer; the frontend decides how often to fetch.
#[tauri::command]
pub async fn get_xai_monthly_history(
    state: State<'_, AppState>,
    provider_id: i64,
) -> CommandResult<Vec<MonthlySpendDto>> {
    // Look up the configured team_id for this provider row. Without it we
    // can't address the billing endpoint, so we fail fast with a clear msg.
    let db = state.db.clone();
    let row = tokio::task::spawn_blocking(move || db.with_conn(move |c| crud::get_provider(c, provider_id)))
        .await
        .map_err(|e| CommandError::new(format!("get_xai_monthly_history task panicked: {e}")))??
        .ok_or_else(|| CommandError::new("xai provider row not found"))?;
    let team_id = row.team_id.ok_or_else(|| {
        CommandError::new(
            "x.ai provider is missing a Team ID. Remove and re-add the provider, \
             entering your team UUID alongside the management key.",
        )
    })?;

    let accessor = state.secrets.accessor_for("xai");
    let provider = XaiProvider::new(accessor, &team_id);

    let history = provider
        .fetch_monthly_history(MAX_MONTHS)
        .await
        .map_err(|e| CommandError::new(format!("xai monthly history: {e}")))?;

    Ok(history.into_iter().map(Into::into).collect())
}
