//! Windows: the track playing right now, from the System Media Transport Controls (the same
//! source as the volume flyout's media card). Spotify, most music players and many other apps
//! report to it.
//!
//! Browsers are left out on purpose: a tab that plays something says what the person is
//! browsing, which is not what they agreed to share. So are sessions the system marks as video.

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
    "chrome", "msedge", "edge", "firefox", "brave", "opera", "vivaldi", "iexplore", "librewolf",
    "waterfox", "zen", "arc",
    // Firefox reports its install id instead of a name.
    "308046b0af4a39cb",
];

fn is_browser(app_id: &str) -> bool {
    let id = app_id.to_lowercase();
    BROWSERS.iter().any(|b| {
        id.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part == *b)
    })
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
    if is_browser(&app) {
        return None;
    }
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    if let Ok(kind) = props.PlaybackType().and_then(|k| k.Value()) {
        if kind == MediaPlaybackType::Video {
            return None;
        }
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
        assert!(!is_browser("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"));
    }
}
