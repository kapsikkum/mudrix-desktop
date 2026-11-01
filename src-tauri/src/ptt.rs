//! Push to talk heard system-wide, so the key works while another app has the focus.
//!
//! The web app hands over its binding (a `KeyboardEvent.code`, possibly a combo such as
//! `Ctrl+KeyV`; `parseCombo` in the web client's `keyCombo.ts` is the format) with `ptt_set_key`.
//! A listener thread then tells the main window (and the call's window, if it is open) with the
//! `push-to-talk` event: `true` when the binding goes down, `false` when it comes up.
//!
//! The listener is `rdev::listen`: it only watches, so the key still reaches whatever app is in
//! front (`tauri-plugin-global-shortcut` would swallow it). `listen` cannot be cancelled, so there
//! is one thread for the life of the process, started the first time a key is set; with no key set
//! it only keeps track of which keys are down. If `listen` fails (no X server, no permission) the
//! thread ends and the next `ptt_set_key` tries again.
//!
//! The matching mirrors the web client's own: the binding goes down when its key is pressed with
//! exactly its modifiers held (the key's own kind of modifier does not count), ignoring
//! auto-repeat, and comes up when its key, or any modifier of the combo, is released.

use crate::popout::POPOUT_WINDOW;
use crate::MAIN_WINDOW;
use rdev::{EventType, Key};
use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// The event the windows hear: `true` = the binding went down, `false` = it went up.
const PTT_EVENT: &str = "push-to-talk";

/// How long to wait for `listen` to fail straight away (no display, no permission) before
/// reporting the listener as active.
const START_GRACE: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Binding
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

impl Modifier {
    fn bit(self) -> u8 {
        1 << self as u8
    }

    fn from_name(name: &str) -> Option<Modifier> {
        match name {
            "Ctrl" => Some(Modifier::Ctrl),
            "Alt" => Some(Modifier::Alt),
            "Shift" => Some(Modifier::Shift),
            "Meta" => Some(Modifier::Meta),
            _ => None,
        }
    }

    /// Which modifier a physical key is one of (either side).
    fn of(key: Key) -> Option<Modifier> {
        match key {
            Key::ControlLeft | Key::ControlRight => Some(Modifier::Ctrl),
            Key::Alt | Key::AltGr => Some(Modifier::Alt),
            Key::ShiftLeft | Key::ShiftRight => Some(Modifier::Shift),
            Key::MetaLeft | Key::MetaRight => Some(Modifier::Meta),
            _ => None,
        }
    }
}

/// A key plus the modifiers (a bit set of `Modifier::bit`) that must be held with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Binding {
    key: Key,
    modifiers: u8,
}

/// The key a legacy one-character binding stands for, as a `KeyboardEvent.code`.
fn legacy_char_code(c: char) -> String {
    match c {
        'a'..='z' | 'A'..='Z' => format!("Key{}", c.to_ascii_uppercase()),
        '0'..='9' => format!("Digit{c}"),
        ' ' => "Space".into(),
        '`' => "Backquote".into(),
        '-' => "Minus".into(),
        '=' => "Equal".into(),
        '[' => "BracketLeft".into(),
        ']' => "BracketRight".into(),
        '\\' => "Backslash".into(),
        ';' => "Semicolon".into(),
        '\'' => "Quote".into(),
        ',' => "Comma".into(),
        '.' => "Period".into(),
        '/' => "Slash".into(),
        other => other.to_string(),
    }
}

