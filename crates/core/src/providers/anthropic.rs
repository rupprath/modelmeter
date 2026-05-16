#![forbid(unsafe_code)]

//! Anthropic provider implementation.
//!
//! Auth:       Admin API key (`sk-ant-admin-` prefix, organization accounts only).
//! Token data: `/v1/organizations/usage_report/messages` and
//!             `/v1/organizations/usage_report/claude_code` (hourly buckets).
//! Cost data:  `/v1/organizations/cost_report` (daily buckets; amounts in cents as strings).
//! Balance:    MTD spend via the cost report for the current UTC calendar month.
//! Validation: Single call to `GET /v1/organizations/usage_report/messages`.

use anyhow::Result;
use serde::Deserialize;
use tracing::{debug, warn};

use super::{
    build_clients, check_status, map_reqwest_err, resolve_creds, unix_now, Balance, BalanceShape,
    BoxFuture, BucketGranularity, CostSource, CredsAccessor, InvalidReason, KeyValidation,
    Provider, ProviderDescriptor, ProviderError, TimeRange, UsageRecord,
};
use zeroize::Zeroizing;

const BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

// Individual-account error marker in the 401 response body.
const INDIVIDUAL_ACCOUNT_MARKER: &str = "organization";

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    creds: Box<dyn Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static>,
    client: reqwest::Client,
    validate_client: reqwest::Client,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static) -> Self {
        Self::with_base_url(BASE_URL, creds)
    }

    /// Constructs the provider pointing at `base_url` instead of the real API.
    /// Used by integration tests to redirect requests at a wiremock server.
    pub fn with_base_url(
        base_url: &str,
        creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static,
    ) -> Self {
        let (client, validate_client) = build_clients("Anthropic");
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
}

// ---------------------------------------------------------------------------
// ProviderDescriptor
// ---------------------------------------------------------------------------

fn build(creds: CredsAccessor, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(AnthropicProvider::new(creds))
}

fn build_with_key(key: Zeroizing<String>, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(AnthropicProvider::new(move || Ok(key.clone())))
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    slug: "anthropic",
    display_name: "Anthropic",
    short: "A",
    color: "#c96442",
    key_docs_url: Some("https://console.anthropic.com/settings/admin-keys"),
    key_label: "API key",
    key_is_secret: true,
    key_required: true,
    aux_field_label: None,
    aux_field_hint: None,
    aux_field_validator: None,
    build,
    build_with_key,
};

// ---------------------------------------------------------------------------
// Raw JSON types (private)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UsagePage {
    data: Vec<UsageBucket>,
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct UsageBucket {
    starting_at: String, // ISO 8601
    ending_at: String,
    results: Vec<UsageResult>,
}

#[derive(Deserialize, Default)]
struct UsageResult {
    #[serde(default)]
    uncached_input_tokens: i64,
    cache_creation: Option<CacheCreation>,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    server_tool_use: Option<ServerToolUse>,
    model: Option<String>,
    service_tier: Option<String>,
    workspace_id: Option<String>,
    api_key_id: Option<String>,
    inference_geo: Option<String>,
}

#[derive(Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: i64,
    #[serde(default)]
    ephemeral_1h_input_tokens: i64,
}

#[derive(Deserialize)]
struct ServerToolUse {
    web_search_requests: Option<i64>,
}

