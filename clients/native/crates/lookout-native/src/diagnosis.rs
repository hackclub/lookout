//! `captureDiagnosis.ts` — turn a raw capture error plus the probed
//! environment into a title, a plain-language summary, numbered steps and a
//! copyable fix. Ported string for string.

use lookout_core::capture_diagnostics::CaptureEnvironment;

#[derive(Clone, Debug)]
pub struct Diagnosis {
    pub title: String,
    pub summary: String,
    pub steps: Vec<String>,
    pub command: Option<String>,
    /// Capture can't work on this machine at all, and the way out is to
    /// record the same session in the browser instead. The dialog turns this
    /// into a button that opens Lookout's web recorder.
    pub record_in_browser: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Apk,
    Nix,
}

fn family_by_distro(id: &str) -> Option<Family> {
    Some(match id {
        "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" | "raspbian" => Family::Apt,
        "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara" => Family::Dnf,
        "arch" | "manjaro" | "endeavouros" | "cachyos" | "garuda" => Family::Pacman,
        "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "sles" => Family::Zypper,
        "alpine" => Family::Apk,
        "nixos" => Family::Nix,
        _ => return None,
    })
}

fn package_family(env: Option<&CaptureEnvironment>) -> Option<Family> {
    let env = env?;
    let mut candidates = vec![env.distro_id.clone().unwrap_or_default()];
    candidates.extend(env.distro_id_like.clone().unwrap_or_default().split_whitespace().map(String::from));
    candidates.iter().find_map(|c| family_by_distro(&c.to_lowercase()))
}

fn install_command(env: Option<&CaptureEnvironment>, pkg: &dyn Fn(Family) -> Option<String>) -> Option<String> {
    let family = package_family(env)?;
    let name = pkg(family)?;
    match family {
        Family::Apt => Some(format!("sudo apt install {name}")),
        Family::Dnf => Some(format!("sudo dnf install {name}")),
        Family::Pacman => Some(format!("sudo pacman -S {name}")),
        Family::Zypper => Some(format!("sudo zypper install {name}")),
        Family::Apk => Some(format!("sudo apk add {name}")),
        Family::Nix => None,
    }
}

fn backend_for(env: Option<&CaptureEnvironment>) -> Option<(&'static str, &'static str)> {
    let desktop = env.and_then(|e| e.desktop.clone()).unwrap_or_default().to_lowercase();
    if desktop.is_empty() {
        return None;
    }
    let any = |words: &[&str]| words.iter().any(|w| desktop.contains(w));
    if any(&["gnome", "cinnamon", "budgie", "unity", "pantheon"]) {
        return Some(("xdg-desktop-portal-gnome", "GNOME"));
    }
    if any(&["kde", "plasma"]) {
        return Some(("xdg-desktop-portal-kde", "KDE Plasma"));
    }
    if any(&["hyprland"]) {
        return Some(("xdg-desktop-portal-hyprland", "Hyprland"));
    }
    if any(&["sway", "wlroots", "river", "wayfire", "labwc"]) {
        return Some(("xdg-desktop-portal-wlr", "wlroots compositors"));
    }
    if any(&["cosmic"]) {
        return Some(("xdg-desktop-portal-cosmic", "COSMIC"));
    }
    None
}

const BACKEND_MENU: &str = "Install the portal backend for your desktop: xdg-desktop-portal-gnome (GNOME, Cinnamon, Budgie), xdg-desktop-portal-kde (KDE Plasma), xdg-desktop-portal-hyprland (Hyprland), or xdg-desktop-portal-wlr (sway and other wlroots compositors).";

fn backend_steps(env: Option<&CaptureEnvironment>) -> (Vec<String>, Option<String>) {
    let backend = backend_for(env);
    if package_family(env) == Some(Family::Nix) {
        let pkg = backend.map(|b| b.0).unwrap_or("xdg-desktop-portal-gnome");
        return (
            vec![
                "Add the portal to your NixOS configuration:".into(),
                format!("xdg.portal = {{ enable = true; extraPortals = [ pkgs.{pkg} ]; }};"),
                "Rebuild with `sudo nixos-rebuild switch`, then log out and back in.".into(),
                "Try casting again.".into(),
            ],
            None,
        );
    }
    let Some((pkg, label)) = backend else {
        return (
            vec![BACKEND_MENU.into(), "Log out and back in so the new backend registers.".into(), "Try casting again.".into()],
            None,
        );
    };
    let command = install_command(env, &|_| Some(pkg.to_string()));
    (
        vec![
            if command.is_some() {
                format!("Install {pkg} — the portal backend for {label}.")
            } else {
                format!("Install {pkg} (the portal backend for {label}) with your package manager.")
            },
            "Log out and back in so the new backend registers with D-Bus.".into(),
            "Try casting again.".into(),
        ],
        command,
    )
}

