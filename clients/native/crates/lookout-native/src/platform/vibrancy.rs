//! The window's system material, as the Tauri app had it (`enable_vibrancy`
//! in `src-tauri/src/lib.rs`):
//!
//! - macOS: an `NSVisualEffectView` with the Sidebar material, blended behind
//!   the window and kept Active even when the window isn't key, corners
//!   rounded to 16px on macOS 26+ and 10px before. GPUI's own `Blurred`
//!   background strips the material's tint and leaves a plain blur, so this
//!   adds the real material under GPUI's view instead, the way Cap does.
//! - Windows 11 (build 22621+): Mica, which GPUI applies through
//!   `WindowBackgroundAppearance::MicaBackdrop`. Older builds ignore that
//!   request, so they keep the flat background.
//! - Linux: none.
//!
//! [`apply`] answers whether a material is actually in place; only then do
//! pages leave the window background unpainted (the Tauri frontend did the
//! same with `html`/`body`/`#root` once `enable_vibrancy` had succeeded).

use gpui_kit::gpui::Window;

pub fn apply(window: &Window) -> bool {
    imp::apply(window, false)
}

/// The onboarding window's material: on macOS the lighter, more
/// see-through under-window material rather than the sidebar's, so the
/// desktop colours glow through behind the stickers. Mica elsewhere.
pub fn apply_onboarding(window: &Window) -> bool {
    imp::apply(window, true)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindowOrderingMode,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub fn apply(window: &Window, onboarding: bool) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let Ok(handle) = HasWindowHandle::window_handle(window) else { return false };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
        // SAFETY: GPUI's AppKit handle points at its live NSView (GPUIView).
        let gpui_view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        let Some(ns_window) = gpui_view.window() else { return false };
        if !onboarding {
            measure_traffic_lights(&ns_window);
        }
        let Some(content) = ns_window.contentView() else { return false };

        let effect = NSVisualEffectView::initWithFrame(mtm.alloc(), content.bounds());
        effect.setMaterial(if onboarding {
            NSVisualEffectMaterial::UnderWindowBackground
        } else {
            NSVisualEffectMaterial::Sidebar
        });
        effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        effect.setState(NSVisualEffectState::Active);
        effect.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        effect.setWantsLayer(true);
        let radius = if macos_major() >= 26 { 16.0 } else { 10.0 };
        // SAFETY: plain CALayer property setters on the view's own layer.
        unsafe {
            let layer: *mut AnyObject = msg_send![&*effect, layer];
            if !layer.is_null() {
                let _: () = msg_send![layer, setCornerRadius: radius as f64];
                let _: () = msg_send![layer, setMasksToBounds: true];
            }
        }
        // Below GPUI's view, inside the window's standard content view.
        content.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, None);
        let _: Retained<NSVisualEffectView> = effect;
        true
    }

    /// Hand the header bar the real width of the traffic-light cluster.
    fn measure_traffic_lights(ns_window: &objc2_app_kit::NSWindow) {
        use objc2_app_kit::NSWindowButton;
        let close = ns_window.standardWindowButton(NSWindowButton::CloseButton);
        let zoom = ns_window.standardWindowButton(NSWindowButton::ZoomButton);
        if let (Some(close), Some(zoom)) = (close, zoom) {
            let (c, z) = (close.frame(), zoom.frame());
            let span = (z.origin.x + z.size.width) - c.origin.x;
            crate::ui::header_bar::set_mac_traffic_light_span(span as f32);
        }
    }

    fn macos_major() -> isize {
        objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion().majorVersion
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::*;

    /// Mica needs build 22621 (`DWMWA_SYSTEMBACKDROP_TYPE`); GPUI skips the
    /// request on anything older.
    pub fn apply(_window: &Window, _onboarding: bool) -> bool {
        let mut info = windows::Win32::System::SystemInformation::OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<windows::Win32::System::SystemInformation::OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: RtlGetVersion fills the struct whose size we set.
        let ok = unsafe { windows::Wdk::System::SystemServices::RtlGetVersion(&mut info) }.is_ok();
        ok && info.dwBuildNumber >= 22621
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::*;

    pub fn apply(_window: &Window, _onboarding: bool) -> bool {
        false
    }
}
