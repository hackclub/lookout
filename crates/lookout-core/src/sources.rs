//! Capture-source enumeration: monitors and windows, plus the on-screen
//! window geometry the redaction pass in [`crate::capture`] keys off.

use serde::Serialize;

#[cfg(target_os = "macos")]
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFNumberType, CFString, CGRect};
#[cfg(target_os = "macos")]
use objc2_core_graphics::{
    CGDataProvider, CGImage, CGRectMakeWithDictionaryRepresentation, CGWindowImageOption,
    CGWindowListCopyWindowInfo, CGWindowListCreateImage, CGWindowListOption,
};
#[cfg(target_os = "macos")]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use std::sync::{Mutex, OnceLock};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct CapturableWindowCacheEntry {
    is_capturable: bool,
    checked_at: Instant,
}

#[cfg(target_os = "macos")]
static CAPTURABLE_WINDOW_CACHE: OnceLock<Mutex<HashMap<u32, CapturableWindowCacheEntry>>> =
    OnceLock::new();

#[cfg(target_os = "macos")]
const CAPTURABLE_WINDOW_CACHE_TTL: Duration = Duration::from_secs(15);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub is_builtin: bool,
    pub scale_factor: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: u32,
    pub app_name: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub is_minimized: bool,
    pub is_focused: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceList {
    pub monitors: Vec<MonitorInfo>,
    pub windows: Vec<WindowInfo>,
}

/// Info about an on-screen window, including its bounds for redaction.
pub(crate) struct OnScreenWindowRect {
    pub(crate) app_name: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// List all on-screen windows (current space only) with their bounds.
/// Used for blacking out blacklisted app windows in monitor captures.
/// Filters out system chrome (Dock, menu bar, etc.) and tiny windows.
#[cfg(target_os = "macos")]
pub(crate) fn list_onscreen_window_rects() -> Vec<OnScreenWindowRect> {
    let Some(entries) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        0,
    ) else {
        return Vec::new();
    };

    let mut rects = Vec::new();

    for i in 0..entries.count() {
        let dict_ref = unsafe { entries.value_at_index(i) } as *const CFDictionary;
        if dict_ref.is_null() {
            continue;
        }
        let dict = unsafe { &*dict_ref };

        let app_name = dict_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if app_name.is_empty() {
            continue;
        }

        let title = dict_string(dict, "kCGWindowName").unwrap_or_default();

        // Skip system chrome — these span the screen and would mask everything
        if should_exclude_window(&app_name, &title) {
            continue;
        }

        let Some(bounds) = window_bounds(dict) else {
            continue;
        };

        // Skip tiny windows (status bar items, badges, etc.)
        if bounds.size.width < 50.0 || bounds.size.height < 50.0 {
            continue;
        }

        // Only include windows that are on-screen
        let is_on_screen = dict_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
        if !is_on_screen {
            continue;
        }

        // Skip fully transparent windows. They draw nothing, so blacking
        // one out would hide whatever is underneath, and counting one as
        // "in front" would let a filtered window show through it.
        if dict_f64(dict, "kCGWindowAlpha").is_some_and(|a| a <= 0.0) {
            continue;
        }

        rects.push(OnScreenWindowRect {
            app_name,
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
        });
    }

    rects
}

/// List all visible windows with their bounds (Windows/Linux).
/// Uses xcap::Window::all() which returns windows in z-order (front-to-back).
/// On Linux/Wayland without XWayland this will return an empty list.
#[cfg(not(target_os = "macos"))]
pub(crate) fn list_onscreen_window_rects() -> Vec<OnScreenWindowRect> {
    use xcap::Window;
    let windows = match Window::all() {
        Ok(w) => w,
        Err(_) => return Vec::new(),
    };

    let mut rects = Vec::new();
    for w in windows {
        let app_name = w.app_name().unwrap_or_default();
        if app_name.is_empty() || app_name == "Lookout" {
            continue;
        }
        let title = w.title().unwrap_or_default();
        if should_exclude_window(&app_name, &title) {
            continue;
        }
        if w.is_minimized().unwrap_or(false) {
            continue;
        }
        #[cfg(target_os = "windows")]
        if w.id().is_ok_and(is_click_through_overlay) {
            continue;
        }
        let width = w.width().unwrap_or(0) as f64;
        let height = w.height().unwrap_or(0) as f64;
        if width < 50.0 || height < 50.0 {
            continue;
        }
        let x = w.x().unwrap_or(0) as f64;
        let y = w.y().unwrap_or(0) as f64;
        rects.push(OnScreenWindowRect {
            app_name,
            x,
            y,
            width,
            height,
        });
    }
    rects
}

