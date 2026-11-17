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
    /// The keyboard focus ring (see `ui::focus`): the accent, or the
    /// strongest text colour under increased contrast. ≥ 3:1 on the body.
    pub focus_ring: Hsla,
    /// Built for the OS "increase contrast" / high-contrast setting.
    pub high_contrast: bool,
    /// The desktop accent passed to [`Theme::with_accent`], kept so the
    /// theme can be rebuilt when a setting flips at runtime.
    pub system_accent: Option<Hsla>,
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
            // 0.5 in the Tauri app: 4.2:1 on the popover, under AA.
            text_tertiary: white(0.55),
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
            focus_ring: accent,
            high_contrast: false,
            system_accent: None,
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
            // A white veil rather than solid white, the way dark's is a tint:
            // over macOS vibrancy / Windows Mica the material shows through
            // cards and controls in both themes. Over the flat #fafafa body
            // (no material) it still comes out white.
            bg_surface: white(0.5),
            bg_sunken: black(0.04),
            bg_selected: black(0.1),
            text_primary: black(0.85),
            text_inverse: hex(0xffffff),
            text_secondary: black(0.65),
            // 0.45 in the Tauri app: 3.3:1, under AA for body text.
            text_tertiary: black(0.55),
            text_quaternary: black(0.25),
            // #ef4444 in the Tauri app: 3.6:1 on the body.
            text_error: hex(0xdc2626),
            border_default: black(0.12),
            border_hover: black(0.22),
            border_selected: black(0.3),
            icon_selected: black(0.8),
            status_neutral: hex(0x000000),
            spinner_base: black(0.1),
            spinner_track: black(0.8),
            skeleton_bg: black(0.05),
            skeleton_shimmer: black(0.08),
            // White on #22c55e was 2.3:1; the same green darkened to AA.
            badge_primary_bg: readable_fill(hex(0x22c55e), hex(0xffffff)),
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
            focus_ring: accent,
            high_contrast: false,
            system_accent: None,
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
        let dark = match std::env::var("LOOKOUT_THEME").as_deref() {
            Ok("dark") => true,
            Ok("light") => false,
            _ => matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark),
        };
        Self::base(dark)
    }

    /// The light or dark palette, in its increased-contrast form when the OS
    /// asks for one ([`more_contrast`]). Windows contrast themes replace the
    /// palette with the system's own colours.
    pub fn base(dark: bool) -> Self {
        if let Some(sys) = system_contrast_colors() {
            return Self::from_system_colors(&sys);
        }
        let t = if dark { Self::dark() } else { Self::light() };
        if more_contrast() { t.with_more_contrast() } else { t }
    }

    /// This theme rebuilt for the current contrast setting, keeping its
    /// light/dark and desktop accent. Used when the setting flips while
    /// Lookout is open.
    pub fn rebuilt(&self) -> Self {
        let accent = self.system_accent;
        Self::base(self.dark).with_accent(accent)
    }

    /// The increased-contrast variant (macOS "Increase contrast", GNOME
    /// "High contrast"): secondary text close to primary, borders that read
    /// as edges (≥ 3:1), opaque surfaces, and a focus ring in the text
    /// colour. Same layout and hues, so nothing moves.
    pub fn with_more_contrast(mut self) -> Self {
        let dark = self.dark;
        let ink = |a: f32| if dark { white(a) } else { black(a) };
        self.high_contrast = true;
        self.text_primary = if self.dark { hex(0xffffff) } else { hex(0x000000) };
        self.text_secondary = ink(0.86);
        self.text_tertiary = ink(0.76);
        self.text_quaternary = ink(0.6);
        self.border_default = ink(0.5);
        self.border_hover = ink(0.65);
        self.border_selected = ink(0.85);
        self.icon_selected = ink(0.95);
        self.status_neutral = if self.dark { white(0.4) } else { hex(0x000000) };
        self.well_border = ink(0.5);
        self.archive_border = ink(0.5);
        self.archive_hover_border = ink(0.7);
        self.track = ink(0.35);
        self.popover_border = ink(0.55);
        self.popover_separator = ink(0.45);
        self.window_border = ink(0.6);
        if self.dark {
            self.bg_surface = white(0.1);
            self.text_error = hex(0xfecaca);
        } else {
            self.bg_surface = white(1.0);
            self.bg_body = hex(0xffffff);
            self.text_error = hex(0xb91c1c);
            self.popover_bg = hex(0xffffff);
        }
        self.bg_backdrop = black(if self.dark { 0.7 } else { 0.5 });
        // The accent doubles as link and selection colour: keep it legible
        // on the body as text, then derive the rest from it.
        self.accent = ensure_contrast(self.accent, self.bg_body, 4.5);
        self.accent_hover = oklab_mix_black(self.accent, 0.88);
        self.on_accent = accent_foreground(self.accent);
        self.focus_ring = self.text_primary;
        self
    }

    /// A Windows contrast theme: the palette comes from the system colours
    /// (`GetSysColor`), the way native Win32/WinUI controls draw.
    pub fn from_system_colors(c: &SystemColors) -> Self {
        let dark = relative_luminance(c.window) < 0.5;
        let mut t = if dark { Self::dark() } else { Self::light() };
        let fg = c.window_text;
        t.high_contrast = true;
        t.bg_body = c.window;
        t.bg_panel = c.window;
        t.popover_bg = c.window;
        t.popover_bg_top = c.window;
        t.bg_surface = c.window;
        t.bg_sunken = c.window;
        t.bg_selected = c.highlight;
        t.text_primary = fg;
        t.text_secondary = fg;
        t.text_tertiary = fg;
        t.text_quaternary = c.gray_text;
        t.text_inverse = c.window;
        t.text_error = fg;
        for b in [
            &mut t.border_default,
            &mut t.border_hover,
            &mut t.border_selected,
            &mut t.well_border,
            &mut t.archive_border,
            &mut t.archive_hover_border,
            &mut t.popover_border,
            &mut t.popover_separator,
            &mut t.window_border,
        ] {
            *b = fg;
        }
        t.icon_selected = fg;
        t.track = c.gray_text;
        t.status_neutral = fg;
        t.headerbar_control = c.window;
        t.headerbar_control_hover = c.highlight;
        t.popover_hover = c.highlight;
        t.accent = c.highlight;
        t.accent_hover = c.highlight;
        t.on_accent = c.highlight_text;
        t.focus_ring = fg;
        t
    }

    /// `setAccentColor(accent, on)` — the Linux desktop's accent (GNOME
    /// `accent-color`) replaces the brand blue; hover is always derived
    /// with `color-mix(in oklab, accent 88%, black)`.
    pub fn with_accent(mut self, accent: Option<Hsla>) -> Self {
        if let Some(accent) = accent {
            self.system_accent = Some(accent);
            if system_contrast_colors().is_some() {
                // A Windows contrast theme's highlight wins.
                return self;
            }
            self.accent = accent;
            if self.high_contrast {
                self.accent = ensure_contrast(accent, self.bg_body, 4.5);
            }
            self.accent_hover = oklab_mix_black(self.accent, 0.88);
            self.on_accent = accent_foreground(self.accent);
            self.focus_ring = if self.high_contrast { self.text_primary } else { self.accent };
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

/// Text on an accent fill: whichever of black and white reads better.
/// (`accentForeground` in linuxChrome.ts switched at luminance 0.45, which
/// put white on mid-tone accents such as GNOME's orange at under 3:1.)
pub fn accent_foreground(accent: Hsla) -> Hsla {
    let (b, w) = (hex(0x000000), hex(0xffffff));
    if contrast_ratio(b, accent) > contrast_ratio(w, accent) { b } else { w }
}

// ── Contrast (WCAG 2.x) ────────────────────────────────────────────────

/// WCAG relative luminance of an opaque colour.
pub fn relative_luminance(c: Hsla) -> f32 {
    let c: Rgba = c.into();
    0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
}

/// `fg` composited over an opaque `bg` (alpha blending in sRGB, as the
/// renderer does).
pub fn over(fg: Hsla, bg: Hsla) -> Hsla {
    let f: Rgba = fg.into();
    let b: Rgba = bg.into();
    let a = f.a;
    Rgba { r: f.r * a + b.r * (1.0 - a), g: f.g * a + b.g * (1.0 - a), b: f.b * a + b.b * (1.0 - a), a: 1.0 }.into()
}

/// WCAG contrast ratio of `fg` drawn on an opaque `bg` (1–21). A
/// translucent `fg` is composited first.
pub fn contrast_ratio(fg: Hsla, bg: Hsla) -> f32 {
    let (a, b) = (relative_luminance(over(fg, bg)), relative_luminance(bg));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// `c` darkened (OKLab, toward black, hue kept) just enough to reach
/// `ratio` against `against`. Returns `c` unchanged when it already does.
/// Only darkens, so `against` should be the lighter of the pair.
pub fn ensure_contrast(c: Hsla, against: Hsla, ratio: f32) -> Hsla {
    if contrast_ratio(c, against) >= ratio {
        return c;
    }
    let mut p = 1.0;
    let mut out = c;
    while p > 0.2 {
        p -= 0.01;
        out = oklab_mix_black(c, p);
        if contrast_ratio(out, against) >= ratio {
            break;
        }
    }
    out
}

/// A fill that keeps white text at AA (4.5:1): the colour darkened just
/// enough (a filled button or badge in the status colours).
pub fn readable_fill(fill: Hsla, text: Hsla) -> Hsla {
    // Darkening only helps light text on a darker fill.
    if fill.a < 0.999 || relative_luminance(text) < relative_luminance(fill) {
        return fill;
    }
    ensure_contrast(fill, text, 4.5)
}

/// A status pill's `(fill, text)`: white on the colour darkened to AA,
/// unless that would turn it muddy (amber), where black text reads better.
pub fn readable_pill(fill: Hsla) -> (Hsla, Hsla) {
    let (w, b) = (hex(0xffffff), hex(0x000000));
    if fill.a < 0.999 || contrast_ratio(w, fill) >= 4.5 {
        return (fill, w);
    }
    let darker = ensure_contrast(fill, w, 4.5);
    let (o, d): (Rgba, Rgba) = (fill.into(), darker.into());
    // How far the darkening moved the colour (max channel drop).
    let shift = (o.r - d.r).max(o.g - d.g).max(o.b - d.b);
    if shift > 0.3 && contrast_ratio(b, fill) >= 4.5 { (fill, b) } else { (darker, w) }
}

// ── OS contrast settings ────────────────────────────────────────────────

static MORE_CONTRAST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static SYSTEM_COLORS: std::sync::Mutex<Option<SystemColors>> = std::sync::Mutex::new(None);

/// Whether the OS asks for more contrast (macOS Increase contrast, GNOME
/// High contrast, `LOOKOUT_CONTRAST=more`). Set by
/// `platform::a11y_settings`; [`Theme::base`] reads it.
pub fn more_contrast() -> bool {
    MORE_CONTRAST.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn set_more_contrast(on: bool) {
    MORE_CONTRAST.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A Windows contrast theme's colours, while one is on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SystemColors {
    pub window: Hsla,
    pub window_text: Hsla,
    pub highlight: Hsla,
    pub highlight_text: Hsla,
    pub gray_text: Hsla,
}

/// The active Windows contrast theme's colours (set by
/// `platform::a11y_settings`).
pub fn system_contrast_colors() -> Option<SystemColors> {
    *SYSTEM_COLORS.lock().unwrap_or_else(|e| e.into_inner())
}
pub fn set_system_contrast_colors(c: Option<SystemColors>) {
    *SYSTEM_COLORS.lock().unwrap_or_else(|e| e.into_inner()) = c;
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

    /// Every text/background pair a screen draws text on, for one theme.
    fn text_backgrounds(t: &Theme) -> Vec<(&'static str, Hsla)> {
        vec![
            ("body", t.bg_body),
            ("panel", t.bg_panel),
            ("popover", t.popover_bg),
            ("card", over(t.bg_surface, t.bg_body)),
            ("card on panel", over(t.bg_surface, t.bg_panel)),
        ]
    }

    fn all_themes() -> Vec<(&'static str, Theme)> {
        vec![
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("dark+contrast", Theme::dark().with_more_contrast()),
            ("light+contrast", Theme::light().with_more_contrast()),
        ]
    }

    #[test]
    fn body_text_meets_wcag_aa() {
        for (name, t) in all_themes() {
            for (bg_name, bg) in text_backgrounds(&t) {
                for (fg_name, fg, min) in [
                    ("primary", t.text_primary, 4.5),
                    ("secondary", t.text_secondary, 4.5),
                    ("tertiary", t.text_tertiary, 4.5),
                    ("error", t.text_error, 4.5),
                ] {
                    let r = contrast_ratio(fg, bg);
                    assert!(r >= min, "{name}: {fg_name} on {bg_name} is {r:.2}:1, needs {min}:1");
                }
            }
        }
    }

    #[test]
    fn more_contrast_raises_secondary_text_to_aaa() {
        for (name, t) in all_themes().into_iter().filter(|(_, t)| t.high_contrast) {
            for (bg_name, bg) in text_backgrounds(&t) {
                for (fg_name, fg) in [("secondary", t.text_secondary), ("tertiary", t.text_tertiary)] {
                    let r = contrast_ratio(fg, bg);
                    assert!(r >= 7.0, "{name}: {fg_name} on {bg_name} is {r:.2}:1");
                }
            }
        }
    }

    #[test]
    fn focus_ring_and_controls_meet_3_to_1() {
        for (name, t) in all_themes() {
            for bg in [t.bg_body, t.bg_panel, t.popover_bg] {
                let r = contrast_ratio(t.focus_ring, bg);
                assert!(r >= 3.0, "{name}: focus ring {r:.2}:1");
            }
            if t.high_contrast {
                for (b, label) in [(t.border_default, "border"), (t.popover_border, "popover border"), (t.well_border, "well border")] {
                    let r = contrast_ratio(b, t.bg_body);
                    assert!(r >= 3.0, "{name}: {label} {r:.2}:1");
                }
            }
        }
    }

    #[test]
    fn filled_buttons_and_badges_keep_labels_legible() {
        for (name, t) in all_themes() {
            let fill = readable_fill(t.accent, t.on_accent);
            assert!(contrast_ratio(t.on_accent, fill) >= 4.5, "{name}: primary button");
            let hover = readable_fill(t.accent_hover, t.on_accent);
            assert!(contrast_ratio(t.on_accent, hover) >= 4.5, "{name}: primary hover");
            let badge_bg = over(t.badge_primary_bg, t.bg_body);
            assert!(contrast_ratio(t.badge_primary_text, badge_bg) >= 4.5, "{name}: primary badge");
        }
        for c in [status::success(), status::info(), status::warning(), status::danger(), hex(0x000000)] {
            let (fill, text) = readable_pill(c);
            assert!(contrast_ratio(text, fill) >= 4.5, "pill {c:?}");
            assert!(contrast_ratio(hex(0xffffff), readable_fill(c, hex(0xffffff))) >= 4.5 || c == status::warning());
        }
        // GNOME's accent colours (libadwaita 1.6) as Linux hands them over.
        for accent in [0x3584e4, 0x2190a4, 0x3a944a, 0xc88800, 0xed5b00, 0xe62d42, 0xd56199, 0x9141ac, 0x6f8396] {
            let t = Theme::light().with_accent(Some(hex(accent)));
            let fill = readable_fill(t.accent, t.on_accent);
            assert!(contrast_ratio(t.on_accent, fill) >= 4.5, "accent {accent:06x}");
        }
    }

    #[test]
    fn contrast_ratio_matches_wcag_examples() {
        assert!((contrast_ratio(hex(0x000000), hex(0xffffff)) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(hex(0x777777), hex(0xffffff)) - 4.48).abs() < 0.02);
        // Translucent text is composited first.
        assert!((contrast_ratio(black(0.0), hex(0xffffff)) - 1.0).abs() < 0.001);
    }

    #[test]
    fn windows_contrast_themes_use_system_colours() {
        // "Night sky" and "Desert".
        let night = SystemColors {
            window: hex(0x000000),
            window_text: hex(0xffffff),
            highlight: hex(0xd6b4fd),
            highlight_text: hex(0x2b2b2b),
            gray_text: hex(0xa6a6a6),
        };
        let t = Theme::from_system_colors(&night);
        assert!(t.dark && t.high_contrast);
        assert_eq!(t.text_secondary, night.window_text);
        assert_eq!(t.accent, night.highlight);
        assert_eq!(t.on_accent, night.highlight_text);
        let desert = SystemColors {
            window: hex(0xfffaef),
            window_text: hex(0x3d3d3d),
            highlight: hex(0x903909),
            highlight_text: hex(0xfff5e3),
            gray_text: hex(0x676767),
        };
        assert!(!Theme::from_system_colors(&desert).dark);
    }
}
