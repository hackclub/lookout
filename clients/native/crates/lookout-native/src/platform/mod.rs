//! OS integration: camera, tray / menu-bar indicator, notifications,
//! window chrome helpers, deep links, single instance.

pub mod app_menu;
pub mod camera;
pub mod decoder;
pub mod mac_menu;
#[cfg(target_os = "macos")]
pub mod macos_occlusion;
pub mod permissions;
pub mod reduce_motion;
pub mod single_instance;
pub mod tray;
pub mod vibrancy;
pub mod webview;
pub mod desktop_appearance;

use std::sync::Arc;

use gpui_kit::gpui::Hsla;

/// The window icon (X11 only reads it; elsewhere the bundle icon is used).
pub fn window_icon() -> Option<Arc<image::RgbaImage>> {
    image::load_from_memory(crate::assets::APP_ICON_PNG).ok().map(|i| Arc::new(i.into_rgba8()))
}

/// The desktop accent colour (GNOME `accent-color`), when there is one.
pub fn accent() -> Option<Hsla> {
    let a = desktop_appearance::desktop_appearance().accent?;
    let hex = u32::from_str_radix(a.trim_start_matches('#'), 16).ok()?;
    Some(crate::theme::hex(hex))
}

/// A desktop notification (`recorder-notice` → `sendNotification`).
pub fn notify(title: &str, body: &str, cx: &mut gpui_kit::gpui::App) {
    cx.show_system_notification(gpui_kit::gpui::SystemNotification {
        tag: "lookout-recorder".into(),
        title: title.to_string().into(),
        body: body.to_string().into(),
        actions: Vec::new(),
    });
}
