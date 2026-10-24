//! What can be shared: every monitor and every window the user would see in Alt+Tab, minus
//! our own, with thumbnails and app icons for the picker.
//!
//! The picker asks again every couple of seconds, so this has to be quick. Thumbnails come from
//! GDI (`StretchBlt` of the screen; `PrintWindow` with `PW_RENDERFULLCONTENT` for windows, which
//! also gets GPU-drawn and covered windows), shrunk by GDI's halftone stretch and encoded as
//! JPEG, several sources at a time. Windows that come out black there get one
//! Windows.Graphics.Capture frame instead, but only where the yellow capture border can be
//! turned off (Windows 11); on Windows 10 that border would flash around them every refresh.
//! App names and icons are cached between calls.

use super::gpu::{downscale_bgra, Gpu};
use super::session::{self, ComGuard};
use super::CaptureSource;
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::{w, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Foundation::Metadata::ApiInformation;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush,
    GetDC, GetMonitorInfoW, ReleaseDC, SelectObject, SetBrushOrgEx, SetStretchBltMode, StretchBlt,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, HGDIOBJ,
    HMONITOR, MONITORINFO, SRCCOPY,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, EnumWindows, FindWindowExW, GetClassLongPtrW, GetClassNameW,
    GetShellWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible,
    SendMessageTimeoutW, DI_MASK, DI_NORMAL, GCLP_HICON, GCLP_HICONSM, GWL_EXSTYLE, GW_OWNER,
    HICON, SMTO_ABORTIFHUNG, WM_GETICON, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Thumbnail width bounds.
const MIN_THUMB: u32 = 16;
const MAX_THUMB: u32 = 1920;
const JPEG_QUALITY: u8 = 72;
const ICON_SIZE: i32 = 32;
/// Thumbnails not done by then are left out (a hung window must not hold up the picker).
const THUMB_DEADLINE: Duration = Duration::from_millis(1500);
/// Budget for the Windows.Graphics.Capture retries of black thumbnails.
const STILL_BUDGET: Duration = Duration::from_millis(600);
const STILL_TIMEOUT: Duration = Duration::from_millis(250);
/// How long a window may take to answer WM_GETICON.
const ICON_TIMEOUT_MS: u32 = 100;
const MAX_WORKERS: usize = 8;
/// `PrintWindow`: include content drawn by the GPU (Windows 8.1+).
const PW_RENDERFULLCONTENT: u32 = 2;
const MONITORINFOF_PRIMARY: u32 = 1;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;
const ICON_SMALL2: usize = 2;
/// Shell windows that are never shareable on their own.
const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// A monitor or window, as named by its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Screen(isize),
    Window(isize),
}

impl Source {
    /// `screen:<HMONITOR>` or `window:<HWND>`.
    pub fn parse(id: &str) -> Option<Source> {
        let (kind, value) = id.split_once(':')?;
        let value: isize = value.parse().ok()?;
        match kind {
            "screen" => Some(Source::Screen(value)),
            "window" => Some(Source::Window(value)),
            _ => None,
        }
    }

    pub fn id(&self) -> String {
        match self {
            Source::Screen(h) => format!("screen:{h}"),
            Source::Window(h) => format!("window:{h}"),
        }
    }

    pub fn exists(&self) -> bool {
        match *self {
            Source::Screen(h) => monitors().iter().any(|m| m.handle == h),
            Source::Window(h) => unsafe { IsWindow(Some(hwnd(h))) }.as_bool(),
        }
    }
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut core::ffi::c_void)
}

// ---------------------------------------------------------------------------
// Monitors
// ---------------------------------------------------------------------------

pub struct Monitor {
    pub handle: isize,
    pub rect: RECT,
    pub primary: bool,
}

/// Every monitor, left to right.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn visit(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            list.push(Monitor {
                handle: monitor.0 as isize,
                rect: info.rcMonitor,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        BOOL(1)
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(visit),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list.sort_by_key(|m| (m.rect.left, m.rect.top));
    list
}

