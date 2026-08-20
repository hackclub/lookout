//! GNOME Shell panel pill.
//!
//! GNOME has no API for putting an item in its top bar, and the pill it shows
//! for its own screen recorder (the `screen-recording-indicator` style class:
//! red fill, 999px radius, bold label) belongs to the shell. The only way to
//! draw one is from inside the shell, so the app ships a small extension
//! (`../gnome-extension`) and exports the recording state for it here.
//!
//! Why a service and not a one-shot push: the extension can be enabled, the
//! shell can restart, and the app can be reopened, all in any order. The
//! extension watches for our bus name and asks for the state whenever it
//! appears, and the 1s tray ticker re-publishes anyway, so both sides recover
//! without either having to know what the other did.
//!
//! It also decides which indicator is showing. When the extension attaches we
//! drop the StatusNotifierItem, since a tray icon beside the pill would be a
//! second indicator for one recording; on detach the tray comes back if a
//! recording is still running.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use gtk::gio;
use gtk::gio::prelude::*;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use zbus::object_server::SignalEmitter;

pub const UUID: &str = "lookout-indicator@hackclub.com";

const BUS_NAME: &str = "com.hackclub.Lookout";
const OBJECT_PATH: &str = "/com/hackclub/Lookout/Indicator";

const METADATA_JSON: &str = include_str!("../gnome-extension/metadata.json");
const EXTENSION_JS: &str = include_str!("../gnome-extension/extension.js");
const STYLESHEET_CSS: &str = include_str!("../gnome-extension/stylesheet.css");
/// The app icon the pill shows while recording. The extension loads it by
/// path from its own directory, so it has to land there too.
const APP_ICON_PNG: &[u8] = include_bytes!("../gnome-extension/icons/lookout.png");

/// Set while the extension is connected, so the tray can stand down.
static PILL_ATTACHED: AtomicBool = AtomicBool::new(false);
/// Whether a recording is being indicated. Latched by `publish` so a ticker
/// that hasn't been cancelled yet can't resurrect the pill after `hide_tray`
/// — the two are separate IPC calls, and a tick landing between them would
/// otherwise leave a pill on screen that nothing updates again.
static INDICATING: AtomicBool = AtomicBool::new(false);
static PUBLISH: OnceLock<UnboundedSender<PillState>> = OnceLock::new();

#[derive(Clone, Default, PartialEq, Eq)]
struct PillState {
    active: bool,
    time: String,
    paused: bool,
}

/// Whether the extension is drawing the pill right now.
pub fn pill_attached() -> bool {
    PILL_ATTACHED.load(Ordering::Relaxed)
}

/// Hand the extension a new state, and latch whether there is one to show.
/// Called from the tray commands, which are authoritative about that.
pub fn publish(active: bool, time: &str, paused: bool) {
    INDICATING.store(active, Ordering::Relaxed);
    send(active, time, paused);
}

/// A tick from the tray timer: the time moved, nothing else. Ignored once the
/// recording is over, whatever the timer thinks.
pub fn publish_tick(time: &str, paused: bool) {
    if INDICATING.load(Ordering::Relaxed) {
        send(true, time, paused);
    }
}

/// Cheap enough for the 1s ticker: drops a value on a channel, and no-ops
/// entirely when the service never started (not GNOME, no session bus, or the
/// name was already taken).
fn send(active: bool, time: &str, paused: bool) {
    if let Some(tx) = PUBLISH.get() {
        let _ = tx.send(PillState {
            active,
            time: time.to_string(),
            paused,
        });
    }
}

/// Export the indicator service. Called once at startup; harmless off GNOME,
/// where nothing will ever connect to it.
pub fn start(app: AppHandle) {
    let (tx, rx) = unbounded_channel();
    if PUBLISH.set(tx).is_err() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        if let Err(err) = serve(app, rx).await {
            // Not fatal: the tray path stays as it was, the pill just never
            // appears.
            eprintln!("[gnome-indicator] service unavailable: {err}");
        }
    });
}

struct Indicator {
    app: AppHandle,
    state: PillState,
}

#[zbus::interface(name = "com.hackclub.Lookout.Indicator")]
impl Indicator {
    /// Current state, for an extension that just started or just woke up.
    fn get_state(&self) -> (bool, String, bool) {
        (
            self.state.active,
            self.state.time.clone(),
            self.state.paused,
        )
    }

    /// The extension is drawing the pill — retire the tray item.
    fn attach(&self) {
        PILL_ATTACHED.store(true, Ordering::Relaxed);
        self.app.remove_tray_by_id("timelapse_tray");
    }

    /// The extension is going away (disabled, or the shell is shutting it
    /// down). Put the tray back if there's still something to indicate.
    fn detach(&self) {
        PILL_ATTACHED.store(false, Ordering::Relaxed);
        if self.state.active {
            let _ = crate::tray::show_tray(self.state.time.clone(), self.app.clone());
        }
    }

    fn pause(&self) {
        let _ = crate::tray::tray_action("pause".into(), self.app.clone());
    }

    fn resume(&self) {
        let _ = crate::tray::tray_action("resume".into(), self.app.clone());
    }

    fn stop(&self) {
        let _ = crate::tray::tray_action("stop".into(), self.app.clone());
    }

    fn open(&self) {
        if let Some(window) = self.app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    #[zbus(signal)]
    async fn state_changed(
        emitter: &SignalEmitter<'_>,
        active: bool,
        time: &str,
        paused: bool,
    ) -> zbus::Result<()>;
}

async fn serve(app: AppHandle, mut rx: UnboundedReceiver<PillState>) -> zbus::Result<()> {
    let connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            Indicator {
                app,
                state: PillState::default(),
            },
        )?
        .build()
        .await?;

