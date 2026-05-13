use anyhow::{Context, Result};
use rusqlite::Connection;

use super::schema;

/// A single migration: an integer ID and the SQL to apply.
struct Migration {
    id: i64,
    sql: &'static str,
}

/// All migrations in order. Add new entries at the end; never modify existing ones.
fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            id: 1,
            sql: concat!(
                "CREATE TABLE IF NOT EXISTS schema_migrations (\n",
                "    id         INTEGER PRIMARY KEY,\n",
                "    applied_at INTEGER NOT NULL\n",
                ");\n",
                // profiles
                "CREATE TABLE IF NOT EXISTS profiles (\n",
                "    id                     INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    name                   TEXT    NOT NULL,\n",
                "    provider               TEXT    NOT NULL\n",
                "                               CHECK (provider IN ('openai','anthropic')),\n",
                "    provider_config        TEXT,\n",
                "    last_sync_attempted_at INTEGER,\n",
                "    last_sync_succeeded_at INTEGER,\n",
                "    last_sync_status       TEXT    NOT NULL\n",
                "                               DEFAULT 'never'\n",
                "                               CHECK (last_sync_status IN ('ok','failed','never')),\n",
                "    created_at             INTEGER NOT NULL\n",
                ");\n",
                // api_keys
                "CREATE TABLE IF NOT EXISTS api_keys (\n",
                "    id         INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    profile_id INTEGER NOT NULL\n",
                "                   REFERENCES profiles(id) ON DELETE CASCADE,\n",
                "    label      TEXT    NOT NULL DEFAULT '',\n",
                "    created_at INTEGER NOT NULL\n",
                ");\n",
                "CREATE INDEX IF NOT EXISTS idx_api_keys_profile\n",
                "    ON api_keys (profile_id);\n",
                // usage_records
                "CREATE TABLE IF NOT EXISTS usage_records (\n",
                "    id                    INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    profile_id            INTEGER NOT NULL\n",
                "                              REFERENCES profiles(id) ON DELETE CASCADE,\n",
                "    key_id                INTEGER\n",
                "                              REFERENCES api_keys(id) ON DELETE SET NULL,\n",
                "    provider              TEXT    NOT NULL,\n",
                "    model                 TEXT    NOT NULL DEFAULT '',\n",
                "    bucket_start          INTEGER NOT NULL,\n",
                "    bucket_end            INTEGER NOT NULL,\n",
                "    bucket_granularity    TEXT    NOT NULL\n",
                "                              CHECK (bucket_granularity IN ('minute','hour','day')),\n",
                "    input_tokens          INTEGER,\n",
                "    output_tokens         INTEGER,\n",
                "    cache_creation_tokens INTEGER,\n",
                "    cache_read_tokens     INTEGER,\n",
                "    request_count         INTEGER,\n",
                "    cost_usd              REAL,\n",
                "    cost_source           TEXT    NOT NULL\n",
                "                              DEFAULT 'reported'\n",
                "                              CHECK (cost_source IN ('reported','computed')),\n",
                "    provider_metadata     TEXT,\n",
                "    fetched_at            INTEGER NOT NULL\n",
                ");\n",
                "CREATE INDEX IF NOT EXISTS idx_usage_records_profile_bucket\n",
                "    ON usage_records (profile_id, bucket_start);\n",
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_records_upsert\n",
                "    ON usage_records (profile_id, model, bucket_start, bucket_granularity, provider);\n",
                // balances
                "CREATE TABLE IF NOT EXISTS balances (\n",
                "    id         INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    profile_id INTEGER NOT NULL\n",
                "                   REFERENCES profiles(id) ON DELETE CASCADE,\n",
                "    amount_usd REAL,\n",
                "    shape      TEXT    NOT NULL\n",
                "                   CHECK (shape IN (\n",
                "                       'remaining_credit',\n",
                "                       'spend_against_cap',\n",
                "                       'spend_this_period',\n",
                "                       'unknown'\n",
                "                   )),\n",
                "    note       TEXT,\n",
                "    as_of      INTEGER NOT NULL,\n",
                "    fetched_at INTEGER NOT NULL\n",
                ");\n",
                "CREATE INDEX IF NOT EXISTS idx_balances_profile\n",
                "    ON balances (profile_id, fetched_at DESC);\n",
                // widget_layouts
                "CREATE TABLE IF NOT EXISTS widget_layouts (\n",
                "    id   INTEGER PRIMARY KEY CHECK (id = 1),\n",
                "    blob TEXT NOT NULL\n",
                ");\n",
            ),
        },
        Migration {
            id: 2,
            sql: concat!(
                // v2: Keys are now provider-aware; profiles are provider-agnostic.
                "ALTER TABLE api_keys ADD COLUMN provider TEXT NOT NULL DEFAULT 'openai';\n",
                "ALTER TABLE api_keys ADD COLUMN key_preview TEXT NOT NULL DEFAULT '';\n",
                "UPDATE api_keys\n",
                "    SET provider = (SELECT provider FROM profiles WHERE profiles.id = api_keys.profile_id);\n",
                "CREATE TABLE profiles_new (\n",
                "    id                     INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    name                   TEXT    NOT NULL,\n",
                "    provider_config        TEXT,\n",
                "    last_sync_attempted_at INTEGER,\n",
                "    last_sync_succeeded_at INTEGER,\n",
                "    last_sync_status       TEXT    NOT NULL\n",
                "                               DEFAULT 'never'\n",
                "                               CHECK (last_sync_status IN ('ok','failed','never')),\n",
                "    created_at             INTEGER NOT NULL\n",
                ");\n",
                "INSERT INTO profiles_new\n",
                "    SELECT id, name, provider_config, last_sync_attempted_at,\n",
                "           last_sync_succeeded_at, last_sync_status, created_at\n",
                "    FROM profiles;\n",
                "DROP TABLE profiles;\n",
                "ALTER TABLE profiles_new RENAME TO profiles;\n",
            ),
        },
        Migration {
            id: 3,
            sql: concat!(
                // v3: Replace profiles + api_keys with a unified providers table.
                // Drop all data-bearing tables in FK-safe order, then recreate.
                "DROP TABLE IF EXISTS balances;\n",
                "DROP TABLE IF EXISTS usage_records;\n",
                "DROP TABLE IF EXISTS api_keys;\n",
                "DROP TABLE IF EXISTS profiles;\n",
                // providers table: one row per provider type, at most two rows.
                "CREATE TABLE providers (\n",
                "    id                     INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    provider_type          TEXT    NOT NULL UNIQUE\n",
                "                               CHECK (provider_type IN ('openai','anthropic')),\n",
                "    display_name           TEXT    NOT NULL,\n",
                "    last_sync_attempted_at INTEGER,\n",
                "    last_sync_succeeded_at INTEGER,\n",
                "    last_sync_status       TEXT    NOT NULL\n",
                "                               DEFAULT 'never'\n",
                "                               CHECK (last_sync_status IN ('ok','failed','never')),\n",
                "    created_at             INTEGER NOT NULL\n",
                ");\n",
                // usage_records now keyed to provider_id (no key_id).
                "CREATE TABLE usage_records (\n",
                "    id                    INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    provider_id           INTEGER NOT NULL\n",
                "                              REFERENCES providers(id) ON DELETE CASCADE,\n",
                "    provider              TEXT    NOT NULL,\n",
                "    model                 TEXT    NOT NULL DEFAULT '',\n",
                "    bucket_start          INTEGER NOT NULL,\n",
                "    bucket_end            INTEGER NOT NULL,\n",
                "    bucket_granularity    TEXT    NOT NULL\n",
                "                              CHECK (bucket_granularity IN ('minute','hour','day')),\n",
                "    input_tokens          INTEGER,\n",
                "    output_tokens         INTEGER,\n",
                "    cache_creation_tokens INTEGER,\n",
                "    cache_read_tokens     INTEGER,\n",
                "    request_count         INTEGER,\n",
                "    cost_usd              REAL,\n",
                "    cost_source           TEXT    NOT NULL\n",
                "                              DEFAULT 'reported'\n",
                "                              CHECK (cost_source IN ('reported','computed')),\n",
                "    provider_metadata     TEXT,\n",
                "    fetched_at            INTEGER NOT NULL\n",
                ");\n",
                "CREATE INDEX idx_usage_records_provider_bucket\n",
                "    ON usage_records (provider_id, bucket_start);\n",
                "CREATE UNIQUE INDEX idx_usage_records_upsert\n",
                "    ON usage_records (provider_id, model, bucket_start, bucket_granularity, provider);\n",
                // balances now keyed to provider_id.
                "CREATE TABLE balances (\n",
                "    id          INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    provider_id INTEGER NOT NULL\n",
                "                    REFERENCES providers(id) ON DELETE CASCADE,\n",
                "    amount_usd  REAL,\n",
                "    shape       TEXT    NOT NULL\n",
                "                    CHECK (shape IN (\n",
                "                        'remaining_credit',\n",
                "                        'spend_against_cap',\n",
                "                        'spend_this_period',\n",
                "                        'unknown'\n",
                "                    )),\n",
                "    note        TEXT,\n",
                "    as_of       INTEGER NOT NULL,\n",
                "    fetched_at  INTEGER NOT NULL\n",
                ");\n",
                "CREATE INDEX idx_balances_provider\n",
                "    ON balances (provider_id, fetched_at DESC);\n",
            ),
        },
        Migration {
            id: 4,
            // v4: Remove the CHECK constraint on provider_type so new providers
            // can be added without a schema migration. The Rust type system
            // enforces valid slugs at the application boundary.
            //
            // SQLite does not support DROP CONSTRAINT. The naive workaround
            // (rename providers → providers_old, create new providers, copy, drop
            // providers_old) fails because SQLite 3.26+ auto-updates FK references
            // in child tables when a parent is renamed — so usage_records and
            // balances end up referencing providers_old, which is then dropped.
            //
            // Safe approach: recreate all three tables with _v4 suffixes, copy data,
            // drop the old tables in FK-safe order (referencing tables first), then
            // rename the new tables into place. Renaming providers_v4 → providers
            // triggers SQLite's FK auto-update intentionally, rewriting
            // REFERENCES providers_v4(id) to REFERENCES providers(id) in the new
            // child tables before they are themselves renamed.
            sql: concat!(
                // New providers table (no CHECK constraint on provider_type).
                "CREATE TABLE providers_v4 (\n",
                "    id                     INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    provider_type          TEXT    NOT NULL UNIQUE,\n",
                "    display_name           TEXT    NOT NULL,\n",
                "    last_sync_attempted_at INTEGER,\n",
                "    last_sync_succeeded_at INTEGER,\n",
                "    last_sync_status       TEXT    NOT NULL\n",
                "                               DEFAULT 'never'\n",
                "                               CHECK (last_sync_status IN ('ok','failed','never')),\n",
                "    created_at             INTEGER NOT NULL\n",
                ");\n",
                // New child tables referencing providers_v4.
                "CREATE TABLE usage_records_v4 (\n",
                "    id                    INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    provider_id           INTEGER NOT NULL\n",
                "                              REFERENCES providers_v4(id) ON DELETE CASCADE,\n",
                "    provider              TEXT    NOT NULL,\n",
                "    model                 TEXT    NOT NULL DEFAULT '',\n",
                "    bucket_start          INTEGER NOT NULL,\n",
                "    bucket_end            INTEGER NOT NULL,\n",
                "    bucket_granularity    TEXT    NOT NULL\n",
                "                              CHECK (bucket_granularity IN ('minute','hour','day')),\n",
                "    input_tokens          INTEGER,\n",
                "    output_tokens         INTEGER,\n",
                "    cache_creation_tokens INTEGER,\n",
                "    cache_read_tokens     INTEGER,\n",
                "    request_count         INTEGER,\n",
                "    cost_usd              REAL,\n",
                "    cost_source           TEXT    NOT NULL\n",
                "                              DEFAULT 'reported'\n",
                "                              CHECK (cost_source IN ('reported','computed')),\n",
                "    provider_metadata     TEXT,\n",
                "    fetched_at            INTEGER NOT NULL\n",
                ");\n",
                "CREATE TABLE balances_v4 (\n",
                "    id          INTEGER PRIMARY KEY AUTOINCREMENT,\n",
                "    provider_id INTEGER NOT NULL\n",
                "                    REFERENCES providers_v4(id) ON DELETE CASCADE,\n",
                "    amount_usd  REAL,\n",
                "    shape       TEXT    NOT NULL\n",
                "                    CHECK (shape IN (\n",
                "                        'remaining_credit',\n",
                "                        'spend_against_cap',\n",
                "                        'spend_this_period',\n",
                "                        'unknown'\n",
                "                    )),\n",
                "    note        TEXT,\n",
                "    as_of       INTEGER NOT NULL,\n",
                "    fetched_at  INTEGER NOT NULL\n",
                ");\n",
                // Copy data.
                "INSERT INTO providers_v4 SELECT * FROM providers;\n",
                "INSERT INTO usage_records_v4 SELECT * FROM usage_records;\n",
                "INSERT INTO balances_v4 SELECT * FROM balances;\n",
                // Drop old tables in FK-safe order: referencing before referenced.
                "DROP TABLE usage_records;\n",
                "DROP TABLE balances;\n",
                "DROP TABLE providers;\n",
                // Rename into place. Renaming providers_v4 → providers triggers
                // FK auto-update in usage_records_v4/balances_v4 (desired here).
                "ALTER TABLE providers_v4 RENAME TO providers;\n",
                "ALTER TABLE usage_records_v4 RENAME TO usage_records;\n",
                "ALTER TABLE balances_v4 RENAME TO balances;\n",
                // Recreate indices (dropped with old tables).
                "CREATE INDEX idx_usage_records_provider_bucket\n",
                "    ON usage_records (provider_id, bucket_start);\n",
                "CREATE UNIQUE INDEX idx_usage_records_upsert\n",
                "    ON usage_records (provider_id, model, bucket_start, bucket_granularity, provider);\n",
                "CREATE INDEX idx_balances_provider\n",
                "    ON balances (provider_id, fetched_at DESC);\n",
            ),
        },
        Migration {
            id: 5,
            sql: concat!(
                "CREATE TABLE IF NOT EXISTS cached_claude_code_result (\n",
                "    id         INTEGER PRIMARY KEY CHECK (id = 1),\n",
                "    blob       TEXT    NOT NULL,\n",
                "    fetched_at INTEGER NOT NULL\n",
                ");\n",
            ),
        },
        Migration {
            id: 6,
            // v6: Drop the widget_layouts table created in migration 1.
            // It was never used by any application code and is not planned for v1.x.
            sql: "DROP TABLE IF EXISTS widget_layouts;\n",
        },
    ]
}

