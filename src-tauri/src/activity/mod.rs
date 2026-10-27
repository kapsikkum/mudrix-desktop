//! What the person is playing or listening to, for the "Playing X" / "Listening to X" line on
//! their Mudrix profile.
//!
//! Off by default and strictly opt-in: nothing is looked at, and no thread runs, until the web
//! app calls `set_activity_detection` (it does so only when the user switched sharing on in
//! Settings, Privacy). Turning both kinds off stops the scan and clears the activity.
//!
//! - `set_activity_detection(games, music)` starts, adjusts or stops the scan.
//! - `get_activity` is what the scan sees right now: a list, as a game and a track can both be
//!   there at once.
//! - The `desktop://activity` event fires when that changes (payload: the list, empty for none).
//!
//! Games are programs with a window that are on the hand-kept list or run from a game library
//! (`classify`); music is the track playing in a media player (`music`, never a browser).
//! Windows only for now: elsewhere the scan reports nothing.

mod classify;
#[cfg(windows)]
mod music;
#[cfg(windows)]
mod scan;

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

/// The event the main window hears when the detected activity changes.
const EVENT: &str = "desktop://activity";

/// How often the scan runs.
#[cfg_attr(not(windows), allow(dead_code))]
const SCAN_INTERVAL: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    /// `"game"` or `"music"`.
    pub kind: &'static str,
    /// The game, or the track title.
    pub name: String,
    /// The artist, for a track.
    pub details: String,
    /// Epoch milliseconds since it has been going, 0 when unknown.
    pub started_at: u64,
}

/// A program the person added by hand: when `exe` is running it counts as the game `name`.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomApp {
    pub exe: String,
    pub name: String,
}

#[derive(Default, Clone)]
struct Config {
    games: bool,
    music: bool,
    custom: Vec<CustomApp>,
}

impl Config {
    fn any(&self) -> bool {
        self.games || self.music
    }
}

#[derive(Default)]
struct Shared {
    config: Mutex<Config>,
    /// Signalled when the config changes, so the scan thread reacts at once.
    wake: Condvar,
    current: Mutex<Vec<Activity>>,
}

