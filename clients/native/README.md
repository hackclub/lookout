# Lookout Native ("Lookout Phoenix")

A native rewrite of the Lookout desktop app in Rust with [GPUI](https://gpui.rs)
(through `gpui-kit` 0.8.1 / `gpui-pre` 0.3.10). It replaced the Tauri app
(removed after v0.4.2; see the `v0.3.14` tag) and drives the recording engine
in-process: `lookout-core` and the three platform-free crates under `crates/`
at the repo root.

Every screen, string, colour token and animation started as a port of the
Tauri app (`docs/ui-spec.md` cites `file:line` for every original value), but
matching it is no longer a goal: Lookout Native should feel like it belongs on
each OS. Where the platform has a better or more native way to do something,
follow the platform, and list the change under "Where it departs from the
Tauri app" below.

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
local video). `LOOKOUT_DEV_VIDEO=<file>` picks the video and
`LOOKOUT_DEV_VIDEO_HTTP=1` serves it over HTTP with Range support, like R2, to
exercise streaming. Other knobs:

| env | effect |
|---|---|
| `LOOKOUT_SLOWMO=5` | plays every animation 5× slower, to check it frame by frame |
| `LOOKOUT_API_BASE` | points this run at another server without changing the saved choice |
| `LOOKOUT_SEED_TOKENS` | adds session tokens to the gallery at startup |
| `RUST_LOG=lookout=debug` | verbose logging |
| `LOOKOUT_UPDATE_DEMO=1` | (debug builds) plays a fake update download → "Restart to Complete Update" in the update pill |
| `LOOKOUT_UPDATE_URL` / `_BETA_URL` / `_PUBKEY` / `_DIR` / `_AS_VERSION` | (debug builds) point the updater at a fake release (see Auto-update) |
| `LOOKOUT_SENTRY_DSN` | Sentry reporting ([`src/crash.rs`](crates/lookout-native/src/crash.rs)): release builds report to `hack-club/lookout-native` by default, debug builds only with this set; empty turns it off |
| `LOOKOUT_CRASH_TEST=segfault\|abort\|panic\|error` | fails on purpose at startup, to check crash reporting end to end |

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
                     ErrorBanner, ProgressRing, NumberFlow, VideoPlayer, tooltips
  screens/           gallery, record (picker ⇄ recorder), source_picker, recorder, modals,
                     session, settings, add_session, permission, editor (TimelapseEditor),
                     program_panel, layout
  windows/           main_window (router, transitions, chrome, overlays), editor, tray_window
  platform/          tray (SNI + GNOME pill / tray-icon / NSStatusItem), camera (GStreamer),
                     permissions (macOS), app_menu (macOS menu bar), desktop_appearance (Linux),
                     single_instance
  player/            video playback backends: AVPlayer (macOS), IMFMediaEngine (Windows),
                     GStreamer playbin (Linux); frames drawn as GPU surfaces (see vendor/PATCHES.md)
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
  windows/           lookout.nsi: the old NSIS installer, unused since lookout.exe installs itself (src/installer/); kept until that's proven on real Windows
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
`TAURI_SIGNING_PRIVATE_KEY`). It also writes `latest.json`, which the old
Tauri app's updater reads, pointing at the native macOS and Windows builds.

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

See [upgrade-verification.md](docs/upgrade-verification.md) for tested migration
behavior and the remaining blockers before an automatic Tauri → native rollout.

The scripts in `packaging/` are what CI runs; each writes to `target/bundle/`
by default (`--out` to change, `--no-build` to reuse `target/release`).
Release builds stamp the tag's version first (`packaging/version.sh --set`).

