//! Lookout can't start on this machine (no display server, no GPU that can
//! draw a window): say so in a native dialog and on stderr, then exit.
//!
//! These are environment problems, not bugs, so they don't panic and don't
//! become Sentry errors; [`gpu`] still leaves a warning, since a GPU we can't
//! drive is sometimes something we could fix.

#[cfg(not(windows))]
use std::process::Command;

/// Why Lookout gave up, in the user's words.
pub struct Failure {
    pub title: &'static str,
    pub body: String,
}

/// Whether a window-open error means no GPU adapter could draw it (as
/// opposed to a bug in our window code).
pub fn is_gpu_error(err: &str) -> bool {
    let l = err.to_lowercase();
    l.contains("no gpu adapter") || l.contains("failed to create surface") || l.contains("not compatible with the display surface")
}

/// No GPU adapter could present to the window.
pub fn gpu(err: &str) -> Failure {
    let mut body = String::from(
        "Lookout couldn't find a graphics driver that can draw its window, so it can't start.\n\n",
    );
    if cfg!(target_os = "linux") {
        body.push_str(
            "Lookout draws with Vulkan (or OpenGL as a fallback). Make sure the Vulkan loader and your GPU's \
             Vulkan driver are installed: vulkan-icd-loader plus mesa-vulkan-drivers (AMD/Intel) or the NVIDIA \
             driver's Vulkan package.\n\n\
             On NixOS, enable hardware.graphics and run Lookout through nix-ld or nixGL so it can load the \
             system's drivers.\n\n",
        );
        if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) && std::env::var_os("DISPLAY").is_some() {
            body.push_str("On Wayland, starting it under XWayland can also help:  WAYLAND_DISPLAY= lookout\n\n");
        }
    } else {
        body.push_str("Updating your graphics driver usually fixes this.\n\n");
    }
    body.push_str(&format!("Details: {err}"));
    Failure { title: "Lookout can't draw its window", body }
}

/// The X server named by `DISPLAY` refused the connection.
pub fn x11(display: &str, err: &str) -> Failure {
    Failure {
        title: "Lookout can't reach the display",
        body: format!(
            "Lookout couldn't connect to the X server at DISPLAY={display}, so it can't open a window.\n\n\
             Start Lookout from inside your desktop session (not over SSH, from another user, or with sudo). \
             If it's started from a script, make sure DISPLAY and XAUTHORITY match your session.\n\n\
             Details: {err}"
        ),
    }
}

/// No display server at all.
pub fn no_display() -> Failure {
    Failure {
        title: "Lookout needs a desktop session",
        body: "Neither WAYLAND_DISPLAY nor DISPLAY is set, so there's no display to open a window on. \
               Start Lookout from inside your desktop session."
            .into(),
    }
}

/// Tells the user and exits with status 1. Never returns.
pub fn exit(f: Failure) -> ! {
    log::warn!("[startup] {}: {}", f.title, f.body.replace('\n', " "));
    eprintln!("\n{}\n\n{}\n", f.title, f.body);
    show_dialog(f.title, &f.body);
    // End the release-health session as a clean exit; nothing crashed.
    sentry::end_session_with_status(sentry::protocol::SessionStatus::Exited);
    if let Some(client) = sentry::Hub::current().client() {
        client.flush(Some(std::time::Duration::from_secs(2)));
    }
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn show_dialog(title: &str, body: &str) {
    // Whichever dialog tool the desktop has; GTK/Qt draw in software, so they
    // work when we couldn't get a GPU. With no display at all they fail
    // quietly and stderr is all there is.
    let dialogs: [(&str, Vec<&str>); 2] = [
        ("zenity", vec!["--error", "--no-wrap", "--title", title, "--text", body]),
        ("kdialog", vec!["--title", title, "--error", body]),
    ];
    for (bin, args) in dialogs {
        // Waits until the user closes it; Err = not installed.
        if Command::new(bin).args(&args).status().is_ok() {
            return;
        }
    }
    // Last resort, not waited for: with no notification daemon it blocks
    // until D-Bus activation times out.
    let _ = Command::new("notify-send")
        .args(["--urgency=critical", "--app-name=Lookout", title, body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(target_os = "macos")]
fn show_dialog(title: &str, body: &str) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display alert \"{}\" message \"{}\" as critical", esc(title), esc(body));
    let _ = Command::new("osascript").args(["-e", &script]).status();
}

#[cfg(windows)]
fn show_dialog(title: &str, body: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    unsafe {
        MessageBoxW(None, &HSTRING::from(body), &HSTRING::from(title), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn show_dialog(_title: &str, _body: &str) {}

/// Linux: before GPUI connects (it panics if the display server it picks
/// can't be reached), check that one can. If `DISPLAY`'s X server refuses us
/// but a Wayland compositor's socket is there, use that instead. Returns the
/// failure to report (with [`exit`]) once we know this is the primary
/// instance: a second launch only hands its links over and needs no display.
/// Must run before any thread is started (it may set `WAYLAND_DISPLAY`).
#[cfg(target_os = "linux")]
pub fn preflight_display() -> Option<Failure> {
    let set = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if set("WAYLAND_DISPLAY").is_some() || set("WAYLAND_SOCKET").is_some() {
        // GPUI uses Wayland.
        return None;
    }
    let use_wayland = |socket: String, why: String| {
        log::warn!("[startup] {why}; using Wayland ({socket})");
        // SAFETY: called first thing in main, before any thread exists.
        unsafe { std::env::set_var("WAYLAND_DISPLAY", &socket) };
    };
    let Some(display) = set("DISPLAY") else {
        return match wayland_socket() {
            Some(socket) => {
                use_wayland(socket, "neither DISPLAY nor WAYLAND_DISPLAY is set".into());
                None
            }
            None => Some(no_display()),
        };
    };
    let display = display.to_string_lossy().into_owned();
    let name = std::ffi::CString::new(display.clone()).ok()?;
    // The same connection call GPUI makes (libxcb), so a pass here means a
    // pass there.
    match x11rb::xcb_ffi::XCBConnection::connect(Some(&name)) {
        Ok(_) => None,
        Err(e) => match wayland_socket() {
            Some(socket) => {
                use_wayland(socket, format!("X server {display:?} refused the connection ({e})"));
                None
            }
            None => Some(x11(&display, &e.to_string())),
        },
    }
}

/// A live Wayland socket in `XDG_RUNTIME_DIR`, when exactly one is there
/// (with several, we can't tell which is this session's).
#[cfg(target_os = "linux")]
fn wayland_socket() -> Option<String> {
    use std::os::unix::fs::FileTypeExt;
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let mut found = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_errors_are_recognised() {
        assert!(is_gpu_error("No GPU adapter found that can configure the display surface"));
        assert!(is_gpu_error("No GPU adapters found"));
        assert!(!is_gpu_error("window not found"));
    }
}
