#![forbid(unsafe_code)]

mod commands;
mod error;
mod state;

use modelmeter_core::{
    config::load_config,
    db::Database,
    logging::init_tracing,
    secrets::SecretStore,
    sync::SyncCoordinator,
};

use commands::{
    claude_code::{get_cached_claude_code_result, get_claude_code_plan_usage},
    elevenlabs::get_elevenlabs_state,
    providers::{
        add_provider, list_provider_kinds, list_providers, remove_provider, validate_provider_key,
    },
    queries::{get_latest_balance, get_usage_summary},
    settings::{get_config, set_config},
    sync::{get_sync_status, trigger_sync_all},
    xai::get_xai_monthly_history,
};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let cfg = load_config().unwrap_or_default();
    let _ = init_tracing(cfg.log.level.as_str());

    let db = Database::open().expect("failed to open database");
    let secrets = SecretStore::new();

    let (sync, mut events_rx) =
        SyncCoordinator::new(db.clone(), secrets.clone(), cfg.clone());

    tauri::Builder::default()
        .manage(AppState { db, secrets, sync: sync.clone() })
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // ── Tray icon + menu ────────────────────────────────────────────
            let open_item =
                MenuItem::with_id(app, "open", "Open ModelMeter", true, None::<&str>)?;
            let sync_item =
                MenuItem::with_id(app, "sync_now", "Sync now", true, None::<&str>)?;
            let pause_item =
                CheckMenuItem::with_id(app, "pause", "Pause syncing", true, false, None::<&str>)?;
            let settings_item =
                MenuItem::with_id(app, "settings", "Settings\u{2026}", true, None::<&str>)?;
            let quit_item =
                MenuItem::with_id(app, "quit", "Quit ModelMeter", true, None::<&str>)?;

            let menu = Menu::with_items(app, &[
                &open_item,
                &PredefinedMenuItem::separator(app)?,
                &sync_item,
                &PredefinedMenuItem::separator(app)?,
                &pause_item,
                &PredefinedMenuItem::separator(app)?,
                &settings_item,
                &quit_item,
            ])?;

            // Clone before move into on_menu_event closure.
            let pause_for_menu = pause_item.clone();
            let app_for_tray = app_handle.clone();

            let tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |ah, event| match event.id.as_ref() {
                    "open" | "settings" => {
                        if let Some(w) = ah.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "sync_now" => {
                        let ah = ah.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = ah.state::<AppState>();
                            let _ = state.sync.trigger_all().await;
                        });
                    }
                    "pause" => {
                        let item = pause_for_menu.clone();
                        let ah = ah.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = ah.state::<AppState>();
                            if item.is_checked().unwrap_or(false) {
                                state.sync.pause().await;
                            } else {
                                state.sync.resume().await;
                            }
                        });
                    }
                    "quit" => ah.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(move |_tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = app_for_tray.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Tray must outlive setup; leak deliberately — it lives until process exit.
            std::mem::forget(tray);

            // ── Hide-to-tray on window close ────────────────────────────────
            if let Some(main_window) = app.get_webview_window("main") {
                let w = main_window.clone();
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            // ── Sync coordinator startup ────────────────────────────────────
            tauri::async_runtime::spawn(async move {
                if let Err(e) = sync.start().await {
                    tracing::error!("sync coordinator failed to start: {e}");
                }
            });

            // ── Forward ProviderSyncComplete events to frontend ─────────────
            tauri::async_runtime::spawn(async move {
                loop {
                    match events_rx.recv().await {
                        Ok(evt) => {
                            if let Err(e) = app_handle.emit("provider-sync-complete", &evt) {
                                tracing::warn!("failed to emit provider-sync-complete: {e}");
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                "sync events lagged by {n}; some frontend updates dropped"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Provider CRUD
            list_providers,
            add_provider,
            remove_provider,
            // Key validation
            validate_provider_key,
            // Provider catalog
            list_provider_kinds,
            // Widget queries
            get_latest_balance,
            get_usage_summary,
            // Settings / config
            get_config,
            set_config,
            // Sync engine
            trigger_sync_all,
            get_sync_status,
            // Claude Code plan usage
            get_claude_code_plan_usage,
            get_cached_claude_code_result,
            // x.ai monthly invoice history
            get_xai_monthly_history,
            // ElevenLabs subscription + daily credits
            get_elevenlabs_state,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
