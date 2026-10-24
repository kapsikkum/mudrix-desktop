//! Windows toast notifications that say "Mudrix".
//!
//! tauri-plugin-notification only gives a toast the app's own id when the exe is installed
//! (not in `target\debug` or `target\release`); otherwise the toast is sent as "Windows
//! PowerShell". So on Windows the process gets an explicit AppUserModelID, which is registered
//! with a name and an icon (unpackaged apps have no shortcut to take them from), and toasts are
//! sent with that id directly. Other platforms keep the plugin.

#[cfg(windows)]
pub use imp::{init, show};

#[cfg(windows)]
mod imp {
    use tauri::{AppHandle, Manager};
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    /// The app icon, written to a stable path for the toast header.
    const TOAST_ICON: &[u8] = include_bytes!("../icons/128x128.png");

    /// Set the process AppUserModelID and register it (name and icon) for unpackaged toasts.
    /// Idempotent; failures are logged, a toast then just shows the fallback name.
    pub fn init(app: &AppHandle) {
        let id = app.config().identifier.clone();
        if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(&id)) } {
            eprintln!("[desktop] could not set the AppUserModelID: {e}");
        }
        let icon = write_icon(app);
        if let Err(e) = register(&id, icon.as_deref()) {
            eprintln!("[desktop] could not register the toast identity: {e}");
        }
    }

    /// The icon as a file (WinRT toasts take a path), or `None` when it cannot be written.
    fn write_icon(app: &AppHandle) -> Option<String> {
        let dir = app.path().app_local_data_dir().ok()?;
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("toast-icon.png");
        let current = std::fs::read(&path).ok();
        if current.as_deref() != Some(TOAST_ICON) {
            std::fs::write(&path, TOAST_ICON).ok()?;
        }
        Some(path.to_string_lossy().into_owned())
    }

    /// `HKCU\Software\Classes\AppUserModelId\<id>` with `DisplayName` and `IconUri`.
    fn register(id: &str, icon: Option<&str>) -> Result<(), String> {
        let subkey = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{id}"));
        let mut key = HKEY::default();
        unsafe {
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &subkey,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            );
            if status != ERROR_SUCCESS {
                return Err(format!("RegCreateKeyExW failed: {}", status.0));
            }
            let mut result = set_string(key, w!("DisplayName"), "Mudrix");
            if let Some(icon) = icon {
                result = result.and(set_string(key, w!("IconUri"), icon));
            }
            let _ = RegCloseKey(key);
            result
        }
    }

    unsafe fn set_string(key: HKEY, name: PCWSTR, value: &str) -> Result<(), String> {
        // REG_SZ data is UTF-16 including the terminating zero.
        let wide: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        let status = RegSetValueExW(key, name, None, REG_SZ, Some(&wide));
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("RegSetValueExW failed: {}", status.0))
        }
    }

    /// Avatar files kept for toasts; older ones are deleted past this.
    const AVATAR_CACHE_MAX: usize = 64;

    /// The sender's avatar (PNG bytes from the web app) as a file, since an unpackaged app's toast
    /// can only show a local image. Named by content, so a repeat sender reuses the file.
    fn write_avatar(app: &AppHandle, png: &[u8]) -> Option<std::path::PathBuf> {
        use std::hash::{Hash, Hasher};
        if !png.starts_with(&[0x89, b'P', b'N', b'G']) {
            return None;
        }
        let dir = app.path().app_local_data_dir().ok()?.join("toast-avatars");
        std::fs::create_dir_all(&dir).ok()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        png.hash(&mut hasher);
        let path = dir.join(format!("{:016x}.png", hasher.finish()));
        if !path.exists() {
            std::fs::write(&path, png).ok()?;
            prune_avatars(&dir);
        }
        Some(path)
    }

    fn prune_avatars(dir: &std::path::Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if files.len() <= AVATAR_CACHE_MAX {
            return;
        }
        files.sort();
        for (_, path) in &files[..files.len() - AVATAR_CACHE_MAX] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Send a toast as "Mudrix", with the sender's avatar in place of the app icon when given.
    /// Clicking it brings the window back, which opens the room.
    pub fn show(
        app: &AppHandle,
        title: &str,
        body: &str,
        avatar: Option<&[u8]>,
    ) -> Result<(), String> {
        let id = app.config().identifier.clone();
        let handle = app.clone();
        let mut toast = tauri_winrt_notification::Toast::new(&id);
        if let Some(path) = avatar.and_then(|png| write_avatar(app, png)) {
            toast = toast.icon(&path, tauri_winrt_notification::IconCrop::Circular, title);
        }
        toast
            .title(title)
            .text1(body)
            // Silent: the web app plays the chosen sound pack itself, so Windows' own chime
            // would double it.
            .sound(None)
            .on_activated(move |_| {
                crate::commands::show_main_window(&handle);
                Ok(())
            })
            .show()
            .map_err(|e| e.to_string())
    }
}