| platform | script | artifacts |
|---|---|---|
| macOS (Xcode) | `packaging/macos/bundle.sh --target aarch64-apple-darwin` | `Lookout-native_<v>_darwin-<arch>.app.tar.gz` (updater) + `.dmg` |
| Windows | `cargo build --release`, sign `target/release/lookout.exe`, copy it to the artifact name (the exe is its own installer) | `Lookout-native_<v>_windows-x86_64-setup.exe` |
| Linux packages (x86_64 + arm64) | `packaging/linux/packages.sh` | `Lookout_linux-<v>_<arch>.deb`, `.rpm`, `.pkg.tar.zst` |
| Linux AppImage | `packaging/linux/appimage.sh` | `Lookout-native_<v>_linux-x86_64.AppImage` |

- **macOS**: `Lookout.app` reuses the Tauri app's bundle identifier
  (`com.hackclub.lookout`), so with the same Developer ID team the Screen
  Recording/Camera grants and the data directories carry over. Signing and
  notarizing use the Tauri job's `APPLE_*` secrets; without them the bundle is
  ad-hoc signed. Nothing besides the executable is bundled (AVFoundation,
  VideoToolbox, OS Swift runtime), and the script fails if that changes.
- **macOS System Settings pane**: `packaging/macos/prefpane.sh` builds
  `swift/lookout-prefpane` into a universal `Lookout.prefPane`, which
  `bundle.sh` signs into `Contents/Library/PreferencePanes`. At launch the app
  copies it to `~/Library/PreferencePanes` (`src/platform/pref_pane.rs`), so
  Lookout shows up in System Settings with nothing for the user to install.
  A newer app replaces an older copy, and a pane the user removed stays
  removed. The pane is Lookout's own Settings screen (menu, Filtered Apps,
  Linked Programs, Advanced) redrawn in AppKit with the app's tokens, Button
  squircle and press spring, row hover/press and route slides
  (`Kit.swift`, `Controls.swift`, `Pages.swift`), icons converted from
  `assets/icons` by `svg2pdf.py`. Its menu opens with an About-style header
  (icon, version, Website/Docs/GitHub/Privacy & Terms links, Open Lookout)
  instead of an About row; sub-pages use the header bar's round back button.
  It asks for a fixed 500pt-wide view, so System Settings
  doesn't widen its window for it (the classic 668pt pane did), and the
  Filtered Apps list only builds the rows on screen. It edits settings for real by running
  `lookout --settings-pane '<json>'` (`src/settings_pane.rs`): with Lookout
  open that relays over the per-user single-instance socket and the app
  applies it (store, engine filter list, server reload, unlink revoke);
  with Lookout closed it edits the files itself. `lookout://settings[/…]`,
  `lookout://settings/about` and `lookout://onboarding` links only navigate,
  since any web page can open them. Native CI builds the pane, loads it as
  System Settings would and saves every page, light and dark, as
  `tour/prefpane-*.png`; `LOOKOUT_PANE_PAGE=filtered-apps|linked-programs|
  advanced` opens a page directly.
- **Windows**: one signed `lookout.exe` that is also its installer and
  uninstaller (`src/installer/`). Run from anywhere but the install folder, it
  opens the installer window; it installs per user to `%LOCALAPPDATA%\Lookout`
  with the Tauri/NSIS installers' registry keys, so it upgrades those installs
  in place. Registers `lookout://`, adds a Start menu shortcut, and takes the
  NSIS flags `/S`, `/UPDATE`, `/R`, `/D=` (what the updater runs) plus
  `--uninstall [/S]`. Debug builds on any OS: `LOOKOUT_INSTALLER=demo` (or
  `demo-uninstall`) shows the window with simulated steps;
  `LOOKOUT_INSTALLER_DEMO_FAIL=1` shows the "Installation Aborted" page and
  `LOOKOUT_INSTALLER_DEMO_ADMIN=1` the UAC shield. A folder the user can't
  write to (an older install under Program Files, or one picked with Browse)
  gets the shield on Install; the work then runs in an elevated copy of the
  exe that reports progress back, and Lookout itself is started unelevated.
