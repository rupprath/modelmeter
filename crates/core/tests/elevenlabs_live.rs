//! Live end-to-end smoke test for the ElevenLabs provider.
//!
//! Skipped unless `ELEVENLABS_API_KEY` is set, so CI never runs it. Hits the
//! real ElevenLabs API. Exercises every path the dashboard card depends on:
//! credential validation, subscription snapshot, and the character-stats
//! daily-usage endpoint.

use modelmeter_core::providers::{
    elevenlabs::ElevenLabsProvider, KeyValidation, Provider, TimeRange,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[tokio::test]
async fn live_validate_subscription_and_usage() {
    let Ok(key) = std::env::var("ELEVENLABS_API_KEY") else {
        eprintln!("ELEVENLABS_API_KEY not set — skipping live test");
        return;
    };
    let key = Arc::new(Zeroizing::new(key));

    // ── 1. validate_credential ───────────────────────────────────────────
    let key_for_validate = key.clone();
    let provider = ElevenLabsProvider::new(move || Ok((*key_for_validate).clone()));
    let validation = provider
        .validate_credential()
        .await
        .expect("validate_credential should not error");
    println!("validate: {}", validation_summary(&validation));
    assert!(
        matches!(validation, KeyValidation::Valid),
        "expected Valid; got {validation:?}"
    );

    // ── 2. fetch_subscription_state — what the dashboard card reads ──────
    let key_for_sub = key.clone();
    let provider = ElevenLabsProvider::new(move || Ok((*key_for_sub).clone()));
    let state = provider
        .fetch_subscription_state()
        .await
        .expect("fetch_subscription_state should not error");
    println!(
        "subscription: tier={} status={} credits={}/{} overage=${:.2} reset_in={}d",
        state.tier,
        state.status,
        state.character_count,
        state.character_limit,
        state.current_overage_usd,
        (state.next_reset_unix - now_secs()).max(0) / 86_400,
    );
    assert!(!state.tier.is_empty(), "tier should not be empty");
    assert!(!state.status.is_empty(), "status should not be empty");
    assert!(state.character_limit > 0, "character_limit should be > 0");
    assert!(
        state.character_count <= state.character_limit
            || state.current_overage_usd > 0.0,
        "if character_count > limit then overage must be > 0; got count={} limit={} overage={}",
        state.character_count,
        state.character_limit,
        state.current_overage_usd,
    );

    // ── 3. fetch_usage — daily credit history for the last 30 days ───────
    let now = now_secs();
    let thirty_days_ago = now - 30 * 86_400;
    let key_for_usage = key.clone();
    let provider = ElevenLabsProvider::new(move || Ok((*key_for_usage).clone()));
    let records = provider
        .fetch_usage(TimeRange { start: thirty_days_ago, end: now })
        .await
        .expect("fetch_usage should not error");
    let total_credits: i64 = records
        .iter()
        .filter_map(|r| {
            r.provider_metadata
                .as_ref()
                .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
                .and_then(|v| v.get("credits").and_then(|c| c.as_i64()))
        })
        .sum();
    println!(
        "usage: {} non-zero day records, total {} credits in 30d",
        records.len(),
        total_credits,
    );

    // The character-stats daily total must match the subscription
    // character_count exactly — they're two paths to the same number. If they
    // diverge, something in the parsing has drifted.
    assert_eq!(
        total_credits, state.character_count,
        "sum of daily credits ({}) must match subscription character_count ({})",
        total_credits, state.character_count,
    );

    // Every record must declare day granularity and have credits in metadata.
    for r in &records {
        assert_eq!(
            r.bucket_granularity,
            modelmeter_core::providers::BucketGranularity::Day,
            "elevenlabs records must be daily-bucketed"
        );
        assert!(
            r.cost_usd.is_none(),
            "cost_usd must be None — we never fabricate dollars for elevenlabs"
        );
        assert!(
            r.provider_metadata.is_some(),
            "every record must carry credits in provider_metadata"
        );
    }
}

fn validation_summary(v: &KeyValidation) -> String {
    match v {
        KeyValidation::Valid => "Valid".into(),
        KeyValidation::Invalid { reason } => format!("Invalid({reason})"),
        KeyValidation::InsufficientPermission { hint } => format!("InsufficientPermission({hint})"),
    }
}
