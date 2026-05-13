/// Read-only diagnostic: dumps providers, usage records, and balances from the production DB.
/// Run with: cargo run --example read_db -p modelmeter-core

fn main() {
    let path = dirs::data_dir()
        .expect("data dir")
        .join("modelmeter")
        .join("modelmeter.db");

    println!("Opening: {}", path.display());

    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open DB read-only");

    // ── Providers ────────────────────────────────────────────────────────────
    println!("\n=== PROVIDERS ===");
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, provider_type, display_name, last_sync_status,
                        last_sync_attempted_at, last_sync_succeeded_at
                 FROM providers ORDER BY id",
            )
            .expect("prepare providers");

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })
            .expect("query providers");

        let mut count = 0;
        for row in rows {
            let (id, ptype, name, status, attempted, succeeded) = row.unwrap();
            println!(
                "  id={id}  type={ptype}  name={name:?}  status={status}  attempted={attempted:?}  succeeded={succeeded:?}"
            );
            count += 1;
        }
        if count == 0 {
            println!("  (no providers)");
        }
    }

    // ── Usage records summary ─────────────────────────────────────────────────
    println!("\n=== USAGE RECORDS (summary per provider+model) ===");
    {
        let mut stmt = conn
            .prepare(
                "SELECT provider, model,
                        COUNT(*) as rows,
                        MIN(bucket_start) as earliest,
                        MAX(bucket_start) as latest,
                        SUM(input_tokens) as total_in,
                        SUM(output_tokens) as total_out,
                        SUM(cost_usd) as total_cost
                 FROM usage_records
                 GROUP BY provider, model
                 ORDER BY provider, model",
            )
            .expect("prepare usage summary");

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                ))
            })
            .expect("query usage");

        let mut total = 0i64;
        for row in rows {
            let (provider, model, rows, earliest, latest, total_in, total_out, total_cost) =
                row.unwrap();
            let earliest_dt = unix_to_utc(earliest);
            let latest_dt = unix_to_utc(latest);
            println!(
                "  {provider}/{model}: {rows} rows  {earliest_dt}..{latest_dt}  \
                 in={total_in:?}  out={total_out:?}  cost={total_cost:?}"
            );
            total += rows;
        }
        if total == 0 {
            println!("  (no usage records)");
        } else {
            println!("  Total: {total} usage records");
        }
    }

    // ── Recent usage records ──────────────────────────────────────────────────
    println!("\n=== RECENT USAGE RECORDS (last 20) ===");
    {
        let mut stmt = conn
            .prepare(
                "SELECT provider, model, bucket_start, bucket_granularity,
                        input_tokens, output_tokens, cost_usd, cost_source
                 FROM usage_records
                 ORDER BY bucket_start DESC, id DESC
                 LIMIT 20",
            )
            .expect("prepare recent usage");

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .expect("query recent usage");

        let mut count = 0;
        for row in rows {
            let (provider, model, bucket_start, gran, input, output, cost, source) = row.unwrap();
            let dt = unix_to_utc(bucket_start);
            println!(
                "  {provider}/{model}  {dt}  [{gran}]  \
                 in={input:?}  out={output:?}  cost={cost:?} ({source})"
            );
            count += 1;
        }
        if count == 0 {
            println!("  (no usage records)");
        }
    }

    // ── Balances ──────────────────────────────────────────────────────────────
    println!("\n=== BALANCES (latest per provider) ===");
    {
        let mut stmt = conn
            .prepare(
                "SELECT p.provider_type, b.amount_usd, b.shape, b.note, b.as_of
                 FROM balances b
                 JOIN providers p ON p.id = b.provider_id
                 WHERE b.id IN (
                     SELECT id FROM balances b2
                     WHERE b2.provider_id = b.provider_id
                     ORDER BY b2.fetched_at DESC LIMIT 1
                 )
                 ORDER BY p.provider_type",
            )
            .expect("prepare balances");

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<f64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .expect("query balances");

        let mut count = 0;
        for row in rows {
            let (provider, amount, shape, note, as_of) = row.unwrap();
            let dt = unix_to_utc(as_of);
            println!(
                "  {provider}: amount={amount:?}  shape={shape}  note={note:?}  as_of={dt}"
            );
            count += 1;
        }
        if count == 0 {
            println!("  (no balances)");
        }
    }

    println!();
}

fn unix_to_utc(ts: i64) -> String {
    // Simple UTC formatting without pulling in chrono — good enough for diagnostics.
    let secs = ts % 60;
    let mins = (ts / 60) % 60;
    let hours = (ts / 3600) % 24;
    let days_since_epoch = ts / 86400;
    // Rata Die → Gregorian (Fliegel-Van Flandern algorithm)
    let z = days_since_epoch + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{mins:02}:{secs:02}Z")
}
