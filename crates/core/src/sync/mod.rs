#![forbid(unsafe_code)]

//! Sync engine — coordinator + per-provider worker state machine.
//!
//! See docs/ModelMeter_Sync_Engine.md for the full specification.
//!
//! # Architecture
//!
//! `SyncCoordinator` is a clonable handle wrapping `Arc<Inner>`. `Inner` holds
//! the shared mutable state (a `tokio::sync::Mutex<CoordState>`), the DB, the
//! secret store, a concurrency semaphore, and a broadcast channel for
//! `ProviderSyncComplete` events.
//!
//! Workers are not long-lived tasks. Instead the coordinator maintains a
//! `HashMap<i64, WorkerState>` and spawns ad-hoc `tokio` tasks when a provider
//! needs syncing. WaitingToRetry is handled within the same spawned task: the
//! task drops its semaphore permit, sleeps `max(30s, Retry-After)`, atomically
//! rechecks state, and re-acquires the permit before retrying. A concurrent
//! `dispatch_providers` call that sees `WaitingToRetry` atomically transitions
//! the state to `Running` and spawns a fresh task; the sleeping task then sees
//! a non-WaitingToRetry state and exits without duplicating work.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{broadcast, Mutex, Semaphore};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::config::AppConfig;
use crate::crud;
use crate::db::Database;
use crate::providers::{build_provider, unix_now, ProviderError, TimeRange};
use crate::secrets::SecretStore;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Emitted after a worker transitions out of Running/WaitingToRetry.
/// Fired once per provider per sync run, after the DB write commits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderSyncComplete {
    pub provider_id: i64,
    /// `"succeeded"` or `"failed"`.
    pub status: &'static str,
    /// Populated on failure. One of: `"auth"`, `"billing"`, `"network"`,
    /// `"transient_exhausted"`, `"unknown"`.
    pub reason: Option<String>,
    /// Unix UTC seconds.
    pub timestamp: i64,
}

/// Snapshot returned by `get_status`. Used by the dashboard indicator.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncStatus {
    pub paused: bool,
    pub last_tick_at: Option<i64>,
    /// `"green"` | `"amber"` | `"spinner"` | `"grey"`.
    pub indicator: &'static str,
    pub providers: HashMap<i64, ProviderSyncStateDto>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderSyncStateDto {
    /// `"idle"` | `"running"` | `"waiting_to_retry"` | `"succeeded"` | `"failed"`.
    pub state: &'static str,
    pub reason: Option<String>,
}

/// Error returned when a trigger command is rejected.
#[derive(Debug, thiserror::Error, serde::Serialize)]
pub enum SyncTriggerError {
    #[error("sync is paused")]
    Paused,
    #[error("unknown provider id: {0}")]
    UnknownProvider(i64),
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

enum WorkerState {
    Idle,
    Running,
    WaitingToRetry,
    Succeeded,
    Failed { reason: String },
}

impl WorkerState {
    fn as_str(&self) -> &'static str {
        match self {
            WorkerState::Idle => "idle",
            WorkerState::Running => "running",
            WorkerState::WaitingToRetry => "waiting_to_retry",
            WorkerState::Succeeded => "succeeded",
            WorkerState::Failed { .. } => "failed",
        }
    }
}

struct CoordState {
    providers: HashMap<i64, WorkerState>,
    paused: bool,
    last_tick_at: Option<i64>,
    /// Tracks the last time a manual trigger was accepted, to enforce a
    /// minimum cooldown between back-to-back manual syncs.
    last_manual_trigger_at: Option<Instant>,
}

struct Inner {
    db: Database,
    secrets: SecretStore,
    cfg: AppConfig,
    state: Mutex<CoordState>,
    semaphore: Arc<Semaphore>,
    events: broadcast::Sender<ProviderSyncComplete>,
}

// ---------------------------------------------------------------------------
// SyncCoordinator
// ---------------------------------------------------------------------------