#[derive(Deserialize)]
struct CostPage {
    data: Vec<CostBucket>,
    has_more: bool,
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct CostBucket {
    starting_at: String,
    ending_at: String,
    results: Vec<CostResult>,
}

#[derive(Deserialize)]
struct CostResult {
    amount: String, // decimal USD string, e.g. "23.140485" = $23.14
    currency: String,
    description: Option<String>,
    model: Option<String>,
    inference_geo: Option<String>,
    workspace_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Domain helpers
// ---------------------------------------------------------------------------

/// Converts a unix UTC timestamp to an ISO 8601 string (`2026-02-01T00:00:00Z`).
fn to_iso8601(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Converts a unix UTC timestamp to a date-only string (`2026-02-01`).
/// Used for the cost_report endpoint which uses day-level granularity.
fn to_date_str(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// Parses an ISO 8601 string to a unix UTC timestamp.
fn from_iso8601(s: &str) -> Result<i64, ProviderError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp())
        .map_err(|e| {
            ProviderError::MalformedResponse(format!("anthropic: bad timestamp '{s}': {e}"))
        })
}

/// Parses Anthropic's cost amount string (decimal USD, e.g. `"23.140485"`).
fn parse_amount_usd(s: &str) -> Result<f64, ProviderError> {
    s.trim()
        .parse::<f64>()
        .map_err(|e| {
            ProviderError::MalformedResponse(format!("anthropic: bad cost amount '{s}': {e}"))
        })
}

// ---------------------------------------------------------------------------
// Internal fetch helpers
// ---------------------------------------------------------------------------

impl AnthropicProvider {
    fn authed_get(&self, url: &str, key: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
    }

    fn authed_get_validate(&self, url: &str, key: &str) -> reqwest::RequestBuilder {
        self.validate_client
            .get(url)
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
    }

    /// Fetches all pages from one usage-report endpoint.
    async fn fetch_usage_report(
        &self,
        endpoint: &str,
        range: TimeRange,
    ) -> Result<Vec<UsageRecord>, ProviderError> {
        let key = self.get_key()?;
        let mut records = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut req = self
                .authed_get(
                    &format!("{}/v1/organizations/usage_report/{}", self.base_url, endpoint),
                    key.as_str(),
                )
                .query(&[
                    ("starting_at", to_iso8601(range.start)),
                    ("ending_at", to_iso8601(range.end)),
                    ("bucket_width", "1h".to_string()),
                    ("limit", "168".to_string()),
                ])
                .query(&[("group_by[]", "model")]);

            if let Some(ref c) = cursor {
                req = req.query(&[("page", c.as_str())]);
            }

            let resp = req.send().await.map_err(map_reqwest_err)?;
            let resp = check_status(resp).await?;

            let body = resp.bytes().await.map_err(map_reqwest_err)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::MalformedResponse(
                    format!("anthropic/{endpoint}: response exceeds size limit"),
                ));
            }
            let page: UsagePage = serde_json::from_slice(&body).map_err(|e| {
                ProviderError::MalformedResponse(format!("anthropic/{endpoint}: {e}"))
            })?;

            for bucket in &page.data {
                let bucket_start = from_iso8601(&bucket.starting_at)?;
                let bucket_end = from_iso8601(&bucket.ending_at)?;

                for result in &bucket.results {
                    let model = result.model.clone().unwrap_or_default();
                    if model.is_empty() {
                        warn!(endpoint, "anthropic usage result has no model");
                    }

                    let cache_creation_tokens = result.cache_creation.as_ref().map(|c| {
                        c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens
                    });

                    let metadata = serde_json::json!({
                        "endpoint": endpoint,
                        "service_tier": result.service_tier,
                        "workspace_id": result.workspace_id,
                        "api_key_id": result.api_key_id,
                        "inference_geo": result.inference_geo,
                        "cache_creation_5m": result.cache_creation.as_ref().map(|c| c.ephemeral_5m_input_tokens),
                        "cache_creation_1h": result.cache_creation.as_ref().map(|c| c.ephemeral_1h_input_tokens),
                        "server_tool_use_web_search": result.server_tool_use.as_ref().and_then(|s| s.web_search_requests),
                    });

                    records.push(UsageRecord {
                        id: 0,
                        provider_id: 0,
                        provider: DESCRIPTOR.slug.to_string(),
                        model,
                        bucket_start,
                        bucket_end,
                        bucket_granularity: BucketGranularity::Hour,
                        input_tokens: Some(result.uncached_input_tokens),
                        output_tokens: Some(result.output_tokens),
                        cache_creation_tokens,
                        cache_read_tokens: Some(result.cache_read_input_tokens),
                        request_count: None,
                        cost_usd: None,
                        cost_source: CostSource::Reported,
                        provider_metadata: Some(metadata.to_string()),
                        fetched_at: 0,
                    });
                }
            }

