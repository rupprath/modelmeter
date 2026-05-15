#![forbid(unsafe_code)]

//! x.ai (Grok) provider implementation.
//!
//! Auth:       Management key (Bearer token, `xai-token-…` prefix). The plain
//!             `xai-…` inference key is rejected — billing reads require a
//!             dedicated management key created at console.x.ai → Settings →
//!             Management Keys. We strongly recommend creating it as
//!             read-only.
//! Token data: None — `fetch_usage` returns an empty Vec.
//! Balance:    `GET https://management-api.x.ai/v1/billing/teams/{team_id}/prepaid/balance`.
//!             Response shape (empirical, not formally documented):
//!               { "changes": [...], "total": { "val": "-946" } }
//!             `total.val` is signed net cents: negative = unspent credit
//!             remaining. Displayed balance = -total.val / 100 in USD.
//!
//! ## team_id discovery — DEV LIMITATION
//!
//! Management keys cannot self-discover their team_id: `api.x.ai/v1/api-key`
//! rejects them with 400, and `management-api.x.ai` exposes no introspection
//! endpoint (every `/v1/me`, `/v1/teams`, etc. returns 404). For now the
//! team_id is hardcoded below for the developer's account. Before shipping,
//! either (a) add a "Team ID" input alongside the key, or (b) find a working
//! discovery endpoint x.ai exposes that we haven't tried yet.

use anyhow::Result;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{
    check_status, map_reqwest_err, resolve_creds, unix_now, Balance, BalanceShape, BoxFuture,
    CredsAccessor, InvalidReason, KeyValidation, Provider, ProviderDescriptor, ProviderError,
    TimeRange, UsageRecord,
};

const BASE_URL: &str = "https://management-api.x.ai";
const VALIDATE_TIMEOUT_SECS: u64 = 10;

/// Required prefix for x.ai management keys (vs the `xai-…` inference key).
const MANAGEMENT_KEY_PREFIX: &str = "xai-token-";

/// TODO(team_id): hardcoded for the developer's account during initial
/// integration. Replace with a discovery mechanism or a second user input
/// before the v1 release. See module docstring.
const DEV_TEAM_ID: &str = "ccd583cc-8608-4f5b-8e0b-540038e6755d";

const WRONG_KEY_TYPE_HINT: &str =
    "This looks like an inference API key (used to call Grok models), not a \
     management key. ModelMeter needs a management key with billing scope. \
     Open console.x.ai → Settings → Management Keys → Create New, generate a \
     read-only key, and paste that here. Read-only is strongly recommended — \
     ModelMeter only ever reads.";

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

pub struct XaiProvider {
    creds: Box<dyn Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static>,
    client: reqwest::Client,
    validate_client: reqwest::Client,
    base_url: String,
    team_id: String,
}

impl XaiProvider {
    pub fn new(creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static) -> Self {
        Self::with_base_url(BASE_URL, DEV_TEAM_ID, creds)
    }

    pub fn with_base_url(
        base_url: &str,
        team_id: &str,
        creds: impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            creds: Box::new(creds),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build x.ai HTTP client"),
            validate_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(VALIDATE_TIMEOUT_SECS))
                .build()
                .expect("failed to build x.ai validation HTTP client"),
            base_url: base_url.to_string(),
            team_id: team_id.to_string(),
        }
    }

    fn get_key(&self) -> Result<Zeroizing<String>, ProviderError> {
        resolve_creds(&*self.creds)
    }

    /// Calls the prepaid-balance endpoint and returns the parsed body.
    async fn fetch_balance_inner(
        &self,
        client: &reqwest::Client,
        key: &str,
    ) -> Result<BalanceResponse, ProviderError> {
        let url = format!(
            "{}/v1/billing/teams/{}/prepaid/balance",
            self.base_url, self.team_id
        );
        let resp = client
            .get(&url)
            .bearer_auth(key)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<BalanceResponse>().await.map_err(|e| {
            ProviderError::MalformedResponse(format!("xai/prepaid/balance: {e}"))
        })
    }

    /// Fetches the invoice list and returns one `MonthlySpend` per invoice,
    /// computed by summing the line amounts. x.ai bills monthly, so each
    /// invoice covers one calendar month — the granularity of this data.
    ///
    /// Newest-first, capped at `limit` entries.
    pub async fn fetch_monthly_history(
        &self,
        limit: usize,
    ) -> Result<Vec<MonthlySpend>, ProviderError> {
        let key = self.get_key()?;
        let url = format!(
            "{}/v1/billing/teams/{}/invoices",
            self.base_url, self.team_id
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(key.as_str())
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        let body: InvoicesResponse = resp.json().await.map_err(|e| {
            ProviderError::MalformedResponse(format!("xai/invoices: {e}"))
        })?;

        let mut entries: Vec<MonthlySpend> = body
            .invoices
            .into_iter()
            .filter_map(|inv| inv.to_monthly_spend())
            .collect();

        // Newest first by (year, month).
        entries.sort_by(|a, b| (b.year, b.month).cmp(&(a.year, a.month)));
        entries.truncate(limit);
        Ok(entries)
    }
}

