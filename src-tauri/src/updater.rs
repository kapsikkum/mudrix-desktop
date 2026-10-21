//! In-app updates without a signing service or the GitHub API.
//!
//! The release list comes from the repository's public `releases.atom` feed (a plain web
//! page, so it is not subject to the API's rate limits). On Windows and macOS the matching
//! installer is downloaded in the background, checked against the release's `SHA256SUMS.txt`
//! and the web app is told (`desktop://update-ready`); installing restarts the app. Linux only
//! gets `desktop://update-available` so the web app can point at the download page.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const REPO: &str = "kapsikkum/mudrix-desktop";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

struct Ready {
    version: String,
    installer: PathBuf,
}

#[derive(Default)]
pub struct UpdateState {
    ready: Mutex<Option<Ready>>,
    announced: Mutex<Option<String>>,
}

/// Result of a manual check, reported to the web app as a string.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    UpToDate,
    Ready,
    Available,
    Error,
}

pub fn spawn(app: &AppHandle) {
    app.manage(UpdateState::default());
    // Development builds and opted-out installs never poll.
    if cfg!(debug_assertions) || std::env::var_os("MUDRIX_NO_UPDATE").is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let _ = check(&app).await;
            tokio_sleep(CHECK_EVERY).await;
        }
    });
}

async fn tokio_sleep(d: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await;
}

/// Where update progress and failures are recorded, for diagnosing "update failed" reports.
fn log_path() -> PathBuf {
    std::env::temp_dir()
        .join("mudrix-update")
        .join("update.log")
}

fn log(msg: &str) {
    eprintln!("[desktop] {msg}");
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent(format!("Mudrix-Desktop/{CURRENT}"))
        .timeout(Duration::from_secs(300))
        .build()
}

pub async fn check(app: &AppHandle) -> CheckStatus {
    match check_inner(app).await {
        Ok(status) => status,
        Err(e) => {
            log(&format!("update check failed: {e}"));
            CheckStatus::Error
        }
    }
}

