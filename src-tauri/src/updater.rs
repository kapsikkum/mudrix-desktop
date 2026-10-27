//! In-app updates through `tauri-plugin-updater`, verified against a minisign signature.
//!
//! The plugin reads `latest.json` from the latest GitHub release (see `plugins.updater` in
//! `tauri.conf.json`) and refuses any download that is not signed by the key whose public half is
//! baked into the app. It is only driven from Rust: the webview has no updater permissions and
//! talks to this module through `check_for_updates` / `install_update` and the
//! `desktop://update-ready` / `desktop://update-available` events.
//!
//! * Windows, macOS and Linux AppImage: the update is downloaded and verified in the background,
//!   the web app is told (`desktop://update-ready`), and installing restarts the app.
//! * Linux .deb/.rpm cannot be replaced in place, so they only get `desktop://update-available`
//!   with the release page.

use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const REPO: &str = "kapsikkum/mudrix-desktop";
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
const KEY_PLACEHOLDER: &str = "REPLACE_WITH";

#[derive(Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

/// A downloaded, signature-verified update waiting for the user to restart.
struct Ready {
    update: Update,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct UpdateState {
    ready: Mutex<Option<Ready>>,
    announced: Mutex<Option<String>>,
    /// Serialises the background and manual checks so they never download twice.
    checking: tokio::sync::Mutex<()>,
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
    if let Err(reason) = configured(app) {
        log(&format!("updates disabled: {reason}"));
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let _ = check(&app).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// The updater is only usable once a real public key replaced the placeholder in the config.
fn configured(app: &AppHandle) -> Result<(), String> {
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|c| c.get("pubkey"))
        .and_then(|k| k.as_str())
        .unwrap_or_default();
    if pubkey.is_empty() || pubkey.contains(KEY_PLACEHOLDER) {
        Err("plugins.updater.pubkey is not set (see RELEASING.md)".into())
    } else {
        Ok(())
    }
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

/// Whether this install can replace itself. Linux packages (.deb/.rpm) cannot; an AppImage can.
fn can_install() -> bool {
    cfg!(any(windows, target_os = "macos")) || std::env::var_os("APPIMAGE").is_some()
}

fn release_page(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
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
    configured(app)?;
    let state = app.state::<UpdateState>();
    let _guard = state.checking.lock().await;
    if state.ready.lock().unwrap().is_some() {
        return Ok(CheckStatus::Ready);
    }

    let Some(update) = app.updater()?.check().await? else {
        return Ok(CheckStatus::UpToDate);
    };
    let version = update.version.clone();
    let page = release_page(&version);
    log(&format!(
        "update found: {} -> {version}",
        update.current_version
    ));

    if !can_install() {
        announce(app, &version, &page, "desktop://update-available");
        return Ok(CheckStatus::Available);
    }

    // The plugin verifies the minisign signature while downloading and fails on a mismatch.
    let bytes = update.download(|_, _| {}, || {}).await?;
    log(&format!("downloaded and verified {version}"));
    *state.ready.lock().unwrap() = Some(Ready { update, bytes });
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

/// Install the downloaded update and restart. On Windows the installer (passive mode, with a
/// progress window) ends this process itself.
pub fn install(app: &AppHandle) {
    let ready = app.state::<UpdateState>().ready.lock().unwrap().take();
    let Some(ready) = ready else { return };
    match ready.update.install(&ready.bytes) {
        Ok(()) => {
            log(&format!("installed update {}", ready.update.version));
            app.restart();
        }
        Err(e) => {
            log(&format!("update install failed: {e}"));
            // Keep it so the user can try again.
            *app.state::<UpdateState>().ready.lock().unwrap() = Some(ready);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_page_points_at_the_tag() {
        assert_eq!(
            release_page("26.10.16"),
            "https://github.com/kapsikkum/mudrix-desktop/releases/tag/v26.10.16"
        );
    }

    #[test]
    fn placeholder_marker_is_stable() {
        // tauri.conf.json and build.rs use the same marker.
        assert!("REPLACE_WITH_OUTPUT_OF_tauri_signer_generate".contains(KEY_PLACEHOLDER));
    }
}
