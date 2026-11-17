//! The OS display-accessibility settings besides reduced motion (which
//! `reduce_motion` handles): more contrast, reduced transparency and, on
//! macOS, Full Keyboard Access.
//!
//! | | macOS | Windows | Linux (GNOME) |
//! |---|---|---|---|
//! | more contrast | Increase contrast | a contrast theme (`SPI_GETHIGHCONTRAST`), drawn in its system colours | `a11y.interface high-contrast` |
//! | less transparency | Reduce transparency | Transparency effects off | — |
//! | buttons in Tab order | Keyboard navigation | always | always |
//!
//! Polled every 2s on a background thread, like `reduce_motion`; a
//! foreground task rebuilds the theme and redraws when something changed.
//! `LOOKOUT_CONTRAST=more|normal` and `LOOKOUT_TRANSPARENCY=reduced|normal`
//! override the OS for testing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::gpui::App;

use crate::theme::SystemColors;

const POLL: Duration = Duration::from_secs(2);

static REDUCE_TRANSPARENCY: AtomicBool = AtomicBool::new(false);
static CHANGED: AtomicBool = AtomicBool::new(false);

/// Whether the OS asks for less transparency: system materials (macOS
/// vibrancy, Mica) are left off and pages paint opaque backgrounds.
pub fn reduce_transparency() -> bool {
    REDUCE_TRANSPARENCY.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Settings {
    more_contrast: bool,
    system_colors: Option<SystemColors>,
    reduce_transparency: bool,
    full_keyboard_access: Option<bool>,
}

fn env_flag(name: &str, on: &str, off: &str) -> Option<bool> {
    match std::env::var(name).as_deref() {
        Ok(v) if v == on => Some(true),
        Ok(v) if v == off => Some(false),
        _ => None,
    }
}

fn read() -> Settings {
    let mut s = imp::read();
    if let Some(on) = env_flag("LOOKOUT_CONTRAST", "more", "normal") {
        s.more_contrast = on;
        if !on {
            s.system_colors = None;
        }
    }
    if let Some(on) = env_flag("LOOKOUT_TRANSPARENCY", "reduced", "normal") {
        s.reduce_transparency = on;
    }
    s
}

fn apply(s: Settings) {
    let before = (crate::theme::more_contrast(), crate::theme::system_contrast_colors(), reduce_transparency());
    crate::theme::set_more_contrast(s.more_contrast);
    crate::theme::set_system_contrast_colors(s.system_colors);
    REDUCE_TRANSPARENCY.store(s.reduce_transparency, Ordering::Relaxed);
    crate::ui::focus::set_full_keyboard_access(s.full_keyboard_access);
    let after = (s.more_contrast, s.system_colors, s.reduce_transparency);
    if before != after {
        log::info!(
            "[a11y] contrast {}, transparency {}",
            if s.system_colors.is_some() {
                "system contrast theme"
            } else if s.more_contrast {
                "more"
            } else {
                "normal"
            },
            if s.reduce_transparency { "reduced" } else { "normal" }
        );
        CHANGED.store(true, Ordering::Relaxed);
    }
}

/// Read the settings now (so the first frame uses them) and keep watching.
/// Call once, before windows open.
pub fn observe() {
    apply(read());
    CHANGED.store(false, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("a11y-settings".into()).spawn(|| loop {
        std::thread::sleep(POLL);
        apply(read());
    });
}

/// Rebuild the theme and redraw every window when a setting flips while
/// Lookout is running. Call once from the app's startup.
pub fn observe_foreground(cx: &mut App) {
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(Duration::from_millis(500)).await;
        if !CHANGED.swap(false, Ordering::Relaxed) {
            continue;
        }
        cx.update(|cx| {
            if cx.has_global::<crate::theme::Theme>() {
                let t = crate::theme::theme(cx).rebuilt();
                cx.set_global(t);
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Settings;

    #[allow(unused_unsafe)]
    pub fn read() -> Settings {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::{NSString, NSUserDefaults};
        let ws = NSWorkspace::sharedWorkspace();
        // "Keyboard navigation" (System Settings → Keyboard) sets bit 1 of
        // AppleKeyboardUIMode; NSApplication.isFullKeyboardAccessEnabled
        // reads the same key but wants the main thread.
        let defaults = unsafe { NSUserDefaults::standardUserDefaults() };
        let mode = unsafe { defaults.integerForKey(&NSString::from_str("AppleKeyboardUIMode")) };
        Settings {
            more_contrast: ws.accessibilityDisplayShouldIncreaseContrast(),
            system_colors: None,
            reduce_transparency: ws.accessibilityDisplayShouldReduceTransparency(),
            full_keyboard_access: Some(mode & 2 != 0),
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::Settings;
    use crate::theme::SystemColors;
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    /// `HIGHCONTRASTW.dwFlags & HCF_HIGHCONTRASTON`, from the registry
    /// value SPI_GETHIGHCONTRAST reports (`Control Panel\Accessibility\
    /// HighContrast\Flags`, a decimal string).
    fn high_contrast_on(hk: &RegKey) -> bool {
        hk.open_subkey(r"Control Panel\Accessibility\HighContrast")
            .and_then(|k| k.get_value::<String, _>("Flags"))
            .ok()
            .and_then(|f| f.trim().parse::<u32>().ok())
            .is_some_and(|f| f & 1 != 0)
    }

    fn sys(index: windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX) -> gpui_kit::gpui::Hsla {
        // SAFETY: GetSysColor only reads a colour table entry.
        let c = unsafe { windows::Win32::Graphics::Gdi::GetSysColor(index) };
        crate::theme::rgba8((c & 0xff) as u8, ((c >> 8) & 0xff) as u8, ((c >> 16) & 0xff) as u8, 1.0).into()
    }

    pub fn read() -> Settings {
        use windows::Win32::Graphics::Gdi::{COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT};
        let hk = RegKey::predef(HKEY_CURRENT_USER);
        let hc = high_contrast_on(&hk);
        let transparency = hk
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            .and_then(|k| k.get_value::<u32, _>("EnableTransparency"))
            .unwrap_or(1);
        Settings {
            more_contrast: hc,
            system_colors: hc.then(|| SystemColors {
                window: sys(COLOR_WINDOW),
                window_text: sys(COLOR_WINDOWTEXT),
                highlight: sys(COLOR_HIGHLIGHT),
                highlight_text: sys(COLOR_HIGHLIGHTTEXT),
                gray_text: sys(COLOR_GRAYTEXT),
            }),
            // A contrast theme turns transparency off as well.
            reduce_transparency: transparency == 0 || hc,
            full_keyboard_access: None,
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Settings;

    fn gsettings(schema: &str, key: &str) -> Option<String> {
        let out = std::process::Command::new("gsettings")
            .args(["get", schema, key])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn read() -> Settings {
        // GNOME 42+ keeps it in its own key; older sessions switched the
        // GTK theme to HighContrast(Inverse).
        let hc = match gsettings("org.gnome.desktop.a11y.interface", "high-contrast").as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => gsettings("org.gnome.desktop.interface", "gtk-theme").is_some_and(|t| t.contains("HighContrast")),
        };
        Settings { more_contrast: hc, system_colors: None, reduce_transparency: false, full_keyboard_access: None }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    pub fn read() -> super::Settings {
        super::Settings::default()
    }
}
