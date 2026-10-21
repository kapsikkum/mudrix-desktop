//! Commands the web app invokes (see `src/util/desktop.ts` in mudrix-web) and the window
//! helpers shared with the tray and the window event handlers.

use crate::settings::{self, DesktopSettings, SettingsPatch};
use crate::{auth, tray, MAIN_WINDOW};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tauri_plugin_window_state::{AppHandleExt as _, StateFlags};

/// A notification stays "current" for this long: focusing the window within it opens its room.
const NOTIFICATION_CLICK_WINDOW: Duration = Duration::from_secs(20);

/// What gets remembered (and restored) of the window.
pub fn window_state_flags() -> StateFlags {
    StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED | StateFlags::FULLSCREEN
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Navigation requests (deep links, notification clicks) that arrive before the web app has
/// started listening are parked here until it calls `take_pending_navigation`.
#[derive(Default)]
pub struct NavState {
    web_ready: AtomicBool,
    pending: Mutex<Option<String>>,
}

/// The room of the most recent notification.
#[derive(Default)]
pub struct LastNotification(Mutex<Option<(String, Instant)>>);

// ---------------------------------------------------------------------------
// Window helpers
// ---------------------------------------------------------------------------

fn emit_visibility(app: &AppHandle, visible: bool) {
    let _ = app.emit("desktop://visibility", visible);
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        emit_visibility(app, true);
    }
}

/// Hide to the tray, remembering the window geometry first.
pub fn hide_main_window(app: &AppHandle) {
    let _ = app.save_window_state(window_state_flags());
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.hide();
        emit_visibility(app, false);
    }
}

pub fn toggle_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let showing = window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false);
    if showing {
        hide_main_window(app);
    } else {
        show_main_window(app);
    }
}

/// Window gained focus: a notification clicked a moment ago opens its room.
pub fn on_focus(app: &AppHandle) {
    let target = app
        .state::<LastNotification>()
        .0
        .lock()
        .unwrap()
        .take()
        .filter(|(_, at)| at.elapsed() < NOTIFICATION_CLICK_WINDOW);
    if let Some((room_url, _)) = target {
        navigate(app, room_url);
    }
}

// ---------------------------------------------------------------------------
// Navigation, deep links, auth
// ---------------------------------------------------------------------------

/// Ask the web app to open an in-app path (`/r/...`) or a `matrix:` URI.
pub fn navigate(app: &AppHandle, target: String) {
    let state = app.state::<NavState>();
    if state.web_ready.load(Ordering::SeqCst) {
        let _ = app.emit("desktop://open", target);
    } else {
        *state.pending.lock().unwrap() = Some(target);
    }
}

/// A `matrix:` URL arrived (deep link, or a second launch forwarded by single-instance).
pub fn open_deep_link(app: &AppHandle, url: &str) {
    if !url.to_ascii_lowercase().starts_with("matrix:") {
        return;
    }
    show_main_window(app);
    navigate(app, url.to_string());
}

/// Called by the loopback listener with the in-app callback path.
pub fn deliver_auth_callback(app: &AppHandle, path: String) {
    show_main_window(app);
    let _ = app.emit("desktop://auth-callback", path);
}

pub fn open_url(app: &AppHandle, url: &str) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Persist a settings patch and apply its side effects (autostart entry, tray menu), then
/// tell the web app.
pub fn apply_settings(app: &AppHandle, patch: &SettingsPatch) -> DesktopSettings {
    if let Some(enable) = patch.launch_at_login {
        let autolaunch = app.autolaunch();
        let result = if enable {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        if let Err(e) = result {
            eprintln!("[desktop] could not change the autostart entry: {e}");
        }
    }
    let mut updated = settings::update(app, patch);
    if let Ok(actual) = app.autolaunch().is_enabled() {
        if actual != updated.launch_at_login {
            updated = settings::update(
                app,
                &SettingsPatch {
                    launch_at_login: Some(actual),
                    ..Default::default()
                },
            );
        }
    }
    tray::sync_mute(app, updated.notifications_muted);
    let _ = app.emit("desktop://settings-changed", updated.clone());
    updated
}

#[tauri::command]
pub fn get_desktop_settings(app: AppHandle) -> DesktopSettings {
    // The user may have removed the autostart entry in the OS settings.
    let mut current = settings::get(&app);
    if let Ok(actual) = app.autolaunch().is_enabled() {
        if actual != current.launch_at_login {
            current = settings::update(
                &app,
                &SettingsPatch {
                    launch_at_login: Some(actual),
                    ..Default::default()
                },
            );
        }
    }
    current
}

#[tauri::command]
pub fn set_desktop_settings(app: AppHandle, patch: SettingsPatch) -> DesktopSettings {
    apply_settings(&app, &patch)
}

// ---------------------------------------------------------------------------
// Web app commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn set_app_icon(app: AppHandle, png: Option<Vec<u8>>) {
    tray::set_themed_icon(&app, png);
}

