//! Design tokens — a 1:1 port of `clients/react/src/ui/theme.ts` (dark is
//! the default `:root` block, light the `prefers-color-scheme: light` one),
//! plus the Linux-only header-bar / popover variables from `linuxChrome.ts`.
//!
//! Every value here is copied from the CSS so the two apps render the same
//! pixels; see `docs/ui-spec.md` §1.4-1.7 for the source lines.

use gpui_kit::gpui::{App, BoxShadow, ElementId, Global, Hsla, Rgba, WindowAppearance, point, px};

/// A CSS `box-shadow` layer: `<ox> <oy> <blur> <spread> <color>`.
pub fn sh(ox: f32, oy: f32, blur: f32, spread: f32, color: Hsla) -> BoxShadow {
    BoxShadow { color, offset: point(px(ox), px(oy)), blur_radius: px(blur), spread_radius: px(spread), inset: false }
}

/// An element id derived from a domain key (session token, app name…).
pub fn eid(prefix: &str, key: impl AsRef<str>) -> ElementId {
    ElementId::Name(format!("{prefix}:{}", key.as_ref()).into())
}

/// Which OS we are drawing for — several screens differ per platform
/// exactly as the Tauri app did (back-button labels, header bar, pill
/// placement…).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

pub const OS: Os = if cfg!(target_os = "macos") {
    Os::Mac
} else if cfg!(target_os = "windows") {
    Os::Windows
} else {
    Os::Linux
};

pub fn is_mac() -> bool {
    OS == Os::Mac
}
pub fn is_windows() -> bool {
    OS == Os::Windows
}
pub fn is_linux() -> bool {
    OS == Os::Linux
}