            if !page.has_more {
                break;
            }
            cursor = page.next_page;
            if cursor.is_none() {
                break;
            }
        }

        debug!(endpoint, count = records.len(), "anthropic usage fetched");
        Ok(records)
    }

    /// Fetches all pages from the cost-report endpoint for the given range.
    ///
    /// The cost_report API uses day-level granularity and requires ending_at > starting_at
    /// at the calendar-day level. When both timestamps fall on the same UTC date (e.g. the
    /// sync ran earlier today and is now running again), we return an empty set rather than
    /// sending a same-day range that the API rejects.
    async fn fetch_cost_report_range(
        &self,
        start: i64,
        end: i64,
    ) -> Result<Vec<UsageRecord>, ProviderError> {
        let start_date = to_date_str(start);
        let end_date = to_date_str(end);
        if start_date >= end_date {
            return Ok(vec![]);
        }

        let key = self.get_key()?;
        let mut records = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut req = self
                .authed_get(
                    &format!("{}/v1/organizations/cost_report", self.base_url),
                    key.as_str(),
                )
                .query(&[
                    ("starting_at", start_date.clone()),
                    ("ending_at", end_date.clone()),
                    ("bucket_width", "1d".to_string()),
                    ("limit", "31".to_string()),
                ])
                .query(&[("group_by[]", "description")]);

            if let Some(ref c) = cursor {
                req = req.query(&[("page", c.as_str())]);
            }

            let resp = req.send().await.map_err(map_reqwest_err)?;
            let resp = check_status(resp).await?;

            let body = resp.bytes().await.map_err(map_reqwest_err)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::MalformedResponse(
                    "anthropic/cost_report: response exceeds size limit".into(),
                ));
            }
            let page: CostPage = serde_json::from_slice(&body).map_err(|e| {
                ProviderError::MalformedResponse(format!("anthropic/cost_report: {e}"))
            })?;

            for bucket in &page.data {
                let bucket_start = from_iso8601(&bucket.starting_at)?;
                let bucket_end = from_iso8601(&bucket.ending_at)?;

                for result in &bucket.results {
                    if result.currency.to_uppercase() != "USD" {
                        return Err(ProviderError::MalformedResponse(format!(
                            "anthropic/cost_report: unexpected currency '{}'",
                            result.currency
                        )));
                    }

                    let cost_usd = parse_amount_usd(&result.amount)?;
                    let model = result.model.clone().unwrap_or_default();

                    let metadata = serde_json::json!({
                        "description": result.description,
                        "inference_geo": result.inference_geo,
                        "workspace_id": result.workspace_id,
                    });

                    records.push(UsageRecord {
                        id: 0,
                        provider_id: 0,
                        provider: DESCRIPTOR.slug.to_string(),
                        model,
                        bucket_start,
                        bucket_end,
                        bucket_granularity: BucketGranularity::Day,
                        input_tokens: None,
                        output_tokens: None,
                        cache_creation_tokens: None,
                        cache_read_tokens: None,
                        request_count: None,
                        cost_usd: Some(cost_usd),
                        cost_source: CostSource::Reported,
                        provider_metadata: Some(metadata.to_string()),
                        fetched_at: 0,
                    });
                }
            }

            if !page.has_more {
                break;
            }
            cursor = page.next_page;
            if cursor.is_none() {
                break;
            }
        }

        debug!(count = records.len(), "anthropic cost report fetched");
        Ok(records)
    }
}

// ---------------------------------------------------------------------------
// Provider impl
// ---------------------------------------------------------------------------

