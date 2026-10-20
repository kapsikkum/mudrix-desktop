//! System tray: icon variants, tooltip, menu (Open / Mute notifications / Quit) and
//! the unread badge (tray icon, Windows taskbar overlay, macOS dock badge).

use crate::{commands, settings, MAIN_WINDOW};
use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "main";

const ICON_NORMAL: &[u8] = include_bytes!("../icons/tray/tray.png");
const ICON_UNREAD: &[u8] = include_bytes!("../icons/tray/tray-unread.png");
const ICON_MENTION: &[u8] = include_bytes!("../icons/tray/tray-mention.png");

/// Handle kept so the settings code can reflect "muted" in the menu.
pub struct TrayMenu {
    mute: CheckMenuItem<Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let muted = settings::get(app).notifications_muted;
    let open = MenuItem::with_id(app, "open", "Open Mudrix", true, None::<&str>)?;
    let mute =
        CheckMenuItem::with_id(app, "mute", "Mute notifications", true, muted, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Mudrix", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &mute, &separator, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(ICON_NORMAL)?)
        .tooltip("Mudrix")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => commands::show_main_window(app),
            "mute" => {
                // The check mark has already flipped; persist whatever it shows now.
                if let Some(menu) = app.try_state::<TrayMenu>() {
                    let checked = menu.mute.is_checked().unwrap_or(false);
                    commands::apply_settings(
                        app,
                        &settings::SettingsPatch {
                            notifications_muted: Some(checked),
                            ..Default::default()
                        },
                    );
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                commands::toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(TrayMenu { mute });
    Ok(())
}

/// Reflect the persisted "muted" setting in the menu (when changed from the web UI).
pub fn sync_mute(app: &AppHandle, muted: bool) {
    if let Some(menu) = app.try_state::<TrayMenu>() {
        let _ = menu.mute.set_checked(muted);
    }
}

/// Update every unread indicator. `total` is the unread message count, `highlight` the
/// number of rooms with a mention (0 = plain unread).
pub fn set_unread(app: &AppHandle, total: u32, highlight: u32) {
    let mention = highlight > 0;
    let unread = total > 0 || mention;
    let shown = if total > 0 { total } else { highlight };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let bytes = if mention {
            ICON_MENTION
        } else if unread {
            ICON_UNREAD
        } else {
            ICON_NORMAL
        };
        if let Ok(icon) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
        }
        let tooltip = match (unread, mention) {
            (false, _) => "Mudrix".to_string(),
            (true, false) => format!("Mudrix - {total} unread"),
            (true, true) if total > 0 => {
                format!("Mudrix - {total} unread, {highlight} with mentions")
            }
            (true, true) => format!("Mudrix - {highlight} with mentions"),
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }

    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    #[cfg(windows)]
    {
        if unread {
            let rgba = crate::badge::overlay_rgba(shown, mention);
            let image = Image::new_owned(rgba, crate::badge::SIZE, crate::badge::SIZE);
            let _ = window.set_overlay_icon(Some(image));
        } else {
            let _ = window.set_overlay_icon(None);
        }
    }
    #[cfg(not(windows))]
    {
        // macOS dock badge (on Linux only some launchers honour this).
        let _ = window.set_badge_count(if unread { Some(i64::from(shown)) } else { None });
    }
}