/// One month's total spend, computed from an invoice's line items.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MonthlySpend {
    pub year: i32,
    pub month: u32,
    pub amount_usd: f64,
}

// ---------------------------------------------------------------------------
// ProviderDescriptor
// ---------------------------------------------------------------------------

fn build(creds: CredsAccessor) -> Box<dyn Provider> {
    Box::new(XaiProvider::new(creds))
}

fn build_with_key(key: Zeroizing<String>) -> Box<dyn Provider> {
    Box::new(XaiProvider::new(move || Ok(key.clone())))
}

pub const DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    slug: "xai",
    display_name: "x.ai (Grok)",
    short: "X",
    color: "#52525b",
    key_docs_url: Some("https://console.x.ai"),
    key_label: "Management Key",
    key_is_secret: true,
    key_required: true,
    build,
    build_with_key,
};

// ---------------------------------------------------------------------------
// Raw JSON types (private)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BalanceResponse {
    /// Net change across all transactions. `val` is a signed string of cents.
    /// Negative means the user still has unspent credit; we negate to display.
    total: Amount,
}

#[derive(Deserialize)]
struct Amount {
    val: String,
}

#[derive(Deserialize)]
struct InvoicesResponse {
    #[serde(default)]
    invoices: Vec<Invoice>,
}

#[derive(Deserialize)]
struct Invoice {
    #[serde(default)]
    lines: Vec<InvoiceLine>,
    monthly: Option<MonthlySection>,
}

#[derive(Deserialize)]
struct InvoiceLine {
    /// String-encoded integer USD cents (e.g. "51" = $0.51). The empirical
    /// shape matches what we see in the live invoice; we tolerate decimals
    /// just in case x.ai changes the encoding without warning.
    amount: String,
}

#[derive(Deserialize)]
struct MonthlySection {
    #[serde(rename = "billingCycle")]
    billing_cycle: Option<BillingCycle>,
}

#[derive(Deserialize)]
struct BillingCycle {
    year: i32,
    month: u32,
}

impl Invoice {
    /// Returns one MonthlySpend for this invoice, or `None` if the invoice
    /// has no `monthly.billingCycle` (e.g. one-off PURCHASE invoices that
    /// aren't tied to a calendar month).
    fn to_monthly_spend(self) -> Option<MonthlySpend> {
        let cycle = self.monthly?.billing_cycle?;
        let total_cents: f64 = self
            .lines
            .iter()
            .map(|l| l.amount.trim().parse::<f64>().unwrap_or(0.0))
            .sum();
        Some(MonthlySpend {
            year: cycle.year,
            month: cycle.month,
            amount_usd: total_cents / 100.0,
        })
    }
}

impl BalanceResponse {
    /// Converts the response into a USD balance to display. Negative `total.val`
    /// (unspent credit) becomes a positive balance; zero or positive `total.val`
    /// (no credit / overspent) maps to 0.0 since ModelMeter's balance card is
    /// always a non-negative remaining-credit figure.
    fn remaining_usd(&self) -> Result<f64, ProviderError> {
        let cents: i64 = self.total.val.trim().parse().map_err(|e| {
            ProviderError::MalformedResponse(format!(
                "xai/prepaid/balance: total.val not an integer: '{}': {}",
                self.total.val, e
            ))
        })?;
        let remaining_cents = -cents;
        Ok((remaining_cents.max(0)) as f64 / 100.0)
    }
}

// ---------------------------------------------------------------------------
// Provider impl
// ---------------------------------------------------------------------------