impl Provider for AnthropicProvider {
    /// Validates by probing the messages usage-report endpoint (the same endpoint
    /// used for sync). This tests exactly the permissions ModelMeter requires.
    ///
    /// 200 → Valid. 401 with individual-account body → InsufficientPermission.
    /// Other 401 → Invalid. 403 → InsufficientPermission.
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>> {
        Box::pin(async move {
            let key = self.get_key()?;
            let now = unix_now();
            let resp = self
                .authed_get_validate(
                    &format!("{}/v1/organizations/usage_report/messages", self.base_url),
                    key.as_str(),
                )
                .query(&[
                    ("starting_at", to_iso8601(now - 3600)),
                    ("ending_at", to_iso8601(now)),
                    ("bucket_width", "1h".to_string()),
                    ("limit", "1".to_string()),
                ])
                .send()
                .await
                .map_err(map_reqwest_err)?;

            if resp.status().is_success() {
                return Ok(KeyValidation::Valid);
            }

            match resp.status().as_u16() {
                401 => {
                    let body = resp.text().await.unwrap_or_default();
                    if body.to_lowercase().contains(INDIVIDUAL_ACCOUNT_MARKER) {
                        return Ok(KeyValidation::InsufficientPermission {
                            hint: "Anthropic usage data requires an organization account. \
                                   Personal accounts cannot access the Admin API. Set up an \
                                   organization in the Claude Console under Settings → Organization."
                                .into(),
                        });
                    }
                    Ok(KeyValidation::Invalid { reason: InvalidReason::NotAccepted })
                }
                403 => Ok(KeyValidation::InsufficientPermission {
                    hint: "This key lacks permission to read organization usage data. \
                           Ensure it is an Admin API key created at console.anthropic.com \
                           under Settings → Admin API Keys."
                        .into(),
                }),
                _ => Err(check_status(resp).await.unwrap_err()),
            }
        })
    }

    /// Hits the `messages` usage-report endpoint and the cost-report endpoint
    /// in parallel, returning the combined record set.
    ///
    /// Cost-report failures are non-fatal (logged and skipped).
    fn fetch_usage(&self, range: TimeRange) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>> {
        Box::pin(async move {
            // Cost report uses daily granularity, so always look back at least 7 days
            // regardless of last_sync_succeeded_at. This ensures recently-posted charges
            // (which may appear a day or two late) are captured every sync.
            let cost_start = range.start.min(range.end.saturating_sub(7 * 86_400));
            let (messages, costs) = tokio::join!(
                self.fetch_usage_report("messages", range),
                self.fetch_cost_report_range(cost_start, range.end),
            );

            let mut all = Vec::new();
            all.extend(messages?);

            match costs {
                Ok(records) => all.extend(records),
                Err(e) => warn!("anthropic cost_report endpoint failed, skipping: {}", e),
            }

            Ok(all)
        })
    }

    /// Returns rolling 30-day spend via the cost report.
    ///
    /// Anthropic's Admin API does not expose a prepaid credit balance endpoint;
    /// a 30-day window is used so the widget shows meaningful data even at the
    /// start of a new month.
    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>> {
        Box::pin(async move {
            let end = unix_now();
            let start = end - 30 * 86_400;

            let cost_records = self.fetch_cost_report_range(start, end).await?;
            let total: f64 = cost_records.iter().filter_map(|r| r.cost_usd).sum();

            Ok(Some(Balance {
                amount_usd: Some(total),
                as_of: end,
                shape: BalanceShape::SpendThisPeriod,
                note: Some("Rolling 30-day spend".into()),
            }))
        })
    }
}

// ---------------------------------------------------------------------------
// JSON parsing helpers (test-only; unit-tested in the tests module below)
// ---------------------------------------------------------------------------