/// `KeyboardEvent.code` to the key `rdev` reports. Codes `rdev` has no key for (F13 and up,
/// the numpad's `=` and `,`, media keys) come back as `None`.
fn key_from_code(code: &str) -> Option<Key> {
    Some(match code {
        "KeyA" => Key::KeyA,
        "KeyB" => Key::KeyB,
        "KeyC" => Key::KeyC,
        "KeyD" => Key::KeyD,
        "KeyE" => Key::KeyE,
        "KeyF" => Key::KeyF,
        "KeyG" => Key::KeyG,
        "KeyH" => Key::KeyH,
        "KeyI" => Key::KeyI,
        "KeyJ" => Key::KeyJ,
        "KeyK" => Key::KeyK,
        "KeyL" => Key::KeyL,
        "KeyM" => Key::KeyM,
        "KeyN" => Key::KeyN,
        "KeyO" => Key::KeyO,
        "KeyP" => Key::KeyP,
        "KeyQ" => Key::KeyQ,
        "KeyR" => Key::KeyR,
        "KeyS" => Key::KeyS,
        "KeyT" => Key::KeyT,
        "KeyU" => Key::KeyU,
        "KeyV" => Key::KeyV,
        "KeyW" => Key::KeyW,
        "KeyX" => Key::KeyX,
        "KeyY" => Key::KeyY,
        "KeyZ" => Key::KeyZ,
        "Digit0" => Key::Num0,
        "Digit1" => Key::Num1,
        "Digit2" => Key::Num2,
        "Digit3" => Key::Num3,
        "Digit4" => Key::Num4,
        "Digit5" => Key::Num5,
        "Digit6" => Key::Num6,
        "Digit7" => Key::Num7,
        "Digit8" => Key::Num8,
        "Digit9" => Key::Num9,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Space" => Key::Space,
        "Backquote" => Key::BackQuote,
        "Minus" => Key::Minus,
        "Equal" => Key::Equal,
        "BracketLeft" => Key::LeftBracket,
        "BracketRight" => Key::RightBracket,
        "Semicolon" => Key::SemiColon,
        "Quote" => Key::Quote,
        "Comma" => Key::Comma,
        "Period" => Key::Dot,
        "Slash" => Key::Slash,
        "Backslash" => Key::BackSlash,
        "IntlBackslash" => Key::IntlBackslash,
        "Tab" => Key::Tab,
        "CapsLock" => Key::CapsLock,
        "Enter" => Key::Return,
        "Backspace" => Key::Backspace,
        "Escape" => Key::Escape,
        "ShiftLeft" => Key::ShiftLeft,
        "ShiftRight" => Key::ShiftRight,
        "ControlLeft" => Key::ControlLeft,
        "ControlRight" => Key::ControlRight,
        "AltLeft" => Key::Alt,
        "AltRight" => Key::AltGr,
        "MetaLeft" => Key::MetaLeft,
        "MetaRight" => Key::MetaRight,
        "Numpad0" => Key::Kp0,
        "Numpad1" => Key::Kp1,
        "Numpad2" => Key::Kp2,
        "Numpad3" => Key::Kp3,
        "Numpad4" => Key::Kp4,
        "Numpad5" => Key::Kp5,
        "Numpad6" => Key::Kp6,
        "Numpad7" => Key::Kp7,
        "Numpad8" => Key::Kp8,
        "Numpad9" => Key::Kp9,
        "NumpadAdd" => Key::KpPlus,
        "NumpadSubtract" => Key::KpMinus,
        "NumpadMultiply" => Key::KpMultiply,
        "NumpadDivide" => Key::KpDivide,
        "NumpadDecimal" => Key::KpDelete,
        "NumpadEnter" => Key::KpReturn,
        "NumLock" => Key::NumLock,
        "Insert" => Key::Insert,
        "Delete" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "PrintScreen" => Key::PrintScreen,
        "ScrollLock" => Key::ScrollLock,
        "Pause" => Key::Pause,
        _ => return None,
    })
}