- **Linux AppImage** (build on Ubuntu 24.04, so glibc ≥ 2.39): bundles GTK 3,
  WebKitGTK 4.1 and its helper processes, libsoup 3, glib-networking, and
  GStreamer with the plugins the app uses (x264, libav, VP9, mp4/webm, v4l2,
  souphttpsrc, pipewiresrc, …). It needs from the host: glibc, GL/EGL/Vulkan
  and the GPU drivers, libwayland, libasound, libva, and libpipewire-0.3
  (which loads the host's own PipeWire modules). The AppRun hook `cd`s into
  the AppImage, because WebKitGTK's helper path is patched to a relative one.
  The AppImage doesn't register `lookout://` itself (use an AppImage
  integrator, or `packaging/linux/install-local.sh` for a plain build).
- **Linux packages**: `packages.sh` builds on Ubuntu 24.04 for x86_64 and
  arm64, using host libraries and runtime GStreamer dependencies. It needs
  `dpkg-dev`, `rpm`, and `zstd` in addition to the native build dependencies.
  The package names remain `lookout` (apt/dnf/zypper) and `lookout-bin`
  (pacman), so existing Tauri installs upgrade in place through the same
  repositories and `install.sh`. The repository key and apt/rpm enrollment
  hooks are shared with the desktop packaging. The desktop entry retains
  `Lookout.desktop` and registers `lookout://`; `lookout-desktop` remains an
  alias for deb/rpm installs. User data is left in place: the native app shares
  `secrets.json` and imports Tauri settings on first launch.
  Tagged releases require both architectures before publishing packages.
  Package update notices use the common release version in `latest.json`;
  only AppImages use the native self-updater.

## Accessibility

Screen readers (VoiceOver, Narrator, Orca) see the app through GPUI's
AccessKit integration: macOS AX, Windows UIA, and AT-SPI on Linux, all fed
from one tree. GPUI's guide is `vendor/gpui-pre/src/_accessibility.rs`; our
helpers live in [`src/ui/a11y.rs`](crates/lookout-native/src/ui/a11y.rs)
(`use crate::ui::a11y::prelude::*;`).

**Conventions for new UI**

- An element reaches assistive technology only with an `.id(..)` **and** a
  `.role(..)`. String children are invisible to it: name the element with
  `.aria_label(..)`, or for plain text use `div().a11y_text(id, text)`
  (a `Label`, which AccessKit reads from its *value*), `a11y_heading(id, text,
  level)`, or `a11y_status(id, text, Live::Polite)` for a line that should be
  announced when it changes.
- The shared controls do this for you: `Button` (name from its text, else
  `.aria_label`, else the tooltip; disabled/loading are reported),
  `HeaderBtn`, `PressScale` (`.role/.aria_label/.aria_position`),
  `TextInput` (`a11y_label`, value, placeholder, caret and selection), menus,
  sheets and modals. Icon-only buttons need an `.aria_label` or a tooltip.
- Ids only need to be unique among siblings under the nearest ancestor with
  an id, but **duplicates panic in debug builds once a screen reader is on**
  (release builds silently drop the node). Keyed lists use per-item ids.
- Toggles are `Role::CheckBox` with `.aria_toggled(..)`; selection is
  `.aria_selected(..)`; disclosure is `.aria_expanded(..)`. Disabled, busy,
  modal, live and hidden go through `.a11y_flags(NodeFlags { .. })` (one
  synthetic-children hook per element).
- Give clickable surfaces `.on_a11y_click(..)`: otherwise GPUI turns the
  screen reader's "press" into a fake mouse click at the node's centre, which
  hits the wrong thing when the element is half scrolled away.
- Never put a ticking value in a name or a live region. The recorder clock is
  a `Timer` whose value moves once a minute; state changes (Recording, Paused,
  Stopping) are a separate polite status.
- A modal view names its dialog with `overlay::set_dialog_title(..)` during
  render; `Sheet::a11y_label` names a sheet.