static HEADER_BAR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the main window is showing the Linux header bar, which carries
/// the page title and Back button. When it isn't (macOS, Windows, or a Linux
/// session where the window manager draws the frame), pages draw their own.
/// Set by the main window each frame, before its pages render.
pub fn header_bar_shown() -> bool {
    HEADER_BAR.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_header_bar_shown(on: bool) {
    HEADER_BAR.store(on, std::sync::atomic::Ordering::Relaxed);
}

static VIBRANT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a system material (macOS vibrancy, Windows Mica) shows through the
/// main window. Scroll-edge fades are colour overlays here (GPUI has no
/// gradient masks, which the Tauri app faded content with), and a flat colour
/// can't fade into a translucent material, so they're skipped while it's on.
pub fn vibrant() -> bool {
    VIBRANT.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_vibrant(on: bool) {
    VIBRANT.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// `#rrggbb` + alpha → Hsla.
pub const fn rgba8(r: u8, g: u8, b: u8, a: f32) -> Rgba {
    Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

pub fn hex(rgb: u32) -> Hsla {
    rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 1.0).into()
}

pub fn hexa(rgb: u32, a: f32) -> Hsla {
    rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, a).into()
}

pub fn white(a: f32) -> Hsla {
    rgba8(255, 255, 255, a).into()
}

pub fn black(a: f32) -> Hsla {
    rgba8(0, 0, 0, a).into()
}

pub fn transparent() -> Hsla {
    rgba8(0, 0, 0, 0.0).into()
}

/// Fixed (theme-independent) status colours, `colors.status`.
pub mod status {
    use super::*;
    pub fn success() -> Hsla {
        hex(0x22c55e)
    }
    pub fn info() -> Hsla {
        hex(0x3b82f6)
    }
    pub fn warning() -> Hsla {
        hex(0xf59e0b)
    }
    pub fn danger() -> Hsla {
        hex(0xef4444)
    }
}

/// `spacing` (theme.ts:201).
pub mod spacing {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 20.0;
    pub const XXL: f32 = 24.0;
    pub const XXXL: f32 = 32.0;
}

/// `radii` (theme.ts:202).
pub mod radii {
    pub const SM: f32 = 6.0;
    pub const MD: f32 = 8.0;
    pub const LG: f32 = 10.0;
}

/// `fontSize` (theme.ts:203).
pub mod font_size {
    pub const XS: f32 = 11.0;
    pub const SM: f32 = 12.0;
    pub const MD: f32 = 13.0;
    pub const LG: f32 = 14.0;
    pub const XL: f32 = 16.0;
    pub const XXL: f32 = 18.0;
    pub const HEADING: f32 = 20.0;
    pub const DISPLAY: f32 = 24.0;
    pub const TIMER: f32 = 32.0;
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    pub bg_body: Hsla,
    pub bg_panel: Hsla,
    pub bg_backdrop: Hsla,
    pub bg_surface: Hsla,
    pub bg_sunken: Hsla,
    pub bg_selected: Hsla,
    pub text_primary: Hsla,
    pub text_inverse: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_quaternary: Hsla,
    pub text_error: Hsla,
    pub border_default: Hsla,
    pub border_hover: Hsla,
    pub border_selected: Hsla,
    pub icon_selected: Hsla,
    pub status_neutral: Hsla,
    pub spinner_base: Hsla,
    pub spinner_track: Hsla,
    pub skeleton_bg: Hsla,
    pub skeleton_shimmer: Hsla,
    pub badge_primary_bg: Hsla,
    pub badge_primary_text: Hsla,
    pub badge_overlay_bg: Hsla,
    pub badge_overlay_text: Hsla,
    pub archive_bg: Hsla,
    pub archive_icon: Hsla,
    pub archive_border: Hsla,
    pub archive_hover_bg: Hsla,
    pub archive_hover_border: Hsla,
    pub well: Hsla,
    pub well_border: Hsla,
    pub cut_fill: Hsla,
    pub cut_fill_hover: Hsla,
    pub cut_border: Hsla,
    pub cut_stripe: Hsla,
    pub track: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub on_accent: Hsla,
    // Linux-only (linuxChrome.ts:132-163)
    pub headerbar_control: Hsla,
    pub headerbar_control_hover: Hsla,
    pub popover_bg: Hsla,
    pub popover_bg_top: Hsla,
    pub popover_border: Hsla,
    pub popover_hover: Hsla,
    pub popover_separator: Hsla,
    pub window_border: Hsla,
}

impl Global for Theme {}

impl Theme {
    pub fn dark() -> Self {
        let accent = hex(0x3b82f6);
        Self {
            dark: true,
            bg_body: hex(0x242424),
            bg_panel: hex(0x1e1e1e),
            bg_backdrop: black(0.55),
            bg_surface: white(0.08),
            bg_sunken: white(0.04),
            bg_selected: white(0.12),
            text_primary: hex(0xffffff),
            text_inverse: hex(0x000000),
            text_secondary: white(0.7),
            text_tertiary: white(0.5),
            text_quaternary: white(0.28),
            text_error: hex(0xfca5a5),
            border_default: white(0.12),
            border_hover: white(0.22),
            border_selected: white(0.3),
            icon_selected: white(0.8),
            status_neutral: white(0.2),
            spinner_base: white(0.1),
            spinner_track: white(0.8),
            skeleton_bg: white(0.06),
            skeleton_shimmer: white(0.12),
            badge_primary_bg: hexa(0x22c55e, 0x26 as f32 / 255.0),
            badge_primary_text: hex(0x22c55e),
            badge_overlay_bg: black(0.7),
            badge_overlay_text: hex(0xffffff),
            archive_bg: black(0.6),
            archive_icon: hex(0xffffff),
            archive_border: white(0.1),
            archive_hover_bg: white(0.1),
            archive_hover_border: white(0.2),
            well: black(0.32),
            well_border: white(0.1),
            cut_fill: rgba8(248, 113, 113, 0.26).into(),
            cut_fill_hover: rgba8(248, 113, 113, 0.36).into(),
            cut_border: hex(0xf87171),
            cut_stripe: rgba8(248, 113, 113, 0.13).into(),
            track: white(0.1),
            accent,
            accent_hover: oklab_mix_black(accent, 0.88),
            on_accent: hex(0xffffff),
            headerbar_control: white(0.1),
            headerbar_control_hover: white(0.18),
            popover_bg: hex(0x383838),
            popover_bg_top: hex(0x3d3d3d),
            popover_border: white(0.14),
            popover_hover: white(0.1),
            popover_separator: white(0.15),
            window_border: white(0.18),
        }
    }

    pub fn light() -> Self {
        let accent = hex(0x3b82f6);
        Self {
            dark: false,
            bg_body: hex(0xfafafa),
            bg_panel: hex(0xffffff),
            bg_backdrop: black(0.35),
            bg_surface: hex(0xffffff),
            bg_sunken: black(0.04),
            bg_selected: black(0.1),
            text_primary: black(0.85),
            text_inverse: hex(0xffffff),
            text_secondary: black(0.65),
            text_tertiary: black(0.45),
            text_quaternary: black(0.25),
            text_error: hex(0xef4444),
            border_default: black(0.12),
            border_hover: black(0.22),
            border_selected: black(0.3),
            icon_selected: black(0.8),
            status_neutral: hex(0x000000),
            spinner_base: black(0.1),
            spinner_track: black(0.8),
            skeleton_bg: black(0.05),
            skeleton_shimmer: black(0.08),
            badge_primary_bg: hex(0x22c55e),
            badge_primary_text: hex(0xffffff),
            badge_overlay_bg: hex(0x000000),
            badge_overlay_text: hex(0xffffff),
            archive_bg: white(0.9),
            archive_icon: hex(0x000000),
            archive_border: black(0.1),
            archive_hover_bg: white(1.0),
            archive_hover_border: black(0.2),
            well: black(0.08),
            well_border: black(0.1),
            cut_fill: rgba8(220, 38, 38, 0.20).into(),
            cut_fill_hover: rgba8(220, 38, 38, 0.30).into(),
            cut_border: hex(0xdc2626),
            cut_stripe: rgba8(220, 38, 38, 0.12).into(),
            track: black(0.08),
            accent,
            accent_hover: oklab_mix_black(accent, 0.88),
            on_accent: hex(0xffffff),
            headerbar_control: black(0.08),
            headerbar_control_hover: black(0.16),
            popover_bg: hex(0xf7f7f7),
            popover_bg_top: hex(0xffffff),
            popover_border: black(0.12),
            popover_hover: black(0.08),
            popover_separator: black(0.12),
            window_border: black(0.18),
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        // `LOOKOUT_THEME=dark|light` pins the theme (development / screenshots).
        match std::env::var("LOOKOUT_THEME").as_deref() {
            Ok("dark") => return Self::dark(),
            Ok("light") => return Self::light(),
            _ => {}
        }
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
        }
    }

    /// `setAccentColor(accent, on)` — the Linux desktop's accent (GNOME
    /// `accent-color`) replaces the brand blue; hover is always derived
    /// with `color-mix(in oklab, accent 88%, black)`.
    pub fn with_accent(mut self, accent: Option<Hsla>) -> Self {
        if let Some(accent) = accent {
            self.accent = accent;
            self.accent_hover = oklab_mix_black(accent, 0.88);
            self.on_accent = accent_foreground(accent);
        }
        self
    }

    /// `colors.bg.backdrop` as the modal dim fill; on Windows the app used a
    /// top-to-bottom gradient (`backdrop.ts`), which callers draw themselves.
    pub fn backdrop(&self) -> Hsla {
        self.bg_backdrop
    }
}

pub fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

// ── Colour maths ────────────────────────────────────────────────────────

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// `color-mix(in oklab, <c> <p>, black)`.
pub fn oklab_mix_black(c: Hsla, p: f32) -> Hsla {
    let rgba: Rgba = c.into();
    let (r, g, b) = (srgb_to_linear(rgba.r), srgb_to_linear(rgba.g), srgb_to_linear(rgba.b));
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    let ok_l = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let ok_a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let ok_b = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
    // black is (0, 0, 0) in OKLab
    let (ml, ma, mb) = (ok_l * p, ok_a * p, ok_b * p);
    let l_ = ml + 0.3963377774 * ma + 0.2158037573 * mb;
    let m_ = ml - 0.1055613458 * ma - 0.0638541728 * mb;
    let s_ = ml - 0.0894841775 * ma - 1.2914855480 * mb;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    Rgba {
        r: linear_to_srgb(r).clamp(0.0, 1.0),
        g: linear_to_srgb(g).clamp(0.0, 1.0),
        b: linear_to_srgb(b).clamp(0.0, 1.0),
        a: rgba.a,
    }
    .into()
}

/// `accentForeground(hex)` (linuxChrome.ts:304-324): black text on light
/// accents (relative luminance > 0.45), white otherwise.
pub fn accent_foreground(accent: Hsla) -> Hsla {
    let c: Rgba = accent.into();
    let l = 0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b);
    if l > 0.45 { hex(0x000000) } else { hex(0xffffff) }
}

/// `color-mix(in srgb, <c> <p>, transparent)` — the colour at `p` of its
/// own alpha (premultiplied mixing with transparent keeps the hue).
pub fn fade(c: Hsla, p: f32) -> Hsla {
    Hsla { a: c.a * p, ..c }
}

/// Premultiplied-alpha interpolation between two colours, so a hover from
/// transparent to a tint never flashes through grey (CSS interpolates
/// colours in premultiplied space too).
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let ra: Rgba = a.into();
    let rb: Rgba = b.into();
    let alpha = ra.a + (rb.a - ra.a) * t;
    if alpha <= 0.0001 {
        return transparent();
    }
    let pm = |ca: f32, cb: f32| (ca * ra.a + (cb * rb.a - ca * ra.a) * t) / alpha;
    Rgba { r: pm(ra.r, rb.r), g: pm(ra.g, rb.g), b: pm(ra.b, rb.b), a: alpha }.into()
}

/// The status label + colour per session status (`statusConfig`).
pub fn status_config(status: &str, t: &Theme) -> (&'static str, Hsla) {
    match status {
        "pending" => ("Pending", t.status_neutral),
        "active" => ("Recording", status::success()),
        "paused" => ("Paused", status::warning()),
        "stopped" => ("Processing", status::info()),
        "compiling" => ("Compiling", status::info()),
        "complete" => ("Complete", status::success()),
        "failed" => ("Failed", status::danger()),
        _ => ("", t.status_neutral),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_hover_matches_browser() {
        // color-mix(in oklab, #3b82f6 88%, black) ≈ the CSS fallback #2f6fd0.
        let c: Rgba = oklab_mix_black(hex(0x3b82f6), 0.88).into();
        let (r, g, b) = ((c.r * 255.0).round(), (c.g * 255.0).round(), (c.b * 255.0).round());
        assert!((r - 47.0).abs() <= 2.0 && (g - 111.0).abs() <= 3.0 && (b - 208.0).abs() <= 2.0, "{r} {g} {b}");
    }
}
