//! Native screen and window capture for screen sharing in calls.
//!
//! WebView2's own `getDisplayMedia` picker is slow and names the page "localhost", so on Windows
//! the web app shows its own picker and the frames come from here instead:
//!
//! - `capture_list_sources` lists every monitor and shareable window, with a small JPEG
//!   thumbnail and the app icon (`sources`).
//! - `capture_start` runs a Windows.Graphics.Capture session on one of them (`session`), scales
//!   each frame on the GPU (`gpu`) and copies it into one of three WebView2 shared buffers
//!   (`transport`), then tells the page which slot is ready. With `audio` it also records the
//!   system sound, minus our own process tree, into a fourth buffer used as a ring (`audio`).
//! - `capture_stop` ends it. The `capture-ended` event (and a `mudrix-capture-ended` web
//!   message) says when and why a capture ended.
//!
//! Only one capture runs at a time (`control`). Other platforms answer "unsupported" and keep
//! their webview's own picker.

#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod control;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod sources;
#[cfg(all(test, windows))]
mod tests;
#[cfg(windows)]
mod transport;

#[cfg(windows)]
use control as imp;

use serde::Serialize;
use tauri::AppHandle;

/// The event the main window hears when a capture ends, whatever the reason.
#[cfg_attr(not(windows), allow(dead_code))]
pub const ENDED_EVENT: &str = "capture-ended";

/// One monitor or window the user can share.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSource {
    /// `screen:<HMONITOR>` or `window:<HWND>`; pass it back to `capture_start`.
    pub id: String,
    /// `"screen"` or `"window"`.
    pub kind: &'static str,
    pub name: String,
    pub app_name: Option<String>,
    /// `data:image/png;base64,…` of the app icon, 32 px.
    pub icon: Option<String>,
    /// `data:image/jpeg;base64,…`, `thumb_width` wide.
    pub thumb: Option<String>,
    /// Size in physical pixels.
    pub width: u32,
    pub height: u32,
    /// The primary monitor (always false for windows).
    pub primary: bool,
}

/// What `capture_start` answers: the id of the capture and the size of its first frames.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStarted {
    pub capture_id: String,
    pub width: u32,
    pub height: u32,
}

/// Payload of [`ENDED_EVENT`].
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureEnded {
    pub capture_id: String,
    /// `"stopped"`, `"source-closed"` or `"error"`.
    pub reason: &'static str,
    pub message: Option<String>,
}

/// Why a capture ended.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// `capture_stop`, a new capture, or the app closing.
    Stopped,
    /// The shared window closed or the monitor went away.
    SourceClosed,
    Error,
}

impl EndReason {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Stopped => "stopped",
            EndReason::SourceClosed => "source-closed",
            EndReason::Error => "error",
        }
    }
}

/// Every monitor and shareable window, with thumbnails `thumb_width` pixels wide (0: none).
#[tauri::command]
pub async fn capture_list_sources(thumb_width: u32) -> Result<Vec<CaptureSource>, String> {
    imp::list_sources(thumb_width).await
}

/// Start capturing `source_id` (stopping any capture already running). `max_height` 0 keeps the
/// native size; `fps` caps the frame rate; `audio` adds the system sound; `cursor` draws the
/// mouse pointer into the frames.
#[tauri::command]
pub async fn capture_start(
    app: AppHandle,
    source_id: String,
    max_height: u32,
    fps: u32,
    audio: bool,
    cursor: bool,
) -> Result<CaptureStarted, String> {
    imp::start(app, source_id, max_height, fps, audio, cursor).await
}

/// Stop the capture `capture_id`, if it is still running.
#[tauri::command]
pub async fn capture_stop(app: AppHandle, capture_id: String) -> Result<(), String> {
    imp::stop(app, capture_id).await
}

/// Stop whatever is being captured (the main window is gone, or the app is exiting). Never
/// blocks the calling thread.
pub fn stop_all(app: &AppHandle) {
    imp::stop_all(app);
}

#[cfg(not(windows))]
mod imp {
    use super::{CaptureSource, CaptureStarted};
    use tauri::AppHandle;

    const UNSUPPORTED: &str = "unsupported";

    pub async fn list_sources(_thumb_width: u32) -> Result<Vec<CaptureSource>, String> {
        Err(UNSUPPORTED.into())
    }

    pub async fn start(
        _app: AppHandle,
        _source_id: String,
        _max_height: u32,
        _fps: u32,
        _audio: bool,
        _cursor: bool,
    ) -> Result<CaptureStarted, String> {
        Err(UNSUPPORTED.into())
    }

    pub async fn stop(_app: AppHandle, _capture_id: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub fn stop_all(_app: &AppHandle) {}
}
