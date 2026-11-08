//! Embedded assets: icons (Phosphor exports + the hand-drawn SVGs the Tauri
//! app inlined) and the Geist faces. Everything ships inside the binary.

use std::borrow::Cow;

use gpui_kit::gpui::{AssetSource, SharedString};

macro_rules! embed {
    ($($name:literal),* $(,)?) => {
        &[$(
            (concat!("icons/", $name, ".svg"), include_bytes!(concat!("../../../assets/icons/", $name, ".svg")) as &[u8]),
        )*]
    };
}

static ICONS: &[(&str, &[u8])] = embed!(
    "app-window-regular",
    "arrow-square-out-bold",
    "broadcast-regular",
    "caret-left-bold",
    "caret-right-bold",
    "check-bold",
    "copy-regular",
    "funnel-fill",
    "gear-six-fill",
    "info-fill",
    "link-bold",
    "monitor-regular",
    "plus-bold",
    "video-camera-regular",
    "warning-circle-fill",
    "wrench-fill",
    "x-bold",
    "lk-chevron-left",
    "lk-header-back",
    "lk-header-close",
    "lk-win-minimize",
    "lk-win-close",
    "lk-monitor",
    "lk-camera",
    "lk-error-circle",
    "lk-check-line",
    "lk-pause-glyph",
    "lk-archive-x",
    "lk-pencil",
    "lk-play",
    "lk-pause-bars",
    "lk-power",
    "lk-editor-window",
    "hc-flag",
    "heart-fill",
    "vp-play",
    "vp-pause",
    "vp-replay",
    "vp-loop",
    "vp-expand",
    "vp-shrink",
);

pub static FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"),
];

pub static APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/app-icon.png");
pub static TRAY_ICON_PNG: &[u8] = include_bytes!("../../../assets/tray-icon.png");

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(n, _)| *n == path).map(|(_, b)| Cow::Borrowed(*b)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(n, _)| n.starts_with(path))
            .map(|(n, _)| SharedString::from(*n))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_icon_reference_exists() {
        // Every `Icon::path()` must be embedded.
        for icon in crate::ui::icon::Icon::ALL {
            assert!(
                super::ICONS.iter().any(|(n, _)| *n == icon.path()),
                "missing icon asset {}",
                icon.path()
            );
        }
    }
}
