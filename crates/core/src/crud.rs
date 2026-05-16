#![forbid(unsafe_code)]

//! CRUD operations and widget query primitives.
//!
//! Every function takes a `&Connection` (or any type that `Deref`s to it,
//! such as `Transaction<'_>`). Callers use `Database::with_conn` for simple
//! reads and writes, and `Database::with_transaction` when atomicity is needed
//! (e.g. the sync engine's per-provider write batch).

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::providers::{Balance, BalanceShape, ProviderRow, UsageRecord};

// ---------------------------------------------------------------------------
// Supplementary types
// ---------------------------------------------------------------------------

/// A row from the `balances` table.
#[derive(Debug, Clone)]
pub struct BalanceRow {
    pub id: i64,
    pub provider_id: i64,
    pub amount_usd: Option<f64>,
    pub shape: BalanceShape,
    pub note: Option<String>,
    pub as_of: i64,
    pub fetched_at: i64,
}

/// Aggregated usage figures across all models for a provider and time window.
#[derive(Debug, Clone)]
pub struct UsageSummary {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_creation_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cost_usd: f64,
    pub total_request_count: i64,
}

/// One day's credit total for a credit-denominated provider (e.g. ElevenLabs).
/// `bucket_start` is unix UTC seconds at 00:00 of the day. `credits` is the
/// sum extracted from `usage_records.provider_metadata` for that day.
#[derive(Debug, Clone)]
pub struct DayCredits {
    pub bucket_start: i64,
    pub credits: i64,
}

// ---------------------------------------------------------------------------
// Provider CRUD
// ---------------------------------------------------------------------------

/// Inserts a new provider row and returns the new `id`.
/// Fails if a provider of the same type already exists (UNIQUE constraint).
pub fn create_provider(
    conn: &Connection,
    provider_type: &str,
    display_name: &str,
    created_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO providers (provider_type, display_name, created_at) VALUES (?1, ?2, ?3)",
        params![provider_type, display_name, created_at],
    )
    .context("insert provider")?;
    Ok(conn.last_insert_rowid())
}

/// Returns all providers ordered by creation time.
pub fn list_providers(conn: &Connection) -> Result<Vec<ProviderRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_type,
                    display_name,
                    last_sync_attempted_at, last_sync_succeeded_at,
                    last_sync_status, created_at
             FROM providers
             ORDER BY created_at ASC",
        )
        .context("prepare list_providers")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProviderRow {
                id: row.get(0)?,
                provider_type: row.get(1)?,
                display_name: row.get(2)?,
                last_sync_attempted_at: row.get(3)?,
                last_sync_succeeded_at: row.get(4)?,
                last_sync_status: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .context("query list_providers")?;
    rows.map(|r| r.context("read provider row")).collect()
}