#[derive(Default)]
pub struct ActivityState {
    shared: Arc<Shared>,
    started: Mutex<bool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
pub fn set_activity_detection(
    app: AppHandle,
    state: State<'_, ActivityState>,
    games: bool,
    music: bool,
    custom: Option<Vec<CustomApp>>,
) {
    let config = Config {
        games,
        music,
        custom: custom.unwrap_or_default(),
    };
    let any = config.any();
    *lock(&state.shared.config) = config;
    state.shared.wake.notify_all();

    if any {
        let mut started = lock(&state.started);
        if !*started {
            *started = true;
            let shared = state.shared.clone();
            std::thread::Builder::new()
                .name("mudrix-activity".into())
                .spawn(move || run(app, shared))
                .ok();
        }
    }
}

/// A program that is open right now, for picking one to add.
#[derive(Debug, Clone, Serialize)]
pub struct RunningProgram {
    pub exe: String,
    pub title: String,
}

/// The programs with a window open right now (one per exe), so the person can pick the one to add.
#[tauri::command]
pub fn list_running_programs() -> Vec<RunningProgram> {
    #[cfg(windows)]
    {
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<RunningProgram> = scan::Scanner::default()
            .scan()
            .into_iter()
            .filter(|p| seen.insert(p.exe_file.to_lowercase()))
            .map(|p| RunningProgram {
                title: p
                    .titles
                    .into_iter()
                    .find(|t| !t.trim().is_empty())
                    .unwrap_or_default(),
                exe: p.exe_file,
            })
            .collect();
        out.sort_by_key(|p| p.exe.to_lowercase());
        out
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[tauri::command]
pub fn get_activity(state: State<'_, ActivityState>) -> Vec<Activity> {
    lock(&state.shared.current).clone()
}

/// Register the state the commands use. Called once from `setup`.
pub fn init(app: &AppHandle) {
    app.manage(ActivityState::default());
}

fn publish(app: &AppHandle, shared: &Shared, next: Vec<Activity>) {
    let mut current = lock(&shared.current);
    if *current == next {
        return;
    }
    *current = next.clone();
    drop(current);
    let _ = app.emit(EVENT, next);
}

/// The scan thread: parked while nothing is switched on, scanning every few seconds otherwise.
fn run(app: AppHandle, shared: Arc<Shared>) {
    let mut detector = Detector::new();
    loop {
        let config = {
            let mut config = lock(&shared.config);
            while !config.any() {
                publish(&app, &shared, Vec::new());
                detector.forget();
                config = shared.wake.wait(config).unwrap_or_else(|e| e.into_inner());
            }
            config.clone()
        };

        publish(&app, &shared, detector.detect(&config));

        let guard = lock(&shared.config);
        let _ = shared.wake.wait_timeout(guard, SCAN_INTERVAL);
    }
}

/// Keeps what is needed between scans.
struct Detector {
    #[cfg(windows)]
    scanner: scan::Scanner,
    #[cfg(windows)]
    steam: SteamNames,
    /// The activities reported last time, so a game or track keeps its start time.
    last: Vec<Activity>,
}

impl Detector {
    fn new() -> Self {
        Self {
            #[cfg(windows)]
            scanner: scan::Scanner::default(),
            #[cfg(windows)]
            steam: SteamNames::default(),
            last: Vec::new(),
        }
    }

    fn forget(&mut self) {
        self.last.clear();
    }

    #[cfg(not(windows))]
    fn detect(&mut self, _config: &Config) -> Vec<Activity> {
        Vec::new()
    }

    #[cfg(windows)]
    fn detect(&mut self, config: &Config) -> Vec<Activity> {
        // A game and a track are both reported when both are going.
        let found: Vec<Activity> = [self.game(config), self.track(config)]
            .into_iter()
            .flatten()
            // The same thing seen again keeps the start time it was first given.
            .map(|mut now| {
                if now.started_at == 0 {
                    now.started_at = self
                        .last
                        .iter()
                        .find(|before| {
                            before.kind == now.kind
                                && before.name == now.name
                                && before.details == now.details
                        })
                        .map_or_else(epoch_ms, |before| before.started_at);
                }
                now
            })
            .collect();
        self.last = found.clone();
        found
    }

    #[cfg(windows)]
    fn game(&mut self, config: &Config) -> Option<Activity> {
        if !config.games {
            return None;
        }
        let mut games: Vec<(String, u64, bool)> = Vec::new();
        for process in self.scanner.scan() {
            if let Some(name) = self.game_name(&process, &config.custom) {
                games.push((name, process.started_ms, process.foreground));
            }
        }
        if games.is_empty() {
            return None;
        }
        let previous = self
            .last
            .iter()
            .find(|a| a.kind == "game")
            .map(|a| a.name.as_str());
        // The one in front, else the one already shown, else the one started last.
        let pick = games
            .iter()
            .find(|g| g.2)
            .or_else(|| games.iter().find(|g| Some(g.0.as_str()) == previous))
            .or_else(|| games.iter().max_by_key(|g| g.1))?;
        Some(Activity {
            kind: "game",
            name: pick.0.clone(),
            details: String::new(),
            started_at: pick.1,
        })
    }

    #[cfg(windows)]
    fn game_name(
        &mut self,
        process: &scan::WindowedProcess,
        custom: &[CustomApp],
    ) -> Option<String> {
        // 0. A program the person added themselves always counts, under the name they gave it.
        if let Some(app) = custom
            .iter()
            .find(|a| a.exe.eq_ignore_ascii_case(&process.exe_file))
        {
            let name = app.name.trim();
            return Some(if name.is_empty() {
                app.exe.clone()
            } else {
                name.to_string()
            });
        }

        // 1. Curated list / Discord detectable database match (includes generic runner window title checks)
        if let Some(name) =
            classify::curated_name(&process.exe_file, &process.path, &process.titles)
        {
            return Some(name.to_string());
        }

        // 2. Installed in a known launcher library (Steam, Epic, GOG, EA, Xbox, etc.)
        if let Some(game) = classify::installed_game(&process.path) {
            if let Some(steamapps) = &game.steamapps {
                if let Some(appid) = classify::active_steam_appid() {
                    if let Some(name) = self.steam.name_by_appid(steamapps, appid) {
                        return Some(name);
                    }
                }
                return Some(self.steam.name(steamapps, &game.folder));
            }
            // For Epic, GOG, EA, Ubisoft, Xbox: prefer clean PE Product Name over raw directory name
            if let Some(pe_name) = classify::pe_product_name(&process.path) {
                return Some(pe_name);
            }
            return Some(game.folder);
        }

        // 3. Standalone game or installed in custom directory
        if classify::is_likely_game_path(&process.path) {
            if let Some(pe_name) = classify::pe_product_name(&process.path) {
                return Some(pe_name);
            }
        }

        None
    }

    #[cfg(windows)]
    fn track(&mut self, config: &Config) -> Option<Activity> {
        if !config.music {
            return None;
        }
        // WinRT needs the thread to be initialised for it; repeated calls are harmless.
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            );
        }
        let track = music::playing()?;
        Some(Activity {
            kind: "music",
            name: track.title,
            details: track.artist,
            started_at: 0,
        })
    }
}

#[cfg(windows)]
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Steam game names, read from the library's `appmanifest_*.acf` files and re-read now and then
/// (a game installed while Mudrix runs is found within a minute).
#[cfg(windows)]
#[derive(Default)]
struct SteamNames {
    /// steamapps dir -> (when read, lower-case install folder -> name)
    libraries: std::collections::HashMap<
        String,
        (
            std::time::Instant,
            std::collections::HashMap<String, String>,
        ),
    >,
}

#[cfg(windows)]
impl SteamNames {
    fn name(&mut self, steamapps: &str, folder: &str) -> String {
        const RELOAD_AFTER: Duration = Duration::from_secs(60);
        let key = folder.to_lowercase();
        let stale = self
            .libraries
            .get(steamapps)
            .map(|(read, names)| read.elapsed() > RELOAD_AFTER && !names.contains_key(&key))
            .unwrap_or(true);
        if stale {
            self.libraries.insert(
                steamapps.to_string(),
                (std::time::Instant::now(), read_manifests(steamapps)),
            );
        }
        self.libraries
            .get(steamapps)
            .and_then(|(_, names)| names.get(&key).cloned())
            .unwrap_or_else(|| folder.to_string())
    }

