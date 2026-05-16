#![forbid(unsafe_code)]

//! OpenAI provider implementation.
//!
//! Auth:       Admin API key (`sk-` prefix, org-scoped admin keys).
//! Token data: `/v1/organization/usage/{endpoint}` (hourly buckets).
//! Cost data:  `/v1/organization/costs` (daily buckets).
//! Balance:    MTD spend via the costs endpoint for the current UTC calendar month.

use anyhow::Result;
use serde::Deserialize;
use tracing::{debug, warn};

use super::{
    build_clients, check_status, map_reqwest_err, resolve_creds, unix_now, Balance,
    BalanceShape, BoxFuture, BucketGranularity, CostSource, CredsAccessor, InvalidReason,
    KeyValidation, Provider, ProviderDescriptor, ProviderError, TimeRange,
    UsageRecord,
};
use zeroize::Zeroizing;

const BASE_URL: &str = "https://api.openai.com";
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

pub struct OpenAiProvider {
    creds: Box<dyn Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static>,
    client: reqwest::Client,
    /// Short-timeout client used only for key validation.
    validate_client: reqwest::Client,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static) -> Self {
        Self::with_base_url(BASE_URL, creds)
    }

    /// Constructs the provider pointing at `base_url` instead of the real API.
    /// Used by integration tests to redirect requests at a wiremock server.
    pub fn with_base_url(
        base_url: &str,
        creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static,
    ) -> Self {
        let (client, validate_client) = build_clients("OpenAI");
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
    Box::new(OpenAiProvider::new(creds))
}

fn build_with_key(key: Zeroizing<String>, _aux: Option<&str>) -> Box<dyn Provider> {
    Box::new(OpenAiProvider::new(move || Ok(key.clone())))
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    slug: "openai",
    display_name: "OpenAI",
    short: "O",
    color: "#10a37f",
    key_docs_url: Some("https://platform.openai.com/settings/organization/admin-keys"),
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
    start_time: i64,
    end_time: i64,
    results: Vec<UsageResult>,
}

#[derive(Deserialize)]
struct UsageResult {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    num_model_requests: i64,
    #[serde(default)]
    input_cached_tokens: i64,
    input_audio_tokens: Option<i64>,
    output_audio_tokens: Option<i64>,
    model: Option<String>,
    project_id: Option<String>,
    user_id: Option<String>,
    api_key_id: Option<String>,
    batch: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct CostPage {
    #[serde(default)]
    data: Vec<CostBucket>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct CostBucket {
    start_time: i64,
    end_time: i64,
    #[serde(default)]
    results: Vec<CostResult>,
}

#[derive(Deserialize)]
struct CostResult {
    amount: CostAmount,
    line_item: Option<String>,
    project_id: Option<String>,
}

#[derive(Deserialize)]
struct CostAmount {
    #[serde(deserialize_with = "de_f64_or_string")]
    value: f64,
    currency: String,
}

/// Deserializes a JSON number or a JSON string containing a number into f64.
/// OpenAI returns cost amounts as string-encoded decimals to preserve precision,
/// e.g. `"0.0003640000000000000000000000000000000"`.
fn de_f64_or_string<'de, D: serde::Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    use serde::de::{self, Visitor};
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = f64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a float or a string containing a float")
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> { Ok(v as f64) }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> { Ok(v as f64) }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<f64, E> {
            v.parse::<f64>().map_err(de::Error::custom)
        }
    }
    de.deserialize_any(V)
}

// ---------------------------------------------------------------------------
// Domain helpers
// ---------------------------------------------------------------------------

fn first_day_of_current_month_utc() -> i64 {
    use chrono::{Datelike, TimeZone, Utc};
    let now = Utc::now();
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|| now.timestamp())
}

/// Parses `"gpt-4o-2024-08-06, Input"` → `(Some("gpt-4o-2024-08-06"), Some("Input"))`.
fn parse_line_item(s: &str) -> (Option<String>, Option<String>) {
    match s.split_once(',') {
        Some((model, metric)) => (
            Some(model.trim().to_string()),
            Some(metric.trim().to_string()),
        ),
        None => (None, None),
    }
}

// ---------------------------------------------------------------------------
// Internal fetch helpers
// ---------------------------------------------------------------------------