/// Runs all unapplied migrations in order. Safe to call on every startup:
/// already-applied migrations are skipped.
///
/// The schema_migrations table is bootstrapped first (via CREATE TABLE IF NOT
/// EXISTS) so the runner works even on a brand-new database.
pub fn run_migrations(conn: &Connection) -> Result<()> {
    // Bootstrap the migrations table itself.
    conn.execute_batch(schema::CREATE_SCHEMA_MIGRATIONS)
        .context("failed to create schema_migrations table")?;

    for migration in migrations() {
        let already_applied: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE id = ?1",
                [migration.id],
                |row| row.get(0),
            )
            .context("failed to query schema_migrations")?;

        if already_applied {
            continue;
        }

        conn.execute_batch(migration.sql)
            .with_context(|| format!("failed to apply migration {}", migration.id))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        conn.execute(
            "INSERT INTO schema_migrations (id, applied_at) VALUES (?1, ?2)",
            rusqlite::params![migration.id, now],
        )
        .with_context(|| format!("failed to record migration {}", migration.id))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_in_memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn
    }

    #[test]
    fn migrations_run_on_empty_db() {
        let conn = open_in_memory();
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = open_in_memory();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap(); // second run must not error
    }

    #[test]
    fn all_tables_exist_after_migration() {
        let conn = open_in_memory();
        run_migrations(&conn).unwrap();

        let tables = ["providers", "usage_records", "balances", "schema_migrations"];
        for table in &tables {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table '{table}' missing after migration");
        }
    }

    #[test]
    fn old_tables_removed_after_migration() {
        let conn = open_in_memory();
        run_migrations(&conn).unwrap();

        for table in &["profiles", "api_keys", "widget_layouts"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0, "old table '{table}' should not exist after migrations");
        }
    }

    #[test]
    fn foreign_keys_enforced() {
        let conn = open_in_memory();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run_migrations(&conn).unwrap();

        let result = conn.execute(
            "INSERT INTO usage_records (provider_id, provider, model, bucket_start, bucket_end, bucket_granularity, fetched_at) VALUES (999, 'openai', '', 0, 1, 'hour', 0)",
            [],
        );
        assert!(result.is_err(), "expected FK violation for non-existent provider");
    }

    #[test]
    fn provider_delete_cascades_to_usage() {
        let conn = open_in_memory();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run_migrations(&conn).unwrap();

        conn.execute(
            "INSERT INTO providers (id, provider_type, display_name, created_at) VALUES (1, 'openai', 'OpenAI', 0)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO usage_records (provider_id, provider, model, bucket_start, bucket_end, bucket_granularity, fetched_at) VALUES (1, 'openai', '', 0, 3600, 'hour', 0)",
            [],
        ).unwrap();

        conn.execute("DELETE FROM providers WHERE id = 1", []).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn usage_record_upsert_index_exists() {
        let conn = open_in_memory();
        run_migrations(&conn).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_usage_records_upsert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
