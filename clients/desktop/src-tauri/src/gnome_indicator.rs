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
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use zbus::object_server::SignalEmitter;

pub const UUID: &str = "lookout-indicator@hackclub.com";

const BUS_NAME: &str = "com.hackclub.Lookout";
const OBJECT_PATH: &str = "/com/hackclub/Lookout/Indicator";

const METADATA_JSON: &str = include_str!("../gnome-extension/metadata.json");
const EXTENSION_JS: &str = include_str!("../gnome-extension/extension.js");
const STYLESHEET_CSS: &str = include_str!("../gnome-extension/stylesheet.css");

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
        let _ = self.app.emit("gnome-indicator-attached", true);
    }

    /// The extension is going away (disabled, or the shell is shutting it
    /// down). Put the tray back if there's still something to indicate.
    fn detach(&self) {
        PILL_ATTACHED.store(false, Ordering::Relaxed);
        let _ = self.app.emit("gnome-indicator-attached", false);
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

/// What the settings UI needs to describe the pill's state in one line.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndicatorStatus {
    /// Running under GNOME Shell, so a pill is possible at all.
    pub supported: bool,
    /// The extension is installed, at the version this build ships.
    pub installed: bool,
    /// GNOME has it enabled.
    pub enabled: bool,
    /// It has actually connected to us — the pill really is live.
    pub attached: bool,
}

const SHELL_SCHEMA: &str = "org.gnome.shell";
const ENABLED_KEY: &str = "enabled-extensions";

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

fn is_enabled() -> bool {
    shell_settings().is_some_and(|settings| {
        settings
            .strv(ENABLED_KEY)
            .iter()
            .any(|uuid| uuid.as_str() == UUID)
    })
}

fn is_installed() -> bool {
    extension_dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(dir.join("metadata.json")).ok())
        // Compared against what we ship, so an install from an older build
        // counts as absent and `install` rewrites it.
        .is_some_and(|installed| installed == METADATA_JSON)
}

pub fn status() -> IndicatorStatus {
    IndicatorStatus {
        supported: is_gnome(),
        installed: is_installed(),
        enabled: is_enabled(),
        attached: pill_attached(),
    }
}

/// Write the extension into the user's extension directory and mark it
/// enabled. No root and no package manager, so this works the same from the
/// deb, the rpm and the AppImage.
///
/// It does not come up in this session. GNOME scans the extension directories
/// at startup and exposes no way to ask for a rescan —
/// `org.gnome.Shell.Extensions.ReloadExtension` is declared but not
/// implemented, and `EnableExtension` rejects a UUID the shell has never seen
/// ("does not exist"). `gnome-extensions install` documents the same
/// behaviour: an extension is "loaded in the next session". So the enable is
/// written straight to the `enabled-extensions` key instead, where the shell
/// will find it already switched on at next login — which is why the caller
/// is expected to tell the user to log back in.
pub fn install() -> Result<IndicatorStatus, String> {
    if !is_gnome() {
        return Err("the top-bar pill needs GNOME Shell".into());
    }

    let dir = extension_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    for (name, contents) in [
        ("metadata.json", METADATA_JSON),
        ("extension.js", EXTENSION_JS),
        ("stylesheet.css", STYLESHEET_CSS),
    ] {
        std::fs::write(dir.join(name), contents).map_err(|e| format!("{name}: {e}"))?;
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

    Ok(status())
}
