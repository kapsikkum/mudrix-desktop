//! Decides whether a running program is a game, and what to call it.
//!
//! Pure string work, so it is the same on every platform and easy to test. Three sources, best
//! first:
//!
//! 1. `games.json` (kept readable; build.rs minifies it), a hand-kept list of well-known games by exe name (and, for generic exes such
//!    as `javaw.exe`, by window title). Gives the proper name.
//! 2. Steam: anything running from `steamapps/common/<folder>/` is a Steam game. Its real name is
//!    read from the `appmanifest_*.acf` next to it, falling back to the folder name.
//! 3. Other launchers' install folders (Epic, GOG, Ubisoft, EA, Xbox): the folder name.
//!
//! Helpers that live in those folders (crash reporters, redistributables, anti-cheat) and the
//! non-game tools some launchers install are filtered out.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
struct Entry {
    name: String,
    exes: Vec<String>,
    /// When set, one of the program's window titles must start with one of these.
    #[serde(default)]
    titles: Vec<String>,
}

struct Catalog {
    entries: Vec<Entry>,
    /// Lower-case exe file name -> (index into `entries`, folders the path must end with).
    /// Discord's list writes some as `_retail_/wow.exe`: the folder tells builds apart.
    by_exe: HashMap<String, Vec<(usize, Option<String>)>>,
}

/// `/` separators and lower case.
fn normalise_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let entries: Vec<Entry> = serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/games.min.json"))).unwrap_or_default();
        let mut by_exe: HashMap<String, Vec<(usize, Option<String>)>> = HashMap::new();
        for (i, entry) in entries.iter().enumerate() {
            for exe in &entry.exes {
                // `>name.exe` means "only when started with certain arguments", which we cannot see;
                // taken as the bare name it would call every `javaw.exe` Minecraft.
                if exe.starts_with('>') {
                    continue;
                }
                let exe = normalise_path(exe);
                match exe.rsplit_once('/') {
                    Some((_, file)) => {
                        by_exe.entry(file.to_string()).or_default().push((i, Some(exe.clone())))
                    }
                    None => by_exe.entry(exe).or_default().push((i, None)),
                }
            }
        }
        Catalog { entries, by_exe }
    })
}

/// The name from the hand-kept list for this program, if it is on it.
pub fn curated_name(exe_file: &str, path: &str, titles: &[String]) -> Option<&'static str> {
    let catalog = catalog();
    let candidates = catalog.by_exe.get(&exe_file.to_lowercase())?;
    let path = format!("/{}", normalise_path(path));
    candidates.iter().find_map(|(i, folders)| {
        if let Some(folders) = folders {
            if !path.ends_with(&format!("/{folders}")) {
                return None;
            }
        }
        let entry = &catalog.entries[*i];
        let title_ok = entry.titles.is_empty()
            || titles.iter().any(|title| {
                let title = title.trim().to_lowercase();
                entry.titles.iter().any(|want| title.starts_with(&want.to_lowercase()))
            });
        title_ok.then_some(entry.name.as_str())
    })
}

/// Exe names that are never the game itself.
const HELPER_WORDS: &[&str] = &[
    "crash", "handler", "redist", "setup", "install", "unins", "dxsetup", "helper", "updater",
    "bootstrap", "prereq", "easyanticheat", "battleye", "anticheat", "launcher", "dotnet",
    "cefsharp", "overlay", "benchmark", "report", "diagnostic", "configtool",
];

/// Folders under a launcher's library that are tools or runtimes, not games.
const NOT_GAME_FOLDERS: &[&str] = &[
    "steamworks shared",
    "steamvr",
    "steam controller configs",
    "steam linux runtime",
    "proton",
    "wallpaper engine",
    "launcher",
    "directxredist",
    "epicgameslauncher",
    "easyanticheat",
    "battleye",
    "redist",
    "_commonredist",
];

fn is_helper_exe(exe_file: &str) -> bool {
    let lower = exe_file.to_lowercase();
    HELPER_WORDS.iter().any(|word| lower.contains(word))
}

