//! The OS "reduce motion" setting, fed into [`crate::motion::set_reduce_motion`].
//!
//! - macOS: System Settings → Accessibility → Display → Reduce motion.
//! - Windows: Settings → Accessibility → Visual effects → Animation effects
//!   (`SPI_GETCLIENTAREAANIMATION`).
//! - Linux: GNOME's `enable-animations`, which is also what WebKitGTK maps
//!   `prefers-reduced-motion` to, so the Tauri app saw the same answer.
//!
//! Re-read every couple of seconds on a background thread so flipping the
//! setting applies without a restart; the value is an atomic that every
//! animation reads when it starts.

use std::time::Duration;

const POLL: Duration = Duration::from_secs(2);

pub fn observe() {
    apply(read());
    let _ = std::thread::Builder::new().name("reduce-motion".into()).spawn(|| loop {
        std::thread::sleep(POLL);
        apply(read());
    });
}

fn apply(on: Option<bool>) {
    if let Some(on) = on {
        if on != crate::motion::reduce_motion() {
            log::info!("[motion] reduce motion {}", if on { "on" } else { "off" });
        }
        crate::motion::set_reduce_motion(on);
    }
}

#[cfg(target_os = "macos")]
fn read() -> Option<bool> {
    Some(objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion())
}

#[cfg(target_os = "windows")]
fn read() -> Option<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut enabled = windows::core::BOOL::default();
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL through the pointer.
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(std::ptr::from_mut(&mut enabled).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()
    .map(|()| !enabled.as_bool())
}

#[cfg(target_os = "linux")]
fn read() -> Option<bool> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_gsettings_bool(&String::from_utf8_lossy(&out.stdout)).map(|enabled| !enabled)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn read() -> Option<bool> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn parse_gsettings_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_gsettings_output() {
        assert_eq!(super::parse_gsettings_bool("false\n"), Some(false));
        assert_eq!(super::parse_gsettings_bool("true"), Some(true));
        assert_eq!(super::parse_gsettings_bool("No such schema"), None);
    }
}
