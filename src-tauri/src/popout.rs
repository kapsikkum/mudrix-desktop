//! The call's window of its own.
//!
//! The web app asks for it with `window.open("/desktop/call-window.html", ...)`, and this module
//! is what answers: the main window's new-window handler (see `lib.rs`) creates the window,
//! linked to the main one, so the web app gets a `Window` it can render the call into, while the
//! call itself (the call engine, the Matrix client, the media) stays in the main window. The
//! browser's own Document Picture-in-Picture cannot do this: WebView2 offers the API but has no
//! window to put it in ("Internal error: no window"). Any other `window.open` is refused, as
//! before.

use crate::MAIN_WINDOW;
use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};

/// Label of the call window. Also on the window-state denylist (see `lib.rs`).
pub const POPOUT_WINDOW: &str = "call-popout";

/// The page the web app opens (`public/desktop/call-window.html`); anything else is refused.
const POPOUT_PATH: &str = "/desktop/call-window.html";

/// The event the main window hears when the call window is gone, however it went.
const CLOSED_EVENT: &str = "call-popout-closed";

/// Close the window, if it is open.
pub fn close_popout(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPOUT_WINDOW) {
        let _ = window.destroy();
    }
}

/// The main window's answer to `window.open`.
pub fn on_new_window(
    app: &AppHandle,
    url: Url,
    features: NewWindowFeatures,
) -> NewWindowResponse<Wry> {
    if url.path() != POPOUT_PATH || app.get_webview_window(POPOUT_WINDOW).is_some() {
        return NewWindowResponse::Deny;
    }
    let built = WebviewWindowBuilder::new(
        app,
        POPOUT_WINDOW,
        WebviewUrl::External("about:blank".parse().expect("a valid url")),
    )
    .window_features(features)
    .title("Mudrix")
    .min_inner_size(360.0, 240.0)
    .background_color(tauri::window::Color(26, 27, 30, 255))
    .disable_drag_drop_handler()
    .build();
    match built {
        Ok(window) => {
            let handle = app.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Destroyed = event {
                    let _ = handle.emit_to(MAIN_WINDOW, CLOSED_EVENT, ());
                }
            });
            NewWindowResponse::Create { window }
        }
        Err(e) => {
            eprintln!("[desktop] could not open the call window: {e}");
            NewWindowResponse::Deny
        }
    }
}

/// Close the window (the call ended, or the call was brought back). Also clears one left behind
/// by a reload of the web app, which has lost its link to it.
#[tauri::command]
pub fn hide_call_popout(app: AppHandle) {
    close_popout(&app);
}