#[tauri::command]
pub fn set_unread_count(app: AppHandle, total: u32, highlight: u32) {
    tray::set_unread(&app, total, highlight);
}

#[tauri::command]
pub fn show_notification(
    app: AppHandle,
    title: String,
    body: String,
    room_url: Option<String>,
) -> Result<(), String> {
    if settings::get(&app).notifications_muted {
        return Ok(());
    }
    let body: String = body.chars().take(240).collect();
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())?;
    if let Some(room_url) = room_url.filter(|u| u.starts_with('/')) {
        *app.state::<LastNotification>().0.lock().unwrap() = Some((room_url, Instant::now()));
    }
    Ok(())
}

/// Keeps a file name safe to create: no directories, no characters Windows rejects.
fn sanitize_file_name(raw: &str) -> String {
    let name: String = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let name = name.trim().trim_matches('.').to_string();
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// Picks `name`, `name (1).ext`, `name (2).ext`... so an existing file is never overwritten.
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = std::path::Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let ext = path.extension().and_then(|e| e.to_str());
    for n in 1..1000 {
        let numbered = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(numbered);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}

/// Saves a downloaded attachment into the user's Downloads folder (webview `<a download>` links
/// do nothing in the desktop shell). The body is the raw file; the name comes in the
/// `x-filename` header (URL-encoded). Returns the path it was saved to.
#[tauri::command]
pub fn save_download(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<String, String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected the file as a raw body".into());
    };
    let raw_name = request
        .headers()
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("download");
    let decoded = percent_decode(raw_name);
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| format!("no downloads folder: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = unique_path(&dir, &sanitize_file_name(&decoded));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Minimal `%XX` decoder for the file name header (invalid sequences are kept as they are).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:"))
    {
        return Err("only http(s) and mailto links can be opened".into());
    }
    open_url(&app, &url)
}

#[tauri::command]
pub fn open_auth_in_browser(app: AppHandle, url: String) -> Result<(), String> {
    auth::open_in_browser(&app, &url)
}

/// The web app is listening now: hand over anything that arrived during start-up.
#[tauri::command]
pub fn take_pending_navigation(app: AppHandle) -> Option<String> {
    let state = app.state::<NavState>();
    state.web_ready.store(true, Ordering::SeqCst);
    let pending = state.pending.lock().unwrap().take();
    pending
}

#[tauri::command]
pub fn is_window_visible(app: AppHandle) -> bool {
    app.get_webview_window(MAIN_WINDOW)
        .map(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
        .unwrap_or(false)
}

#[tauri::command]
pub fn install_update(app: AppHandle) {
    crate::updater::install(&app);
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> crate::updater::CheckStatus {
    crate::updater::check(&app).await
}

#[cfg(test)]
mod download_tests {
    use super::*;

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(sanitize_file_name(r"..\..\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name("a/b:c?.txt"), "b_c_.txt");
        assert_eq!(sanitize_file_name("   "), "download");
        assert_eq!(sanitize_file_name("..."), "download");
    }

    #[test]
    fn header_names_are_percent_decoded() {
        assert_eq!(percent_decode("New%20Text%20Document.txt"), "New Text Document.txt");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%E3%81%82.txt"), "あ.txt");
    }
}
