#![forbid(unsafe_code)]

//! Tauri commands for provider CRUD and key validation.

use serde::Serialize;
use tauri::State;
use zeroize::Zeroizing;

use modelmeter_core::{
    crud,
    providers::{build_provider_with_key, unix_now, KeyValidation, REGISTRY},
};

use crate::{
    error::{CommandError, CommandResult},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Data-transfer objects
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ProviderDto {
    pub id: i64,
    pub provider_type: String,
    pub display_name: String,
    pub last_sync_attempted_at: Option<i64>,
    pub last_sync_succeeded_at: Option<i64>,
    pub last_sync_status: String,
    pub created_at: i64,
    /// Optional aux identifier (currently only x.ai's team UUID). Surfaced so
    /// the UI can render "Team: …" subtext on the provider row if desired.
    pub team_id: Option<String>,
}

impl From<modelmeter_core::providers::ProviderRow> for ProviderDto {
    fn from(r: modelmeter_core::providers::ProviderRow) -> Self {
        Self {
            id: r.id,
            provider_type: r.provider_type,
            display_name: r.display_name,
            last_sync_attempted_at: r.last_sync_attempted_at,
            last_sync_succeeded_at: r.last_sync_succeeded_at,
            last_sync_status: r.last_sync_status,
            created_at: r.created_at,
            team_id: r.team_id,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KeyValidationDto {
    Valid,
    Invalid { reason: String },
    InsufficientPermission { hint: String },
}

impl From<KeyValidation> for KeyValidationDto {
    fn from(kv: KeyValidation) -> Self {
        match kv {
            KeyValidation::Valid => KeyValidationDto::Valid,
            KeyValidation::Invalid { reason } => {
                KeyValidationDto::Invalid { reason: reason.to_string() }
            }
            KeyValidation::InsufficientPermission { hint } => {
                KeyValidationDto::InsufficientPermission { hint }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Provider commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> CommandResult<Vec<ProviderDto>> {
    let db = state.db.clone();
    let rows = tokio::task::spawn_blocking(move || db.with_conn(crud::list_providers))
        .await
        .map_err(|e| CommandError::new(format!("list_providers task panicked: {e}")))??;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Adds a provider: stores the key in the OS keyring, creates a DB row, and
/// triggers an immediate sync. If keyring storage fails, the DB row is rolled back.
///
/// `aux_value` is an optional second identifier some providers need alongside
/// the key (currently only x.ai's team UUID). Must satisfy the descriptor's
/// `aux_field_validator` if one is defined.
#[tauri::command]
pub async fn add_provider(
    state: State<'_, AppState>,
    provider_type: String,
    display_name: String,
    key: String,
    aux_value: Option<String>,
) -> CommandResult<i64> {
    // Wrap immediately so the plaintext is zeroed from memory when dropped.
    let key = Zeroizing::new(key);
    require_known_slug(&provider_type)?;
    let aux_value = validate_aux(&provider_type, aux_value.as_deref())?;
    let provider_type_str = provider_type;
    let now = unix_now();

    // 1. Insert DB row.
    let db = state.db.clone();
    let pt_for_db = provider_type_str.clone();
    let display_name_clone = display_name.clone();
    let aux_for_db = aux_value.clone();
    let provider_id = tokio::task::spawn_blocking(move || {
        db.with_conn(move |c| {
            crud::create_provider(
                c,
                &pt_for_db,
                &display_name_clone,
                aux_for_db.as_deref(),
                now,
            )
        })
    })
    .await
    .map_err(|e| CommandError::new(format!("add_provider task panicked: {e}")))??;

    // 2. Store key in OS keyring.
    let secrets = state.secrets.clone();
    let key_clone = key.clone();
    let pt = provider_type_str.clone();
    let keyring_result = tokio::task::spawn_blocking(move || secrets.set(&pt, key_clone.as_str()))
        .await
        .map_err(|e| CommandError::new(format!("add_provider keyring task panicked: {e}")))?;

    if let Err(e) = keyring_result {
        // Best-effort rollback of the DB row.
        let db = state.db.clone();
        let _ = tokio::task::spawn_blocking(move || {
            db.with_conn(move |c| crud::delete_provider(c, provider_id))
        })
        .await;
        return Err(CommandError::new(format!("failed to store provider key: {e}")));
    }

    // 3. Register with the sync coordinator and trigger an immediate sync.
    state.sync.on_provider_added(provider_id).await;

    Ok(provider_id)
}

/// Removes a provider: deletes the DB row (cascades to usage/balance), removes
/// the keyring secret, and deregisters the provider from the sync coordinator.
#[tauri::command]
pub async fn remove_provider(state: State<'_, AppState>, id: i64) -> CommandResult<bool> {
    tracing::info!(provider_id = id, "remove_provider: invoked");

    // 1. Fetch provider_type before deleting (needed for keyring delete).
    let db = state.db.clone();
    let maybe_row = tokio::task::spawn_blocking(move || db.with_conn(move |c| crud::get_provider(c, id)))
        .await
        .map_err(|e| CommandError::new(format!("remove_provider task panicked: {e}")))??;

    let Some(row) = maybe_row else { return Ok(false) };
    let provider_type_str = row.provider_type.as_str().to_owned();

    // 2. Delete DB row (CASCADE removes usage_records and balances).
    let db = state.db.clone();
    let deleted = tokio::task::spawn_blocking(move || db.with_conn(move |c| crud::delete_provider(c, id)))
        .await
        .map_err(|e| CommandError::new(format!("remove_provider task panicked: {e}")))??;

    // 3. Delete keyring secret (non-fatal if already gone).
    if deleted {
        let secrets = state.secrets.clone();
        let pt = provider_type_str;
        let _ = tokio::task::spawn_blocking(move || secrets.delete(&pt)).await;

        // 4. Deregister from sync coordinator.
        state.sync.on_provider_deleted(id).await;
    }

    tracing::info!(provider_id = id, deleted, "remove_provider: completed");
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// Provider catalog (static metadata from the in-process REGISTRY)
// ---------------------------------------------------------------------------

/// Static metadata for one provider type, sent to the frontend.
#[derive(Debug, Serialize)]
pub struct ProviderKindDto {
    pub slug: String,
    pub display_name: String,
    pub short: String,
    pub color: String,
    pub key_docs_url: Option<String>,
    pub key_label: String,
    pub key_is_secret: bool,
    /// Whether the user must supply a credential to add this provider.
    pub key_required: bool,
    /// Label for the secondary (aux) input the UI should render alongside the
    /// key. `None` for providers that need only a key.
    pub aux_field_label: Option<String>,
    /// Optional hint shown below the aux input.
    pub aux_field_hint: Option<String>,
}

/// Returns the list of supported provider types with their display metadata.
#[tauri::command]
pub async fn list_provider_kinds() -> Vec<ProviderKindDto> {
    REGISTRY
        .iter()
        .map(|d| ProviderKindDto {
            slug: d.slug.to_string(),
            display_name: d.display_name.to_string(),
            short: d.short.to_string(),
            color: d.color.to_string(),
            key_docs_url: d.key_docs_url.map(|s| s.to_string()),
            key_label: d.key_label.to_string(),
            key_is_secret: d.key_is_secret,
            key_required: d.key_required,
            aux_field_label: d.aux_field_label.map(|s| s.to_string()),
            aux_field_hint: d.aux_field_hint.map(|s| s.to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Key validation (non-writing, separate from the main sync path)
// ---------------------------------------------------------------------------

/// Validates a raw API key against the provider without persisting anything.
/// Uses the REGISTRY so no per-provider match arm is needed here.
#[tauri::command]
pub async fn validate_provider_key(
    provider_type: String,
    key: String,
    aux_value: Option<String>,
) -> CommandResult<KeyValidationDto> {
    // Wrap immediately so the plaintext is zeroed from memory when dropped.
    let key = Zeroizing::new(key);

    require_known_slug(&provider_type)?;
    let aux_value = validate_aux(&provider_type, aux_value.as_deref())?;

    let provider = build_provider_with_key(&provider_type, key, aux_value.as_deref())
        .map_err(|e| CommandError::new(e))?;

    let result = provider.validate_credential().await?;
    Ok(result.into())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_known_slug(provider_type: &str) -> CommandResult<()> {
    if REGISTRY.iter().any(|d| d.slug == provider_type) {
        Ok(())
    } else {
        Err(CommandError::new(format!("unknown provider type: {provider_type}")))
    }
}

/// Looks up the descriptor for `provider_type` and applies its aux-field
/// rules to `aux_value`. Returns the trimmed value (or `None` for descriptors
/// without an aux field). Errors if the descriptor requires an aux value and
/// none was given, or if the descriptor's validator rejects the value.
fn validate_aux(provider_type: &str, aux_value: Option<&str>) -> CommandResult<Option<String>> {
    let desc = REGISTRY
        .iter()
        .find(|d| d.slug == provider_type)
        .ok_or_else(|| CommandError::new(format!("unknown provider type: {provider_type}")))?;

    match (desc.aux_field_label, aux_value) {
        (None, _) => Ok(None),
        (Some(label), None) | (Some(label), Some("")) => Err(CommandError::new(format!(
            "{label} is required for this provider."
        ))),
        (Some(_), Some(v)) => {
            let trimmed = v.trim().to_string();
            if let Some(validator) = desc.aux_field_validator {
                validator(&trimmed).map_err(|m| CommandError::new(m.to_string()))?;
            }
            Ok(Some(trimmed))
        }
    }
}
