//! The call's pop out window.
//!
//! A small always-on-top window of ours (`desktop/call-popout.html` in the web app) that mirrors
//! the call's video and has mute, "Return to call" and hang up buttons. The browser's own
//! Document Picture-in-Picture cannot do this: WebView2 offers the API but has no window to put
//! it in ("Internal error: no window"). The call itself stays in the main window; the two pages
//! share frames and button presses over a `BroadcastChannel` (same origin), so the only things
//! done natively are opening, closing and noticing that the user closed it.

use crate::{commands, MAIN_WINDOW};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Label of the pop out window. Also on the window-state denylist (see `lib.rs`).
pub const POPOUT_WINDOW: &str = "call-popout";

/// Where the web app serves the page (`public/desktop/call-popout.html`).
const POPOUT_PAGE: &str = "desktop/call-popout.html";

/// The event the main window hears when the pop out window is gone, however it went.
const CLOSED_EVENT: &str = "call-popout-closed";

/// Size in logical pixels.
const POPOUT_WIDTH: f64 = 480.0;
const POPOUT_HEIGHT: f64 = 320.0;

/// The page address with the (already translated) labels in the query string.
fn popout_path(labels: &[(&str, &str)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in labels {
        query.append_pair(key, value);
    }
    format!("{POPOUT_PAGE}?{}", query.finish())
}

/// Close the window, if it is open.
pub fn close_popout(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPOUT_WINDOW) {
        let _ = window.destroy();
    }
}

/// Open the window with these labels, or bring it forward when it is already open.
#[tauri::command]
pub async fn show_call_popout(
    app: AppHandle,
    title: String,
    mute: String,
    unmute: String,
    back: String,
    hangup: String,
    no_video: String,
) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(POPOUT_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let path = popout_path(&[
        ("title", &title),
        ("mute", &mute),
        ("unmute", &unmute),
        ("back", &back),
        ("hangup", &hangup),
        ("novideo", &no_video),
    ]);
    let window = WebviewWindowBuilder::new(&app, POPOUT_WINDOW, WebviewUrl::App(path.into()))
        .title(title)
        .inner_size(POPOUT_WIDTH, POPOUT_HEIGHT)
        .min_inner_size(240.0, 180.0)
        .always_on_top(true)
        .background_color(tauri::window::Color(0, 0, 0, 255))
        .disable_drag_drop_handler()
        .build()
        .map_err(|e| e.to_string())?;
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Destroyed = event {
            let _ = handle.emit_to(MAIN_WINDOW, CLOSED_EVENT, ());
        }
    });
    Ok(())
}

/// Close the window (the call ended, or the button was pressed again).
#[tauri::command]
pub fn hide_call_popout(app: AppHandle) {
    close_popout(&app);
}

/// "Return to call" in the window: bring the main window back (it may be minimised or in the
/// tray). The page itself asks the web app to go to the call's room.
#[tauri::command]
pub fn call_popout_focus_main(app: AppHandle) {
    commands::show_main_window(&app);
}