fn is_helper_product_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    is_helper_exe(&lower)
        || lower.contains("microsoft .net")
        || lower.contains("directx")
        || lower.contains("visual c++")
        || lower.contains("crashpad")
        || lower.contains("sentry native")
        || lower.contains("unreal engine")
        || lower.contains("unitycrashhandler")
        || lower == "game"
        || lower == "game client"
        || lower == "client"
        || lower == "bootstrap"
}

/// Reads the embedded Product Name or File Description from the executable's PE version resource.
#[cfg(windows)]
pub fn pe_product_name(path: &str) -> Option<String> {
    use std::ffi::c_void;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };

    let wide_path: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide_path.as_ptr()), None) };
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    let ok = unsafe {
        GetFileVersionInfoW(
            PCWSTR(wide_path.as_ptr()),
            None,
            size,
            buffer.as_mut_ptr() as *mut c_void,
        )
    };
    if ok.is_err() {
        return None;
    }

    let mut langs: Vec<String> = Vec::new();
    let query_trans: Vec<u16> = "\\VarFileInfo\\Translation\0".encode_utf16().collect();
    let mut trans_ptr = std::ptr::null_mut();
    let mut trans_len = 0u32;
    unsafe {
        if VerQueryValueW(
            buffer.as_ptr() as *const c_void,
            PCWSTR(query_trans.as_ptr()),
            &mut trans_ptr,
            &mut trans_len,
        )
        .as_bool()
            && !trans_ptr.is_null()
            && trans_len >= 4
        {
            let slice = std::slice::from_raw_parts(trans_ptr as *const u16, (trans_len / 2) as usize);
            for chunk in slice.chunks_exact(2) {
                langs.push(format!("{:04x}{:04x}", chunk[0], chunk[1]));
            }
        }
    }

    for fallback in ["040904b0", "040904e4", "000004b0", "04090000", "00000000"] {
        if !langs.iter().any(|l| l.eq_ignore_ascii_case(fallback)) {
            langs.push(fallback.to_string());
        }
    }

    let mut candidate_product = None;
    let mut candidate_desc = None;

    for lang in &langs {
        for (prop, target) in [
            ("ProductName", &mut candidate_product),
            ("FileDescription", &mut candidate_desc),
        ] {
            if target.is_some() {
                continue;
            }
            let sub = format!("\\StringFileInfo\\{}\\{}\0", lang, prop);
            let wide_sub: Vec<u16> = sub.encode_utf16().collect();
            let mut val_ptr = std::ptr::null_mut();
            let mut val_len = 0u32;
            unsafe {
                if VerQueryValueW(
                    buffer.as_ptr() as *const c_void,
                    PCWSTR(wide_sub.as_ptr()),
                    &mut val_ptr,
                    &mut val_len,
                )
                .as_bool()
                    && !val_ptr.is_null()
                    && val_len > 1
                {
                    let u16_slice = std::slice::from_raw_parts(val_ptr as *const u16, val_len as usize);
                    let mut s = String::from_utf16_lossy(u16_slice);
                    if let Some(nul) = s.find('\0') {
                        s.truncate(nul);
                    }
                    let trimmed = s.trim().to_string();
                    if !trimmed.is_empty() && !is_helper_product_name(&trimmed) {
                        *target = Some(trimmed);
                    }
                }
            }
        }
        if candidate_product.is_some() {
            break;
        }
    }

    candidate_product.or(candidate_desc)
}

#[cfg(not(windows))]
pub fn pe_product_name(_path: &str) -> Option<String> {
    None
}

