//! Microphone and camera access for calls.
//!
//! WebView2 prompts the first time a page asks for the microphone or camera, and `tauri dev`
//! (http://localhost) is asked again every run. The prompt is easy to miss behind the call UI,
//! and until it is answered the call never connects. This answers it: only those two kinds are
//! allowed, anything else keeps WebView2's own prompt.

#[cfg(windows)]
pub fn allow_call_media(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;

    let result = window.with_webview(|webview| unsafe {
        let core = match webview.controller().CoreWebView2() {
            Ok(core) => core,
            Err(e) => {
                eprintln!("[desktop] no WebView2 core for the permission handler: {e}");
                return;
            }
        };
        let handler = PermissionRequestedEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                args.PermissionKind(&mut kind)?;
                if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                    || kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA
                {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
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
