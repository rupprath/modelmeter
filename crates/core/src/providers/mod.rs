#![forbid(unsafe_code)]

//! Provider abstraction: `Provider` trait, canonical types, `ProviderDescriptor`,
//! and the static `REGISTRY`.
//!
//! Each provider module exports a `pub const DESCRIPTOR: ProviderDescriptor` and
//! is listed in `REGISTRY`. All routing — building a live client, validating a
//! key, resolving a slug to a `ProviderKind` — is driven by iterating the
//! registry rather than by exhaustive match arms.
//!
//! # Adding a new provider
//!
//! 1. Create `src/providers/newprovider.rs` implementing `Provider` and
//!    exporting `pub const DESCRIPTOR: super::ProviderDescriptor`.
//! 2. Add `pub mod newprovider;` below.
//! 3. Add `newprovider::DESCRIPTOR` to `REGISTRY`.

pub mod anthropic;
pub mod claude_code;
pub mod openai;
pub mod xai;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::secrets::SecretStore;

// ---------------------------------------------------------------------------
// BoxFuture — object-safe async return type
// ---------------------------------------------------------------------------

/// Heap-allocated, type-erased future. Used as the return type for all
/// `Provider` trait methods so that `Box<dyn Provider>` works without the
/// `async_trait` crate.
pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Canonical types
// ---------------------------------------------------------------------------

/// Time range for a `fetch_usage` call.
#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    /// Inclusive start, unix UTC seconds.
    pub start: i64,
    /// Exclusive end, unix UTC seconds.
    pub end: i64,
}

/// Granularity of a usage bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BucketGranularity {
    Minute,
    Hour,
    Day,
}

impl BucketGranularity {
    pub fn as_str(self) -> &'static str {
        match self {
            BucketGranularity::Minute => "minute",
            BucketGranularity::Hour => "hour",
            BucketGranularity::Day => "day",
        }
    }
}

/// Whether a cost figure was reported by the provider or computed locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostSource {
    Reported,
    Computed,
}

impl CostSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CostSource::Reported => "reported",
            CostSource::Computed => "computed",
        }
    }
}

/// Canonical usage record. Fields set by provider impls are marked (P); fields
/// set by the sync engine before the DB write are marked (S).
#[derive(Debug, Clone)]
pub struct UsageRecord {
    /// (S) Set by the database on insert; provider impls leave this as 0.
    pub id: i64,
    /// (S) Database primary key of the owning provider.
    pub provider_id: i64,
    /// (S) Which provider produced this record.
    pub provider: String,
    /// (P) Provider's model identifier. Empty string when not reported.
    pub model: String,
    /// (P) Bucket start, unix UTC seconds (inclusive).
    pub bucket_start: i64,
    /// (P) Bucket end, unix UTC seconds (exclusive).
    pub bucket_end: i64,
    /// (P) Granularity of the bucket.
    pub bucket_granularity: BucketGranularity,
    /// (P) Input tokens (regular, non-cached). Null on cost-only rows.
    pub input_tokens: Option<i64>,
    /// (P) Output tokens. Null on cost-only rows.
    pub output_tokens: Option<i64>,
    /// (P) Cache-creation tokens. Null if not reported or on cost-only rows.
    pub cache_creation_tokens: Option<i64>,
    /// (P) Cache-read (cached input) tokens. Null if not reported.
    pub cache_read_tokens: Option<i64>,
    /// (P) Request count. Null on cost-only rows or when not reported.
    pub request_count: Option<i64>,
    /// (P) Cost in USD. Null on token-only rows.
    pub cost_usd: Option<f64>,
    /// (P) How the cost was determined.
    pub cost_source: CostSource,
    /// (P) JSON blob with provider-specific fields not in the canonical schema.
    pub provider_metadata: Option<String>,
    /// (S) When this record was fetched (unix UTC seconds).
    pub fetched_at: i64,
}

/// What the current balance / spend figure actually represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceShape {
    /// Remaining prepay credit.
    RemainingCredit,
    /// Spend against a configured cap.
    SpendAgainstCap,
    /// Month-to-date spend (UTC calendar month). Both v1 providers produce this.
    SpendThisPeriod,
    /// Provider has no balance-shaped data.
    Unknown,
}

