#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub ui: UiConfig,
}

/// UI display preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// "light", "dark", or "system" (follows OS). Default: "system".
    pub theme: String,
    /// Preferred window height in logical pixels. None means use the OS default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_height_px: Option<u32>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { theme: "system".to_string(), window_height_px: None }
    }
}

/// Sync-engine settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Seconds between scheduled syncs. Range: 60–86 400. Default: 900 (15 min).
    pub interval_seconds: u64,
    /// Max profiles synced concurrently. Range: 1–50. Default: 10.
    pub max_concurrent_profiles: usize,
    /// Whether background sync is paused. Survives app restarts. Default: false.
    pub paused: bool,
}

/// Data-retention settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Drop usage records older than this many days. Default: 90.
    pub max_days: u32,
    /// Drop oldest records when the database file exceeds this many MiB. Default: 1024 (1 GiB).
    pub max_size_mb: u64,
}

/// Logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// tracing filter string; e.g. "info", "modelmeter=debug". Default: "info".
    pub level: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            interval_seconds: 900,
            max_concurrent_profiles: 10,
            paused: false,
        }
    }
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            max_days: 90,
            max_size_mb: 1024,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            sync: SyncConfig::default(),
            retention: RetentionConfig::default(),
            log: LogConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl AppConfig {
    /// Clamps numeric values to valid ranges and validates the log level.
    pub fn validated(mut self) -> Result<Self> {
        self.sync.interval_seconds = self.sync.interval_seconds.clamp(60, 86_400);
        self.sync.max_concurrent_profiles = self.sync.max_concurrent_profiles.clamp(1, 50);
        self.retention.max_days = self.retention.max_days.clamp(1, 3_650);
        self.retention.max_size_mb = self.retention.max_size_mb.clamp(100, 102_400);

        let valid_levels = ["error", "warn", "info", "debug", "trace"];
        let base_level = self.log.level.split(',').next().unwrap_or("info").trim();
        if !valid_levels.contains(&base_level) && !base_level.contains('=') {
            anyhow::bail!(
                "invalid log level '{}'; expected one of error|warn|info|debug|trace \
                 or a tracing filter like 'modelmeter=debug'",
                self.log.level
            );
        }

        let valid_themes = ["light", "dark", "system"];
        if !valid_themes.contains(&self.ui.theme.as_str()) {
            self.ui.theme = "system".to_string();
        }

        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// File-system helpers
// ---------------------------------------------------------------------------

/// Path to the config file:
///   Windows:  %APPDATA%\modelmeter\config.toml
///   macOS:    ~/Library/Application Support/modelmeter/config.toml
pub fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine OS config directory")?
        .join("modelmeter");
    Ok(dir.join("config.toml"))
}

/// Path to the data directory (database file, future log files):
///   Windows:  %APPDATA%\modelmeter\   (same root as config on Windows)
///   macOS:    ~/Library/Application Support/modelmeter/
pub fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("could not determine OS data directory")?
        .join("modelmeter");
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Load / save
// ---------------------------------------------------------------------------

/// Loads config from disk, returning defaults if the file does not exist.
/// Unknown TOML keys are silently ignored; values out of range are clamped.
pub fn load_config() -> Result<AppConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    let config: AppConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse config at {}", path.display()))?;
    config.validated()
}

/// Writes config to disk, creating the directory if needed.
pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(config).context("failed to serialise config")?;
    std::fs::write(&path, &contents)
        .with_context(|| format!("failed to write config to {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        AppConfig::default().validated().unwrap();
    }

    #[test]
    fn interval_clamped_to_minimum() {
        let mut cfg = AppConfig::default();
        cfg.sync.interval_seconds = 0;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.sync.interval_seconds, 60);
    }

    #[test]
    fn interval_clamped_to_maximum() {
        let mut cfg = AppConfig::default();
        cfg.sync.interval_seconds = 999_999;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.sync.interval_seconds, 86_400);
    }

    #[test]
    fn concurrency_clamped() {
        let mut cfg = AppConfig::default();
        cfg.sync.max_concurrent_profiles = 0;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.sync.max_concurrent_profiles, 1);
    }

    #[test]
    fn invalid_log_level_is_rejected() {
        let mut cfg = AppConfig::default();
        cfg.log.level = "verbose".to_string();
        assert!(cfg.validated().is_err());
    }

    #[test]
    fn filter_string_log_level_is_accepted() {
        let mut cfg = AppConfig::default();
        cfg.log.level = "modelmeter=debug,info".to_string();
        assert!(cfg.validated().is_ok());
    }

    #[test]
    fn roundtrip_toml() {
        let original = AppConfig::default();
        let serialised = toml::to_string_pretty(&original).unwrap();
        let parsed: AppConfig = toml::from_str(&serialised).unwrap();
        assert_eq!(parsed.sync.interval_seconds, original.sync.interval_seconds);
        assert_eq!(parsed.retention.max_days, original.retention.max_days);
        assert_eq!(parsed.log.level, original.log.level);
    }

    #[test]
    fn parse_explicit_toml_string() {
        let toml = r#"
[sync]
interval_seconds = 1800
max_concurrent_profiles = 5
paused = true

[retention]
max_days = 30
max_size_mb = 512

[log]
level = "debug"

[ui]
theme = "dark"
"#;
        let cfg: AppConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.sync.interval_seconds, 1800);
        assert_eq!(cfg.sync.max_concurrent_profiles, 5);
        assert!(cfg.sync.paused);
        assert_eq!(cfg.retention.max_days, 30);
        assert_eq!(cfg.retention.max_size_mb, 512);
        assert_eq!(cfg.log.level, "debug");
        assert_eq!(cfg.ui.theme, "dark");
    }

    #[test]
    fn defaults_when_optional_sections_absent() {
        let cfg: AppConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.sync.interval_seconds, 900);
        assert_eq!(cfg.retention.max_days, 90);
        assert_eq!(cfg.log.level, "info");
        assert_eq!(cfg.ui.theme, "system");
    }

    #[test]
    fn retention_max_days_clamped_below_minimum() {
        let mut cfg = AppConfig::default();
        cfg.retention.max_days = 0;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.retention.max_days, 1);
    }

    #[test]
    fn retention_max_days_clamped_above_maximum() {
        let mut cfg = AppConfig::default();
        cfg.retention.max_days = 99_999;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.retention.max_days, 3_650);
    }

    #[test]
    fn retention_max_size_mb_clamped_below_minimum() {
        let mut cfg = AppConfig::default();
        cfg.retention.max_size_mb = 1;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.retention.max_size_mb, 100);
    }

    #[test]
    fn retention_max_size_mb_clamped_above_maximum() {
        let mut cfg = AppConfig::default();
        cfg.retention.max_size_mb = 999_999;
        let cfg = cfg.validated().unwrap();
        assert_eq!(cfg.retention.max_size_mb, 102_400);
    }
}
