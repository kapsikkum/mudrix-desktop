//! In-app updates through tauri-plugin-updater (only built with `--features updater`).
//!
//! Checks on start-up and every six hours, downloads in the background and tells the web
//! app (`desktop://update-ready`), which shows its usual "update available" toast. Installing
//! restarts the app. Updates are verified against the minisign public key in the config.

use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Default)]
pub struct Downloaded(Mutex<Option<(Update, Vec<u8>)>>);

pub fn spawn(app: &AppHandle) {
    app.manage(Downloaded::default());
    let app = app.clone();
    std::thread::spawn(move || loop {
        if let Err(e) = tauri::async_runtime::block_on(check(&app)) {
            eprintln!("[desktop] update check failed: {e}");
        }
        std::thread::sleep(CHECK_EVERY);
    });
}

async fn check(app: &AppHandle) -> tauri_plugin_updater::Result<()> {
    if let Some(update) = app.updater()?.check().await? {
        let version = update.version.clone();
        let bytes = update.download(|_, _| {}, || {}).await?;
        *app.state::<Downloaded>().0.lock().unwrap() = Some((update, bytes));
        let _ = app.emit("desktop://update-ready", version);
    }
    Ok(())
}

pub fn install(app: &AppHandle) {
    let ready = app.state::<Downloaded>().0.lock().unwrap().take();
    if let Some((update, bytes)) = ready {
        match update.install(bytes) {
            Ok(()) => app.restart(),
            Err(e) => eprintln!("[desktop] update install failed: {e}"),
        }
    }
}
