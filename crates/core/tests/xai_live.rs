//! Live end-to-end smoke test for the x.ai provider.
//!
//! Skipped unless the `XAI_MGMT_KEY` env var is set, so CI won't try to run it.
//! Hits the real x.ai management API.

use modelmeter_core::providers::{xai::XaiProvider, KeyValidation, Provider};
use std::sync::Arc;
use zeroize::Zeroizing;

#[tokio::test]
async fn live_validate_and_fetch_balance() {
    let Ok(key) = std::env::var("XAI_MGMT_KEY") else {
        eprintln!("XAI_MGMT_KEY not set — skipping live test");
        return;
    };
    let key = Arc::new(Zeroizing::new(key));
    let key_for_validate = key.clone();
    let provider = XaiProvider::new(move || Ok((*key_for_validate).clone()));

    let validation = provider
        .validate_credential()
        .await
        .expect("validate should not error");
    println!("validate result: {:?}", validation_summary(&validation));
    assert!(matches!(validation, KeyValidation::Valid), "validate should be Valid");

    let balance = provider
        .fetch_balance()
        .await
        .expect("fetch_balance should not error")
        .expect("fetch_balance should return Some");
    println!(
        "balance: ${:.2} (shape={:?}, note={:?})",
        balance.amount_usd.unwrap_or(0.0),
        balance.shape,
        balance.note,
    );
    assert!(balance.amount_usd.is_some());

    // Also exercise the new monthly-history path.
    let key_for_history = key.clone();
    let history_provider = XaiProvider::new(move || Ok((*key_for_history).clone()));
    let history = history_provider
        .fetch_monthly_history(6)
        .await
        .expect("fetch_monthly_history should not error");
    println!("monthly history ({} months):", history.len());
    for m in &history {
        println!("  {}-{:02}  ${:.2}", m.year, m.month, m.amount_usd);
    }
    assert!(!history.is_empty(), "expected at least one invoice");
}

fn validation_summary(v: &KeyValidation) -> &'static str {
    match v {
        KeyValidation::Valid => "Valid",
        KeyValidation::Invalid { .. } => "Invalid",
        KeyValidation::InsufficientPermission { .. } => "InsufficientPermission",
    }
}