fn matches_any(raw: &str, needles: &[&str]) -> bool {
    let l = raw.to_lowercase();
    needles.iter().any(|n| l.contains(&n.to_lowercase()))
}

/// No D-Bus session bus to reach the portal over: zbus couldn't even parse
/// `DBUS_SESSION_BUS_ADDRESS` (`disabled:` on Artix/runit and similar
/// setups without a user bus), or there's nothing listening at it.
pub fn no_session_bus(raw: &str) -> bool {
    matches_any(
        raw,
        &[
            "address error",
            "unsupported transport",
            "DBUS_SESSION_BUS_ADDRESS",
            "Unable to autolaunch a dbus-daemon",
            "Failed to connect to the session bus",
        ],
    )
}

/// The capture failure is the machine's setup (nothing Lookout can fix),
/// so it's logged as a warning rather than reported as an error.
pub fn is_environment_issue(raw: &str) -> bool {
    no_session_bus(raw)
}

fn portal_missing(raw: &str) -> bool {
    matches_any(
        raw,
        &[
            "not provided by any .service files",
            "ServiceUnknown",
            "NameHasNoOwner",
            "Failed to connect to screencast portal",
            "org.freedesktop.portal.Desktop",
        ],
    )
}
fn backend_missing(raw: &str) -> bool {
    matches_any(
        raw,
        &[
            "No such interface",
            "UnknownMethod",
            "UnknownObject",
            "NoReply",
            "Failed to select sources",
            "Failed to start screencast",
            "Failed to create screencast session",
            // ashpd's PortalNotFound: the portal is running but nothing
            // implements the interface we asked for.
            "A portal frontend implementing",
        ],
    )
}
fn gst_missing(raw: &str) -> bool {
    let l = raw.to_lowercase();
    l.contains("no element pipewiresrc")
        || l.contains("no element \"pipewiresrc")
        || l.contains("no element 'pipewiresrc")
        || matches_any(raw, &["Failed to create pipeline", "Failed to init gstreamer", "Failed to load plugin"])
}
fn no_frames(raw: &str) -> bool {
    matches_any(raw, &["Failed to pull sample within timeout", "Sample has no buffer"])
}
fn x11_enumeration(raw: &str) -> bool {
    matches_any(raw, &["Failed to list monitors", "Failed to list windows", "XOpenDisplay", "Can't open display"])
}

