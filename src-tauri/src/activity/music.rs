//! Windows: the track playing right now, from the System Media Transport Controls (the same
//! source as the volume flyout's media card). Spotify, most music players and many other apps
//! report to it.
//!
//! Only music is shared. Browsers are left out on purpose: a tab that plays something says what
//! the person is browsing, which is not what they agreed to share. So are video players (VLC, MPC,
//! mpv, Movies & TV, streaming apps): their "title" is the name of the film or file. A session has
//! to say it is music; one that says nothing about its type counts only for a known music app.

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Media::MediaPlaybackType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
}

/// Parts of the source app's id that mean "a web browser".
const BROWSERS: &[&str] = &[
    "chrome",
    "msedge",
    "edge",
    "firefox",
    "brave",
    "opera",
    "vivaldi",
    "iexplore",
    "librewolf",
    "waterfox",
    "zen",
    "arc",
    // Firefox reports its install id instead of a name.
    "308046b0af4a39cb",
];

fn id_has_word(app_id: &str, words: &[&str]) -> bool {
    let id = app_id.to_lowercase();
    words.iter().any(|w| {
        id.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|part| part == *w)
    })
}

fn is_browser(app_id: &str) -> bool {
    id_has_word(app_id, BROWSERS)
}

/// Players and apps whose "track" is a film, episode or file name: never shared.
const VIDEO_APPS: &[&str] = &[
    "vlc",
    "mpc",
    "mpchc",
    "mpchc64",
    "mpv",
    "potplayer",
    "potplayermini",
    "kmplayer",
    "gom",
    "smplayer",
    "mplayer",
    "wmplayer",
    "zunevideo",
    "movies",
    "netflix",
    "plex",
    "disney",
    "primevideo",
    "hulu",
    "crunchyroll",
    "twitch",
    "obs",
    "kodi",
    "jellyfin",
    "emby",
    "stremio",
    "youtube",
    "videos",
    "video",
];

/// Music apps trusted even when their session does not say what type it is.
const MUSIC_APPS: &[&str] = &[
    "spotify",
    "applemusic",
    "itunes",
    "zunemusic",
    "music",
    "foobar2000",
    "musicbee",
    "aimp",
    "winamp",
    "tidal",
    "deezer",
    "amazonmusic",
    "plexamp",
    "ytmdesktop",
    "youtubemusic",
    "pulsar",
    "strawberry",
    "qobuz",
    "groove",
    "mediamonkey",
];

fn is_video_app(app_id: &str) -> bool {
    id_has_word(app_id, VIDEO_APPS)
}

fn is_music_app(app_id: &str) -> bool {
    // App ids look like `Spotify.exe` or `Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic`.
    let id = app_id.to_lowercase();
    MUSIC_APPS.iter().any(|m| id.contains(m))
}

/// Whether a session counts as music: it says so, or it is a known music app and says nothing.
fn counts_as_music(app_id: &str, kind: Option<MediaPlaybackType>) -> bool {
    if is_browser(app_id) || is_video_app(app_id) {
        return false;
    }
    match kind {
        Some(MediaPlaybackType::Music) => true,
        Some(MediaPlaybackType::Video) | Some(MediaPlaybackType::Image) => false,
        _ => is_music_app(app_id),
    }
}

/// The track that is playing, if any. Prefers the system's current session.
pub fn playing() -> Option<Track> {
    let manager = Manager::RequestAsync().ok()?.join().ok()?;
    let mut sessions: Vec<Session> = Vec::new();
    if let Ok(current) = manager.GetCurrentSession() {
        sessions.push(current);
    }
    if let Ok(all) = manager.GetSessions() {
        sessions.extend(all);
    }
    sessions.into_iter().find_map(|session| track_of(&session))
}

fn track_of(session: &Session) -> Option<Track> {
    let status = session.GetPlaybackInfo().ok()?.PlaybackStatus().ok()?;
    if status != Status::Playing {
        return None;
    }
    let app = session.SourceAppUserModelId().ok()?.to_string();
    if is_browser(&app) || is_video_app(&app) {
        return None;
    }
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let kind = props.PlaybackType().and_then(|k| k.Value()).ok();
    if !counts_as_music(&app, kind) {
        return None;
    }
    let title = props.Title().ok()?.to_string();
    if title.trim().is_empty() {
        return None;
    }
    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
    Some(Track { title, artist })
}

/// Every media session the system knows, as "app id: status" (for the probe test).
#[cfg(test)]
pub fn describe_sessions() -> Result<Vec<String>, windows::core::Error> {
    let manager = Manager::RequestAsync()?.join()?;
    let mut lines = Vec::new();
    for session in manager.GetSessions()? {
        let status = session.GetPlaybackInfo()?.PlaybackStatus()?;
        lines.push(format!("{}: {:?}", session.SourceAppUserModelId()?, status));
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browsers_are_recognised_by_app_id() {
        assert!(is_browser("Chrome"));
        assert!(is_browser("MSEdge"));
        assert!(is_browser("308046B0AF4A39CB"));
        assert!(!is_browser("Spotify.exe"));
        assert!(!is_browser(
            "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"
        ));
    }

    #[test]
    fn video_players_never_count() {
        for app in [
            "vlc.exe",
            "VLC",
            "org.videolan.vlc",
            "MPC-HC64.exe",
            "mpv.exe",
            "Microsoft.ZuneVideo_8wekyb3d8bbwe!Microsoft.ZuneVideo",
        ] {
            assert!(is_video_app(app), "{app}");
            assert!(
                !counts_as_music(app, Some(MediaPlaybackType::Music)),
                "{app}"
            );
            assert!(!counts_as_music(app, None), "{app}");
        }
    }

    #[test]
    fn music_needs_a_music_type_or_a_known_music_app() {
        assert!(counts_as_music(
            "Spotify.exe",
            Some(MediaPlaybackType::Music)
        ));
        assert!(counts_as_music("Spotify.exe", None));
        assert!(counts_as_music(
            "SomeNewPlayer.exe",
            Some(MediaPlaybackType::Music)
        ));
        assert!(!counts_as_music("SomeNewPlayer.exe", None));
        assert!(!counts_as_music(
            "Spotify.exe",
            Some(MediaPlaybackType::Video)
        ));
        assert!(counts_as_music(
            "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic",
            None
        ));
    }
}
