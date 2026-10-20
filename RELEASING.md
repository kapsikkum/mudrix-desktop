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

## In-app updates (off by default)

`tauri-plugin-updater` is wired but compiled out (cargo feature `updater`) until a key pair and endpoint
exist. The web app's existing "update available" toast already listens for `desktop://update-ready`.

1. Generate the key pair (keep the private key secret and backed up; losing it strands installed apps):
   ```sh
   npx tauri signer generate -w ~/.tauri/mudrix.key
   ```
2. Put the **public** key (the content of `mudrix.key.pub`) into `src-tauri/tauri.updater.conf.json` as
   `plugins.updater.pubkey`; check the `endpoints` URL.
3. Add repository secrets `TAURI_SIGNING_PRIVATE_KEY` (content of the private key file) and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
4. Build with the feature and config:
   `npm run tauri -- build --features updater --config src-tauri/tauri.updater.conf.json`.
   In `release.yml` add these to the tauri-action `args` (and `uploadWorkflowArtifacts` already uploads the
   `.sig` files and update bundles).
5. Publish `latest.json` as a release asset (the endpoint
   `https://github.com/kapsikkum/mudrix-desktop/releases/latest/download/latest.json`), in the shape
   ```json
   {
     "version": "26.10.3",
     "notes": "Release notes",
     "pub_date": "2026-10-21T12:00:00Z",
     "platforms": {
       "windows-x86_64": { "signature": "<.sig content>", "url": "https://github.com/kapsikkum/mudrix-desktop/releases/download/v26.10.3/<setup.exe or .nsis.zip>" },
       "darwin-x86_64":  { "signature": "...", "url": ".../Mudrix.app.tar.gz" },
       "darwin-aarch64": { "signature": "...", "url": ".../Mudrix.app.tar.gz" },
       "linux-x86_64":   { "signature": "...", "url": ".../Mudrix_26.10.3_amd64.AppImage" }
     }
   }
   ```
   `tauri-action` can generate and upload this file when it runs with `includeUpdaterJson: true` and a
   `tagName`; since this workflow publishes the release in a separate job, do that from the build job or add a
   small step that assembles the JSON from the `.sig` files.

   Updates are verified against the public key, so an unsigned platform build is unaffected by this setup.

## Rollback

Delete the pre-release and its tag. Settings live in the OS config directory
(`chat.mudrix.desktop/desktop.json`) and the web profile in the webview data directory, so a downgrade
keeps the user's data.

## Not verified yet

- A full `tauri build` on each OS (only `cargo check` was run locally).
- The first end-to-end SSO/OIDC login through the loopback listener against a real homeserver.
- Windows taskbar overlay, macOS dock badge, `matrix:` handler registration from an installed build.
