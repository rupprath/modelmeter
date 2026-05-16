#![forbid(unsafe_code)]

pub mod migrations;
pub mod schema;

use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::config;
use crate::secrets::SecretStore;
use migrations::run_migrations;

/// Thread-safe handle to the SQLite database.
///
/// Internally wraps a single connection protected by a Mutex. WAL journal mode
/// is enabled so that reads do not block writes at the OS level; the Mutex
/// serialises access within the process, which is correct and sufficient for
/// v1's workload (one sync writer at a time, fast dashboard reads).
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Opens (or creates) the database at the default OS data path, applies
    /// SQLCipher encryption, and runs all pending migrations.
    ///
    /// The encryption key is stored in the OS keystore (Windows Credential
    /// Manager / macOS Keychain). On first open a 256-bit random key is
    /// generated and persisted there. On subsequent opens the stored key is
    /// retrieved and used to decrypt the database.
    ///
    /// **Migration from an unencrypted database:** if no key exists in the
    /// keystore but a database file is already present (upgrade from an older
    /// build), the file is treated as plaintext, migrated via
    /// `sqlcipher_export()` into a new encrypted copy, and the original is
    /// replaced atomically.
    pub fn open() -> Result<Self> {
        let dir = config::data_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create data dir {}", dir.display()))?;
        let path = dir.join("modelmeter.db");

        let (db_key, key_is_new) = SecretStore::new()
            .get_or_generate_db_key()
            .context("failed to obtain database encryption key from keystore")?;

        // Migration path: existing plaintext database from an older install.
        // PRAGMA rekey only works on already-encrypted databases; to encrypt a
        // plaintext database we must export via sqlcipher_export() instead.
        if key_is_new && path.exists() {
            let conn = Connection::open(&path)
                .with_context(|| format!("failed to open plaintext database at {}", path.display()))?;
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA synchronous = NORMAL;",
            )
            .context("failed to set connection pragmas")?;
            run_migrations(&conn).context("failed to run database migrations")?;

            let tmp = path.with_extension("db.new");
            let enc = tmp.to_string_lossy().replace('\\', "/").replace('\'', "''");
            conn.execute_batch(&format!(
                "ATTACH DATABASE '{enc}' AS enc KEY '{key}';
                 SELECT sqlcipher_export('enc');
                 DETACH DATABASE enc;",
                key = db_key.as_str(),
            ))
            .context("failed to export database to encrypted copy")?;
            drop(conn);
            std::fs::rename(&tmp, &path)
                .context("failed to replace plaintext database with encrypted version")?;
        }

        // Normal path: new database or already-encrypted database.
        // PRAGMA key must be the very first SQL operation on the connection.
        //
        // String interpolation looks like a SQL-injection footgun but is safe
        // here: SQLCipher does not allow PRAGMA key to be parameterised, and
        // `db_key` is a 64-char lowercase hex string from the OS CSPRNG (see
        // secrets::generate_db_key), so no quote character can appear in it.
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        conn.execute_batch(&format!("PRAGMA key = '{}';", db_key.as_str()))
            .context("failed to set database encryption key")?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
        )
        .context("failed to set connection pragmas")?;
        run_migrations(&conn).context("failed to run database migrations")?;

        // Restrict file to owner-only on Unix (macOS). On Windows, %APPDATA%
        // already inherits user-restricted ACLs from the directory.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .context("get db file metadata")?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&path, perms)
                .context("set db file permissions to 0o600")?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Opens (or creates) the database at an explicit path. Useful for tests.
    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;

        // These pragmas must be set before anything else on every connection.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
        )
        .context("failed to set connection pragmas")?;

        run_migrations(&conn).context("failed to run database migrations")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Opens an in-memory database (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("failed to open in-memory database")?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;",
        )
        .context("failed to set connection pragmas")?;
        run_migrations(&conn).context("failed to run database migrations")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs a closure with exclusive access to the underlying connection.
    ///
    /// Prefer this over exposing the `Connection` directly: it ensures the
    /// Mutex is always held for the duration of any DB operation and prevents
    /// partial-state reads between statements.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.conn.lock().expect("database mutex poisoned");
        f(&conn)
    }

    /// Runs a closure inside a SQLite transaction. Commits on success,
    /// automatically rolls back on error (via `Transaction` drop).
    ///
    /// Functions in `crate::crud` that take `&Connection` also accept
    /// `&Transaction<'_>` via `Deref`, so they can be called unchanged
    /// inside this closure.
    pub fn with_transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: for<'tx> FnOnce(&Transaction<'tx>) -> Result<T>,
    {
        let mut guard = self.conn.lock().expect("database mutex poisoned");
        let tx = guard.transaction().context("begin transaction")?;
        let result = f(&tx)?;
        tx.commit().context("commit transaction")?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_works() {
        Database::open_in_memory().unwrap();
    }

    #[test]
    fn with_conn_runs_query() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db
            .with_conn(|c| {
                Ok(c.query_row("SELECT COUNT(*) FROM providers", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