pub fn diagnose(raw: &str, env: Option<&CaptureEnvironment>) -> Option<Diagnosis> {
    let linux = env.map(|e| e.os.eq_ignore_ascii_case("linux")).unwrap_or(false) || cfg!(target_os = "linux");
    if !linux {
        if cfg!(target_os = "macos") && matches_any(raw, &["not authorized", "screen recording", "TCC", "permission"]) {
            return Some(Diagnosis {
                title: "Screen Recording permission is off".into(),
                summary: "macOS blocks screen capture until Lookout is allowed in System Settings.".into(),
                steps: vec![
                    "Open System Settings → Privacy & Security → Screen & System Audio Recording.".into(),
                    "Turn on Lookout.".into(),
                    "Quit and reopen Lookout — macOS only applies the change on relaunch.".into(),
                ],
                command: None,
                record_in_browser: false,
            });
        }
        return None;
    }
    if env.and_then(|e| e.chromeos) == Some(true) {
        return Some(chromeos());
    }
    // Before the portal checks: the error also says "Failed to connect to
    // screencast portal", but installing the portal wouldn't help.
    if no_session_bus(raw) {
        return Some(Diagnosis {
            title: "No D-Bus session bus".into(),
            summary: "Screen capture on Wayland asks your desktop for permission over the D-Bus session bus, and this session doesn't have one, so there's nobody to ask. Desktops usually start it at login; some setups (runit, s6, a bare window manager started from a TTY) don't.".into(),
            steps: vec![
                "Make sure a D-Bus session bus starts with your desktop. On systemd it comes with dbus; on runit/OpenRC/s6, start your compositor with `dbus-run-session`.".into(),
                "Or start Lookout inside one for now: `dbus-run-session -- lookout`".into(),
                "The xdg-desktop-portal service has to run on the same bus, so it may need starting too.".into(),
            ],
            command: Some("dbus-run-session -- lookout".into()),
            record_in_browser: false,
        });
    }
    let portal_installed = env.and_then(|e| e.portal_installed);
    if portal_installed == Some(false) || (portal_installed != Some(true) && portal_missing(raw) && !backend_missing(raw)) {
        let backend = backend_for(env);
        let pkgs = match backend {
            Some((pkg, _)) => format!("xdg-desktop-portal {pkg}"),
            None => "xdg-desktop-portal".into(),
        };
        let command = install_command(env, &|f| (f != Family::Nix).then(|| pkgs.clone()));
        let steps = if command.is_some() {
            let mut s = vec![match backend {
                Some((pkg, label)) => format!("Install xdg-desktop-portal and {pkg} (the backend for {label})."),
                None => "Install xdg-desktop-portal.".into(),
            }];
            if backend.is_none() {
                s.push(BACKEND_MENU.into());
            }
            s.push("Log out and back in so the portal registers with D-Bus.".into());
            s.push("Try casting again.".into());
            s
        } else {
            let mut s = vec!["Install xdg-desktop-portal with your package manager.".to_string()];
            s.extend(backend_steps(env).0);
            s
        };
        return Some(Diagnosis {
            title: "Screen capture needs the desktop portal".into(),
            summary: "Wayland only lets an app record what you explicitly hand it, through a system component called the XDG desktop portal. It isn't installed here, so there's nothing to ask permission with.".into(),
            steps,
            command,
            record_in_browser: false,
        });
    }
    if env.and_then(|e| e.screencast_available) == Some(false) || backend_missing(raw) {
        let (steps, command) = backend_steps(env);
        let known = backend_for(env);
        let backends = env.map(|e| e.portal_backends.clone()).unwrap_or_default();
        // A desktop we couldn't place (`X-Generic`, a bare window manager)
        // gets the full menu of backends; say why, or the list reads as if
        // we hadn't looked.
        let unplaced = match env.and_then(|e| e.desktop.as_deref()).map(str::trim).filter(|d| !d.is_empty()) {
            Some(desktop) if known.is_none() => {
                format!(" Your desktop calls itself “{desktop}”, which doesn't say which backend it needs.")
            }
            _ => String::new(),
        };
        return Some(Diagnosis {
            title: match known {
                Some((_, label)) => format!("No screen-sharing backend for {label}"),
                None => "No screen-sharing backend installed".into(),
            },
            summary: format!(
                "The desktop portal is running, but nothing on this system implements screen sharing for it. The portal itself is just a front door — the backend that matches your desktop does the actual capturing.{}{unplaced}",
                if backends.is_empty() { String::new() } else { format!(" Installed backends: {}.", backends.join(", ")) }
            ),
            steps,
            command,
            record_in_browser: false,
        });
    }
    if env.and_then(|e| e.pipewire_running) == Some(false) {
        return Some(Diagnosis {
            title: "PipeWire isn't running".into(),
            summary: "Screen capture on Wayland streams through PipeWire, and its socket isn't there — the service is stopped or not installed.".into(),
            steps: vec![
                "Check the service: `systemctl --user status pipewire`".into(),
                "Start it: `systemctl --user enable --now pipewire pipewire-pulse`".into(),
                "If the command isn't found, install the `pipewire` package first.".into(),
            ],
            command: Some("systemctl --user enable --now pipewire pipewire-pulse".into()),
            record_in_browser: false,
        });
    }
    if env.and_then(|e| e.pipewire_gst_element) == Some(false) || gst_missing(raw) {
        let command = install_command(env, &|f| match f {
            Family::Apt => Some("gstreamer1.0-pipewire".into()),
            Family::Dnf => Some("pipewire-gstreamer".into()),
            Family::Pacman => Some("gst-plugin-pipewire".into()),
            Family::Zypper => Some("gstreamer-plugin-pipewire".into()),
            Family::Apk => Some("gst-plugin-pipewire".into()),
            Family::Nix => None,
        });
        return Some(Diagnosis {
            title: "PipeWire's GStreamer plugin is missing".into(),
            summary: "You granted the capture, but the app can't read the video stream: the piece that bridges PipeWire to GStreamer ships as its own package and isn't installed.".into(),
            steps: vec![
                if command.is_some() {
                    "Install the PipeWire GStreamer plugin.".into()
                } else {
                    "Install the PipeWire GStreamer plugin (gstreamer1.0-pipewire, pipewire-gstreamer, or gst-plugin-pipewire, depending on your distro).".into()
                },
                "Restart Lookout.".into(),
                "Try casting again.".into(),
            ],
            command,
            record_in_browser: false,
        });
    }
    if no_frames(raw) {
        return Some(Diagnosis {
            title: "The capture started but no frames arrived".into(),
            summary: "The stream was set up and then stayed empty. This usually means the selected window was closed or minimised, or the compositor dropped the share.".into(),
            steps: vec![
                "Make sure the window or screen you picked is still open and not minimised.".into(),
                "Pick the source again from the Cast tab to start a fresh share.".into(),
                "If it keeps happening, capture a whole screen instead of a single window.".into(),
            ],
            command: None,
            record_in_browser: false,
        });
    }
    if x11_enumeration(raw) && env.and_then(|e| e.session_type.clone()).unwrap_or_default().eq_ignore_ascii_case("wayland") {
        return Some(Diagnosis {
            title: "Screens can't be listed on Wayland".into(),
            summary: "Wayland deliberately hides the list of windows and monitors from apps. Use the Cast tab, which asks your desktop for permission and lets you choose the source there.".into(),
            steps: vec!["Switch to the Cast tab.".into(), "Choose the screen or window you want to record.".into()],
            command: None,
            record_in_browser: false,
        });
    }
    None
}

