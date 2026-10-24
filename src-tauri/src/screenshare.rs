//! Our own "you are sharing your screen" bar.
//!
//! When the web app shares the screen, WebView2 floats its own "localhost is sharing your
//! screen" bar, and it offers no API to hide or restyle it (`ScreenCaptureStarting` can only
//! allow or cancel). So on Windows the bar is hidden from outside, by finding the small
//! top-level window the WebView2 browser process owns, and replaced by a small always-on-top
//! window of ours (`desktop/screenshare-bar.html` in the web app) with "Return to call" and
//! "Stop sharing" buttons.

use crate::{commands, MAIN_WINDOW};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// Label of the bar window. Also on the window-state denylist (see `lib.rs`).
pub const BAR_WINDOW: &str = "screenshare-bar";

/// Where the web app serves the bar page (`public/desktop/screenshare-bar.html`).
const BAR_PAGE: &str = "desktop/screenshare-bar.html";

/// Size and placement of the bar, in logical pixels.
const BAR_WIDTH: f64 = 460.0;
const BAR_HEIGHT: f64 = 48.0;
const BAR_TOP_MARGIN: f64 = 8.0;

/// The event the main window hears when "Stop sharing" is pressed on the bar.
const STOP_EVENT: &str = "screen-share-stop";

/// The page address with the (already translated) labels in the query string.
fn bar_path(title: &str, stop: &str, back: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("title", title)
        .append_pair("stop", stop)
        .append_pair("back", back)
        .finish();
    format!("{BAR_PAGE}?{query}")
}

/// Top-centre of the primary monitor's work area (clear of the taskbar), as logical pixels.
fn bar_position(app: &AppHandle) -> Option<(f64, f64)> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let x = (area.position.x as f64 + (area.size.width as f64 - BAR_WIDTH * scale) / 2.0) / scale;
    let y = area.position.y as f64 / scale + BAR_TOP_MARGIN;
    Some((x, y))
}

/// Close the bar, if it is open, and stop looking for WebView2's own bar.
pub fn close_bar(app: &AppHandle) {
    cancel_native_bar_hider();
    if let Some(bar) = app.get_webview_window(BAR_WINDOW) {
        let _ = bar.destroy();
    }
}

/// Show the bar with these labels (or relabel it when it is already open) and start hiding
/// WebView2's own bar.
#[tauri::command]
pub async fn show_screen_share_bar(
    app: AppHandle,
    title: String,
    stop: String,
    back: String,
) -> Result<(), String> {
    let path = bar_path(&title, &stop, &back);
    if let Some(bar) = app.get_webview_window(BAR_WINDOW) {
        // Already open: only the labels may have changed (language switch, second share).
        if let Ok(mut url) = bar.url() {
            url.set_query(path.split_once('?').map(|(_, q)| q));
            let _ = bar.navigate(url);
        }
        let _ = bar.show();
    } else {
        let mut builder = WebviewWindowBuilder::new(&app, BAR_WINDOW, WebviewUrl::App(path.into()))
            .title("Mudrix")
            .inner_size(BAR_WIDTH, BAR_HEIGHT)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .decorations(false)
            .shadow(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .focusable(false)
            .background_color(tauri::window::Color(26, 27, 30, 255))
            .disable_drag_drop_handler();
        if let Some((x, y)) = bar_position(&app) {
            builder = builder.position(x, y);
        }
        builder.build().map_err(|e| e.to_string())?;
    }
    hide_native_bar(&app);
    Ok(())
}

/// Close the bar (the share ended, or the call was left).
#[tauri::command]
pub fn hide_screen_share_bar(app: AppHandle) {
    close_bar(&app);
}

/// "Stop sharing" on the bar: tell the web app to stop the share and close the bar.
#[tauri::command]
pub fn screen_share_bar_stop(app: AppHandle) {
    let _ = app.emit_to(MAIN_WINDOW, STOP_EVENT, ());
    close_bar(&app);
}

/// "Return to call" on the bar: bring the main window back (it may be minimised or in the tray).
#[tauri::command]
pub fn screen_share_bar_return(app: AppHandle) {
    commands::show_main_window(&app);
}

// ---------------------------------------------------------------------------
// Hiding WebView2's own bar (Windows)
// ---------------------------------------------------------------------------

/// Bumped by every start and cancel, so an older search for the bar stops by itself.
#[cfg(windows)]
static HIDER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(windows)]
fn cancel_native_bar_hider() {
    HIDER_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(not(windows))]
fn cancel_native_bar_hider() {}

/// Other platforms have no WebView2 bar to hide.
#[cfg(not(windows))]
fn hide_native_bar(_app: &AppHandle) {}

/// The WebView2 bar is a small top-level window of the WebView2 browser process. It appears a
/// moment after the share starts, so look for it on a background thread for a few seconds. If
/// it is never found the native bar simply stays visible, which is the safe failure.
#[cfg(windows)]
fn hide_native_bar(app: &AppHandle) {
    use std::sync::atomic::Ordering;

    let Some(main) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    // Our own windows are never touched, whatever process owns them.
    let own: Vec<isize> = app
        .webview_windows()
        .values()
        .filter_map(|w| w.hwnd().ok())
        .map(|h| h.0 as isize)
        .collect();
    let generation = HIDER_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let result = main.with_webview(move |webview| unsafe {
        let pid = match webview.controller().CoreWebView2() {
            Ok(core) => {
                let mut pid = 0u32;
                match core.BrowserProcessId(&mut pid) {
                    Ok(()) if pid != 0 => pid,
                    _ => return,
                }
            }
            Err(e) => {
                eprintln!("[desktop] no WebView2 core for the screen share bar: {e}");
                return;
            }
        };
        std::thread::spawn(move || hide_loop(pid, own, generation));
    });
    if let Err(e) = result {
        eprintln!("[desktop] could not reach the webview for the screen share bar: {e}");
    }
}

#[cfg(windows)]
fn hide_loop(pid: u32, own: Vec<isize>, generation: u64) {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if HIDER_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        if hide_matching_windows(pid, &own) > 0 {
            eprintln!("[desktop] hid the WebView2 screen share bar");
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The bar is a thin strip; anything taller is not it.
#[cfg(windows)]
const NATIVE_BAR_MAX_HEIGHT: i32 = 240;
/// And it is wide enough to hold its text and buttons; this keeps tooltips and small popups
/// of the browser process out of reach.
#[cfg(windows)]
const NATIVE_BAR_MIN_WIDTH: i32 = 200;

#[cfg(windows)]
struct Search<'a> {
    pid: u32,
    own: &'a [isize],
    hidden: u32,
}

/// Hide every visible, small, top-level window owned by `pid`. Returns how many were hidden.
#[cfg(windows)]
fn hide_matching_windows(pid: u32, own: &[isize]) -> u32 {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, ShowWindowAsync,
        SW_HIDE,
    };

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if search.own.contains(&(hwnd.0 as isize)) || !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        if owner != search.pid {
            return BOOL(1);
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return BOOL(1);
        }
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        if height > 0 && height <= NATIVE_BAR_MAX_HEIGHT && width >= NATIVE_BAR_MIN_WIDTH {
            let _ = ShowWindowAsync(hwnd, SW_HIDE);
            search.hidden += 1;
        }
        BOOL(1)
    }

    let mut search = Search {
        pid,
        own,
        hidden: 0,
    };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.hidden
}
