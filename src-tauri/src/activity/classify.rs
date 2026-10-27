//! Decides whether a running program is a game, and what to call it.
//!
//! Pure string work, so it is the same on every platform and easy to test. Three sources, best
//! first:
//!
//! 1. `games.json`, a hand-kept list of well-known games by exe name (and, for generic exes such
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
    /// Lower-case exe file name -> indexes into `entries`.
    by_exe: HashMap<String, Vec<usize>>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let entries: Vec<Entry> = serde_json::from_str(include_str!("games.json")).unwrap_or_default();
        let mut by_exe: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, entry) in entries.iter().enumerate() {
            for exe in &entry.exes {
                by_exe.entry(exe.to_lowercase()).or_default().push(i);
            }
        }
        Catalog { entries, by_exe }
    })
}

/// The name from the hand-kept list for this program, if it is on it.
pub fn curated_name(exe_file: &str, titles: &[String]) -> Option<&'static str> {
    let catalog = catalog();
    let candidates = catalog.by_exe.get(&exe_file.to_lowercase())?;
    candidates.iter().find_map(|&i| {
        let entry = &catalog.entries[i];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_list_loads_and_matches_by_exe() {
        assert_eq!(curated_name("VALORANT-Win64-Shipping.exe", &[]), Some("VALORANT"));
        assert_eq!(curated_name("valorant-win64-shipping.EXE", &[]), Some("VALORANT"));
        assert_eq!(curated_name("notepad.exe", &[]), None);
    }

    #[test]
    fn generic_exes_need_a_matching_window_title() {
        assert_eq!(curated_name("javaw.exe", &["IntelliJ IDEA".into()]), None);
        assert_eq!(curated_name("javaw.exe", &["Minecraft 1.21".into()]), Some("Minecraft"));
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
}
