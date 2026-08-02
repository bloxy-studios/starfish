//! Starfish desktop shell — a thin Tauri layer over `starfish-core`.
//!
//! Responsibilities: own the shared [`AppState`], expose Tauri commands
//! (`commands.rs`), forward the gateway's live request log to the webview,
//! and provide desktop niceties (tray, single instance, launch-at-login,
//! close-to-tray).

mod commands;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};
use tokio::sync::{Mutex, RwLock};

use starfish_core::gateway::{GatewayState, ServerHandle};
use starfish_core::logbuf::LogBuffer;
use starfish_core::upstream::{HyperagentUpstream, MockUpstream, Upstream};
use starfish_core::vault::Vault;

/// Shared state managed by Tauri.
pub struct AppState {
    pub gateway: Arc<GatewayState>,
    pub vault: Arc<dyn Vault>,
    pub http: starfish_core::reqwest::Client,
    pub server: Mutex<Option<ServerHandle>>,
    /// True when running against the offline mock upstream
    /// (`STARFISH_MOCK_UPSTREAM=1`).
    pub mock: bool,
}

impl AppState {
    fn build() -> Self {
        let config = starfish_core::config::load().unwrap_or_else(|e| {
            tracing::error!("failed to load config ({e}); starting with defaults");
            Default::default()
        });
        let config = Arc::new(RwLock::new(config));
        let vault: Arc<dyn Vault> = Arc::from(
            starfish_core::vault::open_default_vault().expect("no vault backend available"),
        );
        let http = starfish_core::default_http_client();
        let log = Arc::new(LogBuffer::new());

        let mock = std::env::var("STARFISH_MOCK_UPSTREAM")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let upstream: Arc<dyn Upstream> = if mock {
            tracing::warn!("STARFISH_MOCK_UPSTREAM set — using the offline mock upstream");
            Arc::new(MockUpstream::new())
        } else {
            Arc::new(HyperagentUpstream::new(
                http.clone(),
                config.clone(),
                vault.clone(),
            ))
        };

        let gateway = Arc::new(GatewayState::new(config, upstream, log));
        Self {
            gateway,
            vault,
            http,
            server: Mutex::new(None),
            mock,
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        // Must be first: focus the existing window instead of a second app.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(AppState::build())
        .invoke_handler(tauri::generate_handler![
            commands::app_snapshot,
            commands::server_start,
            commands::server_stop,
            commands::server_status,
            commands::set_server_config,
            commands::set_settings,
            commands::set_onboarded,
            commands::begin_sign_in,
            commands::remove_account,
            commands::reauth_account,
            commands::set_account_nickname,
            commands::set_account_default_agent,
            commands::doctor,
            commands::list_agents,
            commands::create_key,
            commands::reveal_key,
            commands::revoke_key,
            commands::rotate_key,
            commands::rename_key,
            commands::set_key_agent,
            commands::set_mappings,
            commands::logs_recent,
            commands::clear_logs,
            commands::open_external,
            commands::set_launch_at_login,
            commands::get_launch_at_login,
        ])
        .setup(|app| {
            // ---- live request log → webview events -------------------------
            let state = app.state::<AppState>();
            let log = state.gateway.log.clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut rx = log.subscribe();
                loop {
                    match rx.recv().await {
                        Ok(entry) => {
                            let _ = handle.emit("gateway://log", &entry);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // ---- optionally start the gateway on launch --------------------
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<AppState>();
                    let autostart = {
                        let cfg = state.gateway.config.read().await;
                        cfg.settings.autostart_server
                    };
                    if autostart {
                        match commands::start_server_inner(&state).await {
                            Ok(status) => {
                                let _ = handle.emit("server://status", &status);
                            }
                            Err(e) => tracing::error!("autostart failed: {e}"),
                        }
                    }
                });
            }

            // ---- system tray ------------------------------------------------
            let show = MenuItem::with_id(app, "show", "Show Starfish", true, None::<&str>)?;
            let start = MenuItem::with_id(app, "start", "Start gateway", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop gateway", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Starfish", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &start, &stop, &quit])?;
            let mut tray = TrayIconBuilder::with_id("starfish-tray")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("Starfish — Hyperagent gateway")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    "start" | "stop" => {
                        let starting = event.id.as_ref() == "start";
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = handle.state::<AppState>();
                            let result = if starting {
                                commands::start_server_inner(&state).await
                            } else {
                                commands::stop_server_inner(&state).await
                            };
                            match result {
                                Ok(status) => {
                                    let _ = handle.emit("server://status", &status);
                                }
                                Err(e) => tracing::error!("tray server toggle failed: {e}"),
                            }
                        });
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            Ok(())
        })
        // Close-to-tray: keep serving when the window is dismissed.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