impl BalanceShape {
    pub fn as_str(self) -> &'static str {
        match self {
            BalanceShape::RemainingCredit => "remaining_credit",
            BalanceShape::SpendAgainstCap => "spend_against_cap",
            BalanceShape::SpendThisPeriod => "spend_this_period",
            BalanceShape::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for BalanceShape {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "remaining_credit" => Ok(BalanceShape::RemainingCredit),
            "spend_against_cap" => Ok(BalanceShape::SpendAgainstCap),
            "spend_this_period" => Ok(BalanceShape::SpendThisPeriod),
            "unknown" => Ok(BalanceShape::Unknown),
            other => anyhow::bail!("unknown balance shape '{other}'"),
        }
    }
}

/// A balance snapshot returned by `Provider::fetch_balance`.
#[derive(Debug, Clone)]
pub struct Balance {
    /// Amount in USD; null when the provider did not return a figure.
    pub amount_usd: Option<f64>,
    /// When the snapshot was taken, unix UTC seconds.
    pub as_of: i64,
    /// What the figure represents.
    pub shape: BalanceShape,
    /// Optional human-readable note (e.g. "calendar-month UTC spend").
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// KeyValidation
// ---------------------------------------------------------------------------

/// Reason a credential was rejected by `validate_credential`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidReason {
    /// 401, generic invalid credential.
    NotAccepted,
    /// Wrong shape (e.g. wrong prefix).
    Malformed,
    /// 402 payment required / billing issue.
    BillingIssue,
    /// Other rejection with sanitised detail.
    Other(String),
}

impl std::fmt::Display for InvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidReason::NotAccepted => f.write_str("Credential not accepted by the provider."),
            InvalidReason::Malformed => f.write_str("Key format is invalid."),
            InvalidReason::BillingIssue => f.write_str("Billing issue reported by provider."),
            InvalidReason::Other(msg) => f.write_str(msg),
        }
    }
}

/// Structured result of a `validate_credential` call.
#[derive(Debug, Clone)]
pub enum KeyValidation {
    /// Credential is accepted and has the required permissions.
    Valid,
    /// Credential is rejected.
    Invalid { reason: InvalidReason },
    /// Credential is accepted but lacks required permissions.
    InsufficientPermission { hint: String },
}

// ---------------------------------------------------------------------------
// ProviderError
// ---------------------------------------------------------------------------

/// Structured error type returned by every `Provider` trait method.
///
/// **Redaction contract:** `Display` and `Debug` must never contain the
/// credential value, any decrypted secret, or a full request URL with query
/// parameters. Provider impls sanitise before constructing these variants.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("network error: {0}")]
    Network(String),

    #[error("request timeout reported by provider")]
    RequestTimeout,

    #[error("rate limited; retry after {retry_after_seconds:?}s")]
    RateLimited { retry_after_seconds: Option<u64> },

    #[error("provider server error: HTTP {status}")]
    ServerError { status: u16 },

    #[error("credential not accepted")]
    AuthInvalid,

    #[error("credential lacks required permissions: {detail}")]
    AuthInsufficientPermission { detail: String },

    #[error("billing issue reported by provider")]
    BillingIssue,

    #[error("forbidden: {detail}")]
    Forbidden { detail: String },

    #[error("endpoint not found: {detail}")]
    NotFound { detail: String },

    #[error("client error: HTTP {status}: {detail}")]
    ClientError { status: u16, detail: String },

    #[error("TLS certificate validation failed")]
    TlsCertInvalid,

    #[error("malformed response: {0}")]
    MalformedResponse(String),

    #[error("inconsistent response: {0}")]
    InconsistentResponse(String),
}

