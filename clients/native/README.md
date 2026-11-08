# Lookout Native ("Lookout Phoenix")

A native rewrite of the Lookout desktop app in Rust with [GPUI](https://gpui.rs)
(through `gpui-kit` 0.8.1 / `gpui-pre` 0.3.10). It replaces the Tauri shell in
`clients/desktop` and drives the **same recording engine** in-process:
`lookout-core` and the three platform-free crates under
`clients/desktop/src-tauri/crates`.

The north star is that people can't tell it apart from the Tauri app: every
screen, string, colour token and animation was ported from the original
source (see `docs/ui-spec.md`, which cites `file:line` for every value). Where
the original had an actual flaw, the native app deliberately does better; those
places are listed below.

## Run it

```sh
# Linux: GPUI needs the X11/Wayland/Vulkan dev libs; on the dev WSL box:
source /tmp/lookout-sysroot/env.sh

cd clients/native
cargo run -p lookout-native            # the app (binary: `lookout`)
cargo test -p lookout-native           # motion maths, editor maths, formatting, theme, deep links…
```

Offline development against a fake server:

```sh
cargo run -p lookout-dev-server        # http://127.0.0.1:8787, prints seed session links
LOOKOUT_API_BASE=http://127.0.0.1:8787 \
LOOKOUT_SEED_TOKENS=<comma-separated tokens from the server's output> \
cargo run -p lookout-native
```

`DEV_TIP=1`, `DEV_ANNOUNCE=1` and `DEV_EDIT=1` on the dev server turn on a tip
sheet, an announcement banner and an edit-hold session (editor with units and a
local video). Other knobs:

| env | effect |
|---|---|
| `LOOKOUT_SLOWMO=5` | plays every animation 5× slower, to check it frame by frame |
| `LOOKOUT_API_BASE` | points this run at another server without changing the saved choice |
| `LOOKOUT_SEED_TOKENS` | adds session tokens to the gallery at startup |
| `RUST_LOG=lookout=debug` | verbose logging |
| `LOOKOUT_UPDATE_DEMO=1` | (debug builds) plays a fake update download → "Restart to Complete Update" in the update pill |
| `LOOKOUT_UPDATE_URL` / `_BETA_URL` / `_PUBKEY` / `_DIR` / `_AS_VERSION` | (debug builds) point the updater at a fake release (see Auto-update) |

`lookout://session/?token=…` and `lookout://pair?…` links can be passed on the
command line. A second launch hands them to the running instance and exits.

## Layout

```
crates/lookout-native/src/
  main.rs            boot: logging, single instance, tokio runtime, fonts, globals, windows
  app.rs             the engine bridge: Frontend impl → channel → RecorderModel entity
  store.rs           persisted state (was localStorage) + secret store; imports the Tauri app's data once
  motion.rs          framer-motion springs, CSS cubic-bezier, NumberFlow's curve, keyframe loops
  theme.rs           every colour token (dark + light), OKLab accent hover, Linux header-bar vars
  ui/                Button (squircle + press spring), Spinner, Skeleton, Badge, Card/PressScale,
                     HeaderBar, TextInput, Overlay/ConfirmDialog, Sheet (vaul), MenuPopup,
                     ErrorBanner, ProgressRing, NumberFlow, VideoPlayer (GStreamer), tooltips
  screens/           gallery, record (picker ⇄ recorder), source_picker, recorder, modals,
                     session, settings, add_session, permission, editor (TimelapseEditor),
                     program_panel, layout
  windows/           main_window (router, transitions, chrome, overlays), editor, tray_window
  platform/          tray (SNI + GNOME pill / tray-icon / NSStatusItem), camera (GStreamer),
                     permissions (macOS), app_menu (macOS menu bar), desktop_appearance (Linux),
                     single_instance
  services.rs        tip / announcement / Linux-update polling
  updater/           auto-update v2 glue (mod.rs) + per-platform install/rollback (platform.rs)
  programs.rs        program registry, PKCE device pairing, instant start
  previews.rs        live source previews through the engine
  filmstrip.rs       editor filmstrip frame extraction
  editor_math.rs     cuts ⇄ regions, ruler, build-progress estimate
  diagnosis.rs       capture-error diagnosis (portal / PipeWire / GStreamer / TCC)
crates/lookout-dev-server/   in-memory Lookout API for offline work (+ fake update releases)
crates/lookout-update/       update v2 core: signed manifest, rollout, zstd patches, resumable downloads, state
crates/lookout-update-tool/  release side: update.json, patches, signing, fake releases
assets/              Geist (TTF), Phosphor + hand-drawn SVG icons, GNOME extension, app icons
docs/                ui-spec.md (source of truth), gpui-research.md, design-guidance.md
packaging/           release bundling, runnable locally (see Packaging):
  version.sh         print / stamp the workspace version
  macos/             bundle.sh (Lookout.app, .app.tar.gz, .dmg; sign + notarize), Info.plist, Entitlements.plist
  windows/           lookout.nsi (per-user NSIS installer, lookout:// handler)
  linux/             appimage.sh, .desktop entry with the lookout:// handler, local install script
```

## Auto-update ("update v2")

Hosted on GitHub Releases. macOS, Windows and Linux **AppImages** (only when
`$APPIMAGE` is set) update themselves; deb/rpm/pacman installs keep the
package-repo path and the UpdateDrawer.

- **Manifest**: `update.json` + `update.json.minisig`, signed as a whole with
  the Tauri app's updater key and verified *before* parsing. Fields: `schema`,
  `version`, `pub_date`, `notes`, `channel`, `min_supported`, `rollout`
  (0..1), `blocked`, and per platform (`native-darwin-aarch64`,
  `native-darwin-x86_64`, `native-windows-x86_64`, `native-linux-x86_64`)
  `{url, size, sha256, format, payload_size, payload_sha256, patches[]}`, with
  each patch `{from_version, from_sha256, url, size, sha256}`. An older
  manifest is refused unless the installed version is in `blocked`.
- **Channels**: Stable reads `releases/latest/download/`, Beta (Settings →
  "Beta updates") the rolling `channel-beta` prerelease. A manifest for the
  other channel is refused.
- **Rollout**: an install takes a release when `sha256(install_id)` (as a
  fraction of 2^64) `< rollout`. Blocked or below-`min_supported` installs
  skip the rollout and install as soon as nothing is being captured.
- **Deltas**: `zstd --patch-from`-format patches (raw-content prefix + long
  distance matching) against the last 3 releases, made and applied in Rust
  (`zstd` crate). Patches cover the *payload*: the `.tar` inside
  `.app.tar.gz`, the setup `.exe` and the `.AppImage` as they are. The client
  checks its base against `from_sha256` and the result against
  `payload_sha256`; any problem falls back to the full file. Full downloads
  resume with `Range` into a `.part` file.
- **Install**: downloads stay in the background; the staged update installs
  on quit, or at the next launch before any window opens. A launch carrying
  `lookout://` links waits for quit instead, so the links aren't lost. The
  pill's "Restart to Complete Update" installs right away. Nothing installs
  during a capture.
- **Rollback**: macOS keeps the old bundle in `.lookout-previous/` beside the
  app, Windows a copy of the install dir, and AppImages a copy of the file
  (in the updates dir). A new version is healthy after its main window has
  been up for 20 s. After 2 launches that don't get there, the next launch
  restores the previous version and never offers that one again unless forced.
- **What's new**: the notes appear once in the tip sheet after an update.
- State lives in `<data-local>/com.hackclub.lookout/updates/state.json`.

Release side: the `native-build` job in `.github/workflows/release.yml`
builds the `Lookout-native_*` artifacts (see Packaging), and `native-updates`
runs `lookout-update-tool manifest … --sign` over them (key from
`TAURI_SIGNING_PRIVATE_KEY`). Both are off until the repository variable
`NATIVE_RELEASE` is `true` (or "native" is ticked on a manual run), and
`latest.json` is untouched either way.

Local end-to-end test (debug builds read the overrides):

```sh
cargo run -p lookout-update-tool -- fake-release --dir /tmp/lu --base-url http://127.0.0.1:8787/updates > /tmp/lu.env
LOOKOUT_DEV_UPDATES=/tmp/lu DEV_UPDATE_DROP=1 cargo run -p lookout-dev-server &   # DROP: first full download stops halfway
set -a; . /tmp/lu.env; set +a      # LOOKOUT_UPDATE_URL/_BETA_URL/_PUBKEY, APPIMAGE (fake), LOOKOUT_UPDATE_AS_VERSION
LOOKOUT_UPDATE_DIR=/tmp/lu-state cargo run -p lookout-native
# or without a window:
cargo run -p lookout-update --example simulate -- "$LOOKOUT_UPDATE_URL" "$LOOKOUT_UPDATE_PUBKEY" 0.4.0 "$APPIMAGE" /tmp/lu-work
```

## Packaging

The scripts in `packaging/` are what CI runs; each writes to `target/bundle/`
by default (`--out` to change, `--no-build` to reuse `target/release`).
Release builds stamp the tag's version first (`packaging/version.sh --set`).

| platform | script | artifacts |
|---|---|---|
| macOS (Xcode) | `packaging/macos/bundle.sh --target aarch64-apple-darwin` | `Lookout-native_<v>_darwin-<arch>.app.tar.gz` (updater) + `.dmg` |
| Windows (NSIS 3.08+) | `cargo build --release`, then `makensis -DVERSION=… -DSRC_DIR=… packaging/windows/lookout.nsi` | `Lookout-native_<v>_windows-x86_64-setup.exe` |
| Linux | `packaging/linux/appimage.sh` | `Lookout-native_<v>_linux-x86_64.AppImage` |

- **macOS**: `Lookout.app` reuses the Tauri app's bundle identifier
  (`com.hackclub.lookout`), so with the same Developer ID team the Screen
  Recording/Camera grants and the data directories carry over. Signing and
  notarizing use the Tauri job's `APPLE_*` secrets; without them the bundle is
  ad-hoc signed. Nothing besides the executable is bundled (AVFoundation,
  VideoToolbox, OS Swift runtime), and the script fails if that changes.
- **Windows**: per-user install to `%LOCALAPPDATA%\Lookout` with the Tauri
  installer's registry keys, so it upgrades a Tauri install in place.
  Registers `lookout://`; takes `/S`, `/UPDATE`, `/R` (the updater's flags).
  `-DSIGN_CMD=<script>` signs the installer and uninstaller as they're built.
- **Linux AppImage** (build on Ubuntu 24.04, so glibc ≥ 2.39): bundles GTK 3,
  WebKitGTK 4.1 and its helper processes, libsoup 3, glib-networking, and
  GStreamer with the plugins the app uses (x264, libav, VP9, mp4/webm, v4l2,
  souphttpsrc, pipewiresrc, …). It needs from the host: glibc, GL/EGL/Vulkan
  and the GPU drivers, libwayland, libasound, libva, and libpipewire-0.3
  (which loads the host's own PipeWire modules). The AppRun hook `cd`s into
  the AppImage, because WebKitGTK's helper path is patched to a relative one.
  The AppImage doesn't register `lookout://` itself (use an AppImage
  integrator, or `packaging/linux/install-local.sh` for a plain build).
  No .deb: deb/rpm installs stay on the Tauri package repo for now.

## How the animations were kept identical

GPUI has no CSS, so the motion is reimplemented from the libraries' source:

- **Springs** use framer-motion's analytic solution (`motion-dom` `spring.mjs`),
  with the same rest thresholds. The button press is `{stiffness 1500, damping 60}`
  on a 0.96 scale. Routes use 460/36/0.7, overlays 420/34/0.7, the add menu
  1800/56, and so on, all as constants in `motion.rs`.
- **Tweens** use a real CSS `cubic-bezier()` solver (the WebKit/Chromium
  algorithm). This covers vaul's `0.32, 0.72, 0, 1`, motion's `easeOut`, CSS
  `ease`, and the editor's ease-out-quart.
- **NumberFlow's** digit roll uses its exact 90-stop `linear()` curve over
  900ms, with the trend direction and 450ms fades.
- **Keyframe loops** run the `pulse` dot (1.5s, eased per segment), `spin`
  (1s linear) and the skeleton `shimmer`, all from the same clock.
- **Retargeting** works like framer-motion: an interrupted animation continues
  from its current value and velocity.
- **Scale effects**: GPUI 0.3.10 can't transform ordinary elements, so anything
  that scaled (the button background, press surfaces, the add menu, preview
  tiles, the hover card) is painted with the scale baked into its geometry.
  The button's squircle is `figma-squircle`'s path with 0.7 smoothing.

## Deliberate improvements

- Modals (Overlay, NamingModal, confirms) **fade out over 120ms** instead of
  vanishing. The Tauri app animated them in but unmounted them instantly.
- Skeletons **always shimmer**. In the Tauri app the `shimmer` keyframes only
  existed after the recorder had mounted once, so most skeletons were static.
- Secondary buttons keep their **1px border around the squircle corners**. The
  web version clipped a rectangular border with the squircle, so the border
  vanished at each corner.
- Linux uses the desktop UI font when it's installed (Adwaita Sans or
  Cantarell). Otherwise it falls back to the bundled Geist instead of
  fontconfig's generic sans.
- The source picker header keeps "Stop Session" on the right on Linux, the
  same as macOS and Windows. Tauri put it on the left only because the Linux
  build hides the Back button.
- Reduced motion (OS setting) collapses spatial springs; fades still play.
- Closing a sheet before it has finished opening reverses smoothly from where
  it is. In the webview, CSS restarted the transition from a jumped position.
- Quitting while a capture is running pauses the session first, from every
  close path: the window button, the Linux header bar and ⌘Q.
- A deep link that arrives before the tip has loaded still gets its tip moment.
- In-app confirm dialogs replace the GTK `confirm()` popups, styled like the
  rest of the app. On Windows the primary button comes first.

## Known gaps and next steps

The macOS and Windows code type-checks from Linux (`aarch64-apple-darwin`,
`x86_64-pc-windows-msvc`) but **has not run on either platform yet**. Things
to check on real hardware first:

- **Video playback and filmstrips** use AVFoundation (`AVAssetReader`) and Media
  Foundation (`IMFSourceReader`) through `platform/decoder`, clocked by a small
  player thread. Linux uses GStreamer. Check seek accuracy, rotation metadata
  (ignored on macOS) and cost of decoding full frames for 112px tiles (Windows).
- **Camera capture** uses AVCaptureSession / Media Foundation. The macOS bundle
  needs `NSCameraUsageDescription`; check Windows frames come out upright.
- **Auto-update** install/rollback on macOS (bundle renames, `ditto`, the
  admin prompt for a non-writable `/Applications`) and Windows (NSIS `/S`,
  copy of the install dir, robocopy rollback script) type-checks but has only
  run on Linux, with a fake AppImage. The release `native-build` job now
  produces the artifacts, but none of them has been through CI yet.
- **Program Panel** embeds the page with wry (WebKitGTK / WKWebView / WebView2).
  On native Wayland it can't embed and shows the "Open in browser" fallback.
  The Linux binary now needs WebKitGTK 4.1; `--no-default-features` drops it.
- **macOS menu-bar item** reuses the Tauri app's Swift package
  (`swift/lookout-tray`) through `swift-rs` for the click-to-open popup and
  digit roll; the Swift build itself is skipped by the cross-check.
- **macOS 26 display link**: `platform/macos_occlusion.rs` works around GPUI
  only starting a window's frame callback on the documented "visible" bit,
  which macOS 26 stops setting (Cap hit the same bug). Check on macOS 26.
- **Animations** were audited one by one against the Tauri source; the table
  and the few deliberate deviations are in `docs/animation-audit.md`.
