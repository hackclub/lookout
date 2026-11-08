//! Typography. The Tauri app set no line-height, so every line box was the
//! font's `normal` height: Geist's hhea metrics give 1.3, San Francisco
//! about 1.19. We reproduce that per face so control heights match.

use std::sync::OnceLock;

use gpui_kit::gpui::{FontWeight, SharedString, Styled, px};

static FAMILY: OnceLock<(SharedString, f32)> = OnceLock::new();

/// Pick the UI face once at startup. macOS used SF (the stack began with
/// `-apple-system`), Windows rendered the bundled Geist, and Linux used the
/// desktop's UI font (Adwaita Sans / Cantarell). Where none of the desktop
/// faces is installed we use Geist rather than whatever generic sans the
/// fontconfig default happens to be — a deliberate improvement.
pub fn init_family(available: &[SharedString]) {
    let has = |n: &str| available.iter().any(|f| f.as_ref() == n);
    let choice: (SharedString, f32) = if cfg!(target_os = "macos") {
        (".SystemUIFont".into(), 1.19)
    } else if cfg!(target_os = "windows") {
        ("Geist".into(), 1.3)
    } else if has("Adwaita Sans") {
        ("Adwaita Sans".into(), 1.21)
    } else if has("Cantarell") {
        ("Cantarell".into(), 1.2)
    } else {
        ("Geist".into(), 1.3)
    };
    let _ = FAMILY.set(choice);
}

pub fn family() -> SharedString {
    FAMILY.get().map(|f| f.0.clone()).unwrap_or_else(|| "Geist".into())
}

/// CSS `line-height: normal` for the chosen face, as a multiple of size.
pub fn normal_lh() -> f32 {
    FAMILY.get().map(|f| f.1).unwrap_or(1.3)
}

pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

pub trait TextExt: Styled + Sized {
    /// `font-size: <size>px` with the face's normal line height.
    fn ts(self, size: f32) -> Self {
        self.text_size(px(size)).line_height(px((size * normal_lh()).round()))
    }
    /// `font-size` with an explicit CSS `line-height` multiplier.
    fn ts_lh(self, size: f32, lh: f32) -> Self {
        self.text_size(px(size)).line_height(px(size * lh))
    }
    fn w400(self) -> Self {
        self.font_weight(FontWeight::NORMAL)
    }
    fn w500(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }
    fn w600(self) -> Self {
        self.font_weight(FontWeight::SEMIBOLD)
    }
    fn w700(self) -> Self {
        self.font_weight(FontWeight::BOLD)
    }
}

impl<T: Styled + Sized> TextExt for T {}
