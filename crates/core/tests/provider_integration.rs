//! Integration tests for OpenAI and Anthropic provider implementations.
//!
//! Each test starts a local wiremock server and points the provider at it via
//! `with_base_url`, so no real network calls are made.

use modelmeter_core::providers::{
    anthropic::AnthropicProvider,
    openai::OpenAiProvider,
    BucketGranularity, Provider, ProviderError, TimeRange,
};
use modelmeter_core::Zeroizing;
use wiremock::{
    matchers::{method, path, path_regex},
    Mock, MockServer, ResponseTemplate,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn time_range() -> TimeRange {
    TimeRange { start: 1_748_736_000, end: 1_748_739_600 }
}

fn dummy_key() -> impl Fn() -> anyhow::Result<Zeroizing<String>> + Send + Sync + 'static {
    || Ok(Zeroizing::new("sk-test-key".to_string()))
}

// Empty success page used for endpoints whose data we don't care about in a
// given test.
fn openai_empty_usage_page() -> serde_json::Value {
    serde_json::json!({"object":"page","data":[],"has_more":false,"next_page":null})
}

fn openai_empty_cost_page() -> serde_json::Value {
    serde_json::json!({"object":"page","data":[],"has_more":false,"next_page":null})
}

fn anthropic_empty_usage_page() -> serde_json::Value {
    serde_json::json!({"data":[],"has_more":false,"next_page":null})
}

fn anthropic_empty_cost_page() -> serde_json::Value {
    serde_json::json!({"data":[],"has_more":false,"next_page":null})
}

// ---------------------------------------------------------------------------
// OpenAI — success path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_fetch_usage_success_path() {
    let server = MockServer::start().await;

    // One completions bucket with real data.
    let usage_body = serde_json::json!({
        "object": "page",
        "data": [{
            "start_time": 1_748_736_000i64,
            "end_time":   1_748_739_600i64,
            "results": [{
                "input_tokens": 1000,
                "output_tokens": 200,
                "num_model_requests": 3,
                "input_cached_tokens": 100,
                "model": "gpt-4o-2024-08-06"
            }]
        }],
        "has_more": false,
        "next_page": null
    });

    // The completions endpoint returns real data; all others return empty.
    Mock::given(method("GET"))
        .and(path("/v1/organization/usage/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage_body))
        .mount(&server)
        .await;

    // All remaining usage endpoints and the costs endpoint return empty pages.
    Mock::given(method("GET"))
        .and(path_regex("/v1/organization/usage/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(openai_empty_usage_page()),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(openai_empty_cost_page()),
        )
        .mount(&server)
        .await;

    let provider = OpenAiProvider::with_base_url(&server.uri(), dummy_key());
    let records = provider.fetch_usage(time_range()).await.unwrap();

    let completions: Vec<_> = records
        .iter()
        .filter(|r| r.model == "gpt-4o-2024-08-06")
        .collect();
    assert_eq!(completions.len(), 1);
    let r = completions[0];
    assert_eq!(r.input_tokens, Some(1000));
    assert_eq!(r.output_tokens, Some(200));
    assert_eq!(r.cache_read_tokens, Some(100));
    assert_eq!(r.request_count, Some(3));
    assert_eq!(r.bucket_granularity, BucketGranularity::Hour);
}

// ---------------------------------------------------------------------------
// OpenAI — auth failure (401)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_fetch_usage_auth_failure() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/v1/organization/usage/"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::with_base_url(&server.uri(), dummy_key());
    let err = provider.fetch_usage(time_range()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::AuthInvalid),
        "expected AuthInvalid, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// OpenAI — rate limit (429)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_fetch_usage_rate_limited() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/v1/organization/usage/"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("retry-after", "60"),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::with_base_url(&server.uri(), dummy_key());
    let err = provider.fetch_usage(time_range()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::RateLimited { .. }),
        "expected RateLimited, got: {err:?}"
    );
    assert!(err.is_transient(), "rate limit should be classified as transient");
}