impl OpenAiProvider {
    /// Fetches all pages from one token-usage endpoint for the given range.
    async fn fetch_usage_endpoint(
        &self,
        endpoint: &str,
        range: TimeRange,
    ) -> Result<Vec<UsageRecord>, ProviderError> {
        let key = self.get_key()?;
        let mut records = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get(format!("{}/v1/organization/usage/{}", self.base_url, endpoint))
                .bearer_auth(key.as_str())
                .query(&[
                    ("start_time", range.start.to_string()),
                    ("end_time", range.end.to_string()),
                    ("bucket_width", "1h".to_string()),
                    ("limit", "168".to_string()),
                ]);
            // vector_stores and code_interpreter_sessions don't support group_by=model.
            if endpoint != "vector_stores" && endpoint != "code_interpreter_sessions" {
                req = req.query(&[("group_by[]", "model")]);
            }
            if let Some(ref c) = cursor {
                req = req.query(&[("page", c.as_str())]);
            }

            let resp = req.send().await.map_err(map_reqwest_err)?;
            let resp = check_status(resp).await?;

            let body = resp.bytes().await.map_err(map_reqwest_err)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::MalformedResponse(
                    format!("openai/{endpoint}: response exceeds size limit"),
                ));
            }
            let page: UsagePage = serde_json::from_slice(&body)
                .map_err(|e| ProviderError::MalformedResponse(format!("openai/{endpoint}: {e}")))?;

            for bucket in &page.data {
                for result in &bucket.results {
                    let model = result.model.clone().unwrap_or_default();
                    if model.is_empty() {
                        warn!(endpoint, "openai usage result has no model; stored as empty string");
                    }

                    let metadata = serde_json::json!({
                        "endpoint": endpoint,
                        "project_id": result.project_id,
                        "user_id": result.user_id,
                        "api_key_id": result.api_key_id,
                        "batch": result.batch,
                        "input_audio_tokens": result.input_audio_tokens,
                        "output_audio_tokens": result.output_audio_tokens,
                    });

                    records.push(UsageRecord {
                        id: 0,
                        provider_id: 0,
                        provider: DESCRIPTOR.slug.to_string(),
                        model,
                        bucket_start: bucket.start_time,
                        bucket_end: bucket.end_time,
                        bucket_granularity: BucketGranularity::Hour,
                        input_tokens: Some(result.input_tokens),
                        output_tokens: Some(result.output_tokens),
                        cache_creation_tokens: None,
                        cache_read_tokens: Some(result.input_cached_tokens),
                        request_count: Some(result.num_model_requests),
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

        debug!(endpoint, count = records.len(), "openai usage fetched");
        Ok(records)
    }

    /// Fetches all pages from the costs endpoint for the given range.
    /// Returns one `UsageRecord` per line item per daily bucket.
    ///
    /// The costs API uses daily granularity; ranges shorter than one day produce no
    /// new cost buckets and may be rejected by the API, so we skip them.
    async fn fetch_costs_range(
        &self,
        start: i64,
        end: i64,
    ) -> Result<Vec<UsageRecord>, ProviderError> {
        if end - start < 86_400 {
            return Ok(vec![]);
        }
        let key = self.get_key()?;
        let mut records = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut req = self
                .client
                .get(format!("{}/v1/organization/costs", self.base_url))
                .bearer_auth(key.as_str())
                .query(&[
                    ("start_time", start.to_string()),
                    ("end_time", end.to_string()),
                    ("bucket_width", "1d".to_string()),
                    ("limit", "30".to_string()),
                ]);
            req = req.query(&[("group_by[]", "line_item")]);
            if let Some(ref c) = cursor {
                req = req.query(&[("page", c.as_str())]);
            }

            let resp = req.send().await.map_err(map_reqwest_err)?;
            let resp = check_status(resp).await?;

            let body = resp.bytes().await.map_err(map_reqwest_err)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::MalformedResponse(
                    "openai/costs: response exceeds size limit".into(),
                ));
            }
            let page: CostPage = serde_json::from_slice(&body)
                .map_err(|e| ProviderError::MalformedResponse(format!("openai/costs: {e}")))?;

            for bucket in &page.data {
                for result in &bucket.results {
                    if result.amount.currency.to_lowercase() != "usd" {
                        return Err(ProviderError::MalformedResponse(format!(
                            "openai/costs: unexpected currency '{}'",
                            result.amount.currency
                        )));
                    }

                    let (model, metric) = result
                        .line_item
                        .as_deref()
                        .map(parse_line_item)
                        .unwrap_or((None, None));

                    let metadata = serde_json::json!({
                        "line_item": result.line_item,
                        "metric": metric,
                        "project_id": result.project_id,
                    });

                    records.push(UsageRecord {
                        id: 0,
                        provider_id: 0,
                        provider: DESCRIPTOR.slug.to_string(),
                        model: model.unwrap_or_default(),
                        bucket_start: bucket.start_time,
                        bucket_end: bucket.end_time,
                        bucket_granularity: BucketGranularity::Day,
                        input_tokens: None,
                        output_tokens: None,
                        cache_creation_tokens: None,
                        cache_read_tokens: None,
                        request_count: None,
                        cost_usd: Some(result.amount.value),
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

        debug!(count = records.len(), "openai costs fetched");
        Ok(records)
    }
}

// ---------------------------------------------------------------------------
// Provider impl
// ---------------------------------------------------------------------------

impl Provider for OpenAiProvider {
    /// Two-step probe:
    /// 1. `GET /v1/organization/usage/completions?limit=1` (checks for admin scope)
    /// 2. `GET /v1/models` on 401 (distinguishes invalid vs underprivileged key)
    ///
    /// `/v1/models` works for all valid key types; `/v1/me` is unreliable for
    /// admin keys (machine credentials with no associated user).
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>> {
        Box::pin(async move {
            let key = self.get_key()?;
            let now = unix_now();

            let resp = self
                .validate_client
                .get(format!("{}/v1/organization/usage/completions", self.base_url))
                .bearer_auth(key.as_str())
                .query(&[
                    ("start_time", (now - 3600).to_string()),
                    ("end_time", now.to_string()),
                    ("limit", "1".to_string()),
                ])
                .send()
                .await
                .map_err(map_reqwest_err)?;

            if resp.status().is_success() {
                return Ok(KeyValidation::Valid);
            }

            let status = resp.status().as_u16();

            // 401 or 403: probe /v1/models to distinguish invalid from underprivileged.
            // Project keys (sk-proj-*) return 403 on org-level endpoints but 200 on
            // /v1/models. Standard user keys and invalid keys both return 401. Either
            // way, /v1/models success means the key is valid but under-permissioned.
            if status == 401 || status == 403 {
                let models_resp = self
                    .validate_client
                    .get(format!("{}/v1/models", self.base_url))
                    .bearer_auth(key.as_str())
                    .send()
                    .await
                    .map_err(map_reqwest_err)?;

                if models_resp.status().is_success() {
                    return Ok(KeyValidation::InsufficientPermission {
                        hint: "Admin API key required. Project keys (sk-proj-) and standard user keys \
                               can call inference endpoints but not the organization usage and costs \
                               endpoints ModelMeter depends on. Create an admin key at \
                               platform.openai.com/settings/organization/admin-keys."
                            .into(),
                    });
                }

                return Ok(KeyValidation::Invalid { reason: InvalidReason::NotAccepted });
            }

            Err(check_status(resp).await.unwrap_err())
        })
    }

    /// Hits all 8 usage endpoints in parallel and the costs endpoint,
    /// returning the combined set of token rows and cost rows.
    ///
    /// Auth errors on usage endpoints propagate immediately (the key is bad).
    /// Non-auth errors on individual usage endpoints are logged and skipped so
    /// a key with partial endpoint access still syncs what it can.
    /// The costs endpoint is always non-fatal (a read-only admin key may lack
    /// access to it, which just means cost rows are omitted from the sync).
    fn fetch_usage(&self, range: TimeRange) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>> {
        Box::pin(async move {
            // Costs use daily granularity; always look back at least 7 days so recently-
            // posted charges are captured even when last_sync_succeeded_at is recent.
            let cost_start = range.start.min(range.end.saturating_sub(7 * 86_400));
            let (c, em, im, asp, atr, mo, vs, ci, costs) = tokio::join!(
                self.fetch_usage_endpoint("completions", range),
                self.fetch_usage_endpoint("embeddings", range),
                self.fetch_usage_endpoint("images", range),
                self.fetch_usage_endpoint("audio_speeches", range),
                self.fetch_usage_endpoint("audio_transcriptions", range),
                self.fetch_usage_endpoint("moderations", range),
                self.fetch_usage_endpoint("vector_stores", range),
                self.fetch_usage_endpoint("code_interpreter_sessions", range),
                self.fetch_costs_range(cost_start, range.end),
            );

            let endpoint_names = [
                "completions", "embeddings", "images", "audio_speeches",
                "audio_transcriptions", "moderations", "vector_stores",
                "code_interpreter_sessions",
            ];
            let usage_results = [c, em, im, asp, atr, mo, vs, ci];

            let mut all = Vec::new();

            for (name, result) in endpoint_names.iter().zip(usage_results) {
                match result {
                    Ok(records) => all.extend(records),
                    // Skip only endpoint-specific 4xx errors (e.g. group_by not supported).
                    // Everything else — auth, transient, malformed — propagates so the sync
                    // engine can handle retries and failure classification correctly.
                    Err(e @ ProviderError::ClientError { .. })
                    | Err(e @ ProviderError::NotFound { .. }) => {
                        warn!("openai usage endpoint {} failed, skipping: {}", name, e);
                    }
                    Err(e) => return Err(e),
                }
            }

            match costs {
                Ok(records) => all.extend(records),
                Err(e) => warn!("openai costs endpoint failed, skipping: {}", e),
            }

            Ok(all)
        })
    }

    /// Returns month-to-date spend for the current UTC calendar month.
    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>> {
        Box::pin(async move {
            let start = first_day_of_current_month_utc();
            let end = unix_now();

            let cost_records = self.fetch_costs_range(start, end).await?;
            let total: f64 = cost_records
                .iter()
                .filter_map(|r| r.cost_usd)
                .sum();

            Ok(Some(Balance {
                amount_usd: Some(total),
                as_of: end,
                shape: BalanceShape::SpendThisPeriod,
                note: Some("UTC calendar month to date".into()),
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
        .map_err(|e| ProviderError::MalformedResponse(format!("openai/{endpoint}: {e}")))?;
    let mut records = Vec::new();
    for bucket in &page.data {
        for result in &bucket.results {
            let model = result.model.clone().unwrap_or_default();
            let metadata = serde_json::json!({
                "endpoint": endpoint,
                "project_id": result.project_id,
                "user_id": result.user_id,
                "api_key_id": result.api_key_id,
                "batch": result.batch,
                "input_audio_tokens": result.input_audio_tokens,
                "output_audio_tokens": result.output_audio_tokens,
            });
            records.push(UsageRecord {
                id: 0,
                provider_id: 0,
                provider: DESCRIPTOR.slug.to_string(),
                model,
                bucket_start: bucket.start_time,
                bucket_end: bucket.end_time,
                bucket_granularity: BucketGranularity::Hour,
                input_tokens: Some(result.input_tokens),
                output_tokens: Some(result.output_tokens),
                cache_creation_tokens: None,
                cache_read_tokens: Some(result.input_cached_tokens),
                request_count: Some(result.num_model_requests),
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
        .map_err(|e| ProviderError::MalformedResponse(format!("openai/costs: {e}")))?;
    let mut records = Vec::new();
    for bucket in &page.data {
        for result in &bucket.results {
            if result.amount.currency.to_lowercase() != "usd" {
                return Err(ProviderError::MalformedResponse(format!(
                    "openai/costs: unexpected currency '{}'",
                    result.amount.currency
                )));
            }
            let (model, metric) = result
                .line_item
                .as_deref()
                .map(parse_line_item)
                .unwrap_or((None, None));
            let metadata = serde_json::json!({
                "line_item": result.line_item,
                "metric": metric,
                "project_id": result.project_id,
            });
            records.push(UsageRecord {
                id: 0,
                provider_id: 0,
                provider: DESCRIPTOR.slug.to_string(),
                model: model.unwrap_or_default(),
                bucket_start: bucket.start_time,
                bucket_end: bucket.end_time,
                bucket_granularity: BucketGranularity::Day,
                input_tokens: None,
                output_tokens: None,
                cache_creation_tokens: None,
                cache_read_tokens: None,
                request_count: None,
                cost_usd: Some(result.amount.value),
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
    fn parse_line_item_standard() {
        let (model, metric) = parse_line_item("gpt-4o-2024-08-06, Input");
        assert_eq!(model.as_deref(), Some("gpt-4o-2024-08-06"));
        assert_eq!(metric.as_deref(), Some("Input"));
    }

    #[test]
    fn parse_line_item_no_comma_returns_none() {
        let (model, metric) = parse_line_item("some-unknown-item");
        assert!(model.is_none());
        assert!(metric.is_none());
    }

    #[test]
    fn parse_line_item_extra_commas_uses_first_split() {
        let (model, _metric) = parse_line_item("gpt-4o, Input, extra");
        assert_eq!(model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn first_day_of_month_is_start_of_month() {
        let ts = first_day_of_current_month_utc();
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();
        use chrono::Timelike;
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
        assert_eq!(dt.second(), 0);
        use chrono::Datelike;
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn provider_error_display_never_contains_sk_key() {
        let cases = [
            ProviderError::AuthInvalid,
            ProviderError::AuthInsufficientPermission { detail: "needs admin".into() },
            ProviderError::Network("connection refused".into()),
            ProviderError::MalformedResponse("bad json".into()),
        ];
        for err in &cases {
            let s = format!("{err}");
            assert!(!s.contains("sk-"), "error display contains key-shaped string: {s}");
        }
    }

    // ── JSON fixture parsing ─────────────────────────────────────────────────

    const USAGE_FIXTURE: &str = r#"{
        "object": "page",
        "data": [{
            "start_time": 1748736000,
            "end_time": 1748739600,
            "results": [{
                "input_tokens": 1500,
                "output_tokens": 300,
                "num_model_requests": 5,
                "input_cached_tokens": 200,
                "model": "gpt-4o-2024-08-06"
            }]
        }],
        "has_more": false,
        "next_page": null
    }"#;

    const COST_FIXTURE: &str = r#"{
        "object": "page",
        "data": [{
            "start_time": 1748736000,
            "end_time": 1748822400,
            "results": [{
                "amount": { "value": 0.0345, "currency": "usd" },
                "line_item": "gpt-4o-2024-08-06, Input",
                "project_id": null
            }]
        }],
        "has_more": false,
        "next_page": null
    }"#;

    #[test]
    fn parse_usage_page_extracts_token_fields() {
        let records = parse_usage_page_json("completions", USAGE_FIXTURE).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "gpt-4o-2024-08-06");
        assert_eq!(r.input_tokens, Some(1500));
        assert_eq!(r.output_tokens, Some(300));
        assert_eq!(r.cache_read_tokens, Some(200));
        assert_eq!(r.request_count, Some(5));
        assert_eq!(r.bucket_start, 1748736000);
        assert_eq!(r.bucket_end, 1748739600);
        assert_eq!(r.bucket_granularity, BucketGranularity::Hour);
        assert!(r.cost_usd.is_none(), "usage rows have no cost");
    }

    #[test]
    fn parse_cost_page_extracts_cost_and_model() {
        let records = parse_cost_page_json(COST_FIXTURE).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.model, "gpt-4o-2024-08-06");
        assert!((r.cost_usd.unwrap() - 0.0345).abs() < 1e-9);
        assert_eq!(r.bucket_granularity, BucketGranularity::Day);
        assert!(r.input_tokens.is_none(), "cost rows have no token counts");
    }

    #[test]
    fn parse_cost_page_rejects_non_usd() {
        let body = r#"{
            "object":"page","has_more":false,"next_page":null,
            "data":[{"start_time":0,"end_time":1,"results":[
                {"amount":{"value":1.0,"currency":"eur"},"line_item":null,"project_id":null}
            ]}]
        }"#;
        assert!(parse_cost_page_json(body).is_err());
    }

    #[test]
    fn parse_usage_page_empty_data_returns_no_records() {
        let body = r#"{"object":"page","data":[],"has_more":false,"next_page":null}"#;
        let records = parse_usage_page_json("completions", body).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn parse_usage_page_malformed_json_returns_error() {
        assert!(parse_usage_page_json("completions", "{}").is_err());
    }
}