impl ProviderError {
    /// Whether the sync engine should treat this error as transient (retry once
    /// after the retry-after window) or permanent (mark the profile failed and
    /// wait for the next scheduled tick).
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::Network(_)
                | ProviderError::RequestTimeout
                | ProviderError::RateLimited { .. }
                | ProviderError::ServerError { .. }
        )
    }

    /// Extracts the `Retry-After` seconds hint, if present.
    pub fn retry_after_seconds(&self) -> Option<u64> {
        if let ProviderError::RateLimited { retry_after_seconds } = self {
            *retry_after_seconds
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Provider trait (object-safe)
// ---------------------------------------------------------------------------

/// The core abstraction every provider must implement.
///
/// All methods are single-attempt and async. Retry, scheduling, and
/// cancellation are the sync engine's responsibility — provider impls never
/// retry on their own. Pagination is handled internally; callers receive the
/// assembled result.
///
/// Methods return `BoxFuture` (a pinned `Box<dyn Future>`) so that
/// `Box<dyn Provider>` is object-safe without the `async_trait` crate.
pub trait Provider: Send + Sync {
    /// Validates the credential with a lightweight, non-writing call.
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>>;

    /// Fetches all usage records for the given time range.
    fn fetch_usage(
        &self,
        range: TimeRange,
    ) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>>;

    /// Fetches the current balance snapshot.
    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>>;
}

// ---------------------------------------------------------------------------
// Provider row (used by sync engine and IPC layer)
// ---------------------------------------------------------------------------

/// A row from the `providers` table, as read by the DB layer.
#[derive(Debug, Clone)]
pub struct ProviderRow {
    pub id: i64,
    pub provider_type: String,
    pub display_name: String,
    pub last_sync_attempted_at: Option<i64>,
    pub last_sync_succeeded_at: Option<i64>,
    pub last_sync_status: String,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// ProviderDescriptor and REGISTRY
// ---------------------------------------------------------------------------

/// A just-in-time keyring accessor, as returned by `SecretStore::accessor_for`.
pub type CredsAccessor = Box<dyn Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static>;

/// Static metadata and factory functions for one provider.
///
/// Each provider module exports a `pub const DESCRIPTOR: ProviderDescriptor`
/// and lists it in `REGISTRY`. All routing is driven by iterating the registry.
pub struct ProviderDescriptor {
    /// URL-safe lowercase identifier stored in the database (e.g. `"openai"`).
    pub slug: &'static str,
    /// Human-readable name shown in the UI (e.g. `"OpenAI"`).
    pub display_name: &'static str,
    /// Single-letter abbreviation for compact UI contexts (e.g. `"O"`).
    pub short: &'static str,
    /// Brand hex color used in provider badges (e.g. `"#10a37f"`).
    pub color: &'static str,
    /// URL for obtaining an admin API key, linked on permission errors.
    pub key_docs_url: Option<&'static str>,
    /// Label shown above the credential input in the Add Provider modal.
    pub key_label: &'static str,
    /// Whether the credential field should be masked (password input).
    pub key_is_secret: bool,
    /// Whether the user must supply a credential. False for providers that
    /// authenticate via a fixed local file (e.g. claude_code).
    pub key_required: bool,
    /// Builds a live provider using a just-in-time keyring accessor.
    pub build: fn(CredsAccessor) -> Box<dyn Provider>,
    /// Builds a provider wrapping a plaintext key (used for key validation only).
    pub build_with_key: fn(Zeroizing<String>) -> Box<dyn Provider>,
}

/// All registered providers. Add one entry here when adding a new provider.
pub static REGISTRY: &[ProviderDescriptor] = &[
    openai::DESCRIPTOR,
    anthropic::DESCRIPTOR,
    claude_code::DESCRIPTOR,
    xai::DESCRIPTOR,
];

// ---------------------------------------------------------------------------
// Shared HTTP helpers (used by every provider module)
// ---------------------------------------------------------------------------

/// Maps a `reqwest` transport error to a `ProviderError`.
pub(super) fn map_reqwest_err(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        return ProviderError::RequestTimeout;
    }
    let msg = format!("{e}");
    let lower = msg.to_lowercase();
    if lower.contains("certificate") || lower.contains("invalid cert") {
        return ProviderError::TlsCertInvalid;
    }
    ProviderError::Network(msg)
}

/// Checks the HTTP status and returns an error for any non-2xx response.
/// Reads the response body on client errors to include it in the detail.
pub(super) async fn check_status(
    resp: reqwest::Response,
) -> Result<reqwest::Response, ProviderError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let code = status.as_u16();
    match code {
        401 => Err(ProviderError::AuthInvalid),
        402 => Err(ProviderError::BillingIssue),
        403 => Err(ProviderError::Forbidden { detail: "forbidden".into() }),
        404 => Err(ProviderError::NotFound { detail: "endpoint not found".into() }),
        408 => Err(ProviderError::RequestTimeout),
        429 => {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            Err(ProviderError::RateLimited { retry_after_seconds: retry_after })
        }
        s if s >= 500 => Err(ProviderError::ServerError { status: s }),
        s => {
            let reason = status.canonical_reason().unwrap_or("unknown client error");
            let body = resp.text().await.unwrap_or_default();
            let detail = if body.is_empty() {
                reason.to_string()
            } else {
                format!("{reason}: {body}")
            };
            Err(ProviderError::ClientError { status: s, detail })
        }
    }
}

// ---------------------------------------------------------------------------
// Shared credential resolver
// ---------------------------------------------------------------------------

/// Calls the credential accessor, mapping keyring errors to `ProviderError`.
pub(super) fn resolve_creds(
    creds: &(dyn Fn() -> Result<Zeroizing<String>> + Send + Sync),
) -> Result<Zeroizing<String>, ProviderError> {
    creds().map_err(|e| {
        let msg = format!("{e:#}");
        if is_keyring_missing(&msg) {
            ProviderError::AuthInvalid
        } else {
            ProviderError::Network(format!("credential unavailable: {msg}"))
        }
    })
}

// ---------------------------------------------------------------------------
// Timestamp helper
// ---------------------------------------------------------------------------

/// Current time as unix UTC seconds.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// Catalog helpers
// ---------------------------------------------------------------------------

/// Returns true if the anyhow error string indicates a missing keyring entry.
/// Covers all known platform-specific messages from the keyring crate v3.
pub(crate) fn is_keyring_missing(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("no matching entry")
        || lower.contains("no such entry")
        || lower.contains("not found in secure storage")
        || lower.contains("entry not found")
}

/// Builds a live `Box<dyn Provider>` for the given slug, fetching
/// credentials just-in-time from the OS keyring.
pub fn build_provider(slug: &str, secrets: &SecretStore) -> Result<Box<dyn Provider>> {
    let desc = REGISTRY
        .iter()
        .find(|d| d.slug == slug)
        .ok_or_else(|| anyhow::anyhow!("no descriptor registered for provider '{slug}'"))?;
    let accessor = Box::new(secrets.accessor_for(desc.slug));
    Ok((desc.build)(accessor))
}

/// Builds a temporary `Box<dyn Provider>` wrapping a plaintext key.
/// Used for key validation before the key is stored in the keyring.
pub fn build_provider_with_key(slug: &str, key: Zeroizing<String>) -> Result<Box<dyn Provider>> {
    let desc = REGISTRY
        .iter()
        .find(|d| d.slug == slug)
        .ok_or_else(|| anyhow::anyhow!("unknown provider slug '{slug}'"))?;
    Ok((desc.build_with_key)(key))
}

// ---------------------------------------------------------------------------
// Mock provider (test-only)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub struct MockProvider {
    pub usage: Vec<UsageRecord>,
    pub balance: Option<Balance>,
    pub validation: KeyValidation,
}

