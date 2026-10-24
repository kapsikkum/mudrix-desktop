//! One Windows.Graphics.Capture session, on its own thread.
//!
//! The thread opens the monitor or window (`GraphicsCaptureItem` through
//! `IGraphicsCaptureItemInterop`) with a free-threaded BGRA frame pool, says how big the frames
//! will be, waits for the shared buffers, then pumps: the pool's `FrameArrived` only wakes the
//! thread; the thread takes the newest frame, holds it until the frame-rate cap allows it,
//! scales it on the GPU and copies it into a free slot. With no free slot it keeps only the
//! newest frame and tries again a few ms later; it never waits on the page. When the picture
//! does not change for a second the last frame is sent again, so a late viewer still gets one.

use super::control;
use super::gpu::{Gpu, Pipeline};
use super::sources::{self, Source};
use super::transport::{self, SharedView, VideoSlot};
use super::EndReason;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use windows::core::Interface;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
// We hold one frame until the frame-rate cap lets it through and read another back; with only
// two buffers DWM was left without a free one, and stalling DWM made the whole desktop (and the
// cursor) stutter while sharing. Four keeps two free for it.
const POOL_BUFFERS: i32 = 4;
/// Re-send the last frame when nothing changed for this long.
const KEEPALIVE: Duration = Duration::from_secs(1);
/// Re-send a "slot ready" notice the page has not acted on for this long.
const RENOTIFY: Duration = Duration::from_secs(1);
/// How often to look for a free slot while a frame waits for one.
const SLOT_RETRY: Duration = Duration::from_millis(4);
/// The longest the thread sleeps without looking at the stop flag.
const MAX_WAIT: Duration = Duration::from_millis(100);
/// How often to check that a shared window still exists (besides the `Closed` event).
const WINDOW_CHECK: Duration = Duration::from_millis(250);

/// What the capture thread was asked to do.
pub struct Params {
    pub app: AppHandle,
    pub capture_id: String,
    pub source: Source,
    /// 0 = native size.
    pub max_height: u32,
    pub fps: u32,
    pub cursor: bool,
    pub stop: Arc<AtomicBool>,
}

/// Sizes known once the source is open.
#[derive(Debug, Clone, Copy)]
pub struct Opened {
    /// Size of the first frames.
    pub width: u32,
    pub height: u32,
    /// The largest frame this capture can produce; the slot buffers are sized for it.
    pub box_w: u32,
    pub box_h: u32,
}

/// What the capture thread reports while starting.
pub enum Ready {
    Opened(Opened),
    Started,
    Failed(String),
}

/// The capture thread. Reports `Opened`, waits for the slot buffers, reports `Started`, then
/// runs until stopped (by the stop flag, quietly) or until the source closes or fails (then it
/// ends the capture through [`control::ended_by_worker`]).
pub fn run(p: Params, ready: mpsc::Sender<Ready>, slots: mpsc::Receiver<Vec<SharedView>>) {
    let _com = ComGuard::mta();
    let mut capture = match Capture::open(&p) {
        Ok(capture) => capture,
        Err(e) => {
            let _ = ready.send(Ready::Failed(e));
            return;
        }
    };
    let _ = ready.send(Ready::Opened(capture.opened));
    let Ok(views) = slots.recv() else {
        capture.close();
        return;
    };
    capture.slots = views
        .into_iter()
        .enumerate()
        .map(|(i, view)| VideoSlot::new(view, &p.capture_id, i as u32))
        .collect();
    if let Err(e) = capture.start(&p) {
        capture.close();
        let _ = ready.send(Ready::Failed(e));
        return;
    }
    let _ = ready.send(Ready::Started);
    drop(ready);

    let ended = capture.pump(&p);
    capture.close();
    if let Some((reason, message)) = ended {
        match &message {
            Some(m) => eprintln!("[capture] {}: {m}", reason.as_str()),
            None => eprintln!("[capture] {}", reason.as_str()),
        }
        control::ended_by_worker(&p.app, &p.capture_id, reason, message);
    }
}

