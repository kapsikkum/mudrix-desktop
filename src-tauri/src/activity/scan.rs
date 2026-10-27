//! Windows: the programs that have a window on screen, with where they live and since when.
//!
//! Only programs with a real window are looked at, which keeps background helpers out and the
//! scan cheap (a hundred or so exe paths, remembered per process id between scans).

use std::collections::{HashMap, HashSet};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

#[derive(Debug, Clone)]
pub struct WindowedProcess {
    pub exe_file: String,
    pub path: String,
    /// Titles of its visible windows.
    pub titles: Vec<String>,
    /// Epoch milliseconds, 0 when unknown.
    pub started_ms: u64,
    /// One of its windows is the one in front.
    pub foreground: bool,
}

/// Remembers each process's path and start time, which never change while it runs.
#[derive(Default)]
pub struct Scanner {
    known: HashMap<u32, Option<(String, u64)>>,
}

impl Scanner {
    pub fn scan(&mut self) -> Vec<WindowedProcess> {
        let own = unsafe { GetCurrentProcessId() };
        let foreground_pid = unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
            pid
        };

        let mut titles: HashMap<u32, Vec<String>> = HashMap::new();
        for window in visible_windows() {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
            if pid == 0 || pid == own {
                continue;
            }
            let list = titles.entry(pid).or_default();
            let title = window_title(window);
            if !title.is_empty() && !list.contains(&title) {
                list.push(title);
            }
        }

        let alive: HashSet<u32> = titles.keys().copied().collect();
        self.known.retain(|pid, _| alive.contains(pid));

        titles
            .into_iter()
            .filter_map(|(pid, titles)| {
                let entry = self.known.entry(pid).or_insert_with(|| process_info(pid));
                let (path, started_ms) = entry.clone()?;
                let exe_file = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
                Some(WindowedProcess {
                    exe_file,
                    path,
                    titles,
                    started_ms,
                    foreground: pid == foreground_pid,
                })
            })
            .collect()
    }
}

fn visible_windows() -> Vec<HWND> {
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> windows::core::BOOL {
        let list = &mut *(data.0 as *mut Vec<HWND>);
        if IsWindowVisible(window).as_bool() && is_user_window(window) {
            list.push(window);
        }
        true.into()
    }
    let mut list: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// Not a tool window and not a suspended (cloaked) app window.
unsafe fn is_user_window(window: HWND) -> bool {
    let ex = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 {
        return false;
    }
    let mut cloaked = 0u32;
    let known = DwmGetWindowAttribute(
        window,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut core::ffi::c_void,
        4,
    )
    .is_ok();
    !(known && cloaked != 0)
}

fn window_title(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    let copied = unsafe { GetWindowTextW(window, &mut buffer) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

/// Full exe path and start time (epoch ms) of a process, or None when it can't be opened.
fn process_info(pid: u32) -> Option<(String, u64)> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let path = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
        .ok()
        .map(|()| String::from_utf16_lossy(&buffer[..len as usize]));

        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let started = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user)
            .ok()
            .map(|()| filetime_to_epoch_ms(&created))
            .unwrap_or(0);
        let _ = CloseHandle(process);
        Some((path?, started))
    }
}

fn filetime_to_epoch_ms(ft: &FILETIME) -> u64 {
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    // 100 ns ticks since 1601-01-01 -> ms since 1970-01-01
    (ticks / 10_000).saturating_sub(11_644_473_600_000)
}
