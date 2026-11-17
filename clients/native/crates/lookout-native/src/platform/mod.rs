//! OS integration: camera, tray / menu-bar indicator, notifications,
//! window chrome helpers, deep links, single instance.

pub mod a11y_settings;
pub mod app_menu;
pub mod camera;
pub mod decoder;
pub mod mac_menu;
#[cfg(target_os = "macos")]
pub mod macos_occlusion;
#[cfg(target_os = "macos")]
pub mod objc_exceptions;
pub mod permission_guide;
pub mod system_icons;
pub mod permissions;
pub mod pref_pane;
pub mod relaunch;
pub mod reduce_motion;
pub mod single_instance;
pub mod tray;
pub mod vibrancy;
pub mod webview;
pub mod desktop_appearance;
pub mod dock_menu;
pub mod haptics;
pub mod notifications;
pub mod taskbar;
pub mod fullscreen;
pub mod startup_failure;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc_compat;

use std::sync::Arc;

use gpui_kit::gpui::Hsla;

/// The window icon (X11 only reads it; elsewhere the bundle icon is used).
pub fn window_icon() -> Option<Arc<image::RgbaImage>> {
    image::load_from_memory(crate::assets::APP_ICON_PNG).ok().map(|i| Arc::new(i.into_rgba8()))
}

/// The desktop's accent colour, when Lookout should use it instead of its
/// own blue: GNOME's `accent-color` on Linux, and on macOS the system accent
/// unless that is blue too (see [`mac_accent`]).
pub fn accent() -> Option<Hsla> {
    #[cfg(target_os = "macos")]
    {
        mac_accent()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let a = desktop_appearance::desktop_appearance().accent?;
        let hex = u32::from_str_radix(a.trim_start_matches('#'), 16).ok()?;
        Some(crate::theme::hex(hex))
    }
}

/// System Settings → Appearance → Accent color. `AppleAccentColor` is absent
/// for Multicolor (which apps show as blue) and 4 for Blue: both keep
/// Lookout's blue. Any other choice (graphite included) becomes the accent.
#[cfg(target_os = "macos")]
#[allow(unused_unsafe)]
fn mac_accent() -> Option<Hsla> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    use objc2_foundation::{NSString, NSUserDefaults};
    let defaults = unsafe { NSUserDefaults::standardUserDefaults() };
    let key = NSString::from_str("AppleAccentColor");
    unsafe { defaults.objectForKey(&key) }?;
    if unsafe { defaults.integerForKey(&key) } == 4 {
        return None;
    }
    let c = unsafe { NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) }?;
    let (r, g, b) = unsafe { (c.redComponent(), c.greenComponent(), c.blueComponent()) };
    Some(gpui_kit::gpui::Rgba { r: r as f32, g: g as f32, b: b as f32, a: 1.0 }.into())
}

/// A recorder notice as a desktop notification (`recorder-notice` →
/// `sendNotification`), with buttons for what's worth doing next: Resume on
/// a pause, and Open (the timelapse in Lookout) on all of them. Clicking the
/// notification itself opens it too. See [`notifications`].
pub fn notify_recorder(token: &str, title: &str, body: &str, kind: lookout_core::NoticeKind, cx: &mut gpui_kit::gpui::App) {
    cx.show_system_notification(gpui_kit::gpui::SystemNotification {
        tag: format!("{}{token}", notifications::RECORDER_TAG).into(),
        title: title.to_string().into(),
        body: body.to_string().into(),
        actions: notifications::actions(kind),
    });
}