    let iface = connection
        .object_server()
        .interface::<_, Indicator>(OBJECT_PATH)
        .await?;

    while let Some(next) = rx.recv().await {
        {
            let mut current = iface.get_mut().await;
            // The ticker publishes every second but the text only changes at
            // minute granularity, so most of these are the same state twice.
            if current.state == next {
                continue;
            }
            current.state = next.clone();
        }

        let _ = Indicator::state_changed(
            iface.signal_emitter(),
            next.active,
            &next.time,
            next.paused,
        )
        .await;
    }

    Ok(())
}

const SHELL_SCHEMA: &str = "org.gnome.shell";
const ENABLED_KEY: &str = "enabled-extensions";
/// Written next to the extension the first time we switch it on. Its absence
/// is what distinguishes "never enabled" from "the user turned it off".
const ENABLED_MARKER: &str = ".lookout-enabled-once";

fn extension_dir() -> Result<PathBuf, String> {
    let data_home = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").ok_or("no HOME in the environment")?)
            .join(".local/share"),
    };
    Ok(data_home.join("gnome-shell/extensions").join(UUID))
}

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|desktops| {
            desktops
                .split(':')
                .any(|desktop| desktop.eq_ignore_ascii_case("GNOME"))
        })
        .unwrap_or(false)
}

/// `Settings::new` aborts the process on a schema that isn't installed, which
/// is every non-GNOME desktop — so never construct one without this.
fn shell_settings() -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(SHELL_SCHEMA, true)?;
    Some(gio::Settings::new(SHELL_SCHEMA))
}

fn is_current(dir: &std::path::Path) -> bool {
    std::fs::read_to_string(dir.join("metadata.json"))
        // Compared against what we ship, so an install from an older build
        // counts as absent and gets rewritten.
        .is_ok_and(|installed| installed == METADATA_JSON)
}

/// Put the extension in place, and switch it on the first time.
///
/// Called at every startup on GNOME: the pill is how the app indicates a
/// recording there, not something to go looking for in a settings pane. It is
/// a no-op once the shipped version is already installed, so the cost is one
/// `read_to_string` per launch, and an app update reinstalls itself.
///
/// It will not be *drawn* until the next login. GNOME scans the extension
/// directories when a session starts and offers no way to ask for a rescan
/// (`org.gnome.Shell.Extensions.ReloadExtension` is declared but returns "not
/// implemented", and `EnableExtension` rejects a UUID the shell has never
/// seen; `man gnome-extensions` says an install is "loaded in the next
/// session"). Until then the StatusNotifierItem is the indicator, which is
/// why that path is still maintained.
pub fn ensure_installed() {
    if !is_gnome() {
        return;
    }

    match install() {
        Ok(Installed::AlreadyCurrent) => {}
        Ok(Installed::Written) => {
            eprintln!("[gnome-indicator] extension installed; the pill appears at next login")
        }
        Err(err) => eprintln!("[gnome-indicator] could not install the extension: {err}"),
    }
}

enum Installed {
    AlreadyCurrent,
    Written,
}

fn install() -> Result<Installed, String> {
    let dir = extension_dir()?;
    if is_current(&dir) {
        // Still take the enable path: it is marker-guarded, and an install
        // predating the marker would otherwise never get one — leaving an
        // app update free to re-enable a pill the user had turned off.
        enable_once(&dir)?;
        return Ok(Installed::AlreadyCurrent);
    }

    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, contents) in [
        ("metadata.json", METADATA_JSON),
        ("extension.js", EXTENSION_JS),
        ("stylesheet.css", STYLESHEET_CSS),
    ] {
        std::fs::write(dir.join(name), contents).map_err(|e| format!("{name}: {e}"))?;
    }

    let icons = dir.join("icons");
    std::fs::create_dir_all(&icons).map_err(|e| format!("{}: {e}", icons.display()))?;
    std::fs::write(icons.join("lookout.png"), APP_ICON_PNG)
        .map_err(|e| format!("lookout.png: {e}"))?;

    enable_once(&dir)?;
    Ok(Installed::Written)
}

/// Add the UUID to `enabled-extensions`, but only the first time.
///
/// Written to the key directly because the shell won't enable a UUID it has
/// not scanned; at next login it finds the extension already switched on.
///
/// Only once, though: someone who turns the pill off in GNOME's own settings
/// has said what they want, and an app update — which reinstalls, since the
/// shipped version no longer matches — must not quietly switch it back on.
fn enable_once(dir: &std::path::Path) -> Result<(), String> {
    let marker = dir.join(ENABLED_MARKER);
    if marker.exists() {
        return Ok(());
    }

    let settings = shell_settings().ok_or("GNOME Shell's settings schema isn't installed")?;
    let enabled = settings.strv(ENABLED_KEY);
    let mut uuids: Vec<&str> = enabled.iter().map(|uuid| uuid.as_str()).collect();
    if !uuids.contains(&UUID) {
        uuids.push(UUID);
        settings
            .set_strv(ENABLED_KEY, uuids)
            .map_err(|e| format!("couldn't enable the extension: {e}"))?;
    }

    // Best effort: a marker we failed to write only costs one redundant
    // enable, which is better than failing an install that otherwise worked.
    let _ = std::fs::write(marker, "");
    Ok(())
}