/// Checks the Windows registry to see if Steam currently reports an active game AppID.
#[cfg(windows)]
pub fn active_steam_appid() -> Option<u32> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
    };

    let subkey: Vec<u16> = "Software\\Valve\\Steam\\ActiveProcess\0".encode_utf16().collect();
    let val_name: Vec<u16> = "RunningAppId\0".encode_utf16().collect();
    let mut hkey = Default::default();

    unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
        .is_ok()
        {
            let mut app_id = 0u32;
            let mut typ = windows::Win32::System::Registry::REG_VALUE_TYPE::default();
            let mut size = std::mem::size_of::<u32>() as u32;
            let res = RegQueryValueExW(
                hkey,
                PCWSTR(val_name.as_ptr()),
                None,
                Some(&mut typ),
                Some(&mut app_id as *mut u32 as *mut u8),
                Some(&mut size),
            );
            let _ = RegCloseKey(hkey);
            if res.is_ok() && typ == REG_DWORD && app_id > 0 {
                return Some(app_id);
            }
        }
    }
    None
}

#[cfg(not(windows))]
pub fn active_steam_appid() -> Option<u32> {
    None
}

/// Returns true if the file path indicates it lives in a known game directory or library.
pub fn is_likely_game_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    let keywords = [
        "/games/", "/game/", "/steam", "/epic", "/gog", "/riot", "/blizzard",
        "/ubisoft", "/ea games/", "/origin", "/itch", "/emulators/", "/roms/",
        "/prime gaming/", "/amazon games/", "/battlenet/", "/battle.net/",
    ];
    keywords.iter().any(|k| lower.contains(k))
}

/// A game found by where it is installed.
#[derive(Debug, PartialEq, Eq)]
pub struct InstalledGame {
    /// The library folder's name, a stand-in until Steam's own name is known.
    pub folder: String,
    /// `<library>/steamapps` for a Steam game, whose manifests give the proper name.
    pub steamapps: Option<String>,
}

/// Library roots other than Steam, matched on the lower-case path with `/` separators.
const OTHER_LIBRARIES: &[&str] = &[
    "/epic games/",
    "/gog galaxy/games/",
    "/gog games/",
    "/ubisoft game launcher/games/",
    "/ea games/",
    "/xboxgames/",
];

/// Where an exe lives, if that is a game library. `path` is the full path to the exe.
pub fn installed_game(path: &str) -> Option<InstalledGame> {
    let slashed = path.replace('\\', "/");
    let lower = slashed.to_lowercase();
    let file = slashed.rsplit('/').next().unwrap_or("");
    if is_helper_exe(file) {
        return None;
    }

    let (root_end, steamapps) = match lower.find("/steamapps/common/") {
        Some(i) => (i + "/steamapps/common/".len(), Some(slashed[..i + "/steamapps".len()].to_string())),
        None => {
            let hit = OTHER_LIBRARIES
                .iter()
                .find_map(|root| lower.find(root).map(|i| i + root.len()))?;
            (hit, None)
        }
    };

    let folder = slashed[root_end..].split('/').next()?;
    // The exe has to be inside a game's folder, not be the library folder's own file.
    if folder.is_empty() || !slashed[root_end..].contains('/') {
        return None;
    }
    if NOT_GAME_FOLDERS.contains(&folder.to_lowercase().as_str()) {
        return None;
    }
    Some(InstalledGame { folder: folder.to_string(), steamapps })
}

/// `installdir` -> `name` for every game in a Steam library's manifests.
pub fn parse_manifest(text: &str) -> Option<(String, String)> {
    let mut install_dir = None;
    let mut name = None;
    for line in text.lines() {
        let mut quoted = line.split('"').skip(1).step_by(2);
        match (quoted.next(), quoted.next()) {
            (Some("installdir"), Some(value)) => install_dir = Some(value.to_string()),
            (Some("name"), Some(value)) => name = Some(value.to_string()),
            _ => {}
        }
        if install_dir.is_some() && name.is_some() {
            break;
        }
    }
    Some((install_dir?, name?))
}