/// Parses a `UsagePage` JSON string into `UsageRecord`s.
#[cfg(test)]
pub(crate) fn parse_usage_page_json(
    endpoint: &str,
    body: &str,
) -> Result<Vec<UsageRecord>, ProviderError> {
    let page: UsagePage = serde_json::from_str(body)
        .map_err(|e| ProviderError::MalformedResponse(format!("anthropic/{endpoint}: {e}")))?;
    let mut records = Vec::new();
    for bucket in &page.data {
        let bucket_start = from_iso8601(&bucket.starting_at)?;
        let bucket_end = from_iso8601(&bucket.ending_at)?;
        for result in &bucket.results {
            let model = result.model.clone().unwrap_or_default();
            let cache_creation_tokens = result.cache_creation.as_ref().map(|c| {
                c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens
            });
            let metadata = serde_json::json!({
                "endpoint": endpoint,
                "service_tier": result.service_tier,
                "workspace_id": result.workspace_id,
                "api_key_id": result.api_key_id,
                "inference_geo": result.inference_geo,
            });
            records.push(UsageRecord {
                id: 0,
                provider_id: 0,
                provider: DESCRIPTOR.slug.to_string(),
                model,
                bucket_start,
                bucket_end,
                bucket_granularity: BucketGranularity::Hour,
                input_tokens: Some(result.uncached_input_tokens),
                output_tokens: Some(result.output_tokens),
                cache_creation_tokens,
                cache_read_tokens: Some(result.cache_read_input_tokens),
                request_count: None,
                cost_usd: None,
                cost_source: CostSource::Reported,
                provider_metadata: Some(metadata.to_string()),
                fetched_at: 0,
            });
        }
    }
    Ok(records)
}

/// Parses a `CostPage` JSON string into `UsageRecord`s.
#[cfg(test)]
pub(crate) fn parse_cost_page_json(body: &str) -> Result<Vec<UsageRecord>, ProviderError> {
    let page: CostPage = serde_json::from_str(body)
        .map_err(|e| ProviderError::MalformedResponse(format!("anthropic/cost_report: {e}")))?;
    let mut records = Vec::new();
    for bucket in &page.data {
        let bucket_start = from_iso8601(&bucket.starting_at)?;
        let bucket_end = from_iso8601(&bucket.ending_at)?;
        for result in &bucket.results {
            if result.currency.to_uppercase() != "USD" {
                return Err(ProviderError::MalformedResponse(format!(
                    "anthropic/cost_report: unexpected currency '{}'",
                    result.currency
                )));
            }
            let cost_usd = parse_amount_usd(&result.amount)?;
            let model = result.model.clone().unwrap_or_default();
            let metadata = serde_json::json!({
                "description": result.description,
                "inference_geo": result.inference_geo,
                "workspace_id": result.workspace_id,
            });
            records.push(UsageRecord {
                id: 0,
                provider_id: 0,
                provider: DESCRIPTOR.slug.to_string(),
                model,
                bucket_start,
                bucket_end,
                bucket_granularity: BucketGranularity::Day,
                input_tokens: None,
                output_tokens: None,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                request_count: None,
                cost_usd: Some(cost_usd),
                cost_source: CostSource::Reported,
                provider_metadata: Some(metadata.to_string()),
                fetched_at: 0,
            });
        }
    }
    Ok(records)
}