    fn name_by_appid(&self, steamapps: &str, appid: u32) -> Option<String> {
        let path = format!("{}/appmanifest_{}.acf", steamapps, appid);
        let text = std::fs::read_to_string(&path).ok()?;
        classify::parse_manifest_name(&text)
    }
}

#[cfg(windows)]
fn read_manifests(steamapps: &str) -> std::collections::HashMap<String, String> {
    let mut names = std::collections::HashMap::new();
    let Ok(dir) = std::fs::read_dir(steamapps) else {
        return names;
    };
    for entry in dir.flatten() {
        let file = entry.file_name().to_string_lossy().to_string();
        if !(file.starts_with("appmanifest_") && file.ends_with(".acf")) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            if let Some((install_dir, name)) = classify::parse_manifest(&text) {
                names.insert(install_dir.to_lowercase(), name);
            }
        }
    }
    names
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// `cargo test activity -- --ignored --nocapture` shows what the scan sees on this computer.
    #[test]
    #[ignore]
    fn probe_this_computer() {
        let mut detector = Detector::new();
        let config = Config {
            games: true,
            music: true,
            custom: Vec::new(),
        };
        println!("detected: {:?}", detector.detect(&config));
        let processes = detector.scanner.scan();
        println!("{} programs with a window", processes.len());
        for process in processes {
            let game = detector.game_name(&process, &[]);
            println!(
                "  {:?} {} fg={} since={}",
                game, process.exe_file, process.foreground, process.started_ms
            );
        }
        println!("media sessions: {:?}", music::describe_sessions());
        println!("music: {:?}", music::playing());
    }
}
