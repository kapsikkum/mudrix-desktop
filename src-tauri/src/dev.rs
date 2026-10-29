//! What tells a dev build (`tauri dev`) from the installed app, so the two do not group together on
//! the taskbar: its own app id, a name that says "Dev" and an icon with a badge on it. Release
//! builds are unchanged.

use tauri::image::Image;

pub const IS_DEV: bool = cfg!(debug_assertions);

const WINDOW_ICON: &[u8] = include_bytes!("../icons/dev/icon.png");
pub const TOAST_ICON: &[u8] = include_bytes!("../icons/dev/128x128.png");

/// The icon for the window and taskbar, or `None` in a release build.
pub fn window_icon() -> Option<Image<'static>> {
    IS_DEV
        .then(|| Image::from_bytes(WINDOW_ICON).ok())
        .flatten()
}

/// The app's name where it is shown to people (toasts, the tray tooltip).
pub fn name() -> &'static str {
    if IS_DEV {
        "Mudrix Dev"
    } else {
        "Mudrix"
    }
}

/// The Windows AppUserModelID, which is what the taskbar groups windows (and toasts) by.
pub fn app_id(identifier: &str) -> String {
    if IS_DEV {
        format!("{identifier}.dev")
    } else {
        identifier.to_owned()
    }
}
