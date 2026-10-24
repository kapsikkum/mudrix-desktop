//! Mudrix Desktop: a Tauri v2 shell around the Mudrix web client.
//!
//! Layout: `settings` (persisted shell settings), `tray` (tray icon, menu, unread badge),
//! `auth` (SSO/OIDC through the system browser), `commands` (what the web app can call),
//! `badge` (Windows taskbar overlay drawing), `updater` (tauri-plugin-updater, signed updates),
//! `screenshare` (our screen share bar in place of the one WebView2 draws), `toast` (Windows
//! toasts under our own app id, so they say "Mudrix"), `capture` (native screen and window
//! capture for screen sharing, in place of WebView2's picker).

mod accent;
mod auth;
mod badge;
mod capture;
mod commands;
mod permissions;
mod screenshare;
mod settings;
mod toast;
mod tray;
mod updater;

use std::sync::Mutex;
use tauri::{Manager, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_window_state::WindowExt;

pub const MAIN_WINDOW: &str = "main";

/// Launched by the OS at login (the autostart entry passes this flag).
const AUTOSTART_FLAG: &str = "--autostart";

pub fn run() {
    let builder = tauri::Builder::default()
        // Must be the first plugin. A second launch focuses the running window; with the
        // `deep-link` feature it also forwards any `matrix:` URL to the deep-link plugin.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            commands::show_main_window(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(commands::window_state_flags())
                .with_denylist(&[screenshare::BAR_WINDOW])
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ));

    let app = builder
        .setup(|app| {
            let handle = app.handle().clone();
            let loaded = settings::load(&handle);
            let start_hidden =
                loaded.start_minimized && std::env::args().any(|a| a == AUTOSTART_FLAG);
            app.manage(settings::SettingsState(Mutex::new(loaded)));
            app.manage(commands::NavState::default());
            app.manage(commands::LastNotification::default());

            #[cfg(windows)]
            toast::init(&handle);

            tray::build(&handle)?;

            // Installers register the `matrix:` scheme; do it at run time where they cannot
            // (Linux AppImage, and `tauri dev` on Windows).
            #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
            {
                if let Err(e) = app.deep_link().register_all() {
                    eprintln!("[desktop] could not register the matrix: scheme: {e}");
                }
            }
            let deep_link_handle = handle.clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    commands::open_deep_link(&deep_link_handle, url.as_str());
                }
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    commands::open_deep_link(&handle, url.as_str());
                }
            }

            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                permissions::allow_call_media(&window);
                // The window starts hidden (no white flash); place it, then show it.
                let _ = window.restore_state(commands::window_state_flags());
                if !start_hidden {
                    commands::show_main_window(&handle);
                }
            }

            updater::spawn(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != MAIN_WINDOW {
                return;
            }
            let app = window.app_handle();
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    if settings::get(app).keep_running_in_background {
                        api.prevent_close();
                        commands::hide_main_window(app);
                    }
                }
                WindowEvent::Focused(true) => commands::on_focus(app),
                WindowEvent::Destroyed => {
                    screenshare::close_bar(app);
                    capture::stop_all(app);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_desktop_settings,
            commands::set_desktop_settings,
            commands::set_unread_count,
            commands::set_app_icon,
            commands::show_notification,
            commands::open_external,
            commands::save_download,
            accent::get_system_accent,
            commands::open_auth_in_browser,
            commands::take_pending_navigation,
            commands::is_window_visible,
            commands::install_update,
            commands::check_for_updates,
            screenshare::show_screen_share_bar,
            screenshare::hide_screen_share_bar,
            screenshare::screen_share_bar_stop,
            screenshare::screen_share_bar_return,
            capture::capture_list_sources,
            capture::capture_start,
            capture::capture_stop,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Mudrix desktop app");

    app.run(|_app, _event| {
        if let tauri::RunEvent::Exit = _event {
            capture::stop_all(_app);
        }
        // macOS: clicking the dock icon with the window hidden brings it back.
        #[cfg(target_os = "macos")]
        {
            if let tauri::RunEvent::Reopen { .. } = _event {
                commands::show_main_window(_app);
            }
        }
    });
}
