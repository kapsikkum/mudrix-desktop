//! The operating system's accent colour, so the web app can theme itself with it.
//!
//! Windows reads the accent from the registry, macOS from the `AppleAccentColor` default and
//! Linux from the XDG desktop portal (`org.freedesktop.appearance` / `accent-color`), falling
//! back to GNOME's `accent-color` setting. The result is a `#rrggbb` string, or `None` when the
//! system has no accent colour to report.

use std::process::Command;

/// Runs a program and returns its stdout when it exits successfully.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: don't flash a console for the lookup.
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `reg query` prints the value as `0xAABBGGRR`; the accent is the low three bytes (BGR).
#[allow(dead_code)]
fn parse_registry_accent(output: &str) -> Option<String> {
    let value = output
        .split_whitespace()
        .find_map(|word| word.strip_prefix("0x"))?;
    let abgr = u32::from_str_radix(value, 16).ok()?;
    Some(hex(
        (abgr & 0xff) as u8,
        ((abgr >> 8) & 0xff) as u8,
        ((abgr >> 16) & 0xff) as u8,
    ))
}

/// The system colours macOS offers for `AppleAccentColor` (-1 graphite, 0 red ... 6 pink); no
/// value at all means the default blue.
#[allow(dead_code)]
fn macos_accent(code: Option<i32>) -> &'static str {
    match code {
        Some(-1) => "#98989d",
        Some(0) => "#ff453a",
        Some(1) => "#ff9f0a",
        Some(2) => "#ffd60a",
        Some(3) => "#32d74b",
        Some(5) => "#bf5af2",
        Some(6) => "#ff375f",
        _ => "#0a84ff",
    }
}

/// The portal answers with a variant such as `(<<(0.2078, 0.5176, 0.8941)>>,)`: three channels
/// in 0..=1. Values outside that range mean "no accent colour set".
#[allow(dead_code)]
fn parse_portal_accent(output: &str) -> Option<String> {
    let channels: Vec<f64> = output
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f64>().ok())
        .collect();
    let [r, g, b] = channels.get(..3)? else {
        return None;
    };
    if ![r, g, b].iter().all(|c| (0.0..=1.0).contains(*c)) {
        return None;
    }
    let byte = |c: &f64| (c * 255.0).round() as u8;
    Some(hex(byte(r), byte(g), byte(b)))
}

/// GNOME's named accent colours.
#[allow(dead_code)]
fn gnome_accent(name: &str) -> Option<&'static str> {
    match name.trim().trim_matches('\'') {
        "blue" => Some("#3584e4"),
        "teal" => Some("#2190a4"),
        "green" => Some("#3a944a"),
        "yellow" => Some("#c88800"),
        "orange" => Some("#ed5b00"),
        "red" => Some("#e62d42"),
        "pink" => Some("#d56199"),
        "purple" => Some("#9141ac"),
        "slate" => Some("#6f8396"),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn system_accent() -> Option<String> {
    run(
        "reg",
        &[
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
            "/v",
            "AccentColorMenu",
        ],
    )
    .and_then(|out| parse_registry_accent(&out))
}

#[cfg(target_os = "macos")]
fn system_accent() -> Option<String> {
    // The key is absent while the accent is "Multicolor", which renders as the default blue.
    let code = run("defaults", &["read", "-g", "AppleAccentColor"])
        .and_then(|out| out.trim().parse::<i32>().ok());
    Some(macos_accent(code).to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_accent() -> Option<String> {
    run(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.Read",
            "org.freedesktop.appearance",
            "accent-color",
        ],
    )
    .and_then(|out| parse_portal_accent(&out))
    .or_else(|| {
        run(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "accent-color"],
        )
        .and_then(|out| gnome_accent(&out).map(str::to_string))
    })
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
fn system_accent() -> Option<String> {
    None
}

#[tauri::command]
pub fn get_system_accent() -> Option<String> {
    system_accent()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_value_is_read_as_bgr() {
        let out = "\r\nHKEY_CURRENT_USER\\...\\Accent\r\n    AccentColorMenu    REG_DWORD    0xffd77800\r\n";
        // 0xffd77800: B=0xd7, G=0x78, R=0x00 -> #0078d7 (the classic Windows blue)
        assert_eq!(parse_registry_accent(out).as_deref(), Some("#0078d7"));
        assert_eq!(parse_registry_accent("no value here"), None);
    }

    #[test]
    fn portal_colour_is_converted() {
        assert_eq!(
            parse_portal_accent("(<<(0.2078, 0.5176, 0.8941)>>,)\n").as_deref(),
            Some("#3584e4")
        );
        assert_eq!(parse_portal_accent("(<<(-1.0, -1.0, -1.0)>>,)"), None);
        assert_eq!(parse_portal_accent("garbage"), None);
    }

    #[test]
    fn macos_codes_map_to_colours() {
        assert_eq!(macos_accent(None), "#0a84ff");
        assert_eq!(macos_accent(Some(0)), "#ff453a");
        assert_eq!(macos_accent(Some(-1)), "#98989d");
    }

    #[test]
    fn gnome_names_map_to_colours() {
        assert_eq!(gnome_accent("'green'\n"), Some("#3a944a"));
        assert_eq!(gnome_accent("'unknown'"), None);
    }
}
