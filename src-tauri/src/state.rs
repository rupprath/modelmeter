#![forbid(unsafe_code)]

use modelmeter_core::{db::Database, secrets::SecretStore, sync::SyncCoordinator};

/// Application-wide state, held by Tauri's managed-state system.
///
/// `Database` contains an `Arc<Mutex<Connection>>` so cloning is cheap and
/// thread-safe. `SecretStore` is a zero-size stateless handle. `SyncCoordinator`
/// is a clonable `Arc`-backed handle.
pub struct AppState {
    pub db: Database,
    pub secrets: SecretStore,
    pub sync: SyncCoordinator,
}
