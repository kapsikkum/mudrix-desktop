//! SSO and OIDC login through the system browser, with a loopback callback.
//!
//! The webview's own origin (`http://tauri.localhost`, `tauri://localhost`) cannot be a
//! redirect target for an identity provider, so, like the previous Electron app, login runs
//! in the user's browser and returns to a tiny HTTP listener on 127.0.0.1:
//!
//! * OIDC: the web app registers `http://127.0.0.1:34759/oidc/callback` as its redirect URI
//!   (a loopback redirect for native apps, RFC 8252). The authorization URL is opened as is.
//! * Matrix SSO: the `redirectUrl` of the `/login/sso/redirect` URL is rewritten to the
//!   loopback listener, carrying the original query (`state`) plus a one-time nonce.
//!
//! A callback is turned into an in-app path (`/oidc/callback?...` or `/sso/callback?...`)
//! that the web app loads, so the existing callback pages finish the login unchanged.

use crate::commands;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::AppHandle;
use url::Url;

/// Must match `getOidcRedirectUri()` in the web app.
pub const OIDC_PORT: u16 = 34759;
const NONCE_PARAM: &str = "_sso";
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Mudrix</title>\
<style>body{margin:0;height:100vh;display:flex;align-items:center;justify-content:center;\
background:#1a1b1e;color:#e8e8f0;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;\
font-size:18px}</style></head><body>You are signed in. You can close this tab and return to Mudrix.</body></html>";

struct Listener {
    port: u16,
    nonce: String,
    /// Pushed forward by every login attempt; the thread exits once it passes.
    deadline: Instant,
}

static ACTIVE: Mutex<Option<Listener>> = Mutex::new(None);

/// Start (or reuse) the loopback listener and return its port and nonce.
fn ensure_listening(app: &AppHandle) -> Result<(u16, String), String> {
    let mut active = ACTIVE.lock().unwrap();
    if let Some(l) = active.as_mut() {
        if l.deadline > Instant::now() {
            l.deadline = Instant::now() + IDLE_TIMEOUT;
            return Ok((l.port, l.nonce.clone()));
        }
    }

    let listener = TcpListener::bind(("127.0.0.1", OIDC_PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .map_err(|e| format!("could not open the login callback port: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    *active = Some(Listener {
        port,
        nonce: nonce.clone(),
        deadline: Instant::now() + IDLE_TIMEOUT,
    });
    drop(active);

    let app = app.clone();
    std::thread::spawn(move || serve(&app, &listener, port));
    Ok((port, nonce))
}

fn serve(app: &AppHandle, listener: &TcpListener, port: u16) {
    loop {
        {
            let active = ACTIVE.lock().unwrap();
            match active.as_ref() {
                Some(l) if l.port == port && l.deadline > Instant::now() => {}
                _ => break,
            }
        }
        match listener.accept() {
            Ok((stream, _)) => handle(app, stream, port),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let mut active = ACTIVE.lock().unwrap();
    if active.as_ref().is_some_and(|l| l.port == port) {
        *active = None;
    }
}

fn respond(mut stream: TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn handle(app: &AppHandle, mut stream: TcpStream, port: u16) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 8192];
    let mut len = 0;
    while len < buf.len() {
        match stream.read(&mut buf[len..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                len += n;
                if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    let request = String::from_utf8_lossy(&buf[..len]).into_owned();
    let mut lines = request.lines();
    let first = lines.next().unwrap_or_default();
    let host = lines
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("host")
                .then(|| value.trim().to_ascii_lowercase())
        })
        .unwrap_or_default();

    let mut parts = first.split_whitespace();
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let host_ok = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let Ok(url) = Url::parse(&format!("http://127.0.0.1:{port}{target}")) else {
        return respond(stream, "400 Bad Request", DONE_PAGE);
    };

    let has = |key: &str| url.query_pairs().any(|(k, _)| k == key);
    let nonce = url
        .query_pairs()
        .find(|(k, _)| k == NONCE_PARAM)
        .map(|(_, v)| v.into_owned());
    let expected = ACTIVE.lock().unwrap().as_ref().map(|l| l.nonce.clone());
    // SSO callbacks carry the nonce we put in; OIDC callbacks (fixed redirect URI) do not.
    let nonce_ok = nonce.is_none() || nonce == expected;

    let is_sso = has("loginToken");
    let is_oidc = has("code") || has("error");
    if method != "GET" || !host_ok || !nonce_ok || !(is_sso || is_oidc) {
        return respond(stream, "400 Bad Request", DONE_PAGE);
    }

    // Everything except our own nonce goes on to the web app.
    let query: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != NONCE_PARAM)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut path = Url::parse(if is_sso {
        "http://app/sso/callback"
    } else {
        "http://app/oidc/callback"
    })
    .expect("static url");
    path.query_pairs_mut().extend_pairs(query);
    let in_app = match path.query() {
        Some(q) => format!("{}?{q}", path.path()),
        None => path.path().to_string(),
    };

    respond(stream, "200 OK", DONE_PAGE);
    commands::deliver_auth_callback(app, in_app);
}

/// Open an authorization URL in the system browser and arrange for the result to come back.
pub fn open_in_browser(app: &AppHandle, raw: &str) -> Result<(), String> {
    let mut url = Url::parse(raw).map_err(|e| format!("invalid url: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http(s) login urls are supported".into());
    }
    let (port, nonce) = ensure_listening(app)?;

    // Matrix SSO: send the user back to the loopback listener instead of the webview origin.
    let redirect = url
        .query_pairs()
        .find(|(k, _)| k == "redirectUrl")
        .map(|(_, v)| v.into_owned());
    if let Some(redirect) = redirect {
        let carried: Vec<(String, String)> = Url::parse(&redirect)
            .map(|r| {
                r.query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let mut loopback = Url::parse(&format!("http://127.0.0.1:{port}/")).expect("static url");
        loopback
            .query_pairs_mut()
            .extend_pairs(carried)
            .append_pair(NONCE_PARAM, &nonce);

        let others: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "redirectUrl")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(others)
            .append_pair("redirectUrl", loopback.as_str());
    }

    commands::open_url(app, url.as_str())
}