/// The web client's `parseCombo`: `Modifier+Modifier+Code`, or a single character from before
/// bindings were `KeyboardEvent.code`s. Unknown modifier names are dropped, as there.
fn parse_binding(value: &str) -> Option<Binding> {
    let mut chars = value.chars();
    let (code, modifiers) = match (chars.next(), chars.next()) {
        (None, _) => return None,
        (Some(c), None) => (legacy_char_code(c), 0),
        _ => {
            let mut parts: Vec<&str> = value.split('+').collect();
            let code = parts.pop().filter(|code| !code.is_empty())?.to_string();
            let modifiers = parts
                .into_iter()
                .filter_map(Modifier::from_name)
                .fold(0, |bits, m| bits | m.bit());
            (code, modifiers)
        }
    };
    Some(Binding {
        key: key_from_code(&code)?,
        modifiers,
    })
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Inner {
    binding: Option<Binding>,
    /// Every key down right now (kept whether or not a binding is set).
    pressed: HashSet<Key>,
    /// The binding is down, and `true` has been sent for it.
    held: bool,
    /// The listener thread is alive.
    running: bool,
}

impl Inner {
    /// Feed a key press or release; `Some(on)` when the binding changed state.
    fn step(&mut self, key: Key, down: bool) -> Option<bool> {
        // `insert` is false when the key was already down: auto-repeat.
        let fresh = if down {
            self.pressed.insert(key)
        } else {
            self.pressed.remove(&key);
            false
        };
        let binding = self.binding?;
        if down {
            if fresh && !self.held && key == binding.key && self.modifiers_match(binding) {
                self.held = true;
                return Some(true);
            }
        } else if self.held
            && (key == binding.key
                || Modifier::of(key).is_some_and(|m| binding.modifiers & m.bit() != 0))
        {
            self.held = false;
            return Some(false);
        }
        None
    }

    /// Exactly the combo's modifiers are held; the binding key's own kind of modifier is left
    /// out (holding Shift to bind `ShiftLeft` is not an extra modifier).
    fn modifiers_match(&self, binding: Binding) -> bool {
        let own = Modifier::of(binding.key).map_or(0, Modifier::bit);
        let held = self
            .pressed
            .iter()
            .filter_map(|k| Modifier::of(*k))
            .fold(0, |bits, m| bits | m.bit());
        held & !own == binding.modifiers
    }

    /// Change the binding. Returns `true` if a held binding was let go, so `false` must be sent.
    fn rebind(&mut self, binding: Option<Binding>) -> bool {
        let was_held = std::mem::take(&mut self.held);
        self.binding = binding;
        // Keys whose release was never seen (a lock screen, say) must not outlive a rebind.
        self.pressed.clear();
        was_held
    }
}

#[derive(Default)]
pub struct PttState(Mutex<Inner>);

impl PttState {
    // The listener runs inside an OS hook callback, where a panic must not happen: shrug off
    // a poisoned lock.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn emit(app: &AppHandle, down: bool) {
    let _ = app.emit_to(MAIN_WINDOW, PTT_EVENT, down);
    // The call's window, when it is open (the key can be held while it has the focus).
    if app.get_webview_window(POPOUT_WINDOW).is_some() {
        let _ = app.emit_to(POPOUT_WINDOW, PTT_EVENT, down);
    }
}

// ---------------------------------------------------------------------------
// Listener
// ---------------------------------------------------------------------------

fn on_event(app: &AppHandle, event: rdev::Event) {
    // Mouse moves are most of what arrives: leave before taking the lock.
    let (key, down) = match event.event_type {
        EventType::KeyPress(key) => (key, true),
        EventType::KeyRelease(key) => (key, false),
        _ => return,
    };
    let change = app.state::<PttState>().lock().step(key, down);
    if let Some(on) = change {
        emit(app, on);
    }
}

/// Starts the listener thread if it is not running. `Err` when `listen` failed at once.
fn ensure_listener(app: &AppHandle) -> Result<(), rdev::ListenError> {
    {
        let state = app.state::<PttState>();
        let mut inner = state.lock();
        if inner.running {
            return Ok(());
        }
        inner.running = true;
    }
    let (failed_tx, failed_rx) = mpsc::channel();
    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("ptt-listener".into())
        .spawn(move || {
            let callback_handle = handle.clone();
            let result = rdev::listen(move |event| on_event(&callback_handle, event));
            // `listen` only returns when it could not start, or the OS ended it.
            let held = {
                let state = handle.state::<PttState>();
                let mut inner = state.lock();
                inner.running = false;
                inner.pressed.clear();
                std::mem::take(&mut inner.held)
            };
            if held {
                emit(&handle, false);
            }
            match result {
                Err(e) => {
                    eprintln!("[desktop] push to talk listener stopped: {e:?}");
                    let _ = failed_tx.send(e);
                }
                Ok(()) => eprintln!("[desktop] push to talk listener stopped"),
            }
        });
    if let Err(e) = spawned {
        eprintln!("[desktop] could not start the push to talk listener: {e}");
        app.state::<PttState>().lock().running = false;
        return Err(rdev::ListenError::KeyboardError);
    }
    match failed_rx.recv_timeout(START_GRACE) {
        Ok(e) => Err(e),
        // Still listening (or the thread ended without an error; the next call will retry).
        Err(_) => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Permission
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightListenEventAccess() -> bool;
        fn CGRequestListenEventAccess() -> bool;
    }

    /// Input Monitoring is allowed for the app.
    pub fn allowed() -> bool {
        // SAFETY: takes no arguments and only reads the permission.
        unsafe { CGPreflightListenEventAccess() }
    }

    /// Asks for Input Monitoring (the system prompt shows once); returns whether it is allowed now.
    pub fn request() -> bool {
        // SAFETY: takes no arguments; the system shows its own prompt.
        unsafe { CGRequestListenEventAccess() }
    }
}

/// Linux: only an X session can be listened to. Under Wayland an X listener hears nothing from
/// native windows.
#[cfg(target_os = "linux")]
fn session_supported() -> bool {
    let wayland =
        std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t.eq_ignore_ascii_case("wayland"));
    !wayland && std::env::var_os("DISPLAY").is_some()
}