/// The widest and the tallest monitor size (a window can grow to that).
pub fn largest_monitor() -> (u32, u32) {
    monitors().iter().fold((0, 0), |(w, h), m| {
        let (mw, mh) = size_of_rect(&m.rect);
        (w.max(mw), h.max(mh))
    })
}

fn size_of_rect(r: &RECT) -> (u32, u32) {
    (
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

struct WindowInfo {
    hwnd: isize,
    title: String,
    pid: u32,
    /// Visible bounds (without the invisible resize border).
    rect: RECT,
}

/// Windows that would show in Alt+Tab, front to back, minus `excluded` processes.
fn shareable_windows(excluded: &HashSet<u32>) -> Vec<WindowInfo> {
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<isize>);
        list.push(window.0 as isize);
        BOOL(1)
    }
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut all as *mut _ as isize));
    }
    let shell = unsafe { GetShellWindow() }.0 as isize;
    all.into_iter()
        .filter(|&h| h != shell)
        .filter_map(|h| unsafe { shareable(h, excluded) })
        .collect()
}

unsafe fn shareable(h: isize, excluded: &HashSet<u32>) -> Option<WindowInfo> {
    let window = hwnd(h);
    if !IsWindowVisible(window).as_bool() || IsIconic(window).as_bool() {
        return None;
    }
    let ex = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
    let app_window = ex & WS_EX_APPWINDOW.0 != 0;
    if ex & WS_EX_TOOLWINDOW.0 != 0 || (ex & WS_EX_NOACTIVATE.0 != 0 && !app_window) {
        return None;
    }
    let owned = GetWindow(window, GW_OWNER).is_ok_and(|o| !o.is_invalid());
    if owned && !app_window {
        return None;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(
        window,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut core::ffi::c_void,
        4,
    )
    .is_ok()
        && cloaked != 0
    {
        return None;
    }
    if SHELL_CLASSES.contains(&class_name(window).as_str()) {
        return None;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(window, Some(&mut pid));
    if excluded.contains(&pid) {
        return None;
    }
    let title = window_title(window)?;
    let rect = frame_bounds(window)?;
    let (w, h) = size_of_rect(&rect);
    if w < 2 || h < 2 {
        return None;
    }
    Some(WindowInfo {
        hwnd: h_of(window),
        title,
        pid,
        rect,
    })
}

fn h_of(window: HWND) -> isize {
    window.0 as isize
}

unsafe fn window_title(window: HWND) -> Option<String> {
    let len = GetWindowTextLengthW(window);
    if len <= 0 {
        return None;
    }
    let mut buffer = vec![0u16; len as usize + 1];
    let copied = GetWindowTextW(window, &mut buffer);
    let title = String::from_utf16_lossy(&buffer[..copied.max(0) as usize]);
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

unsafe fn class_name(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = GetClassNameW(window, &mut buffer);
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

/// The visible bounds (DWM's extended frame), or the window rect.
unsafe fn frame_bounds(window: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    if DwmGetWindowAttribute(
        window,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut RECT as *mut core::ffi::c_void,
        std::mem::size_of::<RECT>() as u32,
    )
    .is_ok()
    {
        return Some(rect);
    }
    GetWindowRect(window, &mut rect).ok()?;
    Some(rect)
}

/// Our process and everything it started (the WebView2 processes, DevTools…).
fn own_process_tree() -> HashSet<u32> {
    let me = unsafe { GetCurrentProcessId() };
    let mut tree = HashSet::from([me]);
    let mut pairs = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return tree;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    loop {
        let before = tree.len();
        for &(pid, parent) in &pairs {
            if pid != 0 && tree.contains(&parent) {
                tree.insert(pid);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}

/// The app behind a shareable window, for sharing only that app's sound.
pub fn window_app_pid(h: isize) -> Option<u32> {
    let window = hwnd(h);
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
        (pid != 0).then(|| app_pid(window, pid))
    }
}

/// The process that really owns the window: for UWP apps that is not ApplicationFrameHost but
/// the app inside it.
unsafe fn app_pid(window: HWND, pid: u32) -> u32 {
    if class_name(window) != "ApplicationFrameWindow" {
        return pid;
    }
    match FindWindowExW(
        Some(window),
        None,
        w!("Windows.UI.Core.CoreWindow"),
        PCWSTR::null(),
    ) {
        Ok(core) if !core.is_invalid() => {
            let mut inner = 0u32;
            GetWindowThreadProcessId(core, Some(&mut inner));
            if inner != 0 {
                inner
            } else {
                pid
            }
        }
        _ => pid,
    }
}

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

fn name_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The app's name: its FileDescription ("Google Chrome"), else the exe name.
fn app_name(exe: &str) -> Option<String> {
    if let Some(name) = name_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(exe)
    {
        return name.clone();
    }
    let name = unsafe { file_description(exe) }.or_else(|| {
        std::path::Path::new(exe)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
    });
    name_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(exe.to_string(), name.clone());
    name
}

unsafe fn file_description(exe: &str) -> Option<String> {
    let path = HSTRING::from(exe);
    let size = GetFileVersionInfoSizeW(&path, None);
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    GetFileVersionInfoW(&path, None, size, data.as_mut_ptr() as *mut _).ok()?;

    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    let mut translations = Vec::new();
    if VerQueryValueW(
        data.as_ptr() as *const _,
        w!("\\VarFileInfo\\Translation"),
        &mut ptr,
        &mut len,
    )
    .as_bool()
        && !ptr.is_null()
    {
        let pairs = std::slice::from_raw_parts(ptr as *const u16, len as usize / 2);
        for pair in pairs.chunks_exact(2) {
            translations.push((pair[0], pair[1]));
        }
    }
    // Common fallbacks: US English with Unicode, and with Western codepage.
    translations.extend([(0x0409, 0x04B0), (0x0409, 0x04E4)]);
    for (language, codepage) in translations {
        let key = HSTRING::from(format!(
            "\\StringFileInfo\\{language:04x}{codepage:04x}\\FileDescription"
        ));
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if VerQueryValueW(data.as_ptr() as *const _, &key, &mut ptr, &mut len).as_bool()
            && !ptr.is_null()
            && len > 0
        {
            let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
            let text = String::from_utf16_lossy(text);
            let text = text.trim_end_matches('\0').trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// GDI drawing surface
// ---------------------------------------------------------------------------

/// A top-down 32-bit DIB selected into its own memory DC.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Dib {
    unsafe fn new(width: i32, height: i32) -> Option<Dib> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) if !bits.is_null() => bitmap,
            _ => {
                let _ = DeleteDC(dc);
                return None;
            }
        };
        let old = SelectObject(dc, bitmap.into());
        Some(Dib {
            dc,
            bitmap,
            old,
            bits: bits as *mut u8,
            width,
            height,
        })
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, (self.width * self.height * 4) as usize)
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

// ---------------------------------------------------------------------------
// Thumbnails and icons
// ---------------------------------------------------------------------------

/// A screen area shrunk to `tw`×`th`, BGRA.
unsafe fn screen_thumb(rect: RECT, tw: i32, th: i32) -> Option<Vec<u8>> {
    let mut thumb = Dib::new(tw, th)?;
    let screen = GetDC(None);
    if screen.is_invalid() {
        return None;
    }
    SetStretchBltMode(thumb.dc, HALFTONE);
    let _ = SetBrushOrgEx(thumb.dc, 0, 0, None);
    let (w, h) = size_of_rect(&rect);
    let ok = StretchBlt(
        thumb.dc,
        0,
        0,
        tw,
        th,
        Some(screen),
        rect.left,
        rect.top,
        w as i32,
        h as i32,
        SRCCOPY,
    )
    .as_bool();
    ReleaseDC(None, screen);
    ok.then(|| thumb.bytes().to_vec())
}

/// A window (its visible part, `frame`) drawn by `PrintWindow`, shrunk to `tw`×`th`, BGRA.
unsafe fn window_thumb(window: HWND, frame: RECT, tw: i32, th: i32) -> Option<Vec<u8>> {
    if IsHungAppWindow(window).as_bool() {
        return None;
    }
    let mut outer = RECT::default();
    GetWindowRect(window, &mut outer).ok()?;
    let (ww, wh) = size_of_rect(&outer);
    let full = Dib::new(ww as i32, wh as i32)?;
    if !PrintWindow(window, full.dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool() {
        return None;
    }
    // Leave out the invisible resize border around the visible frame.
    let x = (frame.left - outer.left).clamp(0, ww as i32 - 1);
    let y = (frame.top - outer.top).clamp(0, wh as i32 - 1);
    let (fw, fh) = size_of_rect(&frame);
    let w = (fw as i32).min(ww as i32 - x).max(1);
    let h = (fh as i32).min(wh as i32 - y).max(1);

    let mut thumb = Dib::new(tw, th)?;
    SetStretchBltMode(thumb.dc, HALFTONE);
    let _ = SetBrushOrgEx(thumb.dc, 0, 0, None);
    if !StretchBlt(thumb.dc, 0, 0, tw, th, Some(full.dc), x, y, w, h, SRCCOPY).as_bool() {
        return None;
    }
    drop(full);
    Some(thumb.bytes().to_vec())
}

/// Nothing but (near) black: what GDI gives for windows it cannot draw.
fn looks_black(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4)
        .step_by(7)
        .all(|p| p[0] < 12 && p[1] < 12 && p[2] < 12)
}

fn jpeg_data_url(bgra: &[u8], width: u32, height: u32) -> Option<String> {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for p in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[p[2], p[1], p[0]]);
    }
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode(&rgb, width, height, ExtendedColorType::Rgb8)
        .ok()?;
    Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(out)
    ))
}

/// The window's icon (what the taskbar shows), else its exe's, as a 32 px PNG data URL.
unsafe fn window_icon(window: HWND, exe: Option<&str>) -> Option<String> {
    let mut icon = HICON::default();
    for which in [ICON_BIG, ICON_SMALL2, ICON_SMALL] {
        let mut result = 0usize;
        let answered = SendMessageTimeoutW(
            window,
            WM_GETICON,
            WPARAM(which),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            ICON_TIMEOUT_MS,
            Some(&mut result),
        );
        if answered.0 != 0 && result != 0 {
            icon = HICON(result as *mut _);
            break;
        }
    }
    if icon.is_invalid() {
        for index in [GCLP_HICON, GCLP_HICONSM] {
            let handle = GetClassLongPtrW(window, index);
            if handle != 0 {
                icon = HICON(handle as *mut _);
                break;
            }
        }
    }
    if !icon.is_invalid() {
        return render_icon(icon);
    }
    // Ours to destroy.
    let exe = HSTRING::from(exe?);
    let mut info = SHFILEINFOW::default();
    let found = SHGetFileInfoW(
        &exe,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        Some(&mut info),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON,
    );
    if found == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let png = render_icon(info.hIcon);
    let _ = DestroyIcon(info.hIcon);
    png
}

unsafe fn render_icon(icon: HICON) -> Option<String> {
    let size = ICON_SIZE;
    let mut color = Dib::new(size, size)?;
    DrawIconEx(color.dc, 0, 0, icon, size, size, 0, None, DI_NORMAL).ok()?;
    let mut pixels = color.bytes().to_vec();
    if pixels.chunks_exact(4).all(|p| p[3] == 0) {
        // An old icon without alpha: opacity comes from its mask (black = opaque).
        let mut mask = Dib::new(size, size)?;
        mask.bytes().fill(0xFF);
        DrawIconEx(mask.dc, 0, 0, icon, size, size, 0, None, DI_MASK).ok()?;
        for (p, m) in pixels.chunks_exact_mut(4).zip(mask.bytes().chunks_exact(4)) {
            p[3] = if m[0] == 0 && m[1] == 0 && m[2] == 0 {
                255
            } else {
                0
            };
        }
    } else {
        // Drawn premultiplied onto transparent black; undo that.
        for p in pixels.chunks_exact_mut(4) {
            let a = u32::from(p[3]);
            if a > 0 && a < 255 {
                for c in &mut p[..3] {
                    *c = (u32::from(*c) * 255 / a).min(255) as u8;
                }
            }
        }
    }
    for p in pixels.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&pixels, size as u32, size as u32, ExtendedColorType::Rgba8)
        .ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(out)
    ))
}

