#![forbid(unsafe_code)]

use modelmeter_core::config::{load_config, save_config};
use serde::{Deserialize, Serialize};

use crate::error::{CommandError, CommandResult};

#[derive(Debug, Serialize)]
pub struct AppSettingsDto {
    pub sync_interval_seconds: u64,
    pub retention_max_days: u32,
    pub retention_max_size_mb: u64,
    pub theme: String,
    pub window_height_px: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SetConfigArgs {
    pub sync_interval_seconds: u64,
    pub retention_max_days: u32,
    pub retention_max_size_mb: u64,
    pub theme: String,
    pub window_height_px: Option<u32>,
}

#[tauri::command]
pub fn get_config() -> CommandResult<AppSettingsDto> {
    let cfg = load_config().map_err(CommandError::new)?;
    Ok(AppSettingsDto {
        sync_interval_seconds: cfg.sync.interval_seconds,
        retention_max_days: cfg.retention.max_days,
        retention_max_size_mb: cfg.retention.max_size_mb,
        theme: cfg.ui.theme,
        window_height_px: cfg.ui.window_height_px,
    })
}

#[tauri::command]
pub fn set_config(args: SetConfigArgs) -> CommandResult<()> {
    let mut cfg = load_config().map_err(CommandError::new)?;
    cfg.sync.interval_seconds = args.sync_interval_seconds;
    cfg.retention.max_days = args.retention_max_days;
    cfg.retention.max_size_mb = args.retention_max_size_mb;
    cfg.ui.theme = args.theme;
    cfg.ui.window_height_px = args.window_height_px;
    let cfg = cfg.validated().map_err(CommandError::new)?;
    save_config(&cfg).map_err(CommandError::new)
}