// ---------------------------------------------------------------------------
// OpenAI — malformed response (200 with empty object)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_fetch_usage_malformed_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/v1/organization/usage/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::with_base_url(&server.uri(), dummy_key());
    let err = provider.fetch_usage(time_range()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::MalformedResponse(_)),
        "expected MalformedResponse, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// OpenAI — cost success path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_fetch_balance_success_path() {
    let server = MockServer::start().await;

    let cost_body = serde_json::json!({
        "object": "page",
        "data": [{
            "start_time": 1_748_736_000i64,
            "end_time":   1_748_822_400i64,
            "results": [
                {
                    "amount": { "value": 1.50, "currency": "usd" },
                    "line_item": "gpt-4o-2024-08-06, Input",
                    "project_id": null
                },
                {
                    "amount": { "value": 0.75, "currency": "usd" },
                    "line_item": "gpt-4o-2024-08-06, Output",
                    "project_id": null
                }
            ]
        }],
        "has_more": false,
        "next_page": null
    });

    Mock::given(method("GET"))
        .and(path("/v1/organization/costs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cost_body))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::with_base_url(&server.uri(), dummy_key());
    let balance = provider.fetch_balance().await.unwrap().unwrap();
    assert!((balance.amount_usd.unwrap() - 2.25).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Anthropic — success path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_fetch_usage_success_path() {
    let server = MockServer::start().await;

    let usage_body = serde_json::json!({
        "data": [{
            "starting_at": "2026-05-31T16:00:00Z",
            "ending_at":   "2026-05-31T17:00:00Z",
            "results": [{
                "uncached_input_tokens": 2000,
                "cache_read_input_tokens": 500,
                "output_tokens": 400,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 100,
                    "ephemeral_1h_input_tokens": 50
                },
                "model": "claude-sonnet-4-6"
            }]
        }],
        "has_more": false,
        "next_page": null
    });

    // messages endpoint
    Mock::given(method("GET"))
        .and(path_regex("/v1/organizations/usage_report/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage_body))
        .mount(&server)
        .await;

    // costs endpoint
    Mock::given(method("GET"))
        .and(path("/v1/organizations/cost_report"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(anthropic_empty_cost_page()),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::with_base_url(&server.uri(), dummy_key());
    let records = provider.fetch_usage(time_range()).await.unwrap();

    // messages endpoint returns one record.
    assert_eq!(records.len(), 1);
    for r in &records {
        assert_eq!(r.model, "claude-sonnet-4-6");
        assert_eq!(r.input_tokens, Some(2000));
        assert_eq!(r.output_tokens, Some(400));
        assert_eq!(r.cache_read_tokens, Some(500));
        assert_eq!(r.cache_creation_tokens, Some(150)); // 100 + 50
        assert_eq!(r.bucket_granularity, BucketGranularity::Hour);
        assert!(r.cost_usd.is_none());
    }
}

// ---------------------------------------------------------------------------
// Anthropic — auth failure (401)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_fetch_usage_auth_failure() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/v1/organizations/"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::with_base_url(&server.uri(), dummy_key());
    let err = provider.fetch_usage(time_range()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::AuthInvalid),
        "expected AuthInvalid, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Anthropic — rate limit (429)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_fetch_usage_rate_limited() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/v1/organizations/"))
        .respond_with(
            ResponseTemplate::new(429).insert_header("retry-after", "30"),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::with_base_url(&server.uri(), dummy_key());
    let err = provider.fetch_usage(time_range()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::RateLimited { .. }),
        "expected RateLimited, got: {err:?}"
    );
    assert!(err.is_transient());
}

// ---------------------------------------------------------------------------
// Anthropic — malformed response (200 with empty object)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_fetch_usage_malformed_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex("/v1/organizations/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::with_base_url(&server.uri(), dummy_key());
    let err = provider.fetch_usage(time_range()).await.unwrap_err();
    assert!(
        matches!(err, ProviderError::MalformedResponse(_)),
        "expected MalformedResponse, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Anthropic — cost success path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_fetch_balance_success_path() {
    let server = MockServer::start().await;

    // usage endpoints return empty
    Mock::given(method("GET"))
        .and(path_regex("/v1/organizations/usage_report/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(anthropic_empty_usage_page()),
        )
        .mount(&server)
        .await;

    let cost_body = serde_json::json!({
        "data": [{
            "starting_at": "2026-05-01T00:00:00Z",
            "ending_at":   "2026-05-02T00:00:00Z",
            "results": [
                {
                    "amount": "12.34",
                    "currency": "USD",
                    "description": "claude-sonnet-4-6 input",
                    "model": "claude-sonnet-4-6",
                    "inference_geo": null,
                    "workspace_id": null
                }
            ]
        }],
        "has_more": false,
        "next_page": null
    });

    Mock::given(method("GET"))
        .and(path("/v1/organizations/cost_report"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cost_body))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::with_base_url(&server.uri(), dummy_key());
    let balance = provider.fetch_balance().await.unwrap().unwrap();
    assert!((balance.amount_usd.unwrap() - 12.34).abs() < 1e-9);
}
