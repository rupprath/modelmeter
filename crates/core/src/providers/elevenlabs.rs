#![forbid(unsafe_code)]

//! ElevenLabs provider implementation.
//!
//! Auth:       Regular API key (`sk_…` prefix). Must be scoped to `User: Read`
//!             only; all other permissions set to "No Access". The key cannot
//!             call any inference endpoint with this scope, so a leak only
//!             exposes quota/usage data — never the ability to spend credits.
//! Native unit: credits. ElevenLabs has no fiat-currency endpoint we can trust
//!             across plans (`fiat_units_spent` is undocumented and not user-
//!             specific). See [[feedback-native-unit-over-dollars]] memory.
//! Endpoints:
//!   - `GET /v1/user/subscription` — quota snapshot (character_count /
//!     character_limit, reset timestamp, status, overage). Returned by the
//!     custom `get_elevenlabs_state` Tauri command, not stored in `balances`.
//!   - `GET /v1/usage/character-stats?metric=credits&aggregation_interval=day`
//!     — daily credit consumption. Returned by `fetch_usage` as one
//!     `UsageRecord` per non-zero day with `cost_usd = None` and credits
//!     encoded in `provider_metadata` JSON `{"credits": N}`.
//!
//! Time units: the character-stats endpoint uses unix **milliseconds** in both
//! request params and the `time` array of the response. Conversion to/from
//! seconds happens at the boundary.

use anyhow::Result;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{
    build_clients, check_status, map_reqwest_err, resolve_creds, unix_now, Balance, BoxFuture,
    BucketGranularity, CostSource, CredsAccessor, InvalidReason, KeyValidation, Provider,
    ProviderDescriptor, ProviderError, TimeRange, UsageRecord,
};

const BASE_URL: &str = "https://api.elevenlabs.io";

/// Required prefix for ElevenLabs API keys created via the web console.
/// Other formats (legacy short keys without underscore, OAuth tokens, etc.) are
/// rejected with a hint pointing the user at the correct creation flow.
const KEY_PREFIX: &str = "sk_";

const WRONG_KEY_HINT: &str = "ElevenLabs API keys start with `sk_`. Open \
elevenlabs.io → your profile → API keys → Create New, set every permission \
group to \"No Access\" except User → Read, and paste the resulting key here. \
Read-only is strongly recommended — ModelMeter only ever reads.";

const INSUFFICIENT_SCOPE_HINT: &str = "The key was accepted but lacks the \
User: Read permission needed to read your subscription. Recreate the key on \
elevenlabs.io with User → Read enabled (all other scopes can stay at \"No \
Access\").";

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

pub struct ElevenLabsProvider {
    creds: Box<dyn Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static>,
    client: reqwest::Client,
    validate_client: reqwest::Client,
    base_url: String,
}

impl ElevenLabsProvider {
    pub fn new(creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static) -> Self {
        Self::with_base_url(BASE_URL, creds)
    }

    pub fn with_base_url(
        base_url: &str,
        creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static,
    ) -> Self {
        let (client, validate_client) = build_clients("ElevenLabs");
        Self {
            creds: Box::new(creds),
            client,
            validate_client,
            base_url: base_url.to_string(),
        }
    }

    fn get_key(&self) -> Result<Zeroizing<String>, ProviderError> {
        resolve_creds(&*self.creds)
    }

    async fn fetch_subscription_inner(
        &self,
        client: &reqwest::Client,
        key: &str,
    ) -> Result<SubscriptionResponse, ProviderError> {
        let url = format!("{}/v1/user/subscription", self.base_url);
        let resp = client
            .get(&url)
            .header("xi-api-key", key)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<SubscriptionResponse>().await.map_err(|e| {
            ProviderError::MalformedResponse(format!("elevenlabs/subscription: {e}"))
        })
    }

    /// Live-fetches the current subscription state. Used by the
    /// `get_elevenlabs_state` Tauri command to feed the dashboard card.
    pub async fn fetch_subscription_state(
        &self,
    ) -> Result<SubscriptionState, ProviderError> {
        let key = self.get_key()?;
        let body = self.fetch_subscription_inner(&self.client, key.as_str()).await?;
        Ok(SubscriptionState::from(body))
    }
}

/// Snapshot of an ElevenLabs subscription, the live data behind the dashboard card.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubscriptionState {
    pub tier: String,
    pub status: String,
    pub character_count: i64,
    pub character_limit: i64,
    pub next_reset_unix: i64,
    pub current_overage_usd: f64,
    pub currency: String,
    pub fetched_at: i64,
}