impl Provider for XaiProvider {
    /// Sanity-checks the key prefix, then calls the balance endpoint. A 200
    /// proves both authentication and billing scope. 401/403 → wrong-key-type
    /// hint pointing the user at the Management Keys page.
    fn validate_credential(&self) -> BoxFuture<'_, Result<KeyValidation, ProviderError>> {
        Box::pin(async move {
            let key = self.get_key()?;
            if !key.starts_with(MANAGEMENT_KEY_PREFIX) {
                return Ok(KeyValidation::Invalid {
                    reason: InvalidReason::Other(WRONG_KEY_TYPE_HINT.into()),
                });
            }

            match self.fetch_balance_inner(&self.validate_client, key.as_str()).await {
                Ok(_) => Ok(KeyValidation::Valid),
                Err(ProviderError::AuthInvalid) | Err(ProviderError::Forbidden { .. }) => {
                    Ok(KeyValidation::InsufficientPermission {
                        hint: WRONG_KEY_TYPE_HINT.into(),
                    })
                }
                Err(e) => Err(e),
            }
        })
    }

    /// No-op: x.ai historical usage is out of scope for this provider.
    fn fetch_usage(
        &self,
        _range: TimeRange,
    ) -> BoxFuture<'_, Result<Vec<UsageRecord>, ProviderError>> {
        Box::pin(async move { Ok(vec![]) })
    }

    /// Reads the prepaid balance via the management API.
    fn fetch_balance(&self) -> BoxFuture<'_, Result<Option<Balance>, ProviderError>> {
        Box::pin(async move {
            let key = self.get_key()?;
            let body = self.fetch_balance_inner(&self.client, key.as_str()).await?;
            let amount_usd = body.remaining_usd()?;
            Ok(Some(Balance {
                amount_usd: Some(amount_usd),
                as_of: unix_now(),
                shape: BalanceShape::RemainingCredit,
                note: Some("x.ai prepaid balance".into()),
            }))
        })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_total_becomes_positive_balance() {
        let body: BalanceResponse =
            serde_json::from_str(r#"{"changes":[],"total":{"val":"-946"}}"#).unwrap();
        let usd = body.remaining_usd().unwrap();
        assert!((usd - 9.46).abs() < 1e-9);
    }

    #[test]
    fn zero_total_is_zero_balance() {
        let body: BalanceResponse =
            serde_json::from_str(r#"{"changes":[],"total":{"val":"0"}}"#).unwrap();
        assert_eq!(body.remaining_usd().unwrap(), 0.0);
    }

    #[test]
    fn positive_total_is_clamped_to_zero() {
        // total > 0 would mean overspent (debt) — display 0 not a negative.
        let body: BalanceResponse =
            serde_json::from_str(r#"{"changes":[],"total":{"val":"500"}}"#).unwrap();
        assert_eq!(body.remaining_usd().unwrap(), 0.0);
    }

    #[test]
    fn non_integer_total_is_malformed() {
        let body: BalanceResponse =
            serde_json::from_str(r#"{"changes":[],"total":{"val":"not-a-number"}}"#).unwrap();
        assert!(body.remaining_usd().is_err());
    }

    #[test]
    fn invoice_sums_lines_correctly() {
        // June 2025 invoice from the live API — 7 lines totalling 127 cents = $1.27.
        let body: InvoicesResponse = serde_json::from_str(
            r#"{"invoices":[{
                "lines":[
                    {"amount":"0"},{"amount":"0"},{"amount":"0"},
                    {"amount":"1"},{"amount":"1"},{"amount":"51"},{"amount":"74"}
                ],
                "monthly":{"billingCycle":{"year":2025,"month":5}}
            }]}"#,
        )
        .unwrap();
        let inv = body.invoices.into_iter().next().unwrap();
        let m = inv.to_monthly_spend().unwrap();
        assert_eq!(m.year, 2025);
        assert_eq!(m.month, 5);
        assert!((m.amount_usd - 1.27).abs() < 1e-9);
    }

    #[test]
    fn invoice_without_billing_cycle_is_skipped() {
        // Initial PURCHASE invoice has no billingCycle — should return None.
        let body: InvoicesResponse = serde_json::from_str(
            r#"{"invoices":[{
                "lines":[{"amount":"2500"}],
                "monthly":null
            }]}"#,
        )
        .unwrap();
        let inv = body.invoices.into_iter().next().unwrap();
        assert!(inv.to_monthly_spend().is_none());
    }

    #[test]
    fn balance_response_with_full_fixture_parses() {
        // Trimmed copy of the actual response observed against the dev account.
        let body: BalanceResponse = serde_json::from_str(
            r#"{
                "changes":[
                    {"teamId":"ccd583cc","changeOrigin":"PURCHASE","amount":{"val":"-2500"}},
                    {"teamId":"ccd583cc","changeOrigin":"SPEND","amount":{"val":"1554"}}
                ],
                "total":{"val":"-946"}
            }"#,
        )
        .unwrap();
        assert!((body.remaining_usd().unwrap() - 9.46).abs() < 1e-9);
    }
}