/// Wakes the capture thread (a frame arrived, or the source closed).
#[derive(Default)]
struct Signal {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Signal {
    fn notify(&self) {
        let mut flag = self.flag.lock().unwrap_or_else(|e| e.into_inner());
        *flag = true;
        self.cv.notify_one();
    }

    fn wait(&self, timeout: Duration) {
        let flag = self.flag.lock().unwrap_or_else(|e| e.into_inner());
        let (mut flag, _) = self
            .cv
            .wait_timeout_while(flag, timeout, |set| !*set)
            .unwrap_or_else(|e| e.into_inner());
        *flag = false;
    }
}

struct Capture {
    gpu: Gpu,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pool_size: SizeInt32,
    signal: Arc<Signal>,
    closed: Arc<AtomicBool>,
    frame_token: Option<i64>,
    closed_token: Option<i64>,
    opened: Opened,
    max_height: u32,
    slots: Vec<VideoSlot>,
    pipeline: Pipeline,
    window: Option<isize>,
}

/// What became of a frame.
enum Emit {
    Sent,
    /// Every slot is still with the page.
    NoSlot,
    /// Nothing to send (an empty frame).
    Skipped,
}

impl Capture {
    fn open(p: &Params) -> Result<Capture, String> {
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err("screen capture is not supported on this version of Windows".into());
        }
        let gpu = Gpu::create()?;
        let item = create_item(p.source)?;
        let size = item
            .Size()
            .map_err(|e| format!("the source has no size: {e}"))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &gpu.winrt,
            PIXEL_FORMAT,
            POOL_BUFFERS,
            size,
        )
        .map_err(|e| format!("could not create the frame pool: {e}"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("could not create the capture session: {e}"))?;

        let (w, h) = (size.Width.max(2) as u32, size.Height.max(2) as u32);
        let (box_w, box_h) = frame_box(p.source, w, h, p.max_height);
        let (width, height) = fit(w, h, box_w, box_h, p.max_height);
        Ok(Capture {
            gpu,
            item,
            pool,
            session,
            pool_size: size,
            signal: Arc::new(Signal::default()),
            closed: Arc::new(AtomicBool::new(false)),
            frame_token: None,
            closed_token: None,
            opened: Opened {
                width,
                height,
                box_w,
                box_h,
            },
            max_height: p.max_height,
            slots: Vec::new(),
            pipeline: Pipeline::default(),
            window: match p.source {
                Source::Window(hwnd) => Some(hwnd),
                Source::Screen(_) => None,
            },
        })
    }

    fn start(&mut self, p: &Params) -> Result<(), String> {
        // Each of these needs a newer Windows; without them the capture still works.
        let _ = self.session.SetIsCursorCaptureEnabled(p.cursor);
        // Windows 11: no yellow border around what is shared.
        let _ = self.session.SetIsBorderRequired(false);
        // Windows 11 24H2: no frames faster than we would send them anyway.
        let interval = 10_000_000 / i64::from(p.fps.max(1));
        let _ = self
            .session
            .SetMinUpdateInterval(windows::Foundation::TimeSpan {
                Duration: interval * 4 / 5,
            });

        let signal = self.signal.clone();
        let token = self
            .pool
            .FrameArrived(&TypedEventHandler::new(move |_, _| {
                signal.notify();
                Ok(())
            }))
            .map_err(|e| format!("could not listen for frames: {e}"))?;
        self.frame_token = Some(token);

        let signal = self.signal.clone();
        let closed = self.closed.clone();
        if let Ok(token) = self.item.Closed(&TypedEventHandler::new(move |_, _| {
            closed.store(true, Ordering::SeqCst);
            signal.notify();
            Ok(())
        })) {
            self.closed_token = Some(token);
        }

        self.session
            .StartCapture()
            .map_err(|e| format!("could not start capturing: {e}"))
    }

