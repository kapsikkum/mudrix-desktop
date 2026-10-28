# Releasing Mudrix Desktop

Releases are plain `v*` tags built by `.github/workflows/release.yml`. A tag with a suffix
(`v26.10.3-next.1`, `-rc.1`, `-beta.1`) is published as a pre-release, which GitHub does not treat as
"latest". Upstream WizardChat used `prepare/v26.10.2-next.N` branches; the numbers here follow the same
`YEAR.MONTH.N` scheme.

Installers are **not code-signed** (no Windows certificate, no Apple notarization, by decision). The release
notes say so; see "Unsigned builds" in the README. In-app update artifacts are signed with a minisign key
(see "In-app updates").

## What a release contains

| file | what |
| --- | --- |
| `Mudrix-<v>-windows-x64-setup.exe` | NSIS installer, per user |
| `Mudrix-<v>-windows-x64.msi` | MSI (stable releases only, see below) |
| `Mudrix-<v>-macos-universal.dmg` | universal (Apple silicon and Intel) |
| `Mudrix-<v>-linux-x64.AppImage` / `.deb` / `.rpm` | Linux |
| `Mudrix-<v>-windows-x64-setup.exe.sig`, `...-macos-universal.app.tar.gz` (+ `.sig`), `...-linux-x64.AppImage.sig` | updater artifacts and minisign signatures |
| `latest.json` | update manifest read by `tauri-plugin-updater` (windows-x86_64, darwin-aarch64, darwin-x86_64, linux-x86_64) |
| `SHA256SUMS.txt` | `sha256sum` format, all of the above |

Keep the `Mudrix-<v>-windows-x64-setup.exe` / `-macos-universal.dmg` names and `SHA256SUMS.txt`: installs
from before the signed updater still use the old updater, which looks for exactly those.

MSI (WiX) only accepts numeric pre-release versions, so pre-release tags build the NSIS installer only
on Windows.

## Cut a release

1. Make sure the `mudrix-web` submodule points at the web commit you want to ship:
   ```sh
   git -C mudrix-web fetch origin
   git -C mudrix-web checkout v26.10.3        # or a commit on main
   git add mudrix-web
   ```
   The submodule URL is `https://github.com/kapsikkum/mudrix-web.git`. Push that repository (and its tag)
   first, or the CI checkout cannot find the commit.
2. Bump the version (single source of truth is `package.json`; `src-tauri/Cargo.toml` is kept in step and
   `tauri.conf.json` reads `package.json`):
   ```sh
   node scripts/set-version.mjs 26.10.3
   git commit -am "chore(release): prepare v26.10.3"
   ```
   `node scripts/check-version.mjs` must pass; CI runs it for the tag as well.
3. `git tag v26.10.3 && git push origin main v26.10.3`.
4. The workflow checks the version and the update signing key, builds the web client from the submodule, builds the Tauri bundles on
   Windows, macOS and Linux, renames them to `Mudrix-<v>-<platform>.<ext>`, writes `latest.json` from the `.sig` files, adds
   `SHA256SUMS.txt` and publishes the release (a pre-release when the version has a suffix).
5. Smoke test on a clean profile: install, sign in, receive a notification with the window closed (tray),
   check the unread badge, open a `matrix:` link, quit from the tray.

`workflow_dispatch` runs the same pipeline for an existing tag/version: enter the version, it must match
`package.json`.

## Locally

```sh
npm install
npm run desktop:build
```

Output is in `src-tauri/target/release/bundle/`.

## Secrets

| secret | used for |
| --- | --- |
| `GITHUB_TOKEN` | provided by GitHub, creates the release |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | **required**: signing the in-app update artifacts |
| `VITE_GIPHY_API_KEY`, `VITE_KLIPY_API_KEY` | optional web client keys |

The release workflow fails in its first job when the two signing secrets are missing or
`plugins.updater.pubkey` has been put back to the placeholder value.

