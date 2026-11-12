//! What the machine can tell us when a capture fails.
//!
//! A screen capture on Linux runs through three separate pieces of system
//! software that a distro may or may not have installed: the XDG desktop
//! portal, a portal *backend* that actually implements ScreenCast (the
//! `-gnome`, `-kde`, `-wlr` packages), and PipeWire with its GStreamer
//! element. Any one of them missing produces a D-Bus or GStreamer string
//! that means nothing to the person reading it.
//!
//! This module answers "which of those three is missing", so the error the
//! user sees can name the package instead of the interface. Every probe is
//! allowed to fail: `None` means "couldn't tell", which the frontend treats
//! as "don't claim anything about it".

use serde::Serialize;

#[derive(Serialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CaptureEnvironment {
    /// `linux`, `macos`, `windows`.
    pub os: String,
    /// `XDG_SESSION_TYPE` — `wayland`, `x11`. Falls back to inferring from
    /// `WAYLAND_DISPLAY`, which is what the capture path itself keys off.
    pub session_type: Option<String>,
    /// `XDG_CURRENT_DESKTOP`, e.g. `GNOME`, `KDE`, `sway`.
    pub desktop: Option<String>,
    /// os-release `ID` / `ID_LIKE` / `PRETTY_NAME`. `ID` picks the package
    /// manager for the install command we suggest.
    pub distro_id: Option<String>,
    pub distro_id_like: Option<String>,
    pub distro_name: Option<String>,
    /// `org.freedesktop.portal.Desktop` is running or D-Bus-activatable.
    pub portal_installed: Option<bool>,
    /// The ScreenCast interface answered a property read. This is the probe
    /// that separates "no portal at all" from "portal with no backend": the
    /// interface only exists once a backend implements it.
    pub screencast_available: Option<bool>,
    /// Backends found on disk, by `.portal` file stem (`gnome`, `kde`, ...).
    pub portal_backends: Vec<String>,
    /// `$XDG_RUNTIME_DIR/pipewire-0` exists — the daemon is up.
    pub pipewire_running: Option<bool>,
    /// GStreamer can find `pipewiresrc`, which is a separate package from
    /// PipeWire itself on most distros.
    pub pipewire_gst_element: Option<bool>,
    /// Which encoder element the per-minute clips will actually be recorded
    /// with, e.g. `x264enc`, `vah264enc`, `vp9enc`, `openh264enc`. `None`
    /// means GStreamer found none of them and the session will fall back to
    /// one JPEG a minute.
    ///
    /// This is a QUALITY probe, not an availability one, and it is the
    /// answer to "why does my Linux timelapse look softer than the same
    /// machine's Windows one": the candidates are not equivalent (see
    /// clips.rs ENCODER_CANDIDATES / VP9_FALLBACK), and which one a machine
    /// gets depends on whether `gstreamer1.0-plugins-ugly` came along with
    /// the package — it is a Recommends, so a `dpkg -i` install or any
    /// Fedora install does not have it.
    pub clip_encoder: Option<String>,
    /// Running inside ChromeOS's Linux container (Crostini). Nothing in
    /// there can capture the Chromebook's screen: the container's
    /// compositor (sommelier) only proxies its own windows to the host, and
    /// no portal backend implements ScreenCast. Worth knowing up front,
    /// because every other diagnosis would send the person to install
    /// packages that can't help.
    pub chromeos: Option<bool>,
}

/// Crostini's fingerprints, any one of which is enough: the milestone file
/// and the cros-containers mount are set up by the container tooling, and
/// sommelier exports its version into every session it starts.
///
/// Takes its probes as arguments so the decision can be tested without a
/// Chromebook.
pub fn looks_like_crostini(
    path_exists: impl Fn(&str) -> bool,
    env_var: impl Fn(&str) -> Option<String>,
) -> bool {
    path_exists("/dev/.cros_milestone")
        || path_exists("/opt/google/cros-containers")
        || env_var("SOMMELIER_VERSION").is_some_and(|v| !v.is_empty())
}

/// Read one key out of os-release.
#[cfg(target_os = "linux")]
fn os_release() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            map.insert(key.trim().to_string(), value.to_string());
        }
    }
    map
}

/// Portal backends installed on disk. A backend advertises itself with a
/// `.portal` file in one of the XDG data dirs; the stem is the backend name.
#[cfg(target_os = "linux")]
fn portal_backends() -> Vec<String> {
    let mut dirs: Vec<std::path::PathBuf> = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| std::path::Path::new(d).join("xdg-desktop-portal/portals"))
        .collect();
    // XDG_DATA_DIRS is frequently unset under a bare session; the spec's own
    // defaults are where the packages actually land.
    dirs.push("/usr/share/xdg-desktop-portal/portals".into());
    dirs.push("/usr/local/share/xdg-desktop-portal/portals".into());

    let mut found: Vec<String> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("portal") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let name = stem.to_string();
                if !found.contains(&name) {
                    found.push(name);
                }
            }
        }
    }
    found.sort();
    found
}