/// Extracts just the game `name` from an `appmanifest_*.acf` file without needing `installdir`.
pub fn parse_manifest_name(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut quoted = line.split('"').skip(1).step_by(2);
        if let (Some("name"), Some(value)) = (quoted.next(), quoted.next()) {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_list_loads_and_matches_by_exe() {
        assert_eq!(curated_name("VALORANT-Win64-Shipping.exe", "", &[]), Some("VALORANT"));
        assert_eq!(curated_name("valorant-win64-shipping.EXE", "", &[]), Some("VALORANT"));
        assert_eq!(curated_name("notepad.exe", "", &[]), None);
    }

    #[test]
    fn folder_qualified_entries_need_that_folder() {
        assert_eq!(
            curated_name("wow-64.exe", "C:\\Games\\World of Warcraft\\_retail_\\Wow-64.exe", &[]),
            Some("World of Warcraft")
        );
        assert_eq!(curated_name("wow-64.exe", "C:\\Other\\wow-64.exe", &[]), None);
    }

    #[test]
    fn generic_exes_need_a_matching_window_title() {
        assert_eq!(curated_name("javaw.exe", "", &["IntelliJ IDEA".into()]), None);
        assert_eq!(
            curated_name("javaw.exe", "", &["Minecraft 1.21".into()]),
            Some("Minecraft")
        );
    }

    #[test]
    fn steam_games_give_the_folder_and_library() {
        let game = installed_game(
            r"D:\SteamLibrary\steamapps\common\Hades II\Ship\Hades2.exe",
        )
        .unwrap();
        assert_eq!(game.folder, "Hades II");
        assert_eq!(game.steamapps.as_deref(), Some("D:/SteamLibrary/steamapps"));
    }

    #[test]
    fn other_launchers_give_the_folder() {
        let epic = installed_game(r"C:\Program Files\Epic Games\Fortnite\FortniteGame\a.exe").unwrap();
        assert_eq!(epic.folder, "Fortnite");
        assert!(epic.steamapps.is_none());
        assert!(installed_game(r"C:\Program Files\Epic Games\Launcher\Portal\Binaries\Win64\x.exe").is_none());
    }

    #[test]
    fn helpers_and_tools_are_not_games() {
        assert!(installed_game(r"C:\Steam\steamapps\common\Foo\UnityCrashHandler64.exe").is_none());
        assert!(installed_game(r"C:\Steam\steamapps\common\Foo\vc_redist.x64.exe").is_none());
        assert!(installed_game(r"C:\Steam\steamapps\common\Wallpaper Engine\wallpaper32.exe").is_none());
        assert!(installed_game(r"C:\Steam\steamapps\common\Steamworks Shared\x.exe").is_none());
    }

    #[test]
    fn ordinary_programs_are_not_games() {
        assert!(installed_game(r"C:\Program Files\Mozilla Firefox\firefox.exe").is_none());
        assert!(installed_game(r"C:\Steam\steamapps\common\stray.exe").is_none());
    }

    #[test]
    fn manifest_gives_install_dir_and_name() {
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"1145350\"\n\t\"name\"\t\t\"Hades II\"\n\t\"installdir\"\t\t\"Hades II\"\n}\n";
        assert_eq!(parse_manifest(acf), Some(("Hades II".into(), "Hades II".into())));
        assert_eq!(parse_manifest("garbage"), None);
    }

    #[test]
    fn likely_game_paths_identified() {
        assert!(is_likely_game_path(r"D:\Games\Baldurs Gate 3\bin\bg3.exe"));
        assert!(is_likely_game_path(r"C:\Riot Games\League of Legends\game.exe"));
        assert!(is_likely_game_path(r"E:\Emulators\rpcs3\rpcs3.exe"));
        assert!(!is_likely_game_path(r"C:\Program Files\Mozilla Firefox\firefox.exe"));
    }

    #[test]
    #[cfg(windows)]
    fn pe_version_reads_product_name() {
        let node_path = r"C:\Program Files\nodejs\node.exe";
        if std::path::Path::new(node_path).exists() {
            let name = pe_product_name(node_path);
            assert_eq!(name.as_deref(), Some("Node.js"));
        }
    }

    #[test]
    #[cfg(windows)]
    fn pe_version_reads_real_game() {
        let game_path = r"C:\Program Files (x86)\Steam\steamapps\common\Terraria\Terraria.exe";
        if std::path::Path::new(game_path).exists() {
            let name = pe_product_name(game_path);
            assert_eq!(name.as_deref(), Some("Terraria"));
        }
    }
}