    fn close(&mut self) {
        if let Some(token) = self.frame_token.take() {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        if let Some(token) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }

    /// Runs until the stop flag (None) or until the source is gone or broken.
    fn pump(&mut self, p: &Params) -> Option<(EndReason, Option<String>)> {
        let interval = 10_000_000 / i64::from(p.fps.max(1));
        let tolerance = interval / 5;
        let start = qpc_100ns();
        let mut next_due = 0i64;
        let mut pending: Option<Direct3D11CaptureFrame> = None;
        let mut waiting_for_slot = false;
        let mut last_sent = Instant::now();
        let mut last_ts_us = 0u64;
        let mut seq = 0u32;
        let mut window_checked = Instant::now();

        loop {
            let wait = if waiting_for_slot {
                SLOT_RETRY
            } else if pending.is_some() {
                let ahead = (next_due - tolerance - qpc_100ns()).max(0) / 10_000;
                Duration::from_millis(ahead as u64).clamp(Duration::from_millis(1), MAX_WAIT)
            } else {
                MAX_WAIT
            };
            self.signal.wait(wait);

            if p.stop.load(Ordering::SeqCst) {
                if let Some(frame) = pending.take() {
                    let _ = frame.Close();
                }
                return None;
            }
            if self.closed.load(Ordering::SeqCst) {
                return Some((EndReason::SourceClosed, None));
            }
            if let Some(hwnd) = self.window {
                if window_checked.elapsed() >= WINDOW_CHECK {
                    window_checked = Instant::now();
                    if !unsafe { IsWindow(Some(HWND(hwnd as *mut _))) }.as_bool() {
                        return Some((EndReason::SourceClosed, None));
                    }
                }
            }

            // Keep only the newest frame.
            while let Ok(frame) = self.pool.TryGetNextFrame() {
                if let Err(e) = self.follow_size(&frame) {
                    return Some((EndReason::Error, Some(e)));
                }
                if let Some(old) = pending.replace(frame) {
                    let _ = old.Close();
                }
                waiting_for_slot = false;
            }

            let now = qpc_100ns();
            if pending.is_some() && (waiting_for_slot || now + tolerance >= next_due) {
                let Some(frame) = pending.take() else {
                    continue;
                };
                let captured = frame
                    .SystemRelativeTime()
                    .map(|t| t.Duration)
                    .unwrap_or(now);
                let ts_us = (((captured - start).max(0) / 10) as u64).max(last_ts_us + 1);
                match self.emit(p, &frame, ts_us, seq) {
                    Ok(Emit::Sent) => {
                        let _ = frame.Close();
                        seq = seq.wrapping_add(1);
                        last_ts_us = ts_us;
                        last_sent = Instant::now();
                        waiting_for_slot = false;
                        next_due = if now - next_due > interval {
                            now + interval
                        } else {
                            next_due + interval
                        };
                    }
                    Ok(Emit::NoSlot) => {
                        // Hold on to it (newer frames replace it) and look again shortly.
                        pending = Some(frame);
                        waiting_for_slot = true;
                    }
                    Ok(Emit::Skipped) => {
                        let _ = frame.Close();
                    }
                    Err(e) => {
                        let _ = frame.Close();
                        return Some((EndReason::Error, Some(e)));
                    }
                }
            } else if pending.is_none()
                && self.pipeline.has_frame()
                && last_sent.elapsed() >= KEEPALIVE
            {
                let ts_us = (((now - start).max(0) / 10) as u64).max(last_ts_us + 1);
                if let Some(index) = self.free_slot() {
                    if let Err(e) = self.write_slot(p, index, ts_us, seq) {
                        return Some((EndReason::Error, Some(e)));
                    }
                    seq = seq.wrapping_add(1);
                    last_ts_us = ts_us;
                }
                last_sent = Instant::now();
            }
        }
    }

    /// When the source changes size, recreate the pool so later frames have the new size.
    fn follow_size(&mut self, frame: &Direct3D11CaptureFrame) -> Result<(), String> {
        let Ok(size) = frame.ContentSize() else {
            return Ok(());
        };
        if size.Width <= 0 || size.Height <= 0 || size == self.pool_size {
            return Ok(());
        }
        self.pool
            .Recreate(&self.gpu.winrt, PIXEL_FORMAT, POOL_BUFFERS, size)
            .map_err(|e| format!("could not follow the new source size: {e}"))?;
        self.pool_size = size;
        Ok(())
    }

    fn free_slot(&mut self) -> Option<usize> {
        if let Some(index) = self.slots.iter().position(|s| s.is_free()) {
            return Some(index);
        }
        None
    }

    /// No slot is free: if the page sat on a notice for a while, it may have missed it (for
    /// example it arrived before the page had the buffer), so send it again.
    fn renotify(&mut self, app: &AppHandle) {
        for slot in &mut self.slots {
            if slot.notified.is_some_and(|t| t.elapsed() >= RENOTIFY) {
                slot.notified = Some(Instant::now());
                transport::post_json(app, slot.message.clone());
            }
        }
    }

    fn emit(
        &mut self,
        p: &Params,
        frame: &Direct3D11CaptureFrame,
        ts_us: u64,
        seq: u32,
    ) -> Result<Emit, String> {
        let Some(index) = self.free_slot() else {
            self.renotify(&p.app);
            return Ok(Emit::NoSlot);
        };
        let texture = frame_texture(frame)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let content = frame.ContentSize().unwrap_or(self.pool_size);
        let cw = (content.Width.max(0) as u32).min(desc.Width);
        let ch = (content.Height.max(0) as u32).min(desc.Height);
        if cw < 2 || ch < 2 {
            return Ok(Emit::Skipped);
        }
        let (out_w, out_h) = fit(
            cw,
            ch,
            self.opened.box_w,
            self.opened.box_h,
            self.max_height,
        );
        // No scaling needed: crop to even and copy.
        let (in_w, in_h) = if (out_w, out_h) == (cw & !1, ch & !1) {
            (out_w, out_h)
        } else {
            (cw, ch)
        };
        self.pipeline
            .render(&self.gpu, &texture, in_w, in_h, out_w, out_h)?;
        self.write_slot(p, index, ts_us, seq)?;
        Ok(Emit::Sent)
    }

    /// Copy the pipeline's last frame into slot `index` and tell the page.
    fn write_slot(&mut self, p: &Params, index: usize, ts_us: u64, seq: u32) -> Result<(), String> {
        let (w, h) = self.pipeline.output_size();
        let stride = w as usize * 4;
        let slot = &mut self.slots[index];
        if stride * h as usize > slot.capacity() {
            // Cannot happen with `fit`; never write past the buffer if it somehow does.
            return Err(format!("a {w}x{h} frame does not fit the shared buffer"));
        }
        self.pipeline.read(&self.gpu, slot.view.payload(), stride)?;
        slot.publish(w, h, stride as u32, ts_us, seq);
        slot.notified = Some(Instant::now());
        transport::post_json(&p.app, slot.message.clone());
        Ok(())
    }
}

/// The D3D11 texture behind a captured frame.
fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D, String> {
    let surface = frame
        .Surface()
        .map_err(|e| format!("a frame without a surface: {e}"))?;
    let access: IDirect3DDxgiInterfaceAccess = surface
        .cast()
        .map_err(|e| format!("a frame surface without D3D11 access: {e}"))?;
    unsafe { access.GetInterface::<ID3D11Texture2D>() }
        .map_err(|e| format!("a frame surface without a texture: {e}"))
}

/// A capture item for a monitor or window.
pub fn create_item(source: Source) -> Result<GraphicsCaptureItem, String> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(|e| format!("screen capture is unavailable: {e}"))?;
    let item = unsafe {
        match source {
            Source::Screen(monitor) => interop.CreateForMonitor(HMONITOR(monitor as *mut _)),
            Source::Window(hwnd) => interop.CreateForWindow(HWND(hwnd as *mut _)),
        }
    };
    item.map_err(|e| format!("this source cannot be captured: {e}"))
}

