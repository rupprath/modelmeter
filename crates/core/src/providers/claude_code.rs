#![forbid(unsafe_code)]

//! Claude Code provider implementation.
//!
//! Auth:       OAuth credentials managed by Claude Code, stored in
//!             `~/.claude/.credentials.json`. Works with any Claude Code
//!             install method (CLI, VS Code extension, etc.).
//! Credential: None required — the credentials file is at a fixed, known path.
//! Usage data: Fetched live via `get_claude_code_plan_usage` Tauri command;
//!             fetch_usage / fetch_balance are intentional no-ops because
//!             rate-limit percentages are point-in-time, not time-series.
//! Validation: Credentials file exists and OAuth token is not expired.

use anyhow::Result;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{
    unix_now, Balance, BoxFuture, CredsAccessor, InvalidReason,
    KeyValidation, Provider, ProviderDescriptor, ProviderError, TimeRange, UsageRecord,
};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

// ---------------------------------------------------------------------------
// Public credential / usage types (used by the Tauri commands layer)
// ---------------------------------------------------------------------------

/// The relevant fields extracted from `~/.claude/.credentials.json`.
pub struct ClaudeCodeCredentials {
    pub access_token: Zeroizing<String>,
    /// Milliseconds since Unix epoch.
    pub expires_at_ms: i64,
    /// e.g. "pro", "max".
    pub subscription_type: String,
}

/// One rate-limit window (session or weekly).
pub struct WindowUsage {
    /// Percentage of the window consumed, 0–100.
    pub utilization: f32,
    /// When the window resets, unix UTC seconds. None if not provided.
    pub resets_at: Option<i64>,
}

/// Live rate-limit snapshot returned by `fetch_plan_usage`.
pub struct PlanUsage {
    pub five_hour: WindowUsage,
    pub seven_day: WindowUsage,
    pub subscription_type: String,
}

/// Reads and returns the OAuth credentials from `~/.claude/.credentials.json`.
/// Returns `Err` with a human-readable message on failure.
pub fn read_claude_credentials() -> Result<ClaudeCodeCredentials, String> {
    let home = dirs::home_dir()
        .ok_or_else(|| "cannot determine home directory".to_string())?;
    let path = home.join(".claude").join(".credentials.json");

    if !path.exists() {
        return Err(
            "Claude Code credentials not found. Open Claude Code and sign in first.".into(),
        );
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read .credentials.json: {e}"))?;

    let file: CredentialsFile = serde_json::from_str(&content)
        .map_err(|e| format!("malformed .credentials.json: {e}"))?;

    let oauth = file
        .claude_ai_oauth
        .ok_or_else(|| "missing claudeAiOauth section in credentials file".to_string())?;

    let access_token = oauth
        .access_token
        .ok_or_else(|| "missing accessToken in credentials file".to_string())?;

    Ok(ClaudeCodeCredentials {
        access_token: Zeroizing::new(access_token),
        expires_at_ms: oauth.expires_at.unwrap_or(0),
        subscription_type: oauth.subscription_type.unwrap_or_else(|| "pro".into()),
    })
}

/// Calls `GET https://api.anthropic.com/api/oauth/usage` with the given
/// access token and returns the parsed rate-limit snapshot.
/// Returns `Err` with a human-readable message on failure (including 429).
pub async fn fetch_plan_usage(access_token: &str) -> Result<PlanUsage, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();

    if status.as_u16() == 429 {
        return Err("rate_limited".into());
    }

    if !status.is_success() {
        return Err(format!("unexpected response: HTTP {}", status.as_u16()));
    }

    let body: UsageResponse = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse usage response: {e}"))?;

    let parse_window = |w: Option<UsageWindow>| -> WindowUsage {
        let w = w.unwrap_or_default();
        WindowUsage {
            utilization: w.utilization.unwrap_or(0.0),
            resets_at: w.resets_at.as_deref().and_then(parse_resets_at),
        }
    };

    Ok(PlanUsage {
        five_hour: parse_window(body.five_hour),
        seven_day: parse_window(body.seven_day),
        subscription_type: String::new(), // filled by caller from credentials
    })
}

fn parse_resets_at(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.timestamp())
}

// ---------------------------------------------------------------------------
// Private API response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct UsageWindow {
    utilization: Option<f32>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
}

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

pub struct ClaudeCodeProvider;

// ---------------------------------------------------------------------------
// ProviderDescriptor
// ---------------------------------------------------------------------------

fn build(_creds: CredsAccessor, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(ClaudeCodeProvider)
}

fn build_with_key(_key: Zeroizing<String>, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(ClaudeCodeProvider)
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    slug: "claude_code",
    display_name: "Claude",
    short: "C",
    color: "#cc785c",
    key_docs_url: None,
    key_label: "",
    key_is_secret: false,
    key_required: false,
    aux_field_label: None,
    aux_field_hint: None,
    aux_field_validator: None,
    build,
    build_with_key,
};

// ---------------------------------------------------------------------------
// Credentials file types (private to this module)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthSection>,
}

#[derive(Deserialize)]
struct OAuthSection {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>, // milliseconds since Unix epoch
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Checks that `~/.claude/.credentials.json` exists and the OAuth token has
/// not expired. Returns Ok(()) on success, Err(reason string) on failure.
fn check_credentials() -> Result<(), String> {
    let creds = read_claude_credentials()?;
    let now_ms = unix_now() * 1000;
    if creds.expires_at_ms > 0 && creds.expires_at_ms < now_ms {
        return Err(
            "Claude Code session has expired. Open Claude Code and sign in again.".into(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider impl
// ---------------------------------------------------------------------------

impl Provider for ClaudeCodeProvider {
    /// Checks that `~/.claude/.credentials.json` exists and the OAuth token
    /// has not expired. No executable or API key is required.
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>> {
        Box::pin(async move {
            tokio::task::spawn_blocking(|| {
                match check_credentials() {
                    Ok(()) => Ok(KeyValidation::Valid),
                    Err(msg) => Ok(KeyValidation::Invalid {
                        reason: InvalidReason::Other(msg),
                    }),
                }
            })
            .await
            .map_err(|e| ProviderError::Network(format!("validate task panicked: {e}")))?
        })
    }

    /// No-op: Claude Code rate-limit data is point-in-time and fetched live
    /// via `get_claude_code_plan_usage`. Nothing is stored in `usage_records`.
    fn fetch_usage(
        &self,
        _range: TimeRange,
    ) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    /// No-op: no credit balance applies to a Claude Pro subscription.
    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>> {
        Box::pin(async move { Ok(None) })
    }
}