- AccessKit's adapters don't map `ShowContextMenu` or custom actions, so a
  context-menu-only action needs another way in: `PressScale::a11y_action`
  adds it as a synthetic child button (the gallery card's "Open in Editor").
- Labels we wrote that aren't on screen: "Pause recording", "Resume
  recording", "Stop recording", "Recorded time", "Start a timelapse",
  "Playhead", "Cuts", "Masks", "Seek", "Copy command", "Session link",
  "Timelapse name", gallery cards as "Name, Status, 1 hour 12 minutes", editor
  cuts as "Cut, minutes 3 to 5" and the playhead as "Minute 6 of 47, cut".

**Checking the tree on Linux** (the same tree reaches macOS and Windows).
Run the app on the virtual desktop and read it over AT-SPI with
[agent-desktop](https://github.com/lahfir/agent-desktop):

```sh
desk agent-desktop snapshot --app lookout --compact       # whole tree; add -i for controls only
desk agent-desktop find --app lookout --role button --name Settings
desk agent-desktop click <ref>                            # AT-SPI "click" action
desk agent-desktop set-value <ref> 4                      # sliders (editor playhead)
```

A panic mentioning "Duplicate a11y node id" is a duplicate id. GPUI also
logs, once per focus change, when the focused element has no node.

**Known gaps.** AccessKit 0.19's AT-SPI adapter has no EditableText and only
maps "disabled" for read-only-capable roles, so on Linux text fields can't be
written through AT-SPI (typing works) and disabled buttons still read as
enabled; both work on macOS and Windows. The gallery builds only the cards near
the viewport, so a screen reader sees those (with "n of N" positions), not the
whole list. The program panel's embedded web page is drawn into a texture and
its own tree isn't bridged. Only the default "click" action is exposed over
AT-SPI, so slider Increment/Decrement is macOS/Windows only (SetValue works
everywhere). VoiceOver's focus fix is a vendored GPUI patch
(`vendor/PATCHES.md`, zed#66691) that has only been type-checked here.

## How the animations were ported

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

## Where it departs from the Tauri app

- **macOS integration.** Settings… (⌘,) in the app menu; a System Settings
  pane (see Packaging); a Dock menu with the open timelapse's Pause/Resume and
  Stop, Start Timelapse and the five most recent timelapses
  (`platform/dock_menu.rs`); notification buttons (Resume on an unplanned
  pause, Open on every recorder notice; clicking the notification opens that
  timelapse, `platform/notifications.rs`); trackpad haptics in the editor when
  a dragged cut, mask span or mask box edge meets the timeline's end, another
  cut or the frame's edge (`platform/haptics.rs`); and the system accent colour
  when it isn't blue (Multicolor and Blue keep Lookout's blue; re-read when
  Lookout comes to the front).
- **Windows taskbar.** While a timelapse is open the taskbar button carries a
  badge (red dot recording, amber pause badge paused, each with a screen-reader
  description) and the thumbnail preview gets Pause/Resume and Stop buttons
  that act like the tray's; the button shows indeterminate progress while a
  timelapse page is compiling (`platform/taskbar.rs`). Glyphs are drawn at the
  taskbar's DPI in its light/dark colour, and the buttons are re-added if
  Explorer restarts.

- **One header bar on every platform.** The Tauri app drew the 47px
  Adwaita-style header bar only on Linux; macOS and Windows got a 72px
  "Your Timelapses" row and boxed "‹ Back" buttons in the page. Native uses the
  Linux bar everywhere (title, round Back, page actions, backdrop dim), with
  each OS's window controls: the traffic lights on macOS, Windows 11 caption
  buttons (hit-tested by the OS, so Snap and accessibility stay native), and
  GNOME's close on Linux. Under a window manager's own frame (X11 without a
  compositor) Linux keeps the bar without its close button.
- Thumbnails that fail on a network error are retried twice with backoff
  instead of staying blank until restart.
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
- On Linux, in-app confirm dialogs replace the GTK `confirm()` popups, styled
  like the rest of the app. macOS and Windows keep the system's own alert
  (NSAlert / task dialog), as the Tauri app's dialog plugin did, with buttons
  named for the action ("Archive", "Publish") instead of OK.
- The Windows tray popup follows the system theme: a light flyout panel in
  light mode, Tauri's dark `rgba(26,26,26,.93)` in dark. Tauri kept the dark
  panel in light mode too, so its theme-coloured text was dark on dark (#209).
  `LOOKOUT_TRAY_POPUP=1` opens the popup at launch to preview it on Linux.
- **Change source…** in the recorder's ⋯ header menu switches what's being
  recorded mid-session (#51). The recording stays open: it keeps capturing
  the old source while the picker is up ("Still recording until you
  switch"), Back or Escape returns to it untouched, and picking calls the
  engine's `recorder_switch_source`, which flushes the old source's partial
  minute and starts the new one without a pause/resume round trip (a paused
  recording resumes on the pick). A lost source uses the same flow. Tauri
  only offered a new source after a loss, and closed and reopened the
  recording to do it, which dropped any undelivered uploads.

## Known gaps and next steps

The macOS and Windows code type-checks from Linux (`aarch64-apple-darwin`,
`x86_64-pc-windows-msvc`) but **has not run on either platform yet**. Things
to check on real hardware first:

- **Video playback** (`player/`) uses `AVPlayer` + `AVPlayerItemVideoOutput`
  painted zero-copy as a Metal surface (macOS) and `IMFMediaEngine` with a
  D3D11 device, read back to a reused BGRA texture (Windows). Only Linux has
  run. Check smooth playback, scrub-seek chasing, streaming from R2, looping,
  and the patched Metal surface shader, which only compiles on a Mac (CI is
  its first compile; see `vendor/PATCHES.md`).
- **Filmstrips and scrub previews** use AVFoundation (`AVAssetReader`) and Media
  Foundation (`IMFSourceReader`) through `platform/decoder` (Linux: GStreamer).
  Check seek accuracy, rotation metadata (ignored on macOS) and the cost of
  decoding full frames for small tiles (Windows).
- **Camera capture** uses AVCaptureSession / Media Foundation. The macOS bundle
  needs `NSCameraUsageDescription`; check Windows frames come out upright.
- **Auto-update** install/rollback on macOS (bundle renames, `ditto`, the
  admin prompt for a non-writable `/Applications`) and Windows (NSIS `/S`,
  copy of the install dir, robocopy rollback script) type-checks but has only
  run on Linux, with a fake AppImage. The release `native-build` job now
  produces the artifacts, but none of them has been through CI yet.
- **Program Panel** embeds the page with wry (WebKitGTK / WKWebView / WebView2).
  Native Wayland composites software-rendered WebKitGTK snapshots into GPUI,
  with pointer, keyboard, clipboard and IME forwarding; it does not require XWayland.
  The Wayland renderer runs at up to 30 fps and disables WebKit GPU acceleration.
  The native Wayland integration test needs a running compositor and no X11 display:
  `env -u DISPLAY cargo test -p lookout-native native_wayland_webview -- --ignored --test-threads=1`.
  Embed errors stay in the app without a browser fallback.
  The Linux binary now needs WebKitGTK 4.1; `--no-default-features` drops it.
- **macOS menu-bar item** reuses the Tauri app's Swift package
  (`swift/lookout-tray`) through `swift-rs` for the click-to-open popup and
  digit roll; the Swift build itself is skipped by the cross-check.
- **macOS 26 display link**: `platform/macos_occlusion.rs` works around GPUI
  only starting a window's frame callback on the documented "visible" bit,
  which macOS 26 stops setting (Cap hit the same bug). Check on macOS 26.
- **Animations** were audited one by one against the Tauri source; the table
  and the few deliberate deviations are in `docs/animation-audit.md`.