/// The largest frame a capture can produce: the source (a window: the source or the largest
/// monitor, whichever is bigger, since it may be maximised later), shrunk to `max_height`.
fn frame_box(source: Source, width: u32, height: u32, max_height: u32) -> (u32, u32) {
    let (w, h) = match source {
        Source::Screen(_) => (width, height),
        Source::Window(_) => {
            let (mw, mh) = sources::largest_monitor();
            (width.max(mw), height.max(mh))
        }
    };
    let (w, h) = (w.max(2), h.max(2));
    if max_height >= 2 && h > max_height {
        let box_h = max_height & !1;
        let box_w = (u64::from(w) * u64::from(box_h)).div_ceil(u64::from(h)) as u32;
        (even_up(box_w), box_h)
    } else {
        (even_up(w), even_up(h))
    }
}

fn even_up(v: u32) -> u32 {
    (v + 1) & !1
}

/// Output size for a `width`×`height` frame: no taller than `max_height` (0: any), inside the
/// box, same aspect, even sides, never upscaled.
pub fn fit(width: u32, height: u32, box_w: u32, box_h: u32, max_height: u32) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(2)), f64::from(height.max(2)));
    let mut scale = (f64::from(box_w) / w).min(f64::from(box_h) / h);
    if max_height > 0 {
        scale = scale.min(f64::from(max_height) / h);
    }
    if scale >= 1.0 {
        return ((width & !1).clamp(2, box_w), (height & !1).clamp(2, box_h));
    }
    let out_w = ((w * scale) as u32 & !1).clamp(2, box_w);
    let out_h = ((h * scale) as u32 & !1).clamp(2, box_h);
    (out_w, out_h)
}