/// Clonable handle to the sync coordinator.
///
/// Create with `SyncCoordinator::new`, then call `start()` once to initialise
/// worker states from the database and start the background tick loop.
#[derive(Clone)]
pub struct SyncCoordinator {
    inner: Arc<Inner>,
}

impl SyncCoordinator {
    /// Creates the coordinator.
    ///
    /// Returns the coordinator plus a broadcast receiver for
    /// `ProviderSyncComplete` events. The Tauri layer subscribes to this
    /// receiver and re-emits events to the frontend.
    pub fn new(
        db: Database,
        secrets: SecretStore,
        cfg: AppConfig,
    ) -> (Self, broadcast::Receiver<ProviderSyncComplete>) {
        let max_concurrent = cfg.sync.max_concurrent_profiles as usize;
        let (events_tx, events_rx) = broadcast::channel(128);
        let inner = Arc::new(Inner {
            db,
            secrets,
            cfg: cfg.clone(),
            state: Mutex::new(CoordState {
                providers: HashMap::new(),
                paused: cfg.sync.paused,
                last_tick_at: None,
                last_manual_trigger_at: None,
            }),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            events: events_tx,
        });
        (SyncCoordinator { inner }, events_rx)
    }

    /// Loads providers from the database, initialises worker states, fires the
    /// launch sync (unless paused), and starts the tick loop.
    ///
    /// Must be called exactly once after construction.
    pub async fn start(&self) -> Result<()> {
        let providers = self.inner.db.with_conn(crud::list_providers)?;
        let provider_ids: Vec<i64> = providers.iter().map(|p| p.id).collect();

        {
            let mut state = self.inner.state.lock().await;
            for id in &provider_ids {
                state.providers.insert(*id, WorkerState::Idle);
            }
        }

        // Launch sync: immediate first tick unless paused or no providers.
        if !self.inner.state.lock().await.paused && !provider_ids.is_empty() {
            self.dispatch_providers(provider_ids).await;
            self.inner.state.lock().await.last_tick_at = Some(unix_now());
        }

        // Tick loop.
        let coord = self.clone();
        tokio::spawn(async move {
            let secs = coord.inner.cfg.sync.interval_seconds;
            let mut ticker = tokio::time::interval(Duration::from_secs(secs));
            // MissedTickBehavior::Delay: if a tick takes longer than the
            // interval, the next tick fires one interval *after it finishes*,
            // not immediately. This prevents burst ticks after suspend/resume.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // discard first immediate tick (launch sync above)
            loop {
                ticker.tick().await;
                let paused = coord.inner.state.lock().await.paused;
                if paused {
                    continue;
                }
                let ids: Vec<i64> = {
                    let state = coord.inner.state.lock().await;
                    state.providers.keys().copied().collect()
                };
                coord.dispatch_providers(ids).await;
                coord.inner.state.lock().await.last_tick_at = Some(unix_now());
            }
        });

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Global manual refresh: bumps every eligible worker to Running.
    ///
    /// Calls within 30 seconds of a previous accepted trigger are silently
    /// coalesced — the in-flight sync already covers the period.
    pub async fn trigger_all(&self) -> Result<(), SyncTriggerError> {
        const MANUAL_COOLDOWN: Duration = Duration::from_secs(30);
        let ids: Vec<i64> = {
            let mut state = self.inner.state.lock().await;
            if state.paused {
                return Err(SyncTriggerError::Paused);
            }
            if let Some(last) = state.last_manual_trigger_at {
                if last.elapsed() < MANUAL_COOLDOWN {
                    return Ok(());
                }
            }
            state.last_manual_trigger_at = Some(Instant::now());
            state.providers.keys().copied().collect()
        };
        self.dispatch_providers(ids).await;
        Ok(())
    }

    /// Per-widget manual refresh: bumps only the listed providers.
    pub async fn trigger_for_providers(
        &self,
        provider_ids: Vec<i64>,
    ) -> Result<(), SyncTriggerError> {
        {
            let state = self.inner.state.lock().await;
            if state.paused {
                return Err(SyncTriggerError::Paused);
            }
            for id in &provider_ids {
                if !state.providers.contains_key(id) {
                    return Err(SyncTriggerError::UnknownProvider(*id));
                }
            }
        }
        self.dispatch_providers(provider_ids).await;
        Ok(())
    }

    /// Switches the coordinator to Paused. In-flight workers finish naturally.
    pub async fn pause(&self) {
        self.inner.state.lock().await.paused = true;
    }

    /// Switches the coordinator to Active. Restarts scheduling from now.
    pub async fn resume(&self) {
        self.inner.state.lock().await.paused = false;
    }

    /// Returns a snapshot of current state for the dashboard indicator.
    pub async fn get_status(&self) -> SyncStatus {
        let state = self.inner.state.lock().await;
        compute_status(&state)
    }

    /// Called by the Tauri command layer when a new provider is created.
    /// Adds a worker and triggers an immediate sync.
    pub async fn on_provider_added(&self, provider_id: i64) {
        let paused = {
            let mut state = self.inner.state.lock().await;
            state.providers.insert(provider_id, WorkerState::Idle);
            state.paused
        };
        if !paused {
            self.dispatch_providers(vec![provider_id]).await;
        }
    }

    /// Called by the Tauri command layer when a provider is deleted.
    /// Removes the worker entry; any in-flight task discards its results.
    pub async fn on_provider_deleted(&self, provider_id: i64) {
        self.inner.state.lock().await.providers.remove(&provider_id);
    }

    // -----------------------------------------------------------------------
    // Internal dispatch
    // -----------------------------------------------------------------------

    /// Locks the state and transitions each eligible worker to Running, then
    /// spawns sync tasks. Called with the Mutex *not* held.
    async fn dispatch_providers(&self, ids: Vec<i64>) {
        let to_start: Vec<i64> = {
            let mut state = self.inner.state.lock().await;
            let mut to_start = Vec::new();
            for id in ids {
                match state.providers.get(&id) {
                    Some(WorkerState::Running) => { /* coalesce: skip */ }
                    Some(
                        WorkerState::Idle
                        | WorkerState::WaitingToRetry
                        | WorkerState::Succeeded
                        | WorkerState::Failed { .. },
                    ) => {
                        // Atomically transition to Running before releasing the lock.
                        // The sleeping retry task checks state while holding the lock
                        // and will see Running, preventing a duplicate run.
                        state.providers.insert(id, WorkerState::Running);
                        to_start.push(id);
                    }
                    None => { /* provider was deleted */ }
                }
            }
            to_start
        };

        for id in to_start {
            let coord = self.clone();
            tokio::spawn(async move { coord.run_provider_sync(id).await });
        }
    }

    // -----------------------------------------------------------------------
    // Provider sync execution
    // -----------------------------------------------------------------------

    async fn run_provider_sync(&self, provider_id: i64) {
        // Acquire the semaphore before making any HTTP calls.
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .expect("semaphore should never close");

        match self.do_provider_sync(provider_id).await {
            SyncOutcome::Success => {
                self.set_state(provider_id, WorkerState::Succeeded).await;
                self.emit(ProviderSyncComplete {
                    provider_id,
                    status: "succeeded",
                    reason: None,
                    timestamp: unix_now(),
                });
                info!(provider_id, "provider sync succeeded");
            }

            SyncOutcome::ProviderGone => {
                // Provider was deleted while in flight. Remove the worker state
                // entry if it still exists (may already be gone).
                self.inner.state.lock().await.providers.remove(&provider_id);
            }

            SyncOutcome::PermanentFailure { reason } => {
                self.set_state(provider_id, WorkerState::Failed { reason: reason.clone() }).await;
                self.emit(ProviderSyncComplete {
                    provider_id,
                    status: "failed",
                    reason: Some(reason.clone()),
                    timestamp: unix_now(),
                });
                error!(provider_id, %reason, "provider sync failed permanently");
            }

            SyncOutcome::TransientFailure { retry_after_secs, reason } => {
                let wait_secs = retry_after_secs.max(30);
                warn!(provider_id, wait_secs, %reason, "transient failure; will retry once");

                // Release semaphore during the retry wait per the spec.
                drop(permit);

                let until = Instant::now() + Duration::from_secs(wait_secs);
                {
                    let mut state = self.inner.state.lock().await;
                    if state.providers.contains_key(&provider_id) {
                        state.providers.insert(provider_id, WorkerState::WaitingToRetry);
                    } else {
                        return; // provider deleted
                    }
                }

                tokio::time::sleep_until(until).await;

                // Atomically check that state is still WaitingToRetry and
                // transition to Running. If a manual refresh already changed
                // the state (and spawned a new task), bail out here.
                {
                    let mut state = self.inner.state.lock().await;
                    match state.providers.get(&provider_id) {
                        Some(WorkerState::WaitingToRetry) => {
                            state.providers.insert(provider_id, WorkerState::Running);
                        }
                        _ => return, // manual refresh or provider gone
                    }
                }

                // Re-acquire semaphore for the one-shot retry.
                let _retry_permit = Arc::clone(&self.inner.semaphore)
                    .acquire_owned()
                    .await
                    .expect("semaphore should never close");

                match self.do_provider_sync(provider_id).await {
                    SyncOutcome::Success | SyncOutcome::ProviderGone => {
                        if matches!(
                            self.inner.state.lock().await.providers.get(&provider_id),
                            Some(WorkerState::Running)
                        ) {
                            self.set_state(provider_id, WorkerState::Succeeded).await;
                        }
                        self.emit(ProviderSyncComplete {
                            provider_id,
                            status: "succeeded",
                            reason: None,
                            timestamp: unix_now(),
                        });
                    }
                    SyncOutcome::TransientFailure { reason, .. }
                    | SyncOutcome::PermanentFailure { reason } => {
                        let r = format!("transient_exhausted: {reason}");
                        self.set_state(
                            provider_id,
                            WorkerState::Failed { reason: r.clone() },
                        )
                        .await;
                        self.emit(ProviderSyncComplete {
                            provider_id,
                            status: "failed",
                            reason: Some(r.clone()),
                            timestamp: unix_now(),
                        });
                        warn!(provider_id, %r, "retry also failed; provider in failed state");
                    }
                }
            }
        }
    }

    /// Performs the actual HTTP calls + DB write for one provider.
    async fn do_provider_sync(&self, provider_id: i64) -> SyncOutcome {
        let db = &self.inner.db;
        let secrets = &self.inner.secrets;

        // -- Read provider row from DB ----------------------------------------
        // block_in_place: the std::sync::Mutex inside with_conn is a blocking
        // call. Wrapping it in block_in_place evacuates other futures off this
        // worker thread first, keeping the tokio runtime responsive for IPC.
        let provider_row = match tokio::task::block_in_place(|| db.with_conn(move |c| crud::get_provider(c, provider_id))) {
            Ok(Some(p)) => p,
            Ok(None) => return SyncOutcome::ProviderGone,
            Err(e) => return SyncOutcome::PermanentFailure { reason: format!("db: {e}") },
        };

        // -- Build the provider client ----------------------------------------
        let provider = match build_provider(
            &provider_row.provider_type,
            secrets,
            provider_row.team_id.as_deref(),
        ) {
            Ok(p) => p,
            Err(e) => {
                return SyncOutcome::PermanentFailure {
                    reason: format!("build_provider: {e}"),
                }
            }
        };

        // -- Determine time range --------------------------------------------
        let now_ts = unix_now();
        let range = TimeRange {
            start: match provider_row.last_sync_succeeded_at {
                Some(ts) => ts,
                None => now_ts - 30 * 24 * 3600, // 30-day backfill for first sync
            },
            end: now_ts,
        };

        // -- Fetch usage records ---------------------------------------------
        let mut records = match provider.fetch_usage(range).await {
            Ok(r) => r,
            Err(e) => return classify_provider_error(e),
        };

        // Stamp the sync-engine fields before writing.
        for r in &mut records {
            r.provider_id = provider_id;
            r.fetched_at = now_ts;
        }

        // -- Fetch balance ---------------------------------------------------
        let latest_balance = match provider.fetch_balance().await {
            Ok(Some(b)) => Some(b),
            Ok(None) => None,
            Err(e) if e.is_transient() => return classify_provider_error(e),
            Err(e) => {
                // Non-transient balance failure: log but don't fail the whole
                // sync — usage data is more important.
                warn!(provider_id, %e, "balance fetch failed");
                None
            }
        };

        // -- Write everything in a single transaction -----------------------
        let write_result = tokio::task::block_in_place(|| db.with_transaction(move |tx| {
            crud::upsert_usage_records(tx, &records)?;
            if let Some(bal) = &latest_balance {
                crud::insert_balance(tx, provider_id, bal, now_ts)?;
            }
            crud::update_provider_sync_status(tx, provider_id, now_ts, Some(now_ts), "ok")?;
            Ok(())
        }));

        if let Err(e) = write_result {
            return SyncOutcome::PermanentFailure { reason: format!("db write: {e}") };
        }

        // -- Prune old records (outside the transaction) --------------------
        // PRAGMA page_count / page_size cannot be used within an active
        // SQLCipher write transaction, so pruning runs on the connection
        // directly after the commit. Failure is non-fatal.
        let max_days = self.inner.cfg.retention.max_days;
        let max_size_mb = self.inner.cfg.retention.max_size_mb;
        if let Err(e) = tokio::task::block_in_place(|| {
            db.with_conn(|c| crud::prune_old_records(c, max_days, max_size_mb))
        }) {
            warn!(provider_id, %e, "prune_old_records failed (non-fatal)");
        }

        SyncOutcome::Success
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    async fn set_state(&self, provider_id: i64, ws: WorkerState) {
        let mut state = self.inner.state.lock().await;
        if state.providers.contains_key(&provider_id) {
            state.providers.insert(provider_id, ws);
        }
    }

    fn emit(&self, event: ProviderSyncComplete) {
        // send() fails only if there are no receivers — that is safe to ignore.
        let _ = self.inner.events.send(event);
    }

}

// ---------------------------------------------------------------------------
// Status computation
// ---------------------------------------------------------------------------

fn compute_status(state: &CoordState) -> SyncStatus {
    let mut any_running = false;
    let mut any_failed = false;
    let mut any_succeeded = false;

    for ws in state.providers.values() {
        match ws {
            WorkerState::Running | WorkerState::WaitingToRetry => any_running = true,
            WorkerState::Failed { .. } => any_failed = true,
            WorkerState::Succeeded => any_succeeded = true,
            WorkerState::Idle => {}
        }
    }

    let indicator = if state.paused {
        "grey"
    } else if any_running {
        "spinner"
    } else if any_failed {
        "amber"
    } else if any_succeeded {
        "green"
    } else {
        "grey" // no providers, or all Idle (startup before first run)
    };

    let providers = state
        .providers
        .iter()
        .map(|(&id, ws)| {
            (
                id,
                ProviderSyncStateDto {
                    state: ws.as_str(),
                    reason: if let WorkerState::Failed { reason } = ws {
                        Some(reason.clone())
                    } else {
                        None
                    },
                },
            )
        })
        .collect();

    SyncStatus {
        paused: state.paused,
        last_tick_at: state.last_tick_at,
        indicator,
        providers,
    }
}

// ---------------------------------------------------------------------------
// Outcome types + helpers
// ---------------------------------------------------------------------------

enum SyncOutcome {
    Success,
    TransientFailure { retry_after_secs: u64, reason: String },
    PermanentFailure { reason: String },
    ProviderGone,
}

fn classify_provider_error(e: ProviderError) -> SyncOutcome {
    if e.is_transient() {
        let secs = e.retry_after_seconds().unwrap_or(30);
        SyncOutcome::TransientFailure {
            retry_after_secs: secs,
            reason: format!("{} ({e})", classify_reason(&e)),
        }
    } else {
        SyncOutcome::PermanentFailure {
            reason: format!("{} ({e})", classify_reason(&e)),
        }
    }
}

fn classify_reason(e: &ProviderError) -> String {
    match e {
        ProviderError::AuthInvalid | ProviderError::AuthInsufficientPermission { .. } => {
            "auth".to_string()
        }
        ProviderError::BillingIssue => "billing".to_string(),
        ProviderError::Network(_) | ProviderError::RequestTimeout => "network".to_string(),
        ProviderError::RateLimited { .. }
        | ProviderError::ServerError { .. } => "transient".to_string(),
        _ => "unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, LogConfig, RetentionConfig, SyncConfig};
    use crate::db::Database;
    use crate::secrets::SecretStore;

    fn test_cfg() -> AppConfig {
        AppConfig {
            sync: SyncConfig {
                interval_seconds: 3600,
                max_concurrent_profiles: 4,
                paused: false,
            },
            retention: RetentionConfig { max_days: 90, max_size_mb: 1024 },
            log: LogConfig { level: "info".to_string() },
            ui: crate::config::UiConfig::default(),
        }
    }

    fn make_coordinator() -> (SyncCoordinator, broadcast::Receiver<ProviderSyncComplete>) {
        let db = Database::open_in_memory().unwrap();
        let secrets = SecretStore::new();
        SyncCoordinator::new(db, secrets, test_cfg())
    }

    #[tokio::test]
    async fn initial_status_is_grey_no_providers() {
        let (coord, _) = make_coordinator();
        let status = coord.get_status().await;
        assert_eq!(status.indicator, "grey");
        assert!(!status.paused);
        assert!(status.providers.is_empty());
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let (coord, _) = make_coordinator();
        coord.pause().await;
        assert!(coord.get_status().await.paused);
        assert_eq!(coord.get_status().await.indicator, "grey");

        coord.resume().await;
        assert!(!coord.get_status().await.paused);
    }

    #[tokio::test]
    async fn trigger_all_rejects_when_paused() {
        let (coord, _) = make_coordinator();
        coord.pause().await;
        let result = coord.trigger_all().await;
        assert!(matches!(result, Err(SyncTriggerError::Paused)));
    }

    #[tokio::test]
    async fn trigger_for_providers_rejects_unknown_id() {
        let (coord, _) = make_coordinator();
        let result = coord.trigger_for_providers(vec![999]).await;
        assert!(matches!(result, Err(SyncTriggerError::UnknownProvider(999))));
    }

    #[tokio::test]
    async fn on_provider_added_and_deleted() {
        let (coord, _) = make_coordinator();
        coord.on_provider_added(42).await;
        {
            let state = coord.inner.state.lock().await;
            assert!(state.providers.contains_key(&42));
        }

        coord.on_provider_deleted(42).await;
        {
            let state = coord.inner.state.lock().await;
            assert!(!state.providers.contains_key(&42));
        }
    }

    #[tokio::test]
    async fn dispatch_skips_running_providers() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Running);
            state.providers.insert(2, WorkerState::Idle);
        }
        // dispatch_providers transitions eligible workers to Running *before*
        // returning (atomically, inside the lock). Check state immediately
        // without yielding to avoid the spawned task modifying state.
        coord.dispatch_providers(vec![1, 2]).await;
        let state = coord.inner.state.lock().await;
        // Provider 1 was already Running → coalesced (still Running).
        assert_eq!(state.providers[&1].as_str(), "running");
        // Provider 2 was Idle → atomically transitioned to Running.
        assert_eq!(state.providers[&2].as_str(), "running");
    }

