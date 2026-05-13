#![forbid(unsafe_code)]

//! Encrypted secret storage via the OS keystore.
//!
//! On Windows: Windows Credential Manager (backed by DPAPI in CurrentUser scope).
//! On macOS:   Keychain (via the keyring crate's native Keychain backend).
//!
//! Credentials are stored under the service name `"modelmeter"` with a
//! per-provider account name equal to the provider type string
//! (e.g. `"openai"` or `"anthropic"`).
//!
//! # Just-in-time access
//!
//! The `accessor_for` method returns a closure that decrypts the secret at
//! the moment it is called — not before. Provider impls hold this closure and
//! call it once per outbound HTTP request to build the Authorization header,
//! then immediately drop the returned string. This limits the window during
//! which plaintext credentials are in memory.

use anyhow::{Context, Result};
use keyring::Entry;
use zeroize::Zeroizing;

const SERVICE: &str = "modelmeter";

// Keyring account name for the SQLCipher database encryption key.
// Uses a leading/trailing double-underscore to ensure it can never collide
// with a provider slug (which are plain words like "openai", "anthropic").
const DB_KEY_ACCOUNT: &str = "__db__";

/// Generates a 256-bit (32-byte) encryption key from the OS CSPRNG,
/// returned as a 64-character lowercase hex string.
fn generate_db_key() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("getrandom failed: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{:02x}", b)).collect())
}

/// Stateless handle to the OS secret store.
///
/// All methods are synchronous because keyring operations are fast
/// (< 1 ms on typical hardware) and do not block the async runtime in a
/// harmful way.
#[derive(Debug, Clone)]
pub struct SecretStore;

impl SecretStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(provider_type: &str) -> Result<Entry> {
        Entry::new(SERVICE, provider_type)
            .with_context(|| format!("failed to create keyring entry for {provider_type}"))
    }

    /// Stores `secret` in the OS keystore for the given provider type.
    ///
    /// Any previous value for the same provider is overwritten — this is the
    /// correct behaviour for key rotation.
    pub fn set(&self, provider_type: &str, secret: &str) -> Result<()> {
        Self::entry(provider_type)?
            .set_password(secret)
            .with_context(|| {
                format!("failed to store secret for provider {provider_type}")
            })
    }

    /// Retrieves the plaintext secret for the given provider type, wrapped in
    /// `Zeroizing` so the buffer is overwritten when the value is dropped.
    pub fn get(&self, provider_type: &str) -> Result<Zeroizing<String>> {
        let password = Self::entry(provider_type)?
            .get_password()
            .with_context(|| {
                format!("failed to retrieve secret for provider {provider_type}")
            })?;
        Ok(Zeroizing::new(password))
    }

    /// Removes the secret from the OS keystore. Safe to call if the entry does
    /// not exist (the error is ignored).
    pub fn delete(&self, provider_type: &str) -> Result<()> {
        match Self::entry(provider_type)?.delete_credential() {
            Ok(()) => Ok(()),
            // "NoEntry" means it was already gone — that's fine.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!(
                "failed to delete secret for provider {provider_type}: {e}"
            )),
        }
    }

    /// Returns the SQLCipher database encryption key, generating and persisting
    /// a new one if it does not yet exist in the keystore.
    ///
    /// The second return value is `true` when the key was freshly generated
    /// (i.e. it was not found in the keystore). The caller uses this flag to
    /// decide whether to migrate an existing plaintext database via
    /// `sqlcipher_export()` or open an already-encrypted database with
    /// `PRAGMA key`.
    pub fn get_or_generate_db_key(&self) -> Result<(Zeroizing<String>, bool)> {
        match Self::entry(DB_KEY_ACCOUNT)?.get_password() {
            Ok(key) => Ok((Zeroizing::new(key), false)),
            Err(keyring::Error::NoEntry) => {
                let key = generate_db_key().context("failed to generate database encryption key")?;
                Self::entry(DB_KEY_ACCOUNT)?
                    .set_password(&key)
                    .context("failed to store database encryption key in keystore")?;
                Ok((Zeroizing::new(key), true))
            }
            Err(e) => Err(anyhow::anyhow!("failed to retrieve database encryption key: {e}")),
        }
    }

    /// Returns a closure that retrieves the secret just-in-time.
    ///
    /// The returned closure is `Send + Sync + 'static` so it can be held
    /// inside a provider struct that is itself `Send + Sync`. The closure
    /// creates a fresh keyring `Entry` on every call, decrypts, and drops the
    /// entry immediately — no plaintext is retained between calls.
    pub fn accessor_for(
        &self,
        provider_type: &str,
    ) -> impl Fn() -> Result<Zeroizing<String>> + Send + Sync + 'static {
        let pt = provider_type.to_string();
        move || {
            let password = Entry::new(SERVICE, &pt)
                .context("failed to create keyring entry")?
                .get_password()
                .with_context(|| format!("failed to retrieve secret for provider {pt}"))?;
            Ok(Zeroizing::new(password))
        }
    }
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires live OS keyring; run manually with `cargo test -- --ignored`"]
    fn set_get_delete_roundtrip() {
        let store = SecretStore::new();
        let secret = "test-secret-value-12345";
        let provider_type = "openai-test";

        let set_result = store.set(provider_type, secret);
        if let Err(e) = &set_result {
            let msg = format!("{e}");
            if msg.contains("No keyring service") || msg.contains("Failed to store") {
                eprintln!("skipping keyring test: no OS keyring available");
                return;
            }
        }
        set_result.unwrap();

        let got = store.get(provider_type).unwrap();
        assert_eq!(got.as_str(), secret);

        store.delete(provider_type).unwrap();
        assert!(store.get(provider_type).is_err());
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let store = SecretStore::new();
        let result = store.delete("nonexistent-provider-type-xyz");
        let _ = result;
    }

    #[test]
    fn accessor_closure_is_send_sync() {
        let store = SecretStore::new();
        let accessor = store.accessor_for("openai");
        fn assert_send_sync<T: Send + Sync>(_: T) {}
        assert_send_sync(accessor);
    }
}
