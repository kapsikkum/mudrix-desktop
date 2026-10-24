//! Getting frames and sound to the page without copying them through IPC.
//!
//! Each capture gets WebView2 shared buffers (`ICoreWebView2Environment12::CreateSharedBuffer`),
//! posted once to the main page with `PostSharedBufferToScript`; the page keeps them as
//! `ArrayBuffer`s. We map our own view of the same memory, so the capture threads write straight
//! into what the page reads. Video buffers are slots with a small header (see [`VideoSlot`]);
//! the audio buffer is a ring the page polls. A short web message says which slot is ready.
//!
//! WebView2 objects live on the UI thread, so the buffers and the `ICoreWebView2` are kept in a
//! thread-local there and every call into them is marshalled with `run_on_main_thread`.

use crate::MAIN_WINDOW;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{fence, AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Environment12, ICoreWebView2SharedBuffer, ICoreWebView2_17,
    COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE,
};
use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::{
    MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
};

/// Bytes before the payload in every buffer.
pub const HEADER_BYTES: usize = 64;

/// How long the page has to let go of a capture's buffers before they are closed.
const CLOSE_DELAY: Duration = Duration::from_millis(500);

/// How long to wait for the UI thread to create the buffers.
const CREATE_TIMEOUT: Duration = Duration::from_secs(5);

/// One buffer to create: posted with `{"type":"mudrix-capture-buffer","captureId","slot","kind"}`.
pub struct BufferSpec {
    pub slot: u32,
    pub kind: &'static str,
    pub size: usize,
    /// Header words (byte offset, value) written before the buffer is posted.
    pub header: Vec<(usize, u32)>,
}

// ---------------------------------------------------------------------------
// Our view of a shared buffer
// ---------------------------------------------------------------------------

/// A writable view of one shared buffer, usable from any thread. Normally our own
/// `MapViewOfFile` of the buffer's file mapping, so it stays valid however the page or WebView2
/// treat the buffer; if that mapping fails it borrows WebView2's view, which stays valid until
/// the buffer is closed (always after the capture threads are done).
pub struct SharedView {
    ptr: *mut u8,
    len: usize,
    owned: bool,
}

// The memory is plain shared memory; the protocol (state words) decides who touches what.
unsafe impl Send for SharedView {}

impl SharedView {
    pub fn len(&self) -> usize {
        self.len
    }

    fn word(&self, offset: usize) -> &AtomicU32 {
        debug_assert!(offset % 4 == 0 && offset + 4 <= self.len);
        // Page-aligned mapping and 4-aligned offsets; AtomicU32 has the layout of u32.
        unsafe { &*(self.ptr.add(offset) as *const AtomicU32) }
    }

    pub fn load_u32(&self, offset: usize) -> u32 {
        u32::from_le(self.word(offset).load(Ordering::Acquire))
    }

    pub fn store_u32(&self, offset: usize, value: u32, order: Ordering) {
        self.word(offset).store(value.to_le(), order);
    }

    pub fn store_u64(&self, offset: usize, value: u64) {
        self.store_u32(offset, value as u32, Ordering::Relaxed);
        self.store_u32(offset + 4, (value >> 32) as u32, Ordering::Relaxed);
    }

    /// The bytes after the header. The caller must own them under the buffer's protocol.
    #[allow(clippy::mut_from_ref)]
    pub fn payload(&self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.ptr.add(HEADER_BYTES),
                self.len.saturating_sub(HEADER_BYTES),
            )
        }
    }
}

#[cfg(test)]
impl SharedView {
    /// Plain (leaked, 8-aligned) memory standing in for a shared buffer.
    pub fn for_test(len: usize) -> SharedView {
        let words = vec![0u64; len.div_ceil(8)].into_boxed_slice();
        SharedView {
            ptr: Box::leak(words).as_mut_ptr() as *mut u8,
            len,
            owned: false,
        }
    }
}