    #[tokio::test]
    async fn indicator_green_when_all_succeeded() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Succeeded);
            state.providers.insert(2, WorkerState::Succeeded);
        }
        assert_eq!(coord.get_status().await.indicator, "green");
    }

    #[tokio::test]
    async fn indicator_amber_when_any_failed() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Succeeded);
            state.providers.insert(2, WorkerState::Failed { reason: "auth".to_string() });
        }
        assert_eq!(coord.get_status().await.indicator, "amber");
    }

    #[tokio::test]
    async fn indicator_spinner_overrides_amber() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Running);
            state.providers.insert(2, WorkerState::Failed { reason: "auth".to_string() });
        }
        assert_eq!(coord.get_status().await.indicator, "spinner");
    }

    #[tokio::test]
    async fn indicator_grey_when_paused() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Succeeded);
            state.paused = true;
        }
        assert_eq!(coord.get_status().await.indicator, "grey");
    }

    // -- State machine transition helpers ------------------------------------

    /// Simulates idle → running by inserting Idle and calling dispatch_providers.
    #[tokio::test]
    async fn idle_transitions_to_running_on_dispatch() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(10, WorkerState::Idle);
        }
        coord.dispatch_providers(vec![10]).await;
        let state = coord.inner.state.lock().await;
        assert_eq!(state.providers[&10].as_str(), "running");
    }

    /// Simulates the success terminal state by directly setting Succeeded.
    #[tokio::test]
    async fn succeeded_state_produces_green_indicator() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Succeeded);
        }
        let status = coord.get_status().await;
        assert_eq!(status.indicator, "green");
        assert_eq!(status.providers[&1].state, "succeeded");
    }

    /// Simulates the failure terminal state by directly setting Failed.
    #[tokio::test]
    async fn failed_state_produces_amber_indicator_with_reason() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::Failed { reason: "auth".to_string() });
        }
        let status = coord.get_status().await;
        assert_eq!(status.indicator, "amber");
        assert_eq!(status.providers[&1].state, "failed");
        assert_eq!(status.providers[&1].reason.as_deref(), Some("auth"));
    }

    /// WaitingToRetry also contributes the spinner indicator.
    #[tokio::test]
    async fn waiting_to_retry_shows_spinner() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::WaitingToRetry);
        }
        assert_eq!(coord.get_status().await.indicator, "spinner");
    }

    /// Dispatch transitions WaitingToRetry → Running (manual refresh beats the sleep).
    #[tokio::test]
    async fn waiting_to_retry_transitions_to_running_on_dispatch() {
        let (coord, _) = make_coordinator();
        {
            let mut state = coord.inner.state.lock().await;
            state.providers.insert(1, WorkerState::WaitingToRetry);
        }
        coord.dispatch_providers(vec![1]).await;
        let state = coord.inner.state.lock().await;
        assert_eq!(state.providers[&1].as_str(), "running");
    }

    /// classify_reason maps each ProviderError variant to the expected string.
    #[test]
    fn classify_reason_maps_errors_correctly() {
        assert_eq!(classify_reason(&ProviderError::AuthInvalid), "auth");
        assert_eq!(
            classify_reason(&ProviderError::AuthInsufficientPermission {
                detail: "x".into()
            }),
            "auth"
        );
        assert_eq!(classify_reason(&ProviderError::BillingIssue), "billing");
        assert_eq!(classify_reason(&ProviderError::Network("x".into())), "network");
        assert_eq!(classify_reason(&ProviderError::RequestTimeout), "network");
        assert_eq!(
            classify_reason(&ProviderError::RateLimited { retry_after_seconds: None }),
            "transient"
        );
        assert_eq!(
            classify_reason(&ProviderError::ServerError { status: 503 }),
            "transient"
        );
        assert_eq!(
            classify_reason(&ProviderError::MalformedResponse("x".into())),
            "unknown"
        );
    }
}