/// Icons by (window, process id), so a reused window handle does not get a stale icon.
type IconCache = Mutex<HashMap<(isize, u32), Option<String>>>;

fn icon_cache() -> &'static IconCache {
    static CACHE: OnceLock<IconCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

enum Target {
    Screen(RECT),
    Window(isize, RECT),
}

/// Work for one source, done on a worker thread.
struct Job {
    index: usize,
    target: Option<(Target, i32, i32)>,
    /// Fetch the icon (not cached yet): window, cache key, exe path.
    icon: Option<(isize, (isize, u32), Option<String>)>,
}

struct Done {
    index: usize,
    thumb: Option<String>,
    /// GDI gave nothing usable for this window.
    retry_window: Option<isize>,
    icon: Option<((isize, u32), Option<String>)>,
}

fn thumb_size(thumb_width: u32, width: u32, height: u32) -> (i32, i32) {
    let tw = thumb_width.clamp(MIN_THUMB, MAX_THUMB);
    let th = (u64::from(tw) * u64::from(height.max(1)) / u64::from(width.max(1)))
        .clamp(1, u64::from(tw) * 4);
    (tw as i32, th as i32)
}

fn run_job(job: &Job) -> Done {
    let mut done = Done {
        index: job.index,
        thumb: None,
        retry_window: None,
        icon: None,
    };
    if let Some((target, tw, th)) = &job.target {
        let (tw, th) = (*tw, *th);
        let pixels = unsafe {
            match target {
                Target::Screen(rect) => screen_thumb(*rect, tw, th),
                Target::Window(h, frame) => window_thumb(hwnd(*h), *frame, tw, th),
            }
        };
        match (pixels, target) {
            (Some(p), target) => {
                if let Target::Window(h, _) = target {
                    if looks_black(&p) {
                        // Keep the black one unless the retry does better.
                        done.retry_window = Some(*h);
                    }
                }
                done.thumb = jpeg_data_url(&p, tw as u32, th as u32);
            }
            (None, Target::Window(h, _)) => done.retry_window = Some(*h),
            (None, Target::Screen(_)) => {}
        }
    }
    if let Some((h, key, exe)) = &job.icon {
        done.icon = Some((*key, unsafe { window_icon(hwnd(*h), exe.as_deref()) }));
    }
    done
}