#[cfg(all(not(target_os = "linux"), any(windows, target_os = "macos")))]
fn session_supported() -> bool {
    true
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn session_supported() -> bool {
    false
}

fn permission_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::allowed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        session_supported()
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `Some(code)` listens system-wide for that binding, `None` stops. Answers `"active"`,
/// `"no-permission"` (macOS Input Monitoring) or `"unsupported"` (no way to listen here, or a key
/// this module cannot map); the web app then keeps to the keys its own window hears.
#[tauri::command]
pub fn ptt_set_key(app: AppHandle, code: Option<String>) -> &'static str {
    let Some(code) = code else {
        if app.state::<PttState>().lock().rebind(None) {
            emit(&app, false);
        }
        return availability();
    };
    let status = availability();
    if status != "active" {
        return status;
    }
    let Some(binding) = parse_binding(&code) else {
        return "unsupported";
    };
    if app.state::<PttState>().lock().rebind(Some(binding)) {
        emit(&app, false);
    }
    match ensure_listener(&app) {
        Ok(()) => "active",
        Err(e) => {
            // Nothing is listening, so nothing should stay bound.
            app.state::<PttState>().lock().rebind(None);
            if cfg!(target_os = "macos") && matches!(e, rdev::ListenError::EventTapError) {
                "no-permission"
            } else {
                "unsupported"
            }
        }
    }
}

fn availability() -> &'static str {
    if !session_supported() {
        "unsupported"
    } else if !permission_granted() {
        "no-permission"
    } else {
        "active"
    }
}

/// Whether the OS lets the app hear keys system-wide.
#[tauri::command]
pub fn ptt_permission() -> bool {
    session_supported() && permission_granted()
}

/// Asks the OS for that (macOS shows its Input Monitoring prompt); answers whether it is allowed.
/// The grant can need a restart of the app to take effect.
#[tauri::command]
pub fn ptt_request_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::request()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ptt_permission()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(value: &str) -> Inner {
        Inner {
            binding: parse_binding(value),
            ..Inner::default()
        }
    }

    #[test]
    fn parses_like_the_web_client() {
        let b = parse_binding("Ctrl+Shift+KeyV").unwrap();
        assert_eq!(b.key, Key::KeyV);
        assert_eq!(b.modifiers, Modifier::Ctrl.bit() | Modifier::Shift.bit());
        assert_eq!(parse_binding("v").unwrap().key, Key::KeyV);
        assert_eq!(parse_binding("`").unwrap().key, Key::BackQuote);
        assert_eq!(parse_binding("F5").unwrap().modifiers, 0);
        assert!(parse_binding("").is_none());
        assert!(parse_binding("Ctrl+").is_none());
        assert!(parse_binding("F13").is_none());
    }

    #[test]
    fn plain_key_ignores_repeat() {
        let mut s = bound("KeyV");
        assert_eq!(s.step(Key::KeyV, true), Some(true));
        assert_eq!(s.step(Key::KeyV, true), None);
        assert_eq!(s.step(Key::KeyV, false), Some(false));
        assert_eq!(s.step(Key::KeyV, false), None);
    }

    #[test]
    fn combo_needs_exact_modifiers() {
        let mut s = bound("Ctrl+KeyV");
        assert_eq!(s.step(Key::ShiftLeft, true), None);
        assert_eq!(s.step(Key::ControlLeft, true), None);
        assert_eq!(s.step(Key::KeyV, true), None);
        let mut s = bound("Ctrl+KeyV");
        s.step(Key::ControlRight, true);
        assert_eq!(s.step(Key::KeyV, true), Some(true));
    }

    #[test]
    fn combo_releases_in_any_order() {
        let mut s = bound("Ctrl+Alt+KeyV");
        s.step(Key::ControlLeft, true);
        s.step(Key::Alt, true);
        assert_eq!(s.step(Key::KeyV, true), Some(true));
        assert_eq!(s.step(Key::Alt, false), Some(false));
        assert_eq!(s.step(Key::KeyV, false), None);
        assert_eq!(s.step(Key::ControlLeft, false), None);
    }

    #[test]
    fn modifier_binding_ignores_its_own_kind() {
        let mut s = bound("ShiftLeft");
        assert_eq!(s.step(Key::ShiftRight, true), None);
        assert_eq!(s.step(Key::ShiftLeft, true), Some(true));
        assert_eq!(s.step(Key::ShiftLeft, false), Some(false));
    }

    #[test]
    fn rebinding_lets_go_of_a_held_key() {
        let mut s = bound("KeyV");
        s.step(Key::KeyV, true);
        assert!(s.rebind(parse_binding("KeyB")));
        assert!(!s.held);
        assert!(!s.rebind(None));
    }
}
