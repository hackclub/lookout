//! macOS privacy permissions (what `tauri-plugin-macos-permissions` did):
//! Screen Recording through CoreGraphics' preflight/request calls, Camera
//! through AVFoundation's authorization status. Other platforms have no
//! gate.

use gpui_kit::gpui::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Screen,
    Camera,
}

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub fn screen_granted() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn request_screen() {
        unsafe {
            CGRequestScreenCaptureAccess();
        }
    }

    /// `AVAuthorizationStatus`: 0 not determined, 1 restricted, 2 denied,
    /// 3 authorized (`None` without AVFoundation).
    fn camera_status() -> Option<isize> {
        use objc2::msg_send;
        use objc2::runtime::AnyClass;
        use objc2_foundation::NSString;
        let cls = AnyClass::get(c"AVCaptureDevice")?;
        let media = NSString::from_str("vide");
        Some(unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] })
    }

    pub fn camera_granted() -> bool {
        camera_status().is_none_or(|s| s == 3)
    }

    /// Never asked: the system's own prompt is all it takes.
    pub fn camera_undetermined() -> bool {
        camera_status() == Some(0)
    }

    pub fn request_camera() {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, Bool};
        use objc2_foundation::NSString;
        let Some(cls) = AnyClass::get(c"AVCaptureDevice") else { return };
        let media = NSString::from_str("vide");
        // The answer shows up in `check` (the setup page polls it).
        let done = block2::RcBlock::new(|_granted: Bool| {});
        let _: () = unsafe { msg_send![cls, requestAccessForMediaType: &*media, completionHandler: &*done] };
    }
}

/// Whether the permission is currently granted.
pub fn check(kind: Kind) -> bool {
    #[cfg(target_os = "macos")]
    {
        match kind {
            Kind::Screen => mac::screen_granted(),
            Kind::Camera => mac::camera_granted(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        true
    }
}

static OPENED_PRIVACY_PANE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this run sent the user to a System Settings privacy pane (so a
/// "Quit & Reopen" from there is expected; see `platform::relaunch`).
pub fn opened_privacy_pane() -> bool {
    OPENED_PRIVACY_PANE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Ask for it: the system prompt where there is one, else System Settings.
/// `source` is the row that asked (the guide's card flies out of it).
pub fn request(kind: Kind, source: Option<super::permission_guide::Source>, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        if kind == Kind::Camera && mac::camera_undetermined() {
            mac::request_camera();
            return;
        }
        OPENED_PRIVACY_PANE.store(true, std::sync::atomic::Ordering::Relaxed);
        // A bundled app gets the guide (the pane plus a drag-in tile docked to
        // System Settings); a dev binary has nothing to drag, so it gets
        // the system prompt and the pane alone.
        if super::permission_guide::show(kind, source) {
            return;
        }
        match kind {
            Kind::Screen => {
                mac::request_screen();
                cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture");
            }
            Kind::Camera => cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Camera"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (kind, source, cx);
    }
}