/// True for a click-through overlay window (`WS_EX_LAYERED` +
/// `WS_EX_TRANSPARENT`): the full-screen, see-through windows that in-game
/// overlays (Discord, Steam, NVIDIA, ...) float over a game. They paint
/// almost nothing, so redaction treats them as absent: blacking out a
/// filtered app's overlay would black out the game under it, and an
/// unfiltered app's overlay must not shield a filtered window beneath it.
#[cfg(target_os = "windows")]
fn is_click_through_overlay(window_id: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };
    // xcap's window id is the HWND.
    let hwnd = HWND(window_id as usize as *mut _);
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let mask = WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0;
    ex_style & mask == mask
}

pub(crate) fn should_exclude_window(app_name: &str, title: &str) -> bool {
    let app_name_lower = app_name.to_ascii_lowercase();
    let title_lower = title.to_ascii_lowercase();

    const EXCLUDED_APP_NAMES: &[&str] = &[
        "dock",
        "control centre",
        "control center",
        "notification centre",
        "notification center",
        "window server",
        "systemuiserver",
        "spotlight",
        "loginwindow",
        "finder",
        "screencapture",
        "screenshot",
        "windows explorer",
        "raycast",
    ];

    const EXCLUDED_TITLES: &[&str] = &["statusindicator", "item-0", "item-1"];

    EXCLUDED_APP_NAMES
        .iter()
        .any(|excluded| app_name_lower == *excluded)
        || EXCLUDED_TITLES
            .iter()
            .any(|excluded| title_lower == *excluded)
}

