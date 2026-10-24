//! System sound for a screen share, without our own call audio.
//!
//! On Windows 10 2004 and later this is WASAPI process loopback in "exclude" mode with our own
//! process id: everything the PC plays except our process tree (which includes the WebView2
//! processes, so the other people in the call are not sent back to them). Older Windows falls
//! back to plain loopback of the default output device, which does include our call audio.
//!
//! The sound goes into one shared buffer used as a ring of 48 kHz stereo float32 samples (one
//! second), converted and resampled here if the device delivers something else. Header
//! (little-endian): 0 writeFrames (total frames written, wrapping u32), 4 sampleRate,
//! 8 channels, 12 capacityFrames; samples from byte 64. The page polls `writeFrames`.

use super::session::ComGuard;
use super::transport::{BufferSpec, SharedView, HEADER_BYTES};
use std::mem::ManuallyDrop;
use std::sync::atomic::{fence, AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::{implement, Interface, Ref, HRESULT};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, BLOB, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcessId, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// One second of sound.
pub const CAPACITY_FRAMES: u32 = SAMPLE_RATE;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// WASAPI buffer, 100 ns units.
const CLIENT_BUFFER: i64 = 2_000_000;
/// How long to wait for the next packet before looking at the stop flag again.
const PACKET_WAIT_MS: u32 = 20;
/// With nothing playing, loopback delivers nothing; fill such gaps with silence so the page's
/// clock keeps moving.
const IDLE_GAP: Duration = Duration::from_millis(50);
/// After the device fails (unplugged, default changed), try again this often.
const REOPEN_DELAY: Duration = Duration::from_millis(500);

/// The audio buffer for `transport::create_buffers`.
pub fn buffer_spec() -> BufferSpec {
    BufferSpec {
        slot: 0,
        kind: "audio",
        size: HEADER_BYTES + (CAPACITY_FRAMES * CHANNELS) as usize * 4,
        header: vec![
            (0, 0),
            (4, SAMPLE_RATE),
            (8, CHANNELS),
            (12, CAPACITY_FRAMES),
        ],
    }
}

/// Whose sound is shared, Discord style: a shared window brings only its own app's sound; a
/// shared screen brings everything except Mudrix (so the call is never fed back into itself).
#[derive(Clone, Copy, Debug)]
pub enum AudioTarget {
    /// Only this process and its children.
    App(u32),
    /// Everything except our own process tree.
    AllButUs,
}

/// The audio thread: record until `stop`, writing into `view`.
pub fn run(view: SharedView, stop: Arc<AtomicBool>, target: AudioTarget) {
    let _com = ComGuard::mta();
    let mut ring = Ring::new(view);
    let mut announced = false;
    while !stop.load(Ordering::SeqCst) {
        let mut source = match Loopback::open(target) {
            Ok(source) => source,
            Err(e) => {
                eprintln!("[capture] no system sound: {e}");
                sleep_unless(&stop, REOPEN_DELAY);
                continue;
            }
        };
        if !announced {
            eprintln!("[capture] system sound: {}", source.describe());
            announced = true;
        }
        if let Err(e) = source.pump(&mut ring, &stop) {
            eprintln!("[capture] system sound stopped ({e}); reopening");
            sleep_unless(&stop, REOPEN_DELAY);
        }
    }
}

fn sleep_unless(stop: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// The ring
// ---------------------------------------------------------------------------

struct Ring {
    view: SharedView,
    /// Total frames written (wrapping), as published at offset 0.
    total: u32,
    /// Next frame index in the ring.
    pos: usize,
}

impl Ring {
    fn new(view: SharedView) -> Ring {
        Ring {
            view,
            total: 0,
            pos: 0,
        }
    }

    fn samples(&mut self) -> &mut [f32] {
        let bytes = self.view.payload();
        let len = (CAPACITY_FRAMES * CHANNELS) as usize;
        // The payload starts 64 bytes into a page-aligned mapping, so it is f32-aligned.
        let (_, samples, _) = unsafe { bytes.align_to_mut::<f32>() };
        let len = len.min(samples.len());
        &mut samples[..len]
    }

    /// Append interleaved stereo frames, then publish the new total (release).
    fn write(&mut self, frames: &[f32]) {
        let channels = CHANNELS as usize;
        let capacity = CAPACITY_FRAMES as usize;
        let count = frames.len() / channels;
        if count == 0 {
            return;
        }
        // More than the ring holds: only the newest second matters.
        let skip = count.saturating_sub(capacity);
        let mut pos = (self.pos + skip) % capacity;
        let mut rest = &frames[skip * channels..count * channels];
        let ring = self.samples();
        if ring.len() < capacity * channels {
            return;
        }
        while !rest.is_empty() {
            let n = (capacity - pos).min(rest.len() / channels);
            ring[pos * channels..(pos + n) * channels].copy_from_slice(&rest[..n * channels]);
            rest = &rest[n * channels..];
            pos = (pos + n) % capacity;
        }
        self.pos = pos;
        self.total = self.total.wrapping_add(count as u32);
        fence(Ordering::Release);
        self.view.store_u32(0, self.total, Ordering::Release);
    }

    fn write_silence(&mut self, frames: usize) {
        let frames = frames.min(CAPACITY_FRAMES as usize);
        if frames > 0 {
            self.write(&vec![0.0; frames * CHANNELS as usize]);
        }
    }
}

// ---------------------------------------------------------------------------
// The WASAPI side
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum SampleKind {
    F32,
    I16,
    I24,
    I32,
}

/// What the client delivers.
#[derive(Clone, Copy)]
struct InFormat {
    rate: u32,
    channels: usize,
    kind: SampleKind,
    block_align: usize,
}

impl InFormat {
    const TARGET: InFormat = InFormat {
        rate: SAMPLE_RATE,
        channels: CHANNELS as usize,
        kind: SampleKind::F32,
        block_align: CHANNELS as usize * 4,
    };

    unsafe fn from_wave(format: *const WAVEFORMATEX) -> Option<InFormat> {
        let f = std::ptr::read_unaligned(format);
        let mut tag = f.wFormatTag;
        if tag == WAVE_FORMAT_EXTENSIBLE && f.cbSize >= 22 {
            let ext = std::ptr::read_unaligned(format as *const WAVEFORMATEXTENSIBLE);
            // KSDATAFORMAT_SUBTYPE_PCM / _IEEE_FLOAT share their tail; Data1 is the format tag.
            tag = ext.SubFormat.data1 as u16;
        }
        let kind = match (tag, f.wBitsPerSample) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleKind::F32,
            (WAVE_FORMAT_PCM, 16) => SampleKind::I16,
            (WAVE_FORMAT_PCM, 24) => SampleKind::I24,
            (WAVE_FORMAT_PCM, 32) => SampleKind::I32,
            _ => return None,
        };
        if f.nChannels == 0 || f.nSamplesPerSec == 0 {
            return None;
        }
        Some(InFormat {
            rate: f.nSamplesPerSec,
            channels: f.nChannels as usize,
            kind,
            block_align: f.nBlockAlign as usize,
        })
    }

    fn sample(&self, frame: &[u8], channel: usize) -> f32 {
        match self.kind {
            SampleKind::F32 => {
                let o = channel * 4;
                f32::from_le_bytes([frame[o], frame[o + 1], frame[o + 2], frame[o + 3]])
            }
            SampleKind::I16 => {
                let o = channel * 2;
                f32::from(i16::from_le_bytes([frame[o], frame[o + 1]])) / 32768.0
            }
            SampleKind::I24 => {
                let o = channel * 3;
                let v = i32::from_le_bytes([0, frame[o], frame[o + 1], frame[o + 2]]) >> 8;
                v as f32 / 8_388_608.0
            }
            SampleKind::I32 => {
                let o = channel * 4;
                let v = i32::from_le_bytes([frame[o], frame[o + 1], frame[o + 2], frame[o + 3]]);
                v as f32 / 2_147_483_648.0
            }
        }
    }

    /// Append `frames` frames of `data` as stereo float (front left/right, plus some centre on
    /// 5.1 and up).
    fn append_stereo(&self, data: &[u8], frames: usize, out: &mut Vec<f32>) {
        let size = self.block_align.max(1);
        for frame in data.chunks_exact(size).take(frames) {
            let (mut l, mut r) = if self.channels >= 2 {
                (self.sample(frame, 0), self.sample(frame, 1))
            } else {
                let m = self.sample(frame, 0);
                (m, m)
            };
            if self.channels >= 6 {
                let c = self.sample(frame, 2) * std::f32::consts::FRAC_1_SQRT_2;
                l = (l + c).clamp(-1.0, 1.0);
                r = (r + c).clamp(-1.0, 1.0);
            }
            out.push(l);
            out.push(r);
        }
    }
}

/// Linear resampling of stereo float to [`SAMPLE_RATE`].
struct Resampler {
    step: f64,
    pos: f64,
    prev: [f32; 2],
}

impl Resampler {
    fn new(rate: u32) -> Resampler {
        Resampler {
            step: f64::from(rate) / f64::from(SAMPLE_RATE),
            pos: 0.0,
            prev: [0.0; 2],
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len() / 2;
        if n == 0 {
            return;
        }
        // Position 0 is the last frame of the previous call, 1.. are `input`.
        while self.pos < n as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a, b) = if i == 0 {
                (self.prev, [input[0], input[1]])
            } else {
                (
                    [input[(i - 1) * 2], input[(i - 1) * 2 + 1]],
                    [input[i * 2], input[i * 2 + 1]],
                )
            };
            out.push(a[0] + (b[0] - a[0]) * frac);
            out.push(a[1] + (b[1] - a[1]) * frac);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = [input[(n - 1) * 2], input[(n - 1) * 2 + 1]];
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(Mutex<Option<mpsc::Sender<()>>>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(
        &self,
        _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        if let Some(tx) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

struct EventHandle(HANDLE);

impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Loopback {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: EventHandle,
    format: InFormat,
    /// Process loopback (without our own sound) rather than the whole device.
    excludes_self: bool,
}

impl Loopback {
    fn open(target: AudioTarget) -> Result<Loopback, String> {
        match unsafe { Self::open_process(target) } {
            Ok(source) => Ok(source),
            // Device loopback is everything, including the call: never a stand-in for one app.
            Err(process_err) if matches!(target, AudioTarget::App(_)) => {
                Err(format!("process loopback: {process_err}"))
            }
            Err(process_err) => unsafe { Self::open_endpoint() }.map_err(|endpoint_err| {
                format!("process loopback: {process_err}; device loopback: {endpoint_err}")
            }),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{} at {} Hz, {} channel(s)",
            if self.excludes_self {
                "everything except Mudrix (process loopback)"
            } else {
                "the default output device, including Mudrix (older Windows)"
            },
            self.format.rate,
            self.format.channels
        )
    }

    fn target_wave() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * CHANNELS * 4,
            nBlockAlign: (CHANNELS * 4) as u16,
            wBitsPerSample: 32,
            cbSize: 0,
        }
    }

    /// One app's process tree, or everything except ours (Windows 10 2004+).
    unsafe fn open_process(target: AudioTarget) -> windows::core::Result<Loopback> {
        let (pid, mode) = match target {
            AudioTarget::App(pid) => (pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE),
            AudioTarget::AllButUs => (
                GetCurrentProcessId(),
                PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            ),
        };
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: mode,
                },
            },
        };
        // A VT_BLOB pointing at `params`; never cleared (it owns nothing).
        let mut prop = ManuallyDrop::new(PROPVARIANT::default());
        {
            let inner = &mut *prop.Anonymous.Anonymous;
            inner.vt = VT_BLOB;
            inner.Anonymous.blob = BLOB {
                cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                pBlobData: &params as *const _ as *mut u8,
            };
        }
        let (tx, rx) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler =
            Activated(Mutex::new(Some(tx))).into();
        let operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*prop as *const PROPVARIANT),
            &handler,
        )?;
        if rx.recv_timeout(Duration::from_secs(3)).is_err() {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_ABORT,
                "process loopback activation timed out",
            ));
        }
        let mut result = HRESULT(0);
        let mut unknown = None;
        operation.GetActivateResult(&mut result, &mut unknown)?;
        result.ok()?;
        let client: IAudioClient = unknown.ok_or_else(windows::core::Error::empty)?.cast()?;
        let wave = Self::target_wave();
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            CLIENT_BUFFER,
            0,
            &wave,
            None,
        )?;
        Self::finish(client, InFormat::TARGET, true)
    }

    /// The whole default output device (includes our own sound).
    unsafe fn open_endpoint() -> windows::core::Result<Loopback> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK;

        // Ask for our format directly and let Windows convert…
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let wave = Self::target_wave();
        if client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                CLIENT_BUFFER,
                0,
                &wave,
                None,
            )
            .is_ok()
        {
            return Self::finish(client, InFormat::TARGET, false);
        }

        // …or take the mix format and convert here.
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let format = InFormat::from_wave(mix);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, CLIENT_BUFFER, 0, mix, None);
        CoTaskMemFree(Some(mix as *const core::ffi::c_void));
        init?;
        let format = format.ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_NOTIMPL,
                "unsupported mix format",
            )
        })?;
        Self::finish(client, format, false)
    }

    unsafe fn finish(
        client: IAudioClient,
        format: InFormat,
        excludes_self: bool,
    ) -> windows::core::Result<Loopback> {
        let event = EventHandle(CreateEventW(None, false, false, None)?);
        client.SetEventHandle(event.0)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(Loopback {
            client,
            capture,
            event,
            format,
            excludes_self,
        })
    }

    /// Copy packets into the ring until `stop` (Ok) or a device error (Err).
    fn pump(&mut self, ring: &mut Ring, stop: &AtomicBool) -> windows::core::Result<()> {
        let mut resampler =
            (self.format.rate != SAMPLE_RATE).then(|| Resampler::new(self.format.rate));
        let mut stereo = Vec::new();
        let mut out = Vec::new();
        let mut covered_until = Instant::now();
        let result = loop {
            if stop.load(Ordering::SeqCst) {
                break Ok(());
            }
            unsafe { WaitForSingleObject(self.event.0, PACKET_WAIT_MS) };
            stereo.clear();
            let mut got = false;
            loop {
                let packet = match unsafe { self.capture.GetNextPacketSize() } {
                    Ok(n) => n,
                    Err(e) => return self.stop_with(Err(e)),
                };
                if packet == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                if let Err(e) = unsafe {
                    self.capture
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                } {
                    return self.stop_with(Err(e));
                }
                let count = frames as usize;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    stereo.resize(stereo.len() + count * 2, 0.0);
                } else {
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            data as *const u8,
                            count * self.format.block_align,
                        )
                    };
                    self.format.append_stereo(bytes, count, &mut stereo);
                }
                if let Err(e) = unsafe { self.capture.ReleaseBuffer(frames) } {
                    return self.stop_with(Err(e));
                }
                got = true;
            }
            if got {
                covered_until = Instant::now();
                match resampler.as_mut() {
                    Some(r) => {
                        out.clear();
                        r.process(&stereo, &mut out);
                        ring.write(&out);
                    }
                    None => ring.write(&stereo),
                }
            } else if covered_until.elapsed() >= IDLE_GAP {
                let gap = covered_until.elapsed();
                ring.write_silence((gap.as_secs_f64() * f64::from(SAMPLE_RATE)) as usize);
                covered_until = Instant::now();
            }
        };
        self.stop_with(result)
    }

    fn stop_with(&self, result: windows::core::Result<()>) -> windows::core::Result<()> {
        unsafe {
            let _ = self.client.Stop();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the system sound for a moment (run with `--ignored` on a Windows desktop).
    #[test]
    #[ignore]
    fn records_system_sound() {
        let _com = ComGuard::mta();
        let spec = buffer_spec();
        let mut ring = Ring::new(SharedView::for_test(spec.size));
        let mut source = Loopback::open().expect("no loopback source");
        println!("source: {}", source.describe());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        source.pump(&mut ring, &stop).expect("pump failed");
        let seconds = started.elapsed().as_secs_f64();
        println!(
            "{} frames in {seconds:.2} s ({:.0} frames/s)",
            ring.total,
            f64::from(ring.total) / seconds
        );
        assert!(ring.total > 0);
    }

    #[test]
    fn resampler_keeps_the_rate() {
        let mut r = Resampler::new(44_100);
        let input = vec![0.5f32; 44_100 * 2];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        let frames = out.len() / 2;
        assert!((47_990..=48_010).contains(&frames), "{frames}");
    }

    #[test]
    fn ring_wraps() {
        let spec = buffer_spec();
        let mut ring = Ring::new(SharedView::for_test(spec.size));
        let chunk = vec![1.0f32; 30_000 * 2];
        ring.write(&chunk);
        ring.write(&chunk);
        assert_eq!(ring.total, 60_000);
        assert_eq!(ring.pos, 12_000);
        assert_eq!(ring.view.load_u32(0), 60_000);
    }
}
