//! Starting and stopping captures (Windows). One capture at a time.
//!
//! A capture is a video thread (`session`) and, with sound, an audio thread (`audio`), sharing a
//! stop flag. Whoever takes the capture out of [`ACTIVE`] ends it: `capture_stop`, a new
//! `capture_start` and the app closing set the flag and wait for the threads; the video thread
//! itself does it when the source closes or fails. Ending posts `mudrix-capture-ended`, emits
//! `capture-ended` and closes the shared buffers a moment later.

use super::session::{self, Ready};
use super::sources::{self, Source};
use super::transport::{self, BufferSpec, HEADER_BYTES};
use super::{audio, CaptureEnded, CaptureSource, CaptureStarted, EndReason, ENDED_EVENT};
use crate::MAIN_WINDOW;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// How long the capture thread may take to open the source or to start.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// Video slots per capture.
const VIDEO_SLOTS: u32 = 3;
/// Frame-rate bounds.
const MIN_FPS: u32 = 1;
const MAX_FPS: u32 = 120;

struct Active {
    id: String,
    stop: Arc<AtomicBool>,
    video: Option<JoinHandle<()>>,
    audio: Option<JoinHandle<()>>,
}

/// The running capture, if any.
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
/// Starts one at a time, so two quick `capture_start`s cannot both win.
static STARTING: Mutex<()> = Mutex::new(());

fn active() -> std::sync::MutexGuard<'static, Option<Active>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

pub async fn list_sources(thumb_width: u32) -> Result<Vec<CaptureSource>, String> {
    tauri::async_runtime::spawn_blocking(move || sources::list(thumb_width))
        .await
        .map_err(|e| format!("listing sources failed: {e}"))?
}

pub async fn start(
    app: AppHandle,
    source_id: String,
    max_height: u32,
    fps: u32,
    audio: bool,
    cursor: bool,
) -> Result<CaptureStarted, String> {
    tauri::async_runtime::spawn_blocking(move || {
        start_blocking(app, &source_id, max_height, fps, audio, cursor)
    })
    .await
    .map_err(|e| format!("starting the capture failed: {e}"))?
}

pub async fn stop(app: AppHandle, capture_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        stop_current(&app, Some(&capture_id), EndReason::Stopped, None);
    })
    .await
    .map_err(|e| format!("stopping the capture failed: {e}"))
}

pub fn stop_all(app: &AppHandle) {
    if active().is_none() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || stop_current(&app, None, EndReason::Stopped, None));
}