/// Returns a single provider by id, or `None` if not found.
pub fn get_provider(conn: &Connection, id: i64) -> Result<Option<ProviderRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_type,
                    display_name,
                    last_sync_attempted_at, last_sync_succeeded_at,
                    last_sync_status, created_at
             FROM providers WHERE id = ?1",
        )
        .context("prepare get_provider")?;
    let mut rows = stmt
        .query_map([id], |row| {
            Ok(ProviderRow {
                id: row.get(0)?,
                provider_type: row.get(1)?,
                display_name: row.get(2)?,
                last_sync_attempted_at: row.get(3)?,
                last_sync_succeeded_at: row.get(4)?,
                last_sync_status: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .context("query get_provider")?;
    rows.next().transpose().context("read provider row")
}

/// Deletes a provider by id (cascade-deletes its usage records and balances).
/// Returns `true` if a row was deleted.
pub fn delete_provider(conn: &Connection, id: i64) -> Result<bool> {
    let affected = conn
        .execute("DELETE FROM providers WHERE id = ?1", [id])
        .context("delete provider")?;
    Ok(affected > 0)
}

// ---------------------------------------------------------------------------
// Sync write operations (used by the sync engine, not exposed as commands)
// ---------------------------------------------------------------------------

/// Upserts a batch of usage records. Uses `INSERT OR REPLACE` against the
/// unique index `(provider_id, model, bucket_start, bucket_granularity,
/// provider)` to overwrite partial-current-hour entries on re-sync.
///
/// Returns the number of records processed.
pub fn upsert_usage_records(conn: &Connection, records: &[UsageRecord]) -> Result<usize> {
    let mut stmt = conn
        .prepare(
            "INSERT OR REPLACE INTO usage_records
             (provider_id, provider, model, bucket_start, bucket_end,
              bucket_granularity, input_tokens, output_tokens,
              cache_creation_tokens, cache_read_tokens, request_count,
              cost_usd, cost_source, provider_metadata, fetched_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        )
        .context("prepare upsert_usage_records")?;

    for r in records {
        stmt.execute(params![
            r.provider_id,
            r.provider.as_str(),
            r.model,
            r.bucket_start,
            r.bucket_end,
            r.bucket_granularity.as_str(),
            r.input_tokens,
            r.output_tokens,
            r.cache_creation_tokens,
            r.cache_read_tokens,
            r.request_count,
            r.cost_usd,
            r.cost_source.as_str(),
            r.provider_metadata,
            r.fetched_at,
        ])
        .context("upsert usage record")?;
    }
    Ok(records.len())
}

/// Appends a balance snapshot for the given provider.
pub fn insert_balance(
    conn: &Connection,
    provider_id: i64,
    balance: &Balance,
    fetched_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO balances (provider_id, amount_usd, shape, note, as_of, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            provider_id,
            balance.amount_usd,
            balance.shape.as_str(),
            balance.note,
            balance.as_of,
            fetched_at,
        ],
    )
    .context("insert balance")?;
    Ok(())
}

/// Updates `last_sync_attempted_at`, `last_sync_succeeded_at`, and
/// `last_sync_status` on a provider row. Pass `succeeded_at = None` and
/// `status = "failed"` on failure; pass the success timestamp and
/// `status = "ok"` on success.
pub fn update_provider_sync_status(
    conn: &Connection,
    provider_id: i64,
    attempted_at: i64,
    succeeded_at: Option<i64>,
    status: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE providers
         SET last_sync_attempted_at = ?1,
             last_sync_succeeded_at = ?2,
             last_sync_status = ?3
         WHERE id = ?4",
        params![attempted_at, succeeded_at, status, provider_id],
    )
    .context("update provider sync status")?;
    Ok(())
}

/// Deletes records older than `max_days`, then trims to `max_size_mb` by
/// removing the oldest records in batches of 1 000.
///
/// Returns the total number of rows deleted.
pub fn prune_old_records(conn: &Connection, max_days: u32, max_size_mb: u64) -> Result<usize> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs() as i64;
    let cutoff = now - (max_days as i64) * 86_400;

    let time_deleted = conn
        .execute(
            "DELETE FROM usage_records WHERE bucket_start < ?1",
            [cutoff],
        )
        .context("prune by time")?;

    let max_bytes = (max_size_mb as i64) * 1_024 * 1_024;
    let mut size_deleted: usize = 0;
    loop {
        // page_size can return no rows under SQLCipher in WAL mode; treat any
        // PRAGMA failure as "size unknown → skip size-based pruning".
        let page_count: i64 = match conn.query_row("PRAGMA page_count", [], |r| r.get(0)) {
            Ok(v) => v,
            Err(_) => break,
        };
        let page_size: i64 = match conn.query_row("PRAGMA page_size", [], |r| r.get(0)) {
            Ok(v) => v,
            Err(_) => break,
        };
        if page_count * page_size <= max_bytes {
            break;
        }
        let n = conn
            .execute(
                "DELETE FROM usage_records WHERE id IN (
                    SELECT id FROM usage_records ORDER BY bucket_start ASC LIMIT 1000
                 )",
                [],
            )
            .context("prune by size")?;
        size_deleted += n;
        if n == 0 {
            break;
        }
    }

    Ok(time_deleted + size_deleted)
}