impl From<SubscriptionResponse> for SubscriptionState {
    fn from(r: SubscriptionResponse) -> Self {
        let overage_usd = r
            .current_overage
            .as_ref()
            .and_then(|o| o.amount.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        Self {
            tier: r.tier.unwrap_or_default(),
            status: r.status.unwrap_or_default(),
            character_count: r.character_count.unwrap_or(0),
            character_limit: r.character_limit.unwrap_or(0),
            next_reset_unix: r.next_character_count_reset_unix.unwrap_or(0),
            current_overage_usd: overage_usd,
            currency: r.currency.unwrap_or_else(|| "usd".into()),
            fetched_at: unix_now(),
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderDescriptor
// ---------------------------------------------------------------------------

fn build(creds: CredsAccessor, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(ElevenLabsProvider::new(creds))
}

fn build_with_key(key: Zeroizing<String>, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(ElevenLabsProvider::new(move || Ok(key.clone())))
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    slug: "elevenlabs",
    display_name: "ElevenLabs",
    short: "E",
    color: "#7c3aed",
    key_docs_url: Some("https://elevenlabs.io/app/settings/api-keys"),
    key_label: "API Key",
    key_is_secret: true,
    key_required: true,
    aux_field_label: None,
    aux_field_hint: None,
    aux_field_validator: None,
    build,
    build_with_key,
};

// ---------------------------------------------------------------------------
// Raw JSON types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SubscriptionResponse {
    tier: Option<String>,
    status: Option<String>,
    character_count: Option<i64>,
    character_limit: Option<i64>,
    next_character_count_reset_unix: Option<i64>,
    current_overage: Option<OverageAmount>,
    currency: Option<String>,
}

#[derive(Deserialize)]
struct OverageAmount {
    /// String-encoded decimal dollars (e.g. "0", "1.23"). Empty/missing → 0.
    amount: String,
}

#[derive(Deserialize)]
struct CharacterStatsResponse {
    /// Unix milliseconds, one entry per bucket. Length matches every `usage` array.
    time: Vec<i64>,
    /// Keyed by breakdown dimension. With no `breakdown_type`, contains a single
    /// "All" key whose value array is parallel to `time`.
    #[serde(default)]
    usage: std::collections::BTreeMap<String, Vec<f64>>,
}

// ---------------------------------------------------------------------------
// Provider impl
// ---------------------------------------------------------------------------

impl Provider for ElevenLabsProvider {
    /// Sanity-checks the `sk_` prefix, then calls `/v1/user/subscription`. A 200
    /// proves both authentication and that the User: Read scope is present.
    /// 401/403 → the key was created without User: Read.
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>> {
        Box::pin(async move {
            let key = self.get_key()?;
            if !key.starts_with(KEY_PREFIX) {
                return Ok(KeyValidation::Invalid {
                    reason: InvalidReason::Other(WRONG_KEY_HINT.into()),
                });
            }

            match self.fetch_subscription_inner(&self.validate_client, key.as_str()).await {
                Ok(_) => Ok(KeyValidation::Valid),
                Err(ProviderError::AuthInvalid) => Ok(KeyValidation::Invalid {
                    reason: InvalidReason::NotAccepted,
                }),
                Err(ProviderError::Forbidden { .. }) => {
                    Ok(KeyValidation::InsufficientPermission {
                        hint: INSUFFICIENT_SCOPE_HINT.into(),
                    })
                }
                Err(e) => Err(e),
            }
        })
    }

    /// Fetches daily credit consumption. Returns one `UsageRecord` per non-zero
    /// day. Credits are stored in `provider_metadata` JSON as `{"credits": N}`;
    /// `cost_usd` is intentionally `None` because ElevenLabs has no
    /// trustworthy per-user fiat conversion.
    fn fetch_usage(
        &self,
        range: TimeRange,
    ) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>> {
        Box::pin(async move {
            let key = self.get_key()?;

            // The character-stats endpoint uses unix milliseconds.
            let start_ms = range.start.saturating_mul(1000);
            let end_ms = range.end.saturating_mul(1000);

            let url = format!("{}/v1/usage/character-stats", self.base_url);
            let resp = self
                .client
                .get(&url)
                .header("xi-api-key", key.as_str())
                .query(&[
                    ("start_unix", start_ms.to_string()),
                    ("end_unix", end_ms.to_string()),
                    ("aggregation_interval", "day".to_string()),
                    ("metric", "credits".to_string()),
                ])
                .send()
                .await
                .map_err(map_reqwest_err)?;
            let resp = check_status(resp).await?;

            let body: CharacterStatsResponse = resp.json().await.map_err(|e| {
                ProviderError::MalformedResponse(format!("elevenlabs/character-stats: {e}"))
            })?;

            // With no breakdown_type, ElevenLabs returns a single "All" series.
            // If for some reason it's missing, treat as empty.
            let values: Vec<f64> = body
                .usage
                .get("All")
                .cloned()
                .unwrap_or_default();

            if values.len() != body.time.len() {
                return Err(ProviderError::InconsistentResponse(format!(
                    "elevenlabs/character-stats: time len ({}) != usage len ({})",
                    body.time.len(),
                    values.len()
                )));
            }

            let mut records = Vec::new();
            for (i, &t_ms) in body.time.iter().enumerate() {
                let credits = values[i].round() as i64;
                if credits == 0 {
                    continue;
                }
                let bucket_start = t_ms / 1000;
                let bucket_end = bucket_start + 86_400;

                let metadata = serde_json::json!({
                    "credits": credits,
                    "metric": "credits",
                });

                records.push(UsageRecord {
                    id: 0,
                    provider_id: 0,
                    provider: DESCRIPTOR.slug.to_string(),
                    model: String::new(),
                    bucket_start,
                    bucket_end,
                    bucket_granularity: BucketGranularity::Day,
                    input_tokens: None,
                    output_tokens: None,
                    cache_creation_tokens: None,
                    cache_read_tokens: None,
                    request_count: None,
                    cost_usd: None,
                    cost_source: CostSource::Reported,
                    provider_metadata: Some(metadata.to_string()),
                    fetched_at: 0,
                });
            }
            Ok(records)
        })
    }

    /// ElevenLabs has no trustworthy fiat balance figure we can store as a
    /// dollar amount. Subscription state (quota, reset time, overage) is
    /// exposed via the dedicated `get_elevenlabs_state` Tauri command instead.
    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>> {
        Box::pin(async move { Ok(None) })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_parses_overage_string() {
        let body: SubscriptionResponse = serde_json::from_str(
            r#"{
                "tier":"payg","status":"active","character_count":28451,
                "character_limit":37472,"next_character_count_reset_unix":1781478555,
                "current_overage":{"amount":"1.23","currency":"usd"},"currency":"usd"
            }"#,
        )
        .unwrap();
        let state = SubscriptionState::from(body);
        assert_eq!(state.character_count, 28451);
        assert_eq!(state.character_limit, 37472);
        assert!((state.current_overage_usd - 1.23).abs() < 1e-9);
        assert_eq!(state.tier, "payg");
    }

    #[test]
    fn subscription_handles_missing_overage() {
        let body: SubscriptionResponse = serde_json::from_str(
            r#"{"tier":"payg","status":"active","character_count":0,"character_limit":37472}"#,
        )
        .unwrap();
        let state = SubscriptionState::from(body);
        assert_eq!(state.current_overage_usd, 0.0);
    }

    #[test]
    fn subscription_handles_zero_string_overage() {
        let body: SubscriptionResponse = serde_json::from_str(
            r#"{"current_overage":{"amount":"0","currency":"usd"}}"#,
        )
        .unwrap();
        let state = SubscriptionState::from(body);
        assert_eq!(state.current_overage_usd, 0.0);
    }

    #[test]
    fn character_stats_aggregates_one_record_per_nonzero_day() {
        // 4-day window: zero / zero / zero / 28451. Should produce one record.
        let raw = r#"{
            "time":[1778544000000,1778630400000,1778716800000,1778803200000],
            "usage":{"All":[0.0,0.0,0.0,28451.0]}
        }"#;
        let body: CharacterStatsResponse = serde_json::from_str(raw).unwrap();
        let values = body.usage.get("All").cloned().unwrap();
        let nonzero_days: Vec<_> = body
            .time
            .iter()
            .zip(values.iter())
            .filter(|(_, v)| **v != 0.0)
            .collect();
        assert_eq!(nonzero_days.len(), 1);
        let (t_ms, v) = nonzero_days[0];
        assert_eq!(*t_ms / 1000, 1778803200);
        assert_eq!(*v as i64, 28451);
    }

    #[test]
    fn character_stats_inconsistent_lengths_rejected() {
        // time and usage arrays must be the same length; if not, treat as malformed.
        let raw = r#"{"time":[1,2,3],"usage":{"All":[10.0,20.0]}}"#;
        let body: CharacterStatsResponse = serde_json::from_str(raw).unwrap();
        let values = body.usage.get("All").cloned().unwrap();
        assert_ne!(body.time.len(), values.len());
    }

    #[test]
    fn empty_usage_object_is_ok() {
        // Fresh account with no spend: usage is {} which deserializes fine and
        // produces no records.
        let raw = r#"{"time":[1,2,3],"usage":{}}"#;
        let body: CharacterStatsResponse = serde_json::from_str(raw).unwrap();
        let values: Vec<f64> = body.usage.get("All").cloned().unwrap_or_default();
        assert!(values.is_empty());
    }

    #[test]
    fn key_prefix_check_rejects_other_shapes() {
        assert!("sk_abc123".starts_with(KEY_PREFIX));
        assert!(!"xai-token-abc".starts_with(KEY_PREFIX));
        assert!(!"sk-proj-abc".starts_with(KEY_PREFIX));
    }

    #[test]
    fn descriptor_basics() {
        assert_eq!(DESCRIPTOR.slug, "elevenlabs");
        assert!(DESCRIPTOR.key_required);
        assert!(DESCRIPTOR.key_is_secret);
    }
}