/// One still frame of `source` as tightly packed BGRA, for thumbnails that GDI cannot make.
/// Only call this where the yellow capture border can be turned off (it would flash).
pub fn grab_still(gpu: &Gpu, source: Source, timeout: Duration) -> Option<(Vec<u8>, u32, u32)> {
    let item = create_item(source).ok()?;
    let size = item.Size().ok()?;
    let pool =
        Direct3D11CaptureFramePool::CreateFreeThreaded(&gpu.winrt, PIXEL_FORMAT, 1, size).ok()?;
    let session = pool.CreateCaptureSession(&item).ok()?;
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(false);
    let mut result = None;
    if session.StartCapture().is_ok() {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(frame) = pool.TryGetNextFrame() {
                result = read_frame(gpu, &frame);
                let _ = frame.Close();
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let _ = session.Close();
    let _ = pool.Close();
    result
}

fn read_frame(gpu: &Gpu, frame: &Direct3D11CaptureFrame) -> Option<(Vec<u8>, u32, u32)> {
    let texture = frame_texture(frame).ok()?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    let content = frame.ContentSize().ok()?;
    let w = (content.Width.max(0) as u32).min(desc.Width);
    let h = (content.Height.max(0) as u32).min(desc.Height);
    if w == 0 || h == 0 {
        return None;
    }
    let staging = gpu.staging(w, h).ok()?;
    gpu.copy_region(&staging, &texture, w, h);
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    gpu.read_staging(&staging, w, h, &mut pixels, w as usize * 4)
        .ok()?;
    Some((pixels, w, h))
}

/// The QPC clock in 100 ns units, the time base of `SystemRelativeTime`.
fn qpc_100ns() -> i64 {
    let mut counter = 0i64;
    let mut frequency = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    if frequency <= 0 {
        return 0;
    }
    (i128::from(counter) * 10_000_000 / i128::from(frequency)) as i64
}

/// COM for the current thread (multithreaded apartment), undone when dropped.
pub struct ComGuard(bool);

impl ComGuard {
    pub fn mta() -> ComGuard {
        ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}