// ---------------------------------------------------------------------------
// Widget query primitives
// ---------------------------------------------------------------------------

/// Returns the most-recent balance snapshot for a provider, or `None` if none
/// has been recorded yet.
pub fn get_latest_balance(conn: &Connection, provider_id: i64) -> Result<Option<BalanceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, provider_id, amount_usd, shape, note, as_of, fetched_at
             FROM balances
             WHERE provider_id = ?1
             ORDER BY fetched_at DESC
             LIMIT 1",
        )
        .context("prepare get_latest_balance")?;
    let mut rows = stmt
        .query_map([provider_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .context("query get_latest_balance")?;
    if let Some(res) = rows.next() {
        let (id, pid, amount_usd, shape_str, note, as_of, fetched_at) = res?;
        let shape = shape_str
            .parse::<BalanceShape>()
            .map_err(|e| anyhow::anyhow!("invalid balance shape in db: {e}"))?;
        Ok(Some(BalanceRow { id, provider_id: pid, amount_usd, shape, note, as_of, fetched_at }))
    } else {
        Ok(None)
    }
}

/// Returns daily credit totals for a credit-denominated provider, oldest-first.
///
/// Reads `provider_metadata` JSON (expecting `{"credits": N}`) for every record
/// in `[since, until)` and sums by `bucket_start`. Rows missing or malformed
/// metadata are skipped. Days with zero credits are not returned (callers that
/// need a zero-filled time-series should backfill at the boundary).
pub fn get_daily_credits(
    conn: &Connection,
    provider_id: i64,
    since: i64,
    until: i64,
) -> Result<Vec<DayCredits>> {
    let mut stmt = conn
        .prepare(
            "SELECT bucket_start, provider_metadata
             FROM usage_records
             WHERE provider_id = ?1 AND bucket_start >= ?2 AND bucket_start < ?3
               AND provider_metadata IS NOT NULL
             ORDER BY bucket_start ASC",
        )
        .context("prepare get_daily_credits")?;

    let rows = stmt
        .query_map(params![provider_id, since, until], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .context("query get_daily_credits")?;

    let mut by_day: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    for r in rows {
        let (bucket_start, meta) = r.context("read daily credits row")?;
        let credits = serde_json::from_str::<serde_json::Value>(&meta)
            .ok()
            .and_then(|v| v.get("credits").and_then(|c| c.as_i64()));
        if let Some(c) = credits {
            *by_day.entry(bucket_start).or_insert(0) += c;
        }
    }
    Ok(by_day
        .into_iter()
        .map(|(bucket_start, credits)| DayCredits { bucket_start, credits })
        .collect())
}

/// Returns aggregated token and cost totals for all usage records whose
/// `bucket_start` falls in `[since, until)`.
pub fn get_usage_summary(
    conn: &Connection,
    provider_id: i64,
    since: i64,
    until: i64,
) -> Result<UsageSummary> {
    conn.query_row(
        "SELECT
            COALESCE(SUM(input_tokens), 0),
            COALESCE(SUM(output_tokens), 0),
            COALESCE(SUM(cache_creation_tokens), 0),
            COALESCE(SUM(cache_read_tokens), 0),
            COALESCE(SUM(cost_usd), 0.0),
            COALESCE(SUM(request_count), 0)
         FROM usage_records
         WHERE provider_id = ?1 AND bucket_start >= ?2 AND bucket_start < ?3",
        params![provider_id, since, until],
        |row| {
            Ok(UsageSummary {
                total_input_tokens: row.get(0)?,
                total_output_tokens: row.get(1)?,
                total_cache_creation_tokens: row.get(2)?,
                total_cache_read_tokens: row.get(3)?,
                total_cost_usd: row.get(4)?,
                total_request_count: row.get(5)?,
            })
        },
    )
    .context("get_usage_summary")
}

// ---------------------------------------------------------------------------
// Cached Claude Code result
// ---------------------------------------------------------------------------

pub fn get_cached_claude_code_result(conn: &Connection) -> Result<Option<(String, i64)>> {
    let mut stmt = conn
        .prepare("SELECT blob, fetched_at FROM cached_claude_code_result WHERE id = 1")
        .context("prepare get_cached_claude_code_result")?;
    let row = stmt
        .query_row([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .optional()
        .context("query get_cached_claude_code_result")?;
    Ok(row)
}

pub fn set_cached_claude_code_result(
    conn: &Connection,
    blob: &str,
    fetched_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO cached_claude_code_result (id, blob, fetched_at)
         VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET blob = excluded.blob,
                                       fetched_at = excluded.fetched_at",
        params![blob, fetched_at],
    )
    .context("set_cached_claude_code_result")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::providers::{Balance, BalanceShape, BucketGranularity, CostSource};

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn make_record(provider_id: i64) -> UsageRecord {
        UsageRecord {
            id: 0,
            provider_id,
            provider: "openai".to_string(),
            model: String::new(),
            bucket_start: 1_000,
            bucket_end: 4_600,
            bucket_granularity: BucketGranularity::Hour,
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_creation_tokens: None,
            cache_read_tokens: None,
            request_count: Some(1),
            cost_usd: Some(0.01),
            cost_source: CostSource::Reported,
            provider_metadata: None,
            fetched_at: 0,
        }
    }

    // -- Provider CRUD --

    #[test]
    fn create_and_list_providers() {
        let db = setup();
        let id = db
            .with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000_000))
            .unwrap();
        assert!(id > 0);
        let providers = db.with_conn(list_providers).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].display_name, "OpenAI");
        assert_eq!(providers[0].provider_type, "openai");
    }

    #[test]
    fn get_provider_returns_none_for_missing() {
        let db = setup();
        let row = db.with_conn(|c| get_provider(c, 999)).unwrap();
        assert!(row.is_none());
    }

    #[test]
    fn delete_provider_returns_true_on_success() {
        let db = setup();
        let id = db
            .with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000))
            .unwrap();
        assert!(db.with_conn(|c| delete_provider(c, id)).unwrap());
        assert!(!db.with_conn(|c| delete_provider(c, id)).unwrap());
    }

    #[test]
    fn provider_type_must_be_unique() {
        let db = setup();
        db.with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000)).unwrap();
        let result = db.with_conn(|c| create_provider(c, "openai", "OpenAI 2", 2_000));
        assert!(result.is_err(), "duplicate provider_type should fail");
    }

    // -- Sync write ops --

    #[test]
    fn upsert_replaces_on_conflict() {
        let db = setup();
        let pid = db
            .with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000))
            .unwrap();

        let mut r = make_record(pid);
        r.input_tokens = Some(100);
        db.with_conn(|c| upsert_usage_records(c, &[r.clone()])).unwrap();

        r.input_tokens = Some(200);
        db.with_conn(|c| upsert_usage_records(c, &[r])).unwrap();

        let summary = db.with_conn(|c| get_usage_summary(c, pid, 0, i64::MAX)).unwrap();
        assert_eq!(summary.total_input_tokens, 200);
    }

    #[test]
    fn insert_balance_and_get_latest() {
        let db = setup();
        let pid = db
            .with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000))
            .unwrap();

        let bal = Balance {
            amount_usd: Some(12.34),
            as_of: 5_000,
            shape: BalanceShape::SpendThisPeriod,
            note: None,
        };
        db.with_conn(|c| insert_balance(c, pid, &bal, 6_000)).unwrap();

        let row = db.with_conn(|c| get_latest_balance(c, pid)).unwrap().unwrap();
        assert_eq!(row.amount_usd, Some(12.34));
        assert_eq!(row.shape, BalanceShape::SpendThisPeriod);
    }

    #[test]
    fn update_provider_sync_status_sets_fields() {
        let db = setup();
        let pid = db
            .with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000))
            .unwrap();

        db.with_conn(|c| update_provider_sync_status(c, pid, 9_000, Some(9_001), "ok"))
            .unwrap();

        let provider = db.with_conn(|c| get_provider(c, pid)).unwrap().unwrap();
        assert_eq!(provider.last_sync_attempted_at, Some(9_000));
        assert_eq!(provider.last_sync_succeeded_at, Some(9_001));
        assert_eq!(provider.last_sync_status, "ok");
    }

    // -- Query primitives --

    #[test]
    fn usage_summary_aggregates_correctly() {
        let db = setup();
        let pid = db
            .with_conn(|c| create_provider(c, "openai", "OpenAI", 1_000))
            .unwrap();

        let mut r1 = make_record(pid);
        r1.bucket_start = 1_000;
        r1.bucket_end = 4_600;
        r1.input_tokens = Some(100);
        r1.output_tokens = Some(50);
        r1.cost_usd = Some(1.0);

        let mut r2 = make_record(pid);
        r2.bucket_start = 2_000;
        r2.bucket_end = 5_600;
        r2.model = "gpt-4o".to_string();
        r2.input_tokens = Some(200);
        r2.output_tokens = Some(75);
        r2.cost_usd = Some(2.0);

        db.with_conn(|c| upsert_usage_records(c, &[r1, r2])).unwrap();

        let summary = db.with_conn(|c| get_usage_summary(c, pid, 0, i64::MAX)).unwrap();
        assert_eq!(summary.total_input_tokens, 300);
        assert_eq!(summary.total_output_tokens, 125);
        assert!((summary.total_cost_usd - 3.0).abs() < 1e-9);
    }

    #[test]
    fn with_transaction_commits_on_success() {
        let db = setup();
        db.with_transaction(|tx| {
            create_provider(tx, "openai", "OpenAI", 1_000)
        })
        .unwrap();

        let count: i64 = db
            .with_conn(|c| {
                Ok(c.query_row("SELECT COUNT(*) FROM providers", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn with_transaction_rolls_back_on_error() {
        let db = setup();
        let _ = db.with_transaction(|tx| -> Result<()> {
            create_provider(tx, "openai", "OpenAI", 1_000)?;
            anyhow::bail!("forced error");
        });

        let count: i64 = db
            .with_conn(|c| {
                Ok(c.query_row("SELECT COUNT(*) FROM providers", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn get_usage_summary_10k_records_under_200ms() {
        use std::time::Instant;

        let db = setup();

        let pids: Vec<i64> = ["openai", "anthropic"]
            .iter()
            .enumerate()
            .map(|(i, &slug)| {
                db.with_conn(|c| create_provider(c, slug, "P", i as i64 * 1000))
                    .unwrap()
            })
            .collect();

        const TOTAL: usize = 10_000;
        let records: Vec<UsageRecord> = (0..TOTAL)
            .map(|i| {
                let pid = pids[i % pids.len()];
                let bucket_start = 1_700_000_000_i64 + (i as i64) * 3600;
                UsageRecord {
                    id: 0,
                    provider_id: pid,
                    provider: "openai".to_string(),
                    model: format!("model-{}", i % 5),
                    bucket_start,
                    bucket_end: bucket_start + 3600,
                    bucket_granularity: BucketGranularity::Hour,
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    cache_creation_tokens: None,
                    cache_read_tokens: None,
                    request_count: Some(1),
                    cost_usd: Some(0.001),
                    cost_source: CostSource::Reported,
                    provider_metadata: None,
                    fetched_at: 0,
                }
            })
            .collect();

        db.with_conn(|c| upsert_usage_records(c, &records)).unwrap();

        let since = 1_700_000_000_i64;
        let until = since + 30 * 24 * 3600;
        let start = Instant::now();
        for &pid in &pids {
            let _ = db.with_conn(|c| get_usage_summary(c, pid, since, until)).unwrap();
        }
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() < 200,
            "get_usage_summary over 10k records took {}ms (budget: 200ms)",
            elapsed.as_millis()
        );
    }
}