/// Run `jobs` on a few threads; whatever is not done by the deadline is left out (its thread
/// finishes on its own).
fn run_jobs(jobs: Vec<Job>) -> Vec<Done> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let count = jobs.len();
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, MAX_WORKERS)
        .min(count);
    let jobs = Arc::new(jobs);
    let next = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    for _ in 0..workers {
        let (jobs, next, tx) = (jobs.clone(), next.clone(), tx.clone());
        let spawned = std::thread::Builder::new()
            .name("capture-thumbs".into())
            .spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(job) = jobs.get(i) else {
                    break;
                };
                if tx.send(run_job(job)).is_err() {
                    break;
                }
            });
        if let Err(e) = spawned {
            eprintln!("[capture] could not start a thumbnail thread: {e}");
        }
    }
    drop(tx);
    let deadline = Instant::now() + THUMB_DEADLINE;
    let mut results = Vec::with_capacity(count);
    while results.len() < count {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(done) => results.push(done),
            Err(_) => break,
        }
    }
    results
}

/// Windows 11 lets a capture run without the yellow border.
fn borderless_capture() -> bool {
    ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from("IsBorderRequired"),
    )
    .unwrap_or(false)
}

/// Every monitor, then every shareable window front to back.
pub fn list(thumb_width: u32) -> Result<Vec<CaptureSource>, String> {
    let _com = ComGuard::mta();
    let started = Instant::now();
    let mut out: Vec<CaptureSource> = Vec::new();
    let mut jobs = Vec::new();

    for (i, monitor) in monitors().into_iter().enumerate() {
        let (width, height) = size_of_rect(&monitor.rect);
        let target = (thumb_width > 0).then(|| {
            let (tw, th) = thumb_size(thumb_width, width, height);
            (Target::Screen(monitor.rect), tw, th)
        });
        jobs.push(Job {
            index: out.len(),
            target,
            icon: None,
        });
        out.push(CaptureSource {
            id: Source::Screen(monitor.handle).id(),
            kind: "screen",
            name: format!("Screen {}", i + 1),
            app_name: None,
            icon: None,
            thumb: None,
            width,
            height,
            primary: monitor.primary,
        });
    }

    let excluded = own_process_tree();
    let mut keys = HashSet::new();
    for window in shareable_windows(&excluded) {
        let pid = unsafe { app_pid(hwnd(window.hwnd), window.pid) };
        let exe = exe_path(pid);
        let app = exe.as_deref().and_then(app_name);
        let key = (window.hwnd, window.pid);
        keys.insert(key);
        let cached = icon_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        let (width, height) = size_of_rect(&window.rect);
        let target = (thumb_width > 0).then(|| {
            let (tw, th) = thumb_size(thumb_width, width, height);
            (Target::Window(window.hwnd, window.rect), tw, th)
        });
        jobs.push(Job {
            index: out.len(),
            target,
            icon: cached.is_none().then(|| (window.hwnd, key, exe.clone())),
        });
        out.push(CaptureSource {
            id: Source::Window(window.hwnd).id(),
            kind: "window",
            name: window.title,
            app_name: app,
            icon: cached.flatten(),
            thumb: None,
            width,
            height,
            primary: false,
        });
    }

    let mut retry = Vec::new();
    {
        let results = run_jobs(jobs);
        let mut icons = icon_cache().lock().unwrap_or_else(|e| e.into_inner());
        for done in results {
            let Some(source) = out.get_mut(done.index) else {
                continue;
            };
            if done.thumb.is_some() {
                source.thumb = done.thumb;
            }
            if let Some((key, icon)) = done.icon {
                source.icon = icon.clone();
                icons.insert(key, icon);
            }
            if let Some(h) = done.retry_window {
                retry.push((done.index, h));
            }
        }
        icons.retain(|key, _| keys.contains(key));
    }

    if thumb_width > 0 && !retry.is_empty() && borderless_capture() {
        retry_with_capture(&mut out, &retry, thumb_width);
    }

    let took = started.elapsed();
    if took > Duration::from_millis(500) {
        eprintln!(
            "[capture] listing {} sources took {} ms",
            out.len(),
            took.as_millis()
        );
    }
    Ok(out)
}

/// Thumbnails GDI could not make, from one Windows.Graphics.Capture frame each.
fn retry_with_capture(out: &mut [CaptureSource], retry: &[(usize, isize)], thumb_width: u32) {
    let gpu = match Gpu::create() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("[capture] no GPU for thumbnails: {e}");
            return;
        }
    };
    let deadline = Instant::now() + STILL_BUDGET;
    for &(index, h) in retry {
        if Instant::now() >= deadline {
            break;
        }
        let Some((pixels, w, h)) = session::grab_still(&gpu, Source::Window(h), STILL_TIMEOUT)
        else {
            continue;
        };
        let (tw, th) = thumb_size(thumb_width, w, h);
        let mut small = vec![0u8; tw as usize * th as usize * 4];
        downscale_bgra(
            &pixels,
            w,
            h,
            w as usize * 4,
            &mut small,
            tw as u32,
            th as u32,
            tw as usize * 4,
        );
        if let Some(source) = out.get_mut(index) {
            source.thumb = jpeg_data_url(&small, tw as u32, th as u32);
        }
    }
}
