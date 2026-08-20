# Lookout recording indicator (GNOME Shell extension)

The pill GNOME shows while its own screen recorder runs is drawn by the shell,
from a panel button carrying the `screen-recording-indicator` style class.
There is no D-Bus API for an app to add a panel item, so the only way to get
one is to run code inside the shell — which is all this extension is for. It
holds no state: everything comes from the app over the session bus, and
nothing is shown when Lookout isn't recording.

`../src/gnome_indicator.rs` embeds these files — `extension.js`,
`metadata.json`, `stylesheet.css` and `icons/lookout.png`, the last one with
`include_bytes!` — and writes them to
`~/.local/share/gnome-shell/extensions/<uuid>/` at every startup on GNOME, so
the install needs no root and works the same from the deb, the rpm and the
AppImage. It is not a setting to opt into: on GNOME the pill is how a
recording is indicated. The
extension loads the icon by path from its own directory, so anything added
here has to be written out there too.

Recording shows the app icon on the shell's own red; paused shows a pause
glyph on amber (`#f59e0b`, dark text — white on amber is about 2:1). The pill
sizes to its content, so `h:mm:ss` is wider than `mm:ss`; horizontal padding
is trimmed to the theme's own `-minimum-hpadding` because an icon and a time
already read as one unit.

## The contract

Bus name `com.hackclub.Lookout`, object `/com/hackclub/Lookout/Indicator`,
interface `com.hackclub.Lookout.Indicator`. The app owns the name; the
extension watches for it, so either side can start, stop or restart first.

| Member | Direction | Purpose |
| --- | --- | --- |
| `GetState() → (b active, s time, b paused)` | app ← ext | initial state, on connect |
| `StateChanged(b active, s time, b paused)` | app → ext | pushed every second while recording |
| `Attach()` / `Detach()` | app ← ext | the pill is up / going away, so the app can retire and restore its tray icon |
| `Pause()` / `Resume()` / `Stop()` / `Open()` | app ← ext | the pill's menu items |

The app pushes the time rather than the extension ticking its own clock, so
the pill can't drift against the window and the menu bar.

## Working on it

GNOME scans the extension directories when a session starts and offers no way
to ask for a rescan (`ReloadExtension` is declared on
`org.gnome.Shell.Extensions` but not implemented, and `EnableExtension`
rejects a UUID the shell has never seen). So a freshly installed extension
only comes up at the next login. Two consequences worth keeping in mind:

- `enabled-extensions` is written directly, since the shell won't enable a
  UUID it has not scanned. That happens once, guarded by a
  `.lookout-enabled-once` marker in the extension directory: turning the pill
  off in GNOME's own settings is an answer, and an app update — which
  reinstalls, because the shipped version no longer matches — must not
  quietly undo it.
- For that first session the StatusNotifierItem is still the indicator, which
  is why the tray path in `../src/tray.rs` is maintained rather than dropped
  on GNOME.

For iterating without logging out or rebuilding the app, load it in a
throwaway shell and drive it with the mock service:

```bash
dbus-run-session -- bash -c '
  gnome-shell --headless --virtual-monitor 1280x720 --wayland-display test &
  sleep 10
  gnome-extensions enable lookout-indicator@hackclub.com
  python3 mock-service.py
'
```

`gnome-extensions info <uuid>` reports the load state, `GetExtensionErrors`
reports exceptions, and `console.log` from the extension lands in the nested
shell's output.