/// Is the portal there, and does anything implement ScreenCast?
///
/// Both probes are wrapped in a short timeout: a portal that is registered
/// on the bus but wedged (no backend running to answer it) would otherwise
/// hold the dialog open for zbus's 25-second method timeout.
#[cfg(target_os = "linux")]
async fn probe_portal() -> (Option<bool>, Option<bool>) {
    use ashpd::zbus;
    use std::time::Duration;

    const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
    const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

    let installed = tokio::time::timeout(PROBE_TIMEOUT, async {
        let connection = zbus::Connection::session().await.ok()?;
        let dbus = zbus::fdo::DBusProxy::new(&connection).await.ok()?;
        let name = zbus::names::BusName::try_from(PORTAL_BUS_NAME).ok()?;
        if dbus.name_has_owner(name).await.ok()? {
            return Some(true);
        }
        // Not running yet is not missing — the portal is started on demand.
        let activatable = dbus.list_activatable_names().await.ok()?;
        Some(activatable.iter().any(|n| n.as_str() == PORTAL_BUS_NAME))
    })
    .await
    .ok()
    .flatten();

    if installed == Some(false) {
        // No portal at all, so there is no backend to ask about either.
        return (installed, Some(false));
    }

    let screencast = tokio::time::timeout(PROBE_TIMEOUT, async {
        use ashpd::desktop::screencast::Screencast;
        let proxy = match Screencast::new().await {
            Ok(proxy) => proxy,
            // The portal answered and said it has no ScreenCast interface:
            // that is a definite "no backend", not "couldn't tell".
            Err(ashpd::Error::PortalNotFound(_)) => return Some(false),
            Err(_) => return None,
        };
        // A property read, not a session — this never prompts the user. The
        // interface itself is absent unless a backend implements ScreenCast.
        Some(!proxy.available_source_types().await.ok()?.is_empty())
    })
    .await
    .ok()
    .flatten();

    (installed, screencast)
}

/// Can GStreamer see `pipewiresrc`? That element ships in its own package
/// (`gstreamer1.0-pipewire`, `pipewire-gstreamer`, `gst-plugin-pipewire`)
/// and its absence is what turns a working portal into a black recording.
#[cfg(target_os = "linux")]
fn pipewire_gst_element() -> Option<bool> {
    gstreamer::init().ok()?;
    Some(gstreamer::ElementFactory::find("pipewiresrc").is_some())
}

/// Which encoder `clips::ClipRecorder` will pick on this machine. Same
/// probe and same order as the recorder's own, so the two can't disagree.
#[cfg(target_os = "linux")]
fn clip_encoder() -> Option<String> {
    gstreamer::init().ok()?;
    crate::clips::available_clip_encoder().map(str::to_string)
}

#[cfg(target_os = "linux")]
async fn collect() -> CaptureEnvironment {
    let release = os_release();
    let (portal_installed, screencast_available) = probe_portal().await;

    let session_type = std::env::var("XDG_SESSION_TYPE").ok().or_else(|| {
        std::env::var("WAYLAND_DISPLAY")
            .ok()
            .map(|_| "wayland".to_string())
    });

    let pipewire_running = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(|dir| std::path::Path::new(&dir).join("pipewire-0").exists());

    CaptureEnvironment {
        os: std::env::consts::OS.to_string(),
        session_type,
        desktop: std::env::var("XDG_CURRENT_DESKTOP").ok(),
        distro_id: release.get("ID").cloned(),
        distro_id_like: release.get("ID_LIKE").cloned(),
        distro_name: release.get("PRETTY_NAME").cloned(),
        portal_installed,
        screencast_available,
        portal_backends: portal_backends(),
        pipewire_running,
        pipewire_gst_element: pipewire_gst_element(),
        clip_encoder: clip_encoder(),
        chromeos: Some(looks_like_crostini(
            |p| std::path::Path::new(p).exists(),
            |k| std::env::var(k).ok(),
        )),
    }
}

#[cfg(not(target_os = "linux"))]
async fn collect() -> CaptureEnvironment {
    CaptureEnvironment {
        os: std::env::consts::OS.to_string(),
        ..Default::default()
    }
}

/// Everything we know about why capture might be failing on this machine.
/// Only called when something has already gone wrong, so the D-Bus round
/// trips cost nothing on the happy path.
pub async fn capture_environment() -> CaptureEnvironment {
    collect().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_the_running_os() {
        let env = collect().await;
        assert_eq!(env.os, std::env::consts::OS);
    }

    /// The encoder the diagnostics report has to be the one the recorder
    /// will actually use — a report that named a different element than the
    /// clips came out of would send people to fix the wrong package.
    #[cfg(target_os = "linux")]
    #[test]
    fn reports_the_encoder_the_recorder_would_pick() {
        gstreamer::init().expect("gst init");
        let reported = clip_encoder();
        assert_eq!(
            reported.as_deref(),
            crate::clips::available_clip_encoder(),
            "diagnostics and ClipRecorder disagree about the clip encoder"
        );
        if let Some(name) = reported {
            assert!(
                gstreamer::ElementFactory::find(&name).is_some(),
                "reported an encoder that isn't installed: {name}"
            );
        }
    }

    #[test]
    fn crostini_is_recognised_by_any_one_fingerprint() {
        let none = |_: &str| None;
        assert!(looks_like_crostini(|p| p == "/dev/.cros_milestone", none));
        assert!(looks_like_crostini(|p| p == "/opt/google/cros-containers", none));
        assert!(looks_like_crostini(|_| false, |k| (k == "SOMMELIER_VERSION").then(|| "0.20".into())));
    }

    #[test]
    fn an_ordinary_desktop_is_not_crostini() {
        assert!(!looks_like_crostini(|_| false, |_| None));
        // An empty variable is not a fingerprint.
        assert!(!looks_like_crostini(|_| false, |k| (k == "SOMMELIER_VERSION").then(String::new)));
        // Other ChromeOS-adjacent paths don't count on their own.
        assert!(!looks_like_crostini(|p| p == "/opt/google/chrome", |_| None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn os_release_strips_quotes() {
        // Not a parser test of the real file — just proof the trimming the
        // frontend depends on (bare `ID`) is applied.
        let map = os_release();
        if let Some(id) = map.get("ID") {
            assert!(!id.starts_with('"'), "ID should be unquoted: {id}");
        }
    }
}
