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

    pub fn camera_granted() -> bool {
        use objc2::runtime::AnyClass;
        use objc2::msg_send;
        use objc2_foundation::NSString;
        let Some(cls) = AnyClass::get(c"AVCaptureDevice") else { return true };
        let media = NSString::from_str("vide");
        // AVAuthorizationStatusAuthorized == 3
        let status: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] };
        status == 3
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
pub fn request(kind: Kind, cx: &mut App) {
    OPENED_PRIVACY_PANE.store(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    {
        // A bundled app gets the guide (the pane plus a drag-in tile docked to
        // System Settings); a dev binary has nothing to drag, so it gets
        // the system prompt and the pane alone.
        if super::permission_guide::show(kind) {
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
        let _ = (kind, cx);
    }
}