impl Drop for SharedView {
    fn drop(&mut self) {
        if self.owned {
            unsafe {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.ptr as *mut core::ffi::c_void,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Video slots
// ---------------------------------------------------------------------------

/// Slot header (little-endian): 0 state (0 free, 1 ready), 4 width, 8 height, 12 stride,
/// 16 timestampUs (u64), 24 seq, 28 format (1 = BGRA). Pixels from byte 64.
pub struct VideoSlot {
    pub view: SharedView,
    /// The `mudrix-capture-frame` message for this slot.
    pub message: String,
    /// When the page was last told about this slot (to re-send a lost notice).
    pub notified: Option<std::time::Instant>,
}

pub const FORMAT_BGRA: u32 = 1;
const STATE_FREE: u32 = 0;
const STATE_READY: u32 = 1;

impl VideoSlot {
    pub fn new(view: SharedView, capture_id: &str, slot: u32) -> Self {
        let message = serde_json::json!({
            "type": "mudrix-capture-frame",
            "captureId": capture_id,
            "slot": slot,
        })
        .to_string();
        VideoSlot {
            view,
            message,
            notified: None,
        }
    }

    /// The page has released this slot (or never took it).
    pub fn is_free(&self) -> bool {
        self.view.load_u32(0) == STATE_FREE
    }

    /// Room for pixels, in bytes.
    pub fn capacity(&self) -> usize {
        self.view.len().saturating_sub(HEADER_BYTES)
    }

    /// Fill in the header, then hand the slot to the page (state = ready, last, with release
    /// ordering so the pixels and header are visible before it).
    pub fn publish(&self, width: u32, height: u32, stride: u32, timestamp_us: u64, seq: u32) {
        let v = &self.view;
        v.store_u32(4, width, Ordering::Relaxed);
        v.store_u32(8, height, Ordering::Relaxed);
        v.store_u32(12, stride, Ordering::Relaxed);
        v.store_u64(16, timestamp_us);
        v.store_u32(24, seq, Ordering::Relaxed);
        v.store_u32(28, FORMAT_BGRA, Ordering::Relaxed);
        fence(Ordering::Release);
        v.store_u32(0, STATE_READY, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// UI-thread side
// ---------------------------------------------------------------------------

#[derive(Default)]
struct UiState {
    /// The main webview the current capture talks to.
    core: Option<ICoreWebView2>,
    /// Shared buffers by capture id, kept alive until closed.
    buffers: HashMap<String, Vec<ICoreWebView2SharedBuffer>>,
}

thread_local! {
    /// Only ever touched on the UI thread (inside `with_webview` / `run_on_main_thread`).
    static UI: RefCell<UiState> = RefCell::new(UiState::default());
}

/// Create the buffers for `capture_id`, post each one to the main page, and return our views
/// of them, in `specs` order. Blocks until the UI thread has done it; never call it from there.
pub fn create_buffers(
    app: &AppHandle,
    capture_id: &str,
    specs: Vec<BufferSpec>,
) -> Result<Vec<SharedView>, String> {
    let window = app
        .get_webview_window(MAIN_WINDOW)
        .ok_or("the main window is gone")?;
    let (tx, rx) = mpsc::channel();
    let id = capture_id.to_string();
    window
        .with_webview(move |webview| {
            let result = unsafe { create_on_ui(&webview, &id, &specs) };
            let _ = tx.send(result);
        })
        .map_err(|e| format!("could not reach the webview: {e}"))?;
    rx.recv_timeout(CREATE_TIMEOUT)
        .map_err(|_| "the webview did not create the shared buffers in time".to_string())?
}

unsafe fn create_on_ui(
    webview: &tauri::webview::PlatformWebview,
    capture_id: &str,
    specs: &[BufferSpec],
) -> Result<Vec<SharedView>, String> {
    let env: ICoreWebView2Environment12 = webview
        .environment()
        .cast()
        .map_err(|e| format!("this WebView2 runtime has no shared buffers: {e}"))?;
    let core = webview
        .controller()
        .CoreWebView2()
        .map_err(|e| format!("no WebView2 core: {e}"))?;
    let core17: ICoreWebView2_17 = core
        .cast()
        .map_err(|e| format!("this WebView2 runtime cannot post shared buffers: {e}"))?;

    let mut buffers = Vec::with_capacity(specs.len());
    let mut views = Vec::with_capacity(specs.len());
    for spec in specs {
        match create_one(&env, &core17, capture_id, spec) {
            Ok((buffer, view)) => {
                buffers.push(buffer);
                views.push(view);
            }
            Err(e) => {
                for buffer in &buffers {
                    let _ = buffer.Close();
                }
                return Err(e);
            }
        }
    }
    UI.with(|ui| {
        let mut ui = ui.borrow_mut();
        ui.core = Some(core);
        ui.buffers.insert(capture_id.to_string(), buffers);
    });
    Ok(views)
}

unsafe fn create_one(
    env: &ICoreWebView2Environment12,
    core17: &ICoreWebView2_17,
    capture_id: &str,
    spec: &BufferSpec,
) -> Result<(ICoreWebView2SharedBuffer, SharedView), String> {
    let buffer = env
        .CreateSharedBuffer(spec.size as u64)
        .map_err(|e| format!("could not create a {} byte shared buffer: {e}", spec.size))?;
    let view = match map_view(&buffer, spec.size) {
        Ok(view) => view,
        Err(e) => {
            let _ = buffer.Close();
            return Err(e);
        }
    };
    for &(offset, value) in &spec.header {
        view.store_u32(offset, value, Ordering::Relaxed);
    }
    fence(Ordering::Release);
    let json = serde_json::json!({
        "type": "mudrix-capture-buffer",
        "captureId": capture_id,
        "slot": spec.slot,
        "kind": spec.kind,
    })
    .to_string();
    let json = HSTRING::from(json.as_str());
    if let Err(e) = core17.PostSharedBufferToScript(
        &buffer,
        COREWEBVIEW2_SHARED_BUFFER_ACCESS_READ_WRITE,
        PCWSTR(json.as_ptr()),
    ) {
        let _ = buffer.Close();
        return Err(format!("could not post the shared buffer to the page: {e}"));
    }
    Ok((buffer, view))
}

unsafe fn map_view(buffer: &ICoreWebView2SharedBuffer, size: usize) -> Result<SharedView, String> {
    let mut handle = HANDLE::default();
    if buffer.FileMappingHandle(&mut handle).is_ok() && !handle.is_invalid() {
        let addr = MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, size);
        if !addr.Value.is_null() {
            return Ok(SharedView {
                ptr: addr.Value as *mut u8,
                len: size,
                owned: true,
            });
        }
    }
    // Fall back to WebView2's own view of it.
    let mut ptr: *mut u8 = std::ptr::null_mut();
    buffer
        .Buffer(&mut ptr)
        .map_err(|e| format!("could not map the shared buffer: {e}"))?;
    if ptr.is_null() {
        return Err("the shared buffer has no memory".into());
    }
    let mut actual = 0u64;
    let _ = buffer.Size(&mut actual);
    Ok(SharedView {
        ptr,
        len: if actual > 0 {
            (actual as usize).min(size)
        } else {
            size
        },
        owned: false,
    })
}

/// Post a JSON web message to the main page (from any thread; it runs on the UI thread).
pub fn post_json(app: &AppHandle, json: String) {
    let result = app.run_on_main_thread(move || {
        UI.with(|ui| {
            let ui = ui.borrow();
            let Some(core) = ui.core.as_ref() else {
                return;
            };
            let message = HSTRING::from(json.as_str());
            if let Err(e) = unsafe { core.PostWebMessageAsJson(PCWSTR(message.as_ptr())) } {
                eprintln!("[capture] could not post a message to the page: {e}");
            }
        });
    });
    if let Err(e) = result {
        eprintln!("[capture] could not reach the UI thread: {e}");
    }
}

/// Close the shared buffers of `capture_id` once the page has had time to release them.
/// The capture threads must be done writing (our own views may still exist; they are fine).
pub fn close_buffers_later(app: &AppHandle, capture_id: &str) {
    let app = app.clone();
    let id = capture_id.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(CLOSE_DELAY);
        let _ = app.run_on_main_thread(move || {
            let buffers = UI.with(|ui| ui.borrow_mut().buffers.remove(&id));
            for buffer in buffers.into_iter().flatten() {
                if let Err(e) = unsafe { buffer.Close() } {
                    eprintln!("[capture] could not close a shared buffer: {e}");
                }
            }
        });
    });
}