#[cfg(target_os = "macos")]
pub(crate) fn get_cf_dictionary_get_value(cf_dictionary: &CFDictionary, key: &str) -> Option<*const c_void> {
    let cf_key = CFString::from_str(key);
    let cf_key_ref = cf_key.as_ref() as *const CFString;
    let value = unsafe { cf_dictionary.value(cf_key_ref.cast()) };
    if value.is_null() {
        return None;
    }
    Some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn dict_i32(dict: &CFDictionary, key: &str) -> Option<i32> {
    let cf_number = get_cf_dictionary_get_value(dict, key)? as *const CFNumber;
    let mut value: i32 = 0;
    let ok =
        unsafe { (*cf_number).value(CFNumberType::IntType, &mut value as *mut _ as *mut c_void) };
    if !ok {
        return None;
    }
    Some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn dict_f64(dict: &CFDictionary, key: &str) -> Option<f64> {
    let cf_number = get_cf_dictionary_get_value(dict, key)? as *const CFNumber;
    let mut value: f64 = 0.0;
    let ok = unsafe {
        (*cf_number).value(CFNumberType::Float64Type, &mut value as *mut _ as *mut c_void)
    };
    if !ok {
        return None;
    }
    Some(value)
}

#[cfg(target_os = "macos")]
pub(crate) fn dict_string(dict: &CFDictionary, key: &str) -> Option<String> {
    let value_ref = get_cf_dictionary_get_value(dict, key)? as *const CFString;
    Some(unsafe { (*value_ref).to_string() })
}

#[cfg(target_os = "macos")]
pub(crate) fn dict_bool(dict: &CFDictionary, key: &str) -> Option<bool> {
    let value_ref = get_cf_dictionary_get_value(dict, key)? as *const CFBoolean;
    Some(unsafe { (*value_ref).value() })
}

#[cfg(target_os = "macos")]
pub(crate) fn window_bounds(dict: &CFDictionary) -> Option<CGRect> {
    let value_ref = get_cf_dictionary_get_value(dict, "kCGWindowBounds")? as *const CFDictionary;
    let mut rect = CGRect::default();
    let ok = unsafe { CGRectMakeWithDictionaryRepresentation(Some(&*value_ref), &mut rect) };
    if !ok {
        return None;
    }
    Some(rect)
}

#[cfg(target_os = "macos")]
fn window_is_capturable(window_id: u32, bounds: CGRect) -> bool {
    let cache = CAPTURABLE_WINDOW_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let now = Instant::now();

    if let Ok(cache_guard) = cache.lock() {
        if let Some(entry) = cache_guard.get(&window_id) {
            if now.duration_since(entry.checked_at) <= CAPTURABLE_WINDOW_CACHE_TTL {
                return entry.is_capturable;
            }
        }
    }

    let image = CGWindowListCreateImage(
        bounds,
        CGWindowListOption::OptionIncludingWindow,
        window_id,
        CGWindowImageOption::Default,
    );

    let Some(image) = image else {
        if let Ok(mut cache_guard) = cache.lock() {
            cache_guard.insert(
                window_id,
                CapturableWindowCacheEntry {
                    is_capturable: false,
                    checked_at: now,
                },
            );
        }
        return false;
    };

    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    let bytes_per_row = CGImage::bytes_per_row(Some(&image));
    let data_provider = CGImage::data_provider(Some(&image));
    let data = CGDataProvider::data(data_provider.as_deref());
    let is_capturable = width > 0
        && height > 0
        && bytes_per_row >= width * 4
        && data.as_ref().is_some_and(|bytes| !bytes.is_empty());

    if let Ok(mut cache_guard) = cache.lock() {
        cache_guard
            .retain(|_, entry| now.duration_since(entry.checked_at) <= CAPTURABLE_WINDOW_CACHE_TTL);
        cache_guard.insert(
            window_id,
            CapturableWindowCacheEntry {
                is_capturable,
                checked_at: now,
            },
        );
    }

    is_capturable
}

#[cfg(target_os = "macos")]
fn list_macos_windows_any_space() -> Vec<WindowInfo> {
    let Some(entries) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements,
        0,
    ) else {
        return Vec::new();
    };

    let mut windows = Vec::new();

    for i in 0..entries.count() {
        let dict_ref = unsafe { entries.value_at_index(i) } as *const CFDictionary;
        if dict_ref.is_null() {
            continue;
        }
        let dict = unsafe { &*dict_ref };

        let Some(id) = dict_i32(dict, "kCGWindowNumber") else {
            continue;
        };
        let Some(sharing_state) = dict_i32(dict, "kCGWindowSharingState") else {
            continue;
        };
        if sharing_state == 0 {
            continue;
        }

        let app_name = dict_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        let title = dict_string(dict, "kCGWindowName").unwrap_or_default();
        let Some(bounds) = window_bounds(dict) else {
            continue;
        };
        let width = bounds.size.width;
        let height = bounds.size.height;

        if should_exclude_window(&app_name, &title) {
            continue;
        }
        if width < 50.0 || height < 50.0 {
            continue;
        }
        if title.is_empty() && app_name.is_empty() {
            continue;
        }
        if app_name == "Lookout" {
            continue;
        }
        if !window_is_capturable(id as u32, bounds) {
            continue;
        }

        let is_on_screen = dict_bool(dict, "kCGWindowIsOnscreen").unwrap_or(true);
        windows.push(WindowInfo {
            id: id as u32,
            app_name,
            title,
            width: width as u32,
            height: height as u32,
            is_minimized: !is_on_screen,
            is_focused: false,
        });
    }

    windows
}

/// List available capture sources (monitors + windows).
pub fn list_capture_sources() -> Result<CaptureSourceList, String> {
    // On Wayland (no X11), xcap cannot enumerate sources.
    // Return an empty list so the frontend falls through to the portal/Cast flow.
    #[cfg(target_os = "linux")]
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        return Ok(CaptureSourceList {
            monitors: Vec::new(),
            windows: Vec::new(),
        });
    }

    use xcap::Monitor;
    #[cfg(not(target_os = "macos"))]
    use xcap::Window;

    let monitors: Vec<MonitorInfo> = Monitor::all()
        .map_err(|e| format!("Failed to list monitors: {e}"))?
        .into_iter()
        .filter_map(|m| {
            Some(MonitorInfo {
                id: m.id().ok()?,
                name: m.friendly_name().or_else(|_| m.name()).unwrap_or_default(),
                width: m.width().ok()?,
                height: m.height().ok()?,
                is_primary: m.is_primary().unwrap_or(false),
                is_builtin: m.is_builtin().unwrap_or(false),
                scale_factor: m.scale_factor().unwrap_or(1.0),
            })
        })
        .collect();

    // Window enumeration can fail on some platforms — treat as empty list, not error
    #[cfg(target_os = "macos")]
    let windows: Vec<WindowInfo> = list_macos_windows_any_space();

    #[cfg(not(target_os = "macos"))]
    let windows: Vec<WindowInfo> = Window::all()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|w| {
            let title = w.title().ok().unwrap_or_default();
            let app_name = w.app_name().ok().unwrap_or_default();
            let width = w.width().ok()?;
            let height = w.height().ok()?;

            if should_exclude_window(&app_name, &title) {
                return None;
            }

            // Filter out tiny/invisible windows and our own app
            if width < 50 || height < 50 {
                return None;
            }
            if title.is_empty() && app_name.is_empty() {
                return None;
            }
            if app_name == "Lookout" {
                return None;
            }
            Some(WindowInfo {
                id: w.id().ok()?,
                app_name,
                title,
                width,
                height,
                is_minimized: w.is_minimized().unwrap_or(false),
                is_focused: w.is_focused().unwrap_or(false),
            })
        })
        .collect();

    Ok(CaptureSourceList { monitors, windows })
}