async fn check_inner(app: &AppHandle) -> Result<CheckStatus, Box<dyn std::error::Error>> {
    let state = app.state::<UpdateState>();
    if state.ready.lock().unwrap().is_some() {
        return Ok(CheckStatus::Ready);
    }
    let client = client()?;
    let feed = client
        .get(format!("https://github.com/{REPO}/releases.atom"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let Some(tag) = newest_stable(&parse_atom_tags(&feed)) else {
        return Ok(CheckStatus::UpToDate);
    };
    let version = tag.trim_start_matches('v').to_string();
    if parse_version(&version) <= parse_version(CURRENT) {
        return Ok(CheckStatus::UpToDate);
    }
    let page = format!("https://github.com/{REPO}/releases/tag/{tag}");
    log(&format!("update found: {CURRENT} -> {version}"));

    let Some(asset) = asset_name(&version) else {
        announce(app, &version, &page, "desktop://update-available");
        return Ok(CheckStatus::Available);
    };

    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let sums = client
        .get(format!("{base}/SHA256SUMS.txt"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let expected = checksum_for(&sums, &asset).ok_or("installer missing from SHA256SUMS.txt")?;

    let dir = std::env::temp_dir().join("mudrix-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&asset);
    let mut response = client
        .get(format!("{base}/{asset}"))
        .send()
        .await?
        .error_for_status()?;
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::create(&path)?;
    while let Some(chunk) = response.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    drop(file);
    let actual = hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(&expected) {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "downloaded installer failed its checksum (got {actual}, expected {expected})"
        )
        .into());
    }

    log(&format!("downloaded and verified {}", path.display()));
    *state.ready.lock().unwrap() = Some(Ready {
        version: version.clone(),
        installer: path,
    });
    announce(app, &version, &page, "desktop://update-ready");
    Ok(CheckStatus::Ready)
}

fn announce(app: &AppHandle, version: &str, url: &str, event: &str) {
    let state = app.state::<UpdateState>();
    let mut announced = state.announced.lock().unwrap();
    let key = format!("{event}:{version}");
    if announced.as_deref() == Some(key.as_str()) {
        return;
    }
    *announced = Some(key);
    let _ = app.emit(
        event,
        UpdateInfo {
            version: version.to_string(),
            url: url.to_string(),
        },
    );
}

pub fn install(app: &AppHandle) {
    let ready = app.state::<UpdateState>().ready.lock().unwrap().take();
    let Some(ready) = ready else { return };
    match launch_installer(&ready.installer) {
        Ok(()) => {
            log(&format!("installing update {}", ready.version));
            app.exit(0);
        }
        Err(e) => log(&format!("update install failed: {e}")),
    }
}

#[cfg(windows)]
fn launch_installer(installer: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()?;
    // Wait for this process to exit, run the per-user NSIS installer silently in update mode,
    // then start the new build.
    let script = format!(
        "ping -n 3 127.0.0.1 >nul & start /wait \"\" \"{}\" /S /UPDATE & echo installer exit code %errorlevel% >> \"{}\" & start \"\" \"{}\"",
        installer.display(),
        log_path().display(),
        exe.display()
    );
    std::process::Command::new("cmd")
        .raw_arg("/C")
        .raw_arg(format!("\"{script}\""))
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn launch_installer(dmg: &Path) -> std::io::Result<()> {
    // current_exe is <App>.app/Contents/MacOS/Mudrix
    let bundle = std::env::current_exe()?
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .filter(|p| p.extension().is_some_and(|e| e == "app"));
    let Some(bundle) = bundle else {
        // Not running from an app bundle: hand the disk image to the user.
        return std::process::Command::new("open")
            .arg(dmg)
            .spawn()
            .map(|_| ());
    };
    // Files fetched by this process carry no quarantine flag, so the copied app opens normally.
    let script = r#"sleep 2
mnt=$(mktemp -d)
if hdiutil attach -nobrowse -quiet -mountpoint "$mnt" "$DMG"; then
  src=$(ls -d "$mnt"/*.app | head -n 1)
  if [ -n "$src" ] && rm -rf "$APP.new" && cp -R "$src" "$APP.new" && rm -rf "$APP" && mv "$APP.new" "$APP"; then
    xattr -dr com.apple.quarantine "$APP" 2>/dev/null
  fi
  hdiutil detach -quiet "$mnt"
fi
open "$APP""#;
    std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .env("DMG", dmg)
        .env("APP", bundle)
        .spawn()
        .map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn launch_installer(_installer: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("no installer for this platform"))
}

/// Installer file for this platform, or None where updates are announced rather than applied.
fn asset_name(version: &str) -> Option<String> {
    if cfg!(windows) {
        Some(format!("Mudrix-{version}-windows-x64-setup.exe"))
    } else if cfg!(target_os = "macos") {
        Some(format!("Mudrix-{version}-macos-universal.dmg"))
    } else {
        None
    }
}

fn parse_atom_tags(xml: &str) -> Vec<String> {
    const NEEDLE: &str = "/releases/tag/";
    let mut tags = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(NEEDLE) {
        rest = &rest[i + NEEDLE.len()..];
        let end = rest.find(['"', '<', '\'']).unwrap_or(rest.len());
        let tag = &rest[..end];
        if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

/// `26.10.6` -> (26, 10, 6). Anything else (pre-release suffixes, extra parts) is not stable.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim_start_matches('v').split('.');
    let out = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(out)
}

fn newest_stable(tags: &[String]) -> Option<String> {
    tags.iter()
        .filter_map(|t| parse_version(t).map(|v| (v, t)))
        .max_by_key(|(v, _)| *v)
        .map(|(_, t)| t.clone())
}

fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_string())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_stable_tag_from_feed() {
        let feed = r#"<link rel="alternate" href="https://github.com/o/r/releases/tag/v26.10.2-next.1"/>
            <link href="https://github.com/o/r/releases/tag/v26.10.10"/>
            <link href="https://github.com/o/r/releases/tag/v26.10.9"/>"#;
        let tags = parse_atom_tags(feed);
        assert_eq!(newest_stable(&tags).as_deref(), Some("v26.10.10"));
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("26.10.10") > parse_version("26.10.9"));
        assert_eq!(parse_version("26.10.3-next.1"), None);
    }

    #[test]
    fn finds_checksum_with_binary_marker() {
        let sums = "abc123  Mudrix-1.0.0-windows-x64-setup.exe\ndef456 *other.dmg\n";
        assert_eq!(
            checksum_for(sums, "Mudrix-1.0.0-windows-x64-setup.exe").as_deref(),
            Some("abc123")
        );
        assert_eq!(checksum_for(sums, "other.dmg").as_deref(), Some("def456"));
        assert_eq!(checksum_for(sums, "missing"), None);
    }
}
