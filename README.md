# Mudrix Desktop

The desktop app for [Mudrix](https://github.com/kapsikkum/mudrix-web), built with [Tauri v2](https://tauri.app).

Mudrix Desktop is a fork of [WizardChat Desktop](https://gitlab.com/wizardchat/wizardchat-desktop)
by Arian Farahani and Jan Philipp Kleinschmidt, rebuilt on Tauri instead of Electron. It is licensed
under the AGPL-3.0 (see `LICENSE`), like upstream.

It wraps the Mudrix web client (the `mudrix-web` git submodule) and adds:

- a system tray icon (Open Mudrix, Mute notifications, Quit); closing the window hides to the tray
  (setting "Keep running in the background", on by default); left-click toggles the window
- a single running instance (a second launch focuses the window and forwards `matrix:` links)
- optional launch at login, optionally minimised to the tray
- the window size and position are remembered
- native notifications; the sound is still played by the web app
- an unread badge: tray icon and tooltip, Windows taskbar overlay, macOS dock badge
- `matrix:` deep links
- in-app updates, verified against a minisign signature (Windows and macOS install them, Linux gets a download prompt; AppImage installs update in place)
- a themed app icon that follows the theme setting (window and tray)
- SSO / OIDC login through the system browser (see below)

## Setup

```sh
git clone --recurse-submodules https://github.com/kapsikkum/mudrix-desktop.git
cd mudrix-desktop
npm install
```

If you cloned without submodules: `git submodule update --init --recursive`.

You need Node 20+, a Rust toolchain ([rustup](https://rustup.rs)) and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS (WebView2 on Windows,
Xcode command line tools on macOS, `webkit2gtk-4.1` and friends on Linux).

## Develop

```sh
npm run desktop:dev
```

This starts the web client's Vite dev server on port 8080 (`--strictPort`) and opens the Tauri window
on it. If you already run the web dev server on 8080, attach to it instead:

```sh
npm run desktop:dev:attach
```

The first run compiles the Rust dependencies, which takes a while and a lot of memory.

## Build

```sh
npm run desktop:build
```

This builds `mudrix-web` (`npm ci` and `npm run build` inside the submodule), then the installers into
`src-tauri/target/release/bundle/` (NSIS and MSI on Windows, `.dmg` and `.app` on macOS, AppImage,
`.deb` and `.rpm` on Linux). For a universal macOS build add `-- --target universal-apple-darwin`.
Releases are built by GitHub Actions, see `RELEASING.md`.

## Unsigned builds

Releases are not code-signed.

- **Windows**: SmartScreen shows "Windows protected your PC". Click "More info", then "Run anyway".
- **macOS**: Gatekeeper blocks the first launch. Open System Settings, Privacy & Security, scroll to
  Security and click "Open Anyway" (or right-click the app and choose Open).
- **Linux**: no warning. Make the AppImage executable (`chmod +x`) and run it.

The installers are unsigned, but in-app updates are not: every update is checked against a minisign
signature (`tauri-plugin-updater`) before it is installed.

## Update signing (maintainers)

Updates are signed with a key pair you generate once. The private key stays in GitHub secrets, the public
key is committed in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`, currently a placeholder):

```sh
npx tauri signer generate -w ~/.tauri/mudrix-updater.key
```

Paste the printed public key into `plugins.updater.pubkey`, and add the secrets
`TAURI_SIGNING_PRIVATE_KEY` (the key file's contents) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to the
repository. Release builds fail while the placeholder is still in place (set
`MUDRIX_ALLOW_PLACEHOLDER_UPDATER_KEY=1` for a throwaway local build; the app then keeps updates
disabled). Details in `RELEASING.md`.

## Login: SSO and OIDC

The webview's own origin (`http://tauri.localhost`, `tauri://localhost`) cannot be a redirect target for
an identity provider, so the app signs in through your system browser and a loopback listener on
`127.0.0.1` (the same approach the Electron app used):

- **OIDC (Matrix Authentication Service)**: the redirect URI is `http://127.0.0.1:34759/oidc/callback`,
  registered dynamically as a native client. The homeserver / MAS must allow loopback redirects for
  native clients (MAS does by default).
- **Matrix SSO (`m.login.sso`)**: the `redirectUrl` is rewritten to `http://127.0.0.1:<port>/`. Synapse only
  redirects to URLs in `sso.client_whitelist`, so the homeserver needs
  `client_whitelist: ["http://127.0.0.1"]` (or the port-less prefix) for desktop SSO to work.
- Password login needs nothing special.

Known gap: the "SSO popup" used for cross-signing / UIA re-authentication needs `window.opener`
messaging and is not reworked for the system browser; use the homeserver's account page in that case.

## Notifications

On Windows and Linux the notification plugin cannot report clicks. Instead, when the window gains focus
within 20 seconds of a notification (for example after clicking it, or the tray icon), the room of that
notification is opened.

## How the pieces fit

| path | what |
| --- | --- |
| `mudrix-web/` | git submodule, the web client (`https://github.com/kapsikkum/mudrix-web`) |
| `src-tauri/` | the Rust shell: tray, settings, notifications, auth loopback, updater |
| `src-tauri/tauri.conf.json` | window, CSP, bundle config (version read from `package.json`) |
| `src-tauri/capabilities/default.json` | permissions of the webview (only `core:default`) |
| `src-tauri/icons/` | app icons (generated from `logo.svg` with `tauri icon`) and `tray/` variants |
| `scripts/` | `check-version.mjs`, `set-version.mjs` |

The web client talks to the shell through `src/util/desktop.ts` in `mudrix-web`; none of it runs in a
normal browser.

The Content Security Policy in `tauri.conf.json` allows `https:` and `wss:` for homeservers, GIF providers
(api.giphy.com, api.klipy.com and their CDNs) and media, `blob:` and `data:` images, and
`'wasm-unsafe-eval'` for the Matrix crypto WebAssembly.
