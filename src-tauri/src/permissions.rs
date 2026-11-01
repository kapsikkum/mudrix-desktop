//! Microphone, camera and location access for calls and location sharing.
//!
//! WebView2 prompts the first time a page asks for the microphone, camera or location, and
//! `tauri dev` (http://localhost) is asked again every run. The prompt looks like a browser
//! popup, is easy to miss behind the call UI, and until it is answered the call never connects.
//! This answers it, but only for the app's own page: a request that comes from any other origin
//! (the window navigated away, a frame the app does not embed) is denied outright. Only those
//! three kinds are touched (location is only asked for by the "Share location" button, and
//! Windows' own location privacy setting still applies), anything else keeps WebView2's own
//! prompt.

#[cfg(windows)]
pub fn allow_call_media(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW, COREWEBVIEW2_PERMISSION_STATE_DENY,
    };
    use webview2_com::PermissionRequestedEventHandler;
    use windows::core::PWSTR;

    // The origin the app was loaded from (`tauri.localhost`, or the dev server in `tauri dev`).
    let app_origin = match window.url() {
        Ok(url) => url.origin(),
        Err(e) => {
            eprintln!("[desktop] no page url for the permission handler: {e}");
            return;
        }
    };

    let result = window.with_webview(move |webview| unsafe {
        let core = match webview.controller().CoreWebView2() {
            Ok(core) => core,
            Err(e) => {
                eprintln!("[desktop] no WebView2 core for the permission handler: {e}");
                return;
            }
        };
        let handler = PermissionRequestedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                args.PermissionKind(&mut kind)?;
                if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                    || kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA
                    || kind == COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION
                {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let from_app = tauri::Url::parse(&webview2_com::take_pwstr(uri))
                        .is_ok_and(|url| url.origin() == app_origin);
                    args.SetState(if from_app {
                        COREWEBVIEW2_PERMISSION_STATE_ALLOW
                    } else {
                        COREWEBVIEW2_PERMISSION_STATE_DENY
                    })?;
                }
            }
            Ok(())
        }));
        let mut token = Default::default();
        if let Err(e) = core.add_PermissionRequested(&handler, &mut token) {
            eprintln!("[desktop] could not install the permission handler: {e}");
        }
    });
    if let Err(e) = result {
        eprintln!("[desktop] could not reach the webview for permissions: {e}");
    }
}

/// Other platforms keep their webview's own permission prompt.
#[cfg(not(windows))]
pub fn allow_call_media(_window: &tauri::WebviewWindow) {}
