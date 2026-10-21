# Releasing Mudrix Desktop

Releases are plain `v*` tags built by `.github/workflows/release.yml`. A tag with a suffix
(`v26.10.3-next.1`, `-rc.1`, `-beta.1`) is published as a pre-release, which GitHub does not treat as
"latest". Upstream WizardChat used `prepare/v26.10.2-next.N` branches; the numbers here follow the same
`YEAR.MONTH.N` scheme.

Builds are **unsigned** (no Windows certificate, no Apple notarization, by decision). The release notes say
so; see "Unsigned builds" in the README.

## What a release contains

| file | what |
| --- | --- |
| `Mudrix-<v>-windows-x64-setup.exe` | NSIS installer, per user |
| `Mudrix-<v>-windows-x64.msi` | MSI (stable releases only, see below) |
| `Mudrix-<v>-macos-universal.dmg` | universal (Apple silicon and Intel) |
| `Mudrix-<v>-linux-x64.AppImage` / `.deb` / `.rpm` | Linux |
| `SHA256SUMS.txt` | `sha256sum` format, all of the above |

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
4. The workflow checks the version, builds the web client from the submodule, builds the Tauri bundles on
   Windows, macOS and Linux, renames them to `Mudrix-<v>-<platform>.<ext>`, adds `SHA256SUMS.txt` and
   publishes the release (a pre-release when the version has a suffix).
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

None are required. Everything below is optional.

| secret | used for |
| --- | --- |
| `GITHUB_TOKEN` | provided by GitHub, creates the release |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | signing in-app update artifacts (below) |

Windows code signing and Apple signing/notarization are intentionally not wired up. If that changes later,
tauri-action reads `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`,
`APPLE_PASSWORD`, `APPLE_TEAM_ID` for macOS and `bundle.windows.signCommand` (or a certificate thumbprint)
for Windows.

## In-app updates

Updates need no signing key or endpoint. The app (`src-tauri/src/updater.rs`) reads the public
`https://github.com/kapsikkum/mudrix-desktop/releases.atom` feed at start-up and every six hours (a plain
web page, so no GitHub API rate limit) and picks the newest stable `vX.Y.Z` tag; pre-release tags are ignored.
Builds from `tauri dev` and installs with `MUDRIX_NO_UPDATE` set never check.

- **Windows:** downloads `Mudrix-<v>-windows-x64-setup.exe`, verifies it against the release's
  `SHA256SUMS.txt`, and shows the web app's update toast. "Update now" quits, runs the per-user installer
  silently (`/S /UPDATE`) and starts the new build.
- **macOS:** downloads and verifies `Mudrix-<v>-macos-universal.dmg`, then replaces the running `.app` and
  reopens it (if it is not run from an app bundle the disk image is opened instead).
- **Linux:** only announces the release (`desktop://update-available`); the toast links to the release page.

The release workflow must keep these asset names and `SHA256SUMS.txt`. Settings > About has a manual
"Check for updates" button. Signed updates (`tauri-plugin-updater`) were dropped: builds are unsigned.

## Rollback

Delete the pre-release and its tag. Settings live in the OS config directory
(`chat.mudrix.desktop/desktop.json`) and the web profile in the webview data directory, so a downgrade
keeps the user's data.

## Not verified yet

- A full `tauri build` on each OS (only `cargo check` was run locally).
- The first end-to-end SSO/OIDC login through the loopback listener against a real homeserver.
- Windows taskbar overlay, macOS dock badge, `matrix:` handler registration from an installed build.
- The in-app updater against a real newer release (the Windows silent install and the macOS bundle swap
  especially); it needs an installed build older than the latest published release to test.