fn start_blocking(
    app: AppHandle,
    source_id: &str,
    max_height: u32,
    fps: u32,
    with_audio: bool,
    cursor: bool,
) -> Result<CaptureStarted, String> {
    let _starting = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    stop_current(&app, None, EndReason::Stopped, None);

    let source = Source::parse(source_id).ok_or_else(|| format!("unknown source {source_id}"))?;
    if !source.exists() {
        return Err("that screen or window is gone".into());
    }
    let capture_id = uuid::Uuid::new_v4().to_string();
    let stop = Arc::new(AtomicBool::new(false));

    // Open the source on the capture thread.
    let (ready_tx, ready_rx) = mpsc::channel();
    let (slots_tx, slots_rx) = mpsc::channel();
    let params = session::Params {
        app: app.clone(),
        capture_id: capture_id.clone(),
        source,
        max_height,
        fps: fps.clamp(MIN_FPS, MAX_FPS),
        cursor,
        stop: stop.clone(),
    };
    let video = std::thread::Builder::new()
        .name("capture-video".into())
        .spawn(move || session::run(params, ready_tx, slots_rx))
        .map_err(|e| format!("could not start the capture thread: {e}"))?;
    let abandon = |video: JoinHandle<()>| {
        stop.store(true, Ordering::SeqCst);
        let _ = video.join();
    };

    let opened = match ready_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ready::Opened(opened)) => opened,
        Ok(Ready::Failed(e)) => {
            abandon(video);
            return Err(e);
        }
        _ => {
            drop(slots_tx);
            abandon(video);
            return Err("the capture did not start in time".into());
        }
    };

    // The buffers, sized for the largest frame this capture can produce.
    let slot_size = HEADER_BYTES + opened.box_w as usize * 4 * opened.box_h as usize;
    let mut specs: Vec<BufferSpec> = (0..VIDEO_SLOTS)
        .map(|slot| BufferSpec {
            slot,
            kind: "video",
            size: slot_size,
            header: Vec::new(),
        })
        .collect();
    if with_audio {
        specs.push(audio::buffer_spec());
    }
    let mut views = match transport::create_buffers(&app, &capture_id, specs) {
        Ok(views) => views,
        Err(e) => {
            drop(slots_tx);
            abandon(video);
            return Err(e);
        }
    };
    let audio_view = if with_audio { views.pop() } else { None };

    // From here on the capture is ours to end, whatever happens.
    let audio_target = match source {
        Source::Window(h) => match sources::window_app_pid(h) {
            Some(pid) => audio::AudioTarget::App(pid),
            None => audio::AudioTarget::AllButUs,
        },
        Source::Screen(_) => audio::AudioTarget::AllButUs,
    };
    let audio_thread = audio_view.and_then(|view| {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("capture-audio".into())
            .spawn(move || audio::run(view, stop, audio_target))
            .map_err(|e| eprintln!("[capture] could not start the audio thread: {e}"))
            .ok()
    });
    *active() = Some(Active {
        id: capture_id.clone(),
        stop: stop.clone(),
        video: Some(video),
        audio: audio_thread,
    });
    let _ = slots_tx.send(views);

    match ready_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ready::Started) => {}
        Ok(Ready::Failed(e)) => {
            stop_current(&app, Some(&capture_id), EndReason::Error, Some(e.clone()));
            return Err(e);
        }
        _ => {
            let e = "the capture did not start in time".to_string();
            stop_current(&app, Some(&capture_id), EndReason::Error, Some(e.clone()));
            return Err(e);
        }
    }
    eprintln!(
        "[capture] started {capture_id} on {source_id}: {}x{} (buffers for {}x{}), {} fps, audio {with_audio}",
        opened.width, opened.height, opened.box_w, opened.box_h, fps
    );
    Ok(CaptureStarted {
        capture_id,
        width: opened.width,
        height: opened.height,
    })
}

/// End the running capture (only if it is `only_id`, when given): stop its threads, wait for
/// them, then tell the page.
fn stop_current(
    app: &AppHandle,
    only_id: Option<&str>,
    reason: EndReason,
    message: Option<String>,
) {
    let taken = {
        let mut active = active();
        match active.as_ref() {
            Some(a) if only_id.map_or(true, |id| id == a.id) => active.take(),
            _ => None,
        }
    };
    let Some(mut capture) = taken else {
        return;
    };
    capture.stop.store(true, Ordering::SeqCst);
    for thread in [capture.video.take(), capture.audio.take()]
        .into_iter()
        .flatten()
    {
        let _ = thread.join();
    }
    finish(app, &capture.id, reason, message);
}

/// The video thread's way out when the source closed or failed. If the capture is already
/// being stopped by someone else, they finish it.
pub fn ended_by_worker(
    app: &AppHandle,
    capture_id: &str,
    reason: EndReason,
    message: Option<String>,
) {
    let taken = {
        let mut active = active();
        match active.as_ref() {
            Some(a) if a.id == capture_id => active.take(),
            _ => None,
        }
    };
    let Some(mut capture) = taken else {
        return;
    };
    capture.stop.store(true, Ordering::SeqCst);
    // This is the video thread itself: let its handle go, wait only for the audio.
    drop(capture.video.take());
    if let Some(thread) = capture.audio.take() {
        let _ = thread.join();
    }
    finish(app, &capture.id, reason, message);
}

/// Tell the page the capture is over and close its buffers once the page let them go.
fn finish(app: &AppHandle, capture_id: &str, reason: EndReason, message: Option<String>) {
    transport::post_json(
        app,
        serde_json::json!({ "type": "mudrix-capture-ended", "captureId": capture_id }).to_string(),
    );
    let payload = CaptureEnded {
        capture_id: capture_id.to_string(),
        reason: reason.as_str(),
        message,
    };
    if let Err(e) = app.emit_to(MAIN_WINDOW, ENDED_EVENT, payload) {
        eprintln!("[capture] could not emit {ENDED_EVENT}: {e}");
    }
    transport::close_buffers_later(app, capture_id);
    eprintln!("[capture] ended {capture_id} ({})", reason.as_str());
}