/// ChromeOS's Linux environment (Crostini). The container's compositor only
/// shows its own windows to ChromeOS, and nothing inside implements screen
/// sharing, so no package or setting fixes this; Chrome on the same machine
/// records the screen fine through `getDisplayMedia`.
fn chromeos() -> Diagnosis {
    Diagnosis {
        title: "Linux apps can't record on ChromeOS".into(),
        summary: "Lookout is running in ChromeOS's Linux environment, a sealed-off container that can't see your Chromebook's screen. No setting or package changes that. Chrome can record the screen, though, and Lookout's web recorder runs right in it.".into(),
        steps: vec![
            "Choose Record in Chrome below. It opens this session in Lookout's web recorder.".into(),
            "Pick This browser, then Entire screen when Chrome asks what to share.".into(),
            "Keep that tab open while you record.".into(),
        ],
        command: None,
        record_in_browser: true,
    }
}

/// The hosted recorder for a session, the same URL the server hands
/// programs as `sessionUrl`.
pub fn web_recorder_url(api_base: &str, token: &str) -> String {
    format!("{}/session?token={token}", api_base.trim_end_matches('/'))
}

pub fn build_report(raw: &str, env: Option<&CaptureEnvironment>, d: Option<&Diagnosis>) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(d) = d {
        lines.push(format!("Diagnosis: {}", d.title));
        if let Some(c) = &d.command {
            lines.push(format!("Suggested fix: {c}"));
        }
        lines.push(String::new());
    }
    let yn = |v: Option<bool>| match v {
        None => "unknown",
        Some(true) => "yes",
        Some(false) => "no",
    };
    if let Some(env) = env {
        lines.push("--- CAPTURE ENVIRONMENT ---".into());
        lines.push(format!("os: {}", env.os));
        lines.push(format!("session: {}", env.session_type.clone().unwrap_or_else(|| "unknown".into())));
        lines.push(format!("desktop: {}", env.desktop.clone().unwrap_or_else(|| "unknown".into())));
        lines.push(format!(
            "distro: {}",
            env.distro_name.clone().or_else(|| env.distro_id.clone()).unwrap_or_else(|| "unknown".into())
        ));
        lines.push(format!("portal installed: {}", yn(env.portal_installed)));
        lines.push(format!("screencast available: {}", yn(env.screencast_available)));
        lines.push(format!(
            "portal backends: {}",
            if env.portal_backends.is_empty() { "none found".into() } else { env.portal_backends.join(", ") }
        ));
        lines.push(format!("chromeos linux: {}", yn(env.chromeos)));
        lines.push(format!("pipewire running: {}", yn(env.pipewire_running)));
        lines.push(format!("pipewiresrc element: {}", yn(env.pipewire_gst_element)));
        lines.push(format!(
            "clip encoder: {}",
            env.clip_encoder.clone().unwrap_or_else(|| "none (recording one JPEG per minute)".into())
        ));
        lines.push(String::new());
    }
    lines.push(crate::logbuf::report(raw));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ashpd says when the portal is up but nothing implements
    /// ScreenCast (issue #212, ChromeOS's Linux; also any desktop with only
    /// the gtk backend).
    const PORTAL_NOT_FOUND: &str = "Failed to connect to screencast portal: A portal frontend implementing `org.freedesktop.portal.ScreenCast` was not found";

    #[test]
    fn disabled_session_bus_is_not_read_as_a_missing_portal() {
        // LOOKOUT-NATIVE-V: Artix with DBUS_SESSION_BUS_ADDRESS=disabled:.
        let raw = "Failed to connect to screencast portal: ZBus Error: address error: unsupported transport 'disabled'";
        assert!(is_environment_issue(raw));
        let d = diagnose(raw, Some(&linux(|_| {}))).expect("diagnosed");
        assert_eq!(d.title, "No D-Bus session bus");
        assert!(!is_environment_issue(PORTAL_NOT_FOUND));
    }

    fn linux(f: impl FnOnce(&mut CaptureEnvironment)) -> CaptureEnvironment {
        let mut env = CaptureEnvironment { os: "linux".into(), ..Default::default() };
        f(&mut env);
        env
    }

    #[test]
    fn portal_not_found_reads_as_a_missing_backend() {
        assert!(backend_missing(PORTAL_NOT_FOUND));
        // The message also carries "Failed to connect to screencast portal",
        // which on its own would read as no portal at all; the backend match
        // has to win (see the next test).
        assert!(portal_missing(PORTAL_NOT_FOUND));
    }

    #[test]
    fn portal_not_found_is_diagnosed_even_before_the_probe_answers() {
        let d = diagnose(PORTAL_NOT_FOUND, None).expect("diagnosis");
        assert_eq!(d.title, "No screen-sharing backend installed");
        assert!(!d.record_in_browser);
    }

    #[test]
    fn chromeos_linux_points_at_the_browser_whatever_the_error() {
        let env = linux(|e| {
            e.chromeos = Some(true);
            e.desktop = Some("X-Generic".into());
            e.portal_installed = Some(true);
            e.screencast_available = Some(false);
            e.portal_backends = vec!["gtk".into()];
            e.distro_id = Some("debian".into());
        });
        for raw in [PORTAL_NOT_FOUND, "no element \"pipewiresrc\"", "something nobody has seen before"] {
            let d = diagnose(raw, Some(&env)).expect("diagnosis");
            assert_eq!(d.title, "Linux apps can't record on ChromeOS", "{raw}");
            assert!(d.record_in_browser);
            // Nothing to install: a command here would send people chasing
            // packages that can't help.
            assert_eq!(d.command, None);
        }
    }

    #[test]
    fn x_generic_without_a_backend_says_why_it_lists_them_all() {
        let env = linux(|e| {
            e.chromeos = Some(false);
            e.desktop = Some("X-Generic".into());
            e.portal_installed = Some(true);
            e.screencast_available = Some(false);
            e.portal_backends = vec!["gtk".into()];
            e.distro_id = Some("debian".into());
        });
        let d = diagnose(PORTAL_NOT_FOUND, Some(&env)).expect("diagnosis");
        assert_eq!(d.title, "No screen-sharing backend installed");
        assert!(d.summary.contains("Installed backends: gtk."), "{}", d.summary);
        assert!(d.summary.contains("“X-Generic”"), "{}", d.summary);
        assert_eq!(d.steps[0], BACKEND_MENU);
        assert!(!d.record_in_browser);
    }

    #[test]
    fn a_known_desktop_without_a_backend_is_named_and_not_called_unplaced() {
        let env = linux(|e| {
            e.desktop = Some("GNOME".into());
            e.portal_installed = Some(true);
            e.screencast_available = Some(false);
            e.distro_id = Some("fedora".into());
        });
        let d = diagnose(PORTAL_NOT_FOUND, Some(&env)).expect("diagnosis");
        assert_eq!(d.title, "No screen-sharing backend for GNOME");
        assert!(!d.summary.contains("calls itself"), "{}", d.summary);
        assert_eq!(d.command.as_deref(), Some("sudo dnf install xdg-desktop-portal-gnome"));
    }

    #[test]
    fn the_probe_alone_is_enough_for_a_missing_backend() {
        let env = linux(|e| {
            e.portal_installed = Some(true);
            e.screencast_available = Some(false);
        });
        let d = diagnose("Failed to do something opaque", Some(&env)).expect("diagnosis");
        assert_eq!(d.title, "No screen-sharing backend installed");
    }

    #[test]
    fn web_recorder_url_matches_the_servers_session_url() {
        let token = "4a55606b76818c97a2adb8c3ced9e4effa05101b26313c47525d68737e89949f";
        assert_eq!(
            web_recorder_url("https://lookout.hackclub.com", token),
            format!("https://lookout.hackclub.com/session?token={token}")
        );
        assert_eq!(web_recorder_url("http://127.0.0.1:8797/", "t"), "http://127.0.0.1:8797/session?token=t");
    }

    #[test]
    fn the_report_says_whether_this_is_chromeos() {
        let env = linux(|e| e.chromeos = Some(true));
        let report = build_report(PORTAL_NOT_FOUND, Some(&env), None);
        assert!(report.contains("chromeos linux: yes"), "{report}");
    }
}