Windows code signing and Apple signing/notarization are intentionally not wired up. If that changes later,
tauri-action reads `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_PASSWORD`, `APPLE_TEAM_ID` for macOS and `bundle.windows.signCommand` (or a certificate thumbprint)
for Windows.

## Update signing key

The key pair has been generated: the public key is committed as `plugins.updater.pubkey` in
`src-tauri/tauri.conf.json`, and the private key and its password are the `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` repository secrets. Keep a backup of the private key. Installs only accept
updates signed by it; if it is lost or changed, existing installs can no longer update in place and users have
to reinstall manually.

To create a new key pair (a fresh fork, or a deliberate rotation):

1. Generate it (pick a password; the workflow needs one):
   ```sh
   npx tauri signer generate -w ~/.tauri/mudrix-updater.key
   ```
   This writes the private key to that file and prints the public key (also saved as `.key.pub`).
2. Put the public key into `src-tauri/tauri.conf.json` as `plugins.updater.pubkey` and commit it.
3. In the GitHub repository, Settings > Secrets and variables > Actions, replace
   `TAURI_SIGNING_PRIVATE_KEY` (the contents of the private key file) and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Local release builds (`npm run desktop:build`) also need `TAURI_SIGNING_PRIVATE_KEY` (and the password) in the
environment because `bundle.createUpdaterArtifacts` is on. The build script panics for release builds when
the public key is the placeholder (as in a fresh fork); `MUDRIX_ALLOW_PLACEHOLDER_UPDATER_KEY=1` skips that
check (the app then logs that updates are disabled and never checks).

## In-app updates

`tauri-plugin-updater`, driven from Rust only (`src-tauri/src/updater.rs`; the webview has no updater
permissions). The app reads `https://github.com/kapsikkum/mudrix-desktop/releases/latest/download/latest.json`
at start-up and every hour; GitHub's "latest" ignores pre-releases, so `-next`/`-rc`/`-beta` tags are
not offered. Builds from `tauri dev` and installs with `MUDRIX_NO_UPDATE` set never check. Every download is
verified against the minisign signature in `latest.json` and the public key baked into the app.

- **Windows:** downloads and verifies the NSIS installer in the background and shows the web app's update
  toast. "Update now" runs the per-user installer in passive mode (a small progress window, no prompts) and
  restarts the app.
- **macOS:** downloads and verifies `Mudrix-<v>-macos-universal.app.tar.gz`, then replaces the app bundle
  and restarts.
- **Linux AppImage:** updates in place like the above. **.deb/.rpm:** only announces the release
  (`desktop://update-available`); the toast links to the release page.

Settings > About has a manual "Check for updates" button. Update progress and failures are appended to
`update.log` in `<temp dir>/mudrix-update/`.

### Upgrading from builds with the old updater

Installs from before this change still run the old updater (releases.atom feed plus `SHA256SUMS.txt`, silent
install). It finds the first release that has the new updater as long as the release still contains
`Mudrix-<v>-windows-x64-setup.exe` / `Mudrix-<v>-macos-universal.dmg` and `SHA256SUMS.txt`, which the
workflow keeps producing. After that one update they use the signed updater. The first release with the
signed updater should be a stable `vX.Y.Z` tag (the old updater ignores pre-releases).

## Rollback

Delete the pre-release and its tag. Settings live in the OS config directory
(`chat.mudrix.desktop/desktop.json`) and the web profile in the webview data directory, so a downgrade
keeps the user's data.

## Not verified yet

The release pipeline has produced working Windows, macOS and Linux bundles (v26.10.18 is the latest), but
these still need a hands-on check on real machines:

- The first end-to-end SSO/OIDC login through the loopback listener against a real homeserver.
- Windows taskbar overlay, macOS dock badge, `matrix:` handler registration from an installed build.
- The signed updater against a real newer release, including the `latest.json` the workflow writes (the
  Windows passive install and the macOS bundle swap especially); it needs an installed build older than the
  latest published release to test.
- Activity detection on real machines (Spotify, VLC and other players, games from the known-games list).
