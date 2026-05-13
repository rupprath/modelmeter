#![forbid(unsafe_code)]

//! Tauri commands for the sync engine (trigger, status).

use tauri::State;

use modelmeter_core::sync::{SyncStatus, SyncTriggerError};

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Re-export as-is; `SyncStatus` and `ProviderSyncStateDto` already derive
/// `Serialize` and are safe to send across the Tauri boundary.
pub type SyncStatusDto = SyncStatus;

impl From<SyncTriggerError> for CommandError {
    fn from(e: SyncTriggerError) -> Self {
        CommandError::new(e)
    }
}

// ---------------------------------------------------------------------------
// Sync commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn trigger_sync_all(state: State<'_, AppState>) -> CommandResult<()> {
    state.sync.trigger_all().await.map_err(Into::into)
}

#[tauri::command]
pub async fn get_sync_status(state: State<'_, AppState>) -> CommandResult<SyncStatusDto> {
    Ok(state.sync.get_status().await)
}
