/// DDL for the schema_migrations bootstrap table, applied before the
/// migration runner loop so the table exists on a brand-new database.
/// All other DDL lives inline in the migration definitions in `migrations.rs`.

pub const CREATE_SCHEMA_MIGRATIONS: &str = "
CREATE TABLE IF NOT EXISTS schema_migrations (
    id         INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
)";