// ---------------------------------------------------------------------------
// Unit tests (translation logic only — no HTTP calls)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_amount_usd_decimal() {
        assert!((parse_amount_usd("23.140485").unwrap() - 23.140485).abs() < 1e-6);
    }

    #[test]
    fn parse_amount_usd_zero() {
        assert_eq!(parse_amount_usd("0").unwrap(), 0.0);
    }

    #[test]
    fn parse_amount_usd_invalid_returns_error() {
        assert!(parse_amount_usd("not-a-number").is_err());
    }

    #[test]
    fn to_iso8601_roundtrip() {
        let ts: i64 = 1_769_904_000; // 2026-02-01T00:00:00Z
        let s = to_iso8601(ts);
        let back = from_iso8601(&s).unwrap();
        assert_eq!(ts, back);
    }

    #[test]
    fn from_iso8601_valid() {
        let ts = from_iso8601("2026-02-01T00:00:00Z").unwrap();
        assert_eq!(ts, 1_769_904_000);
    }

    #[test]
    fn from_iso8601_invalid_returns_error() {
        assert!(from_iso8601("not-a-date").is_err());
    }

    #[test]
    fn cache_creation_tokens_summed() {
        let creation = CacheCreation {
            ephemeral_5m_input_tokens: 3000,
            ephemeral_1h_input_tokens: 1000,
        };
        assert_eq!(
            creation.ephemeral_5m_input_tokens + creation.ephemeral_1h_input_tokens,
            4000
        );
    }

    #[test]
    fn provider_error_display_never_contains_sk_key() {
        let cases = [
            ProviderError::AuthInvalid,
            ProviderError::Network("connection refused".into()),
            ProviderError::MalformedResponse("bad timestamp".into()),
        ];
        for err in &cases {
            let s = format!("{err}");
            assert!(!s.contains("sk-"), "error display contains key-shaped string: {s}");
        }
    }

    // ── JSON fixture parsing ─────────────────────────────────────────────────

    const USAGE_FIXTURE: &str = r#"{
        "data": [{
            "starting_at": "2026-02-01T00:00:00Z",
            "ending_at": "2026-02-01T01:00:00Z",
            "results": [{
                "uncached_input_tokens": 2000,
                "cache_read_input_tokens": 500,
                "output_tokens": 400,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 100,
                    "ephemeral_1h_input_tokens": 50
                },
                "model": "claude-sonnet-4-6",
                "service_tier": "standard"
            }]
        }],
        "has_more": false,
        "next_page": null
    }"#;

    const COST_FIXTURE: &str = r#"{
        "data": [{
            "starting_at": "2026-02-01T00:00:00Z",
            "ending_at": "2026-02-02T00:00:00Z",
            "results": [{
                "amount": "5.67",
                "currency": "USD",
                "description": "claude-sonnet-4-6 input",
                "model": "claude-sonnet-4-6",
                "inference_geo": null,
                "workspace_id": null
            }]
        }],
        "has_more": false,
        "next_page": null
    }"#;

    #[test]
    fn parse_usage_page_extracts_token_fields() {
        let records = parse_usage_page_json("messages", USAGE_FIXTURE).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "claude-sonnet-4-6");
        assert_eq!(r.input_tokens, Some(2000));
        assert_eq!(r.output_tokens, Some(400));
        assert_eq!(r.cache_read_tokens, Some(500));
        assert_eq!(r.cache_creation_tokens, Some(150)); // 100 + 50
        assert_eq!(r.bucket_start, 1769904000); // 2026-02-01T00:00:00Z
        assert_eq!(r.bucket_granularity, BucketGranularity::Hour);
        assert!(r.cost_usd.is_none(), "usage rows have no cost");
    }

    #[test]
    fn parse_cost_page_extracts_cost_and_model() {
        let records = parse_cost_page_json(COST_FIXTURE).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "claude-sonnet-4-6");
        assert!((r.cost_usd.unwrap() - 5.67).abs() < 1e-9);
        assert_eq!(r.bucket_granularity, BucketGranularity::Day);
        assert!(r.input_tokens.is_none(), "cost rows have no token counts");
    }

    #[test]
    fn parse_cost_page_rejects_non_usd() {
        let body = r#"{
            "data":[{"starting_at":"2026-02-01T00:00:00Z","ending_at":"2026-02-02T00:00:00Z",
            "results":[{"amount":"100","currency":"EUR","description":null,"model":null,
            "inference_geo":null,"workspace_id":null}]}],
            "has_more":false,"next_page":null
        }"#;
        assert!(parse_cost_page_json(body).is_err());
    }

    #[test]
    fn parse_usage_page_empty_data_returns_no_records() {
        let body = r#"{"data":[],"has_more":false,"next_page":null}"#;
        let records = parse_usage_page_json("messages", body).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn parse_usage_page_malformed_json_returns_error() {
        assert!(parse_usage_page_json("messages", "{}").is_err());
    }
}