#[cfg(test)]
impl Provider for MockProvider {
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>> {
        let result = Ok(self.validation.clone());
        Box::pin(async move { result })
    }

    fn fetch_usage(&self, _range: TimeRange) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>> {
        let result = Ok(self.usage.clone());
        Box::pin(async move { result })
    }

    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>> {
        let result = Ok(self.balance.clone());
        Box::pin(async move { result })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_slugs_are_unique() {
        let slugs: Vec<_> = REGISTRY.iter().map(|d| d.slug).collect();
        let unique: std::collections::HashSet<_> = slugs.iter().copied().collect();
        assert_eq!(slugs.len(), unique.len(), "duplicate slugs in REGISTRY");
    }

    #[test]
    fn provider_error_transient_classification() {
        assert!(ProviderError::Network("timeout".into()).is_transient());
        assert!(ProviderError::RateLimited { retry_after_seconds: Some(30) }.is_transient());
        assert!(ProviderError::ServerError { status: 503 }.is_transient());
        assert!(!ProviderError::AuthInvalid.is_transient());
        assert!(!ProviderError::MalformedResponse("bad json".into()).is_transient());
    }

    #[test]
    fn provider_error_display_does_not_contain_key() {
        let err = ProviderError::AuthInsufficientPermission {
            detail: "needs admin key".into(),
        };
        let display = format!("{err}");
        assert!(!display.contains("sk-"));
    }

    #[test]
    fn balance_shape_as_str() {
        assert_eq!(BalanceShape::SpendThisPeriod.as_str(), "spend_this_period");
    }

    #[tokio::test]
    async fn mock_provider_validate_and_fetch() {
        let provider: Box<dyn Provider> = Box::new(MockProvider {
            usage: vec![],
            balance: Some(Balance {
                amount_usd: Some(1.23),
                as_of: 0,
                shape: BalanceShape::SpendThisPeriod,
                note: None,
            }),
            validation: KeyValidation::Valid,
        });

        assert!(matches!(provider.validate_credential().await.unwrap(), KeyValidation::Valid));
        assert!(provider.fetch_usage(TimeRange { start: 0, end: 1 }).await.unwrap().is_empty());
        let bal = provider.fetch_balance().await.unwrap().unwrap();
        assert_eq!(bal.amount_usd, Some(1.23));
    }
}
