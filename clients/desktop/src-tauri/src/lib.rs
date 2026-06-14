mod capture;
mod crop;
mod pipewire;
mod screencast;
mod tray;
#[cfg(target_os = "windows")]
mod windows_permissions;

#[cfg(target_os = "macos")]
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFNumberType, CFString, CGRect};
#[cfg(target_os = "macos")]
use objc2_core_graphics::{
    CGDataProvider, CGImage, CGRectMakeWithDictionaryRepresentation, CGWindowImageOption,
    CGWindowListCopyWindowInfo, CGWindowListCreateImage, CGWindowListOption,
};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};
use std::time::Instant as StdInstant;
use tauri::http;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;
use tokio::sync::watch;

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

/// Handle for cancelling the Rust-side capture loop.
struct CaptureLoopHandle {
    cancel_tx: watch::Sender<bool>,
    join_handle: tokio::task::JoinHandle<()>,
}

/// Shared state for the Rust-side tray title timer.
/// Uses atomics so the capture loop can update tracked_seconds
/// without acquiring a mutex on every tick.
struct TrayTimerState {
    /// Authoritative tracked seconds from the last server response.
    tracked_seconds: AtomicI64,
    /// Wall-clock instant when tracking started (or last synced).
    started_at: Mutex<StdInstant>,
    /// Whether the timer is actively ticking (false = paused).
    is_running: AtomicBool,
}

/// Handle for the tray title ticker task.
struct TrayTimerHandle {
    state: Arc<TrayTimerState>,
    cancel_tx: watch::Sender<bool>,
    join_handle: tokio::task::JoinHandle<()>,
}

/// App state shared across commands.
pub struct AppState {
    pub config: Mutex<Option<SessionConfig>>,
    pub cold_start_urls: Mutex<Option<Vec<String>>>,
    /// Maps PipeWire node_id -> the RawFd of the screencast session that owns it.
    /// This allows streams from different portal sessions to coexist (e.g. when
    /// the user incrementally adds sources via the "+" button).
    #[cfg(target_os = "linux")]
    pub pipewire_fds: Mutex<std::collections::HashMap<u32, std::os::fd::RawFd>>,
    /// App names whose windows should be blacked out in monitor captures.
    pub blacklisted_apps: Mutex<Vec<String>>,
    /// Active Rust-side capture loop (if running). Holds the cancel channel
    /// and JoinHandle so we can stop it from `stop_capture_loop`.
    capture_loop: Mutex<Option<CaptureLoopHandle>>,
    /// Rust-side 1s tray title ticker — keeps the menu bar time accurate
    /// even when the WebView's JS timers are throttled.
    tray_timer: Mutex<Option<TrayTimerHandle>>,
}

/// Central deep link handler. All deep link entry points (cold start, single
/// instance, macOS Apple Events) route through here. Stashes URLs for
/// cold-start polling AND emits them for the warm-start JS listener.
fn handle_deep_link_urls(app: &AppHandle, urls: Vec<String>) {
    if urls.is_empty() {
        return;
    }
    eprintln!("[deep-link] handling urls: {urls:?}");

    // Stash for cold-start polling (get_cold_start_urls command)
    if let Ok(mut state) = app.state::<AppState>().cold_start_urls.lock() {
        *state = Some(urls.clone());
    }

    // Emit for warm-start JS listener (onOpenUrl)
    let parsed: Vec<url::Url> = urls
        .iter()
        .filter_map(|u| u.parse::<url::Url>().ok())
        .collect();
    if !parsed.is_empty() {
        let _ = app.emit("lookout-deep-link", parsed);
    }

    // Focus the window
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_focus();
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub token: String,
    pub api_base_url: String,
}

#[derive(Serialize)]
pub struct CaptureResult {
    /// Base64-encoded JPEG bytes
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: usize,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CaptureSource {
    #[serde(rename = "monitor")]
    Monitor { id: u32 },
    #[serde(rename = "window")]
    Window { id: u32 },
    #[serde(rename = "pipewire")]
    PipeWire { id: u32 },
}

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
struct OnScreenWindowRect {
    app_name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// List all on-screen windows (current space only) with their bounds.
/// Used for blacking out blacklisted app windows in monitor captures.
/// Filters out system chrome (Dock, menu bar, etc.) and tiny windows.
#[cfg(target_os = "macos")]
fn list_onscreen_window_rects() -> Vec<OnScreenWindowRect> {
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
fn list_onscreen_window_rects() -> Vec<OnScreenWindowRect> {
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

fn should_exclude_window(app_name: &str, title: &str) -> bool {
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
fn get_cf_dictionary_get_value(cf_dictionary: &CFDictionary, key: &str) -> Option<*const c_void> {
    let cf_key = CFString::from_str(key);
    let cf_key_ref = cf_key.as_ref() as *const CFString;
    let value = unsafe { cf_dictionary.value(cf_key_ref.cast()) };
    if value.is_null() {
        return None;
    }
    Some(value)
}

#[cfg(target_os = "macos")]
fn dict_i32(dict: &CFDictionary, key: &str) -> Option<i32> {
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
fn dict_string(dict: &CFDictionary, key: &str) -> Option<String> {
    let value_ref = get_cf_dictionary_get_value(dict, key)? as *const CFString;
    Some(unsafe { (*value_ref).to_string() })
}

#[cfg(target_os = "macos")]
fn dict_bool(dict: &CFDictionary, key: &str) -> Option<bool> {
    let value_ref = get_cf_dictionary_get_value(dict, key)? as *const CFBoolean;
    Some(unsafe { (*value_ref).value() })
}

#[cfg(target_os = "macos")]
fn window_bounds(dict: &CFDictionary) -> Option<CGRect> {
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

// Response structs use `#[serde(default)]` on new fields so older servers
// that don't include them still deserialize cleanly (no `deny_unknown_fields`
// either — keeps forward-compat for any future additions).
#[derive(Serialize, Deserialize)]
pub struct UploadUrlResponse {
    #[serde(rename = "uploadUrl")]
    pub upload_url: String,
    #[serde(rename = "r2Key")]
    pub r2_key: String,
    #[serde(rename = "screenshotId")]
    pub screenshot_id: String,
    #[serde(rename = "minuteBucket")]
    pub minute_bucket: i32,
    #[serde(rename = "nextExpectedAt")]
    pub next_expected_at: String,
    /// Server wall-clock at response time. Absent on pre-credit-mode servers.
    #[serde(rename = "serverTime", default)]
    pub server_time: Option<String>,
    /// Sticky tracking mode for the session. Absent on pre-credit-mode servers.
    #[serde(rename = "trackingMode", default)]
    pub tracking_mode: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ConfirmResponse {
    pub confirmed: bool,
    #[serde(rename = "trackedSeconds")]
    pub tracked_seconds: i64,
    #[serde(rename = "nextExpectedAt")]
    pub next_expected_at: String,
    #[serde(rename = "serverTime", default)]
    pub server_time: Option<String>,
}

/// Result returned to the frontend from capture_and_upload.
/// Includes the server confirm data AND the screenshot preview.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureUploadResult {
    pub confirmed: bool,
    pub tracked_seconds: i64,
    pub next_expected_at: String,
    /// Base64-encoded JPEG of the captured frame (same image that was uploaded)
    pub preview_base64: String,
    pub preview_width: u32,
    pub preview_height: u32,
}

/// Return the deep link URLs from cold start (if any), then clear them.
#[tauri::command]
fn get_cold_start_urls(state: State<'_, AppState>) -> Vec<String> {
    let mut urls = state
        .cold_start_urls
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    urls.take().unwrap_or_default()
}

/// Set the list of blacklisted app names (replaces current list).
#[tauri::command]
fn set_blacklisted_apps(apps: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    let mut blacklist = state.blacklisted_apps.lock().map_err(|e| e.to_string())?;
    *blacklist = apps;
    Ok(())
}

/// Get the current list of blacklisted app names.
#[tauri::command]
fn get_blacklisted_apps(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let blacklist = state.blacklisted_apps.lock().map_err(|e| e.to_string())?;
    Ok(blacklist.clone())
}

/// List unique app names from all running windows (across all spaces).
/// Returns a sorted, deduplicated list of app names.
#[tauri::command]
fn list_running_apps() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        let Some(entries) = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements,
            0,
        ) else {
            return Vec::new();
        };

        let mut apps = std::collections::BTreeSet::new();
        for i in 0..entries.count() {
            let dict_ref = unsafe { entries.value_at_index(i) } as *const CFDictionary;
            if dict_ref.is_null() {
                continue;
            }
            let dict = unsafe { &*dict_ref };
            let app_name = dict_string(dict, "kCGWindowOwnerName").unwrap_or_default();
            if app_name.is_empty() || app_name == "Lookout" {
                continue;
            }
            let title = dict_string(dict, "kCGWindowName").unwrap_or_default();
            if should_exclude_window(&app_name, &title) {
                continue;
            }
            apps.insert(app_name);
        }
        apps.into_iter().collect()
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On non-macOS, return window app names from xcap
        use xcap::Window;
        let mut apps = std::collections::BTreeSet::new();
        if let Ok(windows) = Window::all() {
            for w in windows {
                if let Ok(name) = w.app_name() {
                    if !name.is_empty() && name != "Lookout" && !should_exclude_window(&name, &w.title().unwrap_or_default()) {
                        apps.insert(name);
                    }
                }
            }
        }
        apps.into_iter().collect()
    }
}

/// List available capture sources (monitors + windows).
#[tauri::command]
fn list_capture_sources() -> Result<CaptureSourceList, String> {
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

#[tauri::command]
fn enable_vibrancy(window: tauri::Window) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};
        use objc2_foundation::NSProcessInfo;

        let version = unsafe { NSProcessInfo::processInfo().operatingSystemVersion() };
        let radius = if version.majorVersion >= 26 { 16.0 } else { 10.0 };

        apply_vibrancy(
            &window,
            NSVisualEffectMaterial::Sidebar,
            Some(NSVisualEffectState::Active),
            Some(radius),
        )
        .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        use window_vibrancy::apply_mica;
        apply_mica(&window, None).map_err(|e| e.to_string())?;
    }
    // Prevent unused variable warning on Linux
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = window;

    Ok(())
}

#[tauri::command]
fn disable_vibrancy(window: tauri::Window) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::clear_vibrancy;
        clear_vibrancy(&window).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        use window_vibrancy::clear_mica;
        clear_mica(&window).map_err(|e| e.to_string())?;
    }
    // Prevent unused variable warning on Linux
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = window;

    Ok(())
}

#[tauri::command]
fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
}

#[tauri::command]
async fn request_screencast(
    #[allow(unused_variables)] state: State<'_, AppState>,
) -> Result<Vec<crate::screencast::StreamInfo>, String> {
    #[cfg(target_os = "linux")]
    {
        crate::screencast::request_screencast(state).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Screencast portal is only supported on Linux".into())
    }
}

#[tauri::command]
async fn add_screencast(
    #[allow(unused_variables)] state: State<'_, AppState>,
) -> Result<Vec<crate::screencast::StreamInfo>, String> {
    #[cfg(target_os = "linux")]
    {
        crate::screencast::add_screencast(state).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Screencast portal is only supported on Linux".into())
    }
}

/// Initialize the session config so Rust knows where the server is.
#[tauri::command]
fn configure(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    *config = Some(SessionConfig {
        token,
        api_base_url,
    });
    Ok(())
}

/// Take a native screenshot, encode as JPEG, return base64.
#[tauri::command]
fn take_screenshot(
    source: CaptureSource,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    #[allow(unused_variables)] state: State<'_, AppState>,
) -> Result<CaptureResult, String> {
    #[allow(unused_mut, unused_assignments)]
    let mut pipewire_fds = std::collections::HashMap::new();
    #[cfg(target_os = "linux")]
    if let Ok(guard) = state.pipewire_fds.lock() {
        pipewire_fds = guard.clone();
    }
    capture::take_screenshot(source, max_width, max_height, jpeg_quality, &pipewire_fds)
}

/// Free-form client telemetry string sent on every upload-url request, e.g.
/// "Lookout Desktop/0.2.6 (macOS 14.3)". Computed once. NOT the HTTP
/// User-Agent — explicit info for server-side telemetry/debugging.
fn client_info() -> &'static str {
    static CLIENT_INFO: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CLIENT_INFO.get_or_init(|| {
        let info = os_info::get();
        let os_type = match info.os_type() {
            os_info::Type::Macos => "macOS".to_string(),
            other => other.to_string(),
        };
        let version = info.version().to_string();
        let os = if version.is_empty() || version == "Unknown" {
            os_type
        } else {
            format!("{os_type} {version}")
        };
        format!("Lookout Desktop/{} ({os})", env!("CARGO_PKG_VERSION"))
    })
}

/// reqwest's `Display` is generic boilerplate ("error sending request for
/// url (...)") that gives zero debugging signal. The real reason — DNS
/// failure, connection refused, TLS handshake error, timeout, or an HTTP
/// status — is either in the `source()` chain or in `.status()`. Skip the
/// boilerplate and return just the signal: an HTTP code, or a short
/// category plus the innermost cause (e.g. "connection failed: Connection
/// refused (os error 61)").
///
/// This runs on the error path, so it must never make things worse: the
/// extraction is best-effort, and we always fall back to the raw error
/// string (never an empty or missing message) if it yields nothing useful.
fn describe_reqwest_error(err: &reqwest::Error) -> String {
    let signal = extract_reqwest_signal(err);
    if !signal.trim().is_empty() {
        return signal;
    }
    // Fallback: report the raw error, never nothing.
    let raw = err.to_string();
    if raw.trim().is_empty() {
        "unknown request error".to_string()
    } else {
        raw
    }
}

/// Compact, consistent message for an HTTP error response: the step that
/// failed, the status code (+ reason phrase, which `StatusCode` Displays),
/// and the response body when there is one. No filler.
fn http_error(step: &str, status: reqwest::StatusCode, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("{step} HTTP {status}")
    } else {
        format!("{step} HTTP {status}: {body}")
    }
}

/// Best-effort signal extraction for [`describe_reqwest_error`]. Returns an
/// empty string when nothing better than the raw boilerplate is available,
/// signalling the caller to fall back. Pure string work — never panics.
fn extract_reqwest_signal(err: &reqwest::Error) -> String {
    use std::error::Error;
    // Walk to the innermost cause — the most specific reason (OS error,
    // TLS detail, etc.).
    let mut deepest: Option<String> = None;
    let mut source = err.source();
    while let Some(cause) = source {
        let s = cause.to_string();
        if !s.trim().is_empty() {
            deepest = Some(s);
        }
        source = cause.source();
    }
    // Status-class error (from `error_for_status`): the code is the signal.
    if let Some(status) = err.status() {
        return match deepest {
            Some(d) => format!("HTTP {}: {d}", status.as_u16()),
            None => format!("HTTP {}", status.as_u16()),
        };
    }
    let (kind, known) = if err.is_timeout() {
        ("timed out", true)
    } else if err.is_connect() {
        ("connection failed", true)
    } else if err.is_decode() {
        ("malformed response", true)
    } else if err.is_body() {
        ("request body error", true)
    } else {
        ("request failed", false)
    };
    match deepest {
        Some(detail) => format!("{kind}: {detail}"),
        // A known category with no cause chain (e.g. a bare timeout) stands
        // on its own. Otherwise return empty so the caller falls back to the
        // raw error rather than the vague "request failed".
        None if known => kind.to_string(),
        None => String::new(),
    }
}

/// Shared upload-and-confirm pipeline: get presigned URL, PUT to R2, POST
/// confirmation. Used by both `capture_and_upload` (screen/window) and
/// `upload_frame` (camera).
///
/// `captured_at` is the ISO-8601 timestamp (in client clock) of when the
/// screenshot was actually taken. Optional — when `None`, the request
/// matches the legacy bucket-mode payload byte-for-byte. When `Some`, it
/// opts the session into credit-mode tracking on the first request.
async fn upload_and_confirm(
    jpeg_base64: &str,
    width: u32,
    height: u32,
    captured_at: Option<&str>,
    config: &SessionConfig,
    app: &AppHandle,
) -> Result<CaptureUploadResult, String> {
    let jpeg_bytes = base64_decode(jpeg_base64)?;
    let size_bytes = jpeg_bytes.len();

    // Step 1: Get presigned URL from server
    let _ = app.emit("capture-progress", "getting upload url from server...");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))?;
    let upload_url_url = format!(
        "{}/api/sessions/{}/upload-url",
        config.api_base_url, config.token
    );
    // Build query params; reqwest percent-encodes them correctly (replacing
    // the old hand-rolled capturedAt encoding). clientInfo is always sent.
    let mut query: Vec<(&str, &str)> = vec![("clientInfo", client_info())];
    if let Some(c) = captured_at {
        query.push(("capturedAt", c));
    }
    let url_response = client
        .get(upload_url_url)
        .query(&query)
        .send()
        .await
        .map_err(|e| format!("upload-url: {}", describe_reqwest_error(&e)))?;
    let url_status = url_response.status();
    if !url_status.is_success() {
        let body = url_response.text().await.unwrap_or_default();
        return Err(http_error("upload-url", url_status, &body));
    }
    let upload_url_resp: UploadUrlResponse = url_response
        .json()
        .await
        .map_err(|e| format!("upload-url: {}", describe_reqwest_error(&e)))?;
    let _ = app.emit(
        "capture-progress",
        format!(
            "got upload url, screenshot id: {}",
            upload_url_resp.screenshot_id
        ),
    );

    // Step 2: Upload JPEG to R2
    let _ = app.emit(
        "capture-progress",
        format!("uploading {}KB to R2...", size_bytes / 1024),
    );
    client
        .put(&upload_url_resp.upload_url)
        .header("Content-Type", "image/jpeg")
        .body(jpeg_bytes)
        .send()
        .await
        .map_err(|e| format!("r2-upload: {}", describe_reqwest_error(&e)))?
        .error_for_status()
        .map_err(|e| format!("r2-upload: {}", describe_reqwest_error(&e)))?;
    let _ = app.emit("capture-progress", "uploaded to R2 successfully");

    // Step 3: Confirm upload with server
    let _ = app.emit("capture-progress", "confirming upload with server...");
    let confirm_response = client
        .post(format!(
            "{}/api/sessions/{}/screenshots",
            config.api_base_url, config.token
        ))
        .json(&serde_json::json!({
            "screenshotId": upload_url_resp.screenshot_id,
            "width": width,
            "height": height,
            "fileSize": size_bytes,
        }))
        .send()
        .await
        .map_err(|e| format!("confirm: {}", describe_reqwest_error(&e)))?;
    let confirm_status = confirm_response.status();
    if !confirm_status.is_success() {
        let body = confirm_response.text().await.unwrap_or_default();
        return Err(http_error("confirm", confirm_status, &body));
    }
    let confirm_resp: ConfirmResponse = confirm_response
        .json()
        .await
        .map_err(|e| format!("confirm: {}", describe_reqwest_error(&e)))?;
    let _ = app.emit(
        "capture-progress",
        format!(
            "confirmed! tracked {}s, next expected at {}",
            confirm_resp.tracked_seconds, confirm_resp.next_expected_at
        ),
    );

    Ok(CaptureUploadResult {
        confirmed: confirm_resp.confirmed,
        tracked_seconds: confirm_resp.tracked_seconds,
        next_expected_at: confirm_resp.next_expected_at,
        preview_base64: jpeg_base64.to_string(),
        preview_width: width,
        preview_height: height,
    })
}

/// Build an ISO-8601 timestamp in UTC for the current instant. Used as the
/// client-attested `capturedAt` on upload requests when credit mode is on.
/// Format: `YYYY-MM-DDTHH:MM:SS.sssZ` — what `Date.parse()` and Go's
/// `time.Parse(time.RFC3339)` both accept without surprises.
fn captured_at_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();

    // Civil date math via days-since-epoch — Howard Hinnant's algorithm.
    let days = total_secs.div_euclid(86_400);
    let time_of_day = total_secs.rem_euclid(86_400) as u32;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Convert days-since-1970-01-01 to civil (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    )
}

/// Credit-mode opt-in toggle. New desktop builds always send `capturedAt`;
/// pinning to one flag keeps the wire-format change reviewable in one spot.
/// Old builds (without this constant) never sent it and the server stays in
/// bucket mode for those sessions.
const ENABLE_CREDIT_MODE: bool = true;

/// Full capture-upload-confirm pipeline in Rust (no browser CORS issues).
/// Returns the confirm data AND the screenshot preview (base64) so the
/// frontend can display the captured frame without a separate IPC call.
#[tauri::command]
async fn capture_and_upload(
    sources: Vec<CaptureSource>,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    #[allow(unused_variables)] state: State<'_, AppState>,
    app: AppHandle,
) -> Result<CaptureUploadResult, String> {
    let config = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard
            .clone()
            .ok_or("Not configured — call configure() first")?
    };

    // Read blacklisted apps
    let blacklisted = {
        let guard = state.blacklisted_apps.lock().map_err(|e| e.to_string())?;
        guard.clone()
    };

    // Native screenshot
    let _ = app.emit("capture-progress", "capturing screen...");
    #[allow(unused_mut, unused_assignments)]
    let mut pipewire_fds = std::collections::HashMap::new();
    #[cfg(target_os = "linux")]
    if let Ok(guard) = state.pipewire_fds.lock() {
        pipewire_fds = guard.clone();
    }

    let screenshot = capture::take_stitched_screenshots_with_blacklist(
        &sources,
        max_width,
        max_height,
        jpeg_quality,
        &pipewire_fds,
        &blacklisted,
    )?;
    let _ = app.emit(
        "capture-progress",
        format!(
            "captured {}x{} ({}KB jpeg)",
            screenshot.width,
            screenshot.height,
            screenshot.size_bytes / 1024
        ),
    );

    let captured_at = if ENABLE_CREDIT_MODE {
        Some(captured_at_now())
    } else {
        None
    };
    upload_and_confirm(
        &screenshot.base64,
        screenshot.width,
        screenshot.height,
        captured_at.as_deref(),
        &config,
        &app,
    )
    .await
}

/// Upload a pre-captured frame (e.g. from browser camera capture).
/// Accepts base64-encoded JPEG from the frontend, runs the upload pipeline.
#[tauri::command]
async fn upload_frame(
    base64: String,
    width: u32,
    height: u32,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<CaptureUploadResult, String> {
    let config = {
        let guard = state.config.lock().map_err(|e| e.to_string())?;
        guard
            .clone()
            .ok_or("Not configured — call configure() first")?
    };

    let _ = app.emit(
        "capture-progress",
        format!("uploading camera frame {}x{}", width, height),
    );

    let captured_at = if ENABLE_CREDIT_MODE {
        Some(captured_at_now())
    } else {
        None
    };
    upload_and_confirm(&base64, width, height, captured_at.as_deref(), &config, &app).await
}

// ── Capture-loop interval (seconds) ─────────────────────────────
const CAPTURE_INTERVAL_SECS: u64 = 60;
/// If the wall-clock gap between ticks exceeds this, the machine
/// probably slept (or the WebView was throttled hard).
const SLEEP_THRESHOLD_SECS: u64 = CAPTURE_INTERVAL_SECS * 2 + 30; // 150s

/// Format seconds into the same tray title format as the JS side:
/// >0h: "{h}h {m}m", 0m: "< 1m", else: "{m}m"
fn format_tray_time(total_seconds: i64) -> String {
    let total = total_seconds.max(0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else if m == 0 {
        "< 1m".to_string()
    } else {
        format!("{m}m")
    }
}

/// The tray title ticker task — updates the menu bar text every second.
/// Runs independently of JS so it works even when the WebView is throttled.
async fn tray_timer_task(
    app: AppHandle,
    timer_state: Arc<TrayTimerState>,
    mut cancel_rx: watch::Receiver<bool>,
) {
    use tokio::time::{interval, Duration, MissedTickBehavior};

    let mut ticker = interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel_rx.changed() => {
                eprintln!("[tray-timer] cancelled");
                break;
            }
        }

        if !timer_state.is_running.load(Ordering::Relaxed) {
            continue;
        }

        let base_seconds = timer_state.tracked_seconds.load(Ordering::Relaxed);
        let elapsed = {
            let started = timer_state.started_at.lock().unwrap();
            started.elapsed().as_secs() as i64
        };
        let display_seconds = base_seconds + elapsed;

        let time_text = format_tray_time(display_seconds);
        if let Some(tray) = app.tray_by_id("timelapse_tray") {
            let _ = tray.set_title(Some(time_text));
        }

        // Also emit to the tray popup window so it stays in sync
        let _ = app.emit("tray-timer-tick", display_seconds);
    }
}

/// Start the tray timer (if not already running). Returns the shared state
/// so the capture loop can sync `tracked_seconds` into it.
fn start_tray_timer(app: &AppHandle, state: &AppState) -> Arc<TrayTimerState> {
    let mut guard = state.tray_timer.lock().unwrap();

    // If already running, just return the existing state handle
    if let Some(ref handle) = *guard {
        return Arc::clone(&handle.state);
    }

    let timer_state = Arc::new(TrayTimerState {
        tracked_seconds: AtomicI64::new(0),
        started_at: Mutex::new(StdInstant::now()),
        is_running: AtomicBool::new(true),
    });

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let app_clone = app.clone();
    let state_clone = Arc::clone(&timer_state);

    let join_handle = tokio::spawn(async move {
        tray_timer_task(app_clone, state_clone, cancel_rx).await;
    });

    eprintln!("[tray-timer] started");

    let handle = TrayTimerHandle {
        state: Arc::clone(&timer_state),
        cancel_tx,
        join_handle,
    };
    *guard = Some(handle);

    timer_state
}

/// Stop the tray timer.
fn stop_tray_timer(state: &AppState) {
    let mut guard = state.tray_timer.lock().unwrap();
    if let Some(handle) = guard.take() {
        eprintln!("[tray-timer] stopping");
        let _ = handle.cancel_tx.send(true);
        handle.join_handle.abort();
    }
}

/// Sync the tray timer to a new authoritative tracked_seconds value
/// (typically from a capture result). Resets the elapsed counter.
fn sync_tray_timer(state: &AppState, tracked_seconds: i64) {
    let guard = state.tray_timer.lock().unwrap();
    if let Some(ref handle) = *guard {
        handle
            .state
            .tracked_seconds
            .store(tracked_seconds, Ordering::Relaxed);
        let mut started = handle.state.started_at.lock().unwrap();
        *started = StdInstant::now();
    }
}

/// Pause the tray timer (freeze the displayed time).
fn pause_tray_timer(state: &AppState) {
    let guard = state.tray_timer.lock().unwrap();
    if let Some(ref handle) = *guard {
        handle.state.is_running.store(false, Ordering::Relaxed);
    }
}

/// Resume the tray timer. Resets the elapsed counter so it continues
/// from the current tracked_seconds.
fn resume_tray_timer(state: &AppState) {
    let guard = state.tray_timer.lock().unwrap();
    if let Some(ref handle) = *guard {
        let mut started = handle.state.started_at.lock().unwrap();
        *started = StdInstant::now();
        handle.state.is_running.store(true, Ordering::Relaxed);
    }
}

/// Event payload emitted to JS after each successful capture.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureTickResult {
    confirmed: bool,
    tracked_seconds: i64,
    next_expected_at: String,
    preview_base64: String,
    preview_width: u32,
    preview_height: u32,
}

impl From<CaptureUploadResult> for CaptureTickResult {
    fn from(r: CaptureUploadResult) -> Self {
        Self {
            confirmed: r.confirmed,
            tracked_seconds: r.tracked_seconds,
            next_expected_at: r.next_expected_at,
            preview_base64: r.preview_base64,
            preview_width: r.preview_width,
            preview_height: r.preview_height,
        }
    }
}

/// Event payload emitted when a capture tick fails.
#[derive(Clone, Serialize)]
struct CaptureTickError {
    message: String,
}

/// Event payload emitted when the capture loop detects a terminal session state.
#[derive(Clone, Serialize)]
struct CaptureSessionTerminated {
    status: String,
}

/// Session status response from the server.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStatusResponse {
    status: String,
    #[serde(default)]
    tracked_seconds: Option<i64>,
}

/// The core capture loop, runs on a tokio task. Captures screenshots at
/// a fixed interval, uploads them, and emits events back to JS.
///
/// This is immune to WebView timer throttling because it runs entirely
/// in the Rust/tokio runtime — no JS setTimeout involved.
async fn capture_loop_task(
    app: AppHandle,
    sources: Vec<CaptureSource>,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    mut cancel_rx: watch::Receiver<bool>,
) {
    use tokio::time::{sleep_until, Duration, Instant as TokioInstant};

    // Self-scheduling chain. The fixed-interval ticker is replaced with a
    // `sleep_until(next_target)` that's recomputed from each confirm's
    // `nextExpectedAt`. When the server returns an ISO timestamp we parse
    // it via system clock delta; otherwise we fall back to the legacy 60s
    // cadence. Catch-up-on-miss: clamp negative delays to 0 so we fire
    // immediately rather than waiting another full interval after sleep.
    let interval_dur = Duration::from_secs(CAPTURE_INTERVAL_SECS);
    // First iteration overwrites this before reading; the value is just a
    // placeholder so the variable is bound for the loop body.
    #[allow(unused_assignments)]
    let mut next_fire = TokioInstant::now();
    let mut last_tick = StdInstant::now();

    // Helper: check session status with the server and handle sleep/pause recovery
    async fn handle_sleep_recovery(
        app: &AppHandle,
        config: &SessionConfig,
    ) -> Result<bool /* should_continue */, ()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        let url = format!("{}/api/sessions/{}/status", config.api_base_url, config.token);
        match client.get(&url).send().await {
            Ok(res) if res.status().is_success() => {
                if let Ok(data) = res.json::<SessionStatusResponse>().await {
                    eprintln!("[capture-loop] session status after sleep: {}", data.status);
                    if let Some(ts) = data.tracked_seconds {
                        let _ = app.emit("capture-tracked-seconds", ts);
                    }
                    if data.status == "paused" {
                        let resume_url = format!(
                            "{}/api/sessions/{}/resume",
                            config.api_base_url, config.token
                        );
                        let _ = client.post(&resume_url).send().await;
                        eprintln!("[capture-loop] session resumed after sleep");
                    } else if data.status != "active" && data.status != "pending" {
                        eprintln!(
                            "[capture-loop] session is {}, stopping capture loop",
                            data.status
                        );
                        let _ = app.emit(
                            "capture-session-terminated",
                            CaptureSessionTerminated {
                                status: data.status,
                            },
                        );
                        return Ok(false);
                    }
                }
            }
            Ok(res) => {
                eprintln!("[capture-loop] status check failed: HTTP {}", res.status());
            }
            Err(e) => {
                eprintln!("[capture-loop] status check failed: {e}");
            }
        }
        Ok(true)
    }

    loop {
        // ── First capture fires immediately; subsequent ones wait for the interval tick ──
        // (We already consumed the first tick above, so this select waits for either
        // the next 60s tick or cancellation.)

        // Run one capture tick
        let now = StdInstant::now();
        let elapsed_secs = now.duration_since(last_tick).as_secs();
        last_tick = now;

        // Read config for this tick
        let config = {
            let state = app.state::<AppState>();
            let guard = state.config.lock().unwrap();
            match guard.clone() {
                Some(c) => c,
                None => {
                    let _ = app.emit(
                        "capture-tick-error",
                        CaptureTickError {
                            message: "Not configured".into(),
                        },
                    );
                    break;
                }
            }
        };

        // Sleep detection
        if elapsed_secs > SLEEP_THRESHOLD_SECS {
            eprintln!(
                "[capture-loop] detected sleep (gap: {}s), checking session status...",
                elapsed_secs
            );
            match handle_sleep_recovery(&app, &config).await {
                Ok(true) => { /* continue capturing */ }
                Ok(false) => break,
                Err(_) => { /* best effort, continue */ }
            }
        }

        // Take screenshot (blocking I/O via xcap — run on blocking threadpool)
        let blacklisted = {
            let state = app.state::<AppState>();
            state
                .blacklisted_apps
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default()
        };

        #[allow(unused_mut, unused_assignments)]
        let mut pipewire_fds = std::collections::HashMap::new();
        #[cfg(target_os = "linux")]
        {
            let state = app.state::<AppState>();
            if let Ok(guard) = state.pipewire_fds.lock() {
                pipewire_fds = guard.clone();
            };
        }

        let sources_clone = sources.clone();
        let bl = blacklisted;
        let pw_fds = pipewire_fds;
        let screenshot_result = tokio::task::spawn_blocking(move || {
            capture::take_stitched_screenshots_with_blacklist(
                &sources_clone,
                max_width,
                max_height,
                jpeg_quality,
                &pw_fds,
                &bl,
            )
        })
        .await
        .map_err(|e| format!("spawn_blocking panicked: {e}"))
        .and_then(|r| r);

        // Capture the wall-clock moment NOW — that's the value we'll send
        // as `capturedAt`, not when the upload eventually reaches the server.
        let captured_at = if ENABLE_CREDIT_MODE {
            Some(captured_at_now())
        } else {
            None
        };

        match screenshot_result {
            Ok(screenshot) => {
                match upload_and_confirm(
                    &screenshot.base64,
                    screenshot.width,
                    screenshot.height,
                    captured_at.as_deref(),
                    &config,
                    &app,
                )
                .await
                {
                    Ok(result) => {
                        // Sync tray timer to authoritative server time
                        {
                            let state = app.state::<AppState>();
                            sync_tray_timer(&state, result.tracked_seconds);
                        }
                        // Compute next fire from the server-provided
                        // nextExpectedAt. If parsing fails or the target is
                        // in the past, default to "fire now" (catch-up).
                        let parsed_target_ms = parse_iso_to_unix_ms(&result.next_expected_at);
                        let now_ms = current_unix_ms();
                        let delay_ms = match parsed_target_ms {
                            Some(target) => (target - now_ms).max(0) as u64,
                            None => CAPTURE_INTERVAL_SECS * 1000,
                        };
                        // Safety upper-bound: never sleep longer than 2x interval,
                        // protects against malformed responses.
                        let delay_ms = delay_ms.min(CAPTURE_INTERVAL_SECS * 2 * 1000);
                        next_fire = TokioInstant::now() + Duration::from_millis(delay_ms);
                        let _ = app.emit("capture-tick-result", CaptureTickResult::from(result));
                    }
                    Err(e) => {
                        eprintln!("[capture-loop] upload failed: {e}");
                        let _ = app.emit(
                            "capture-tick-error",
                            CaptureTickError {
                                message: e.clone(),
                            },
                        );
                        // No server target available — fall back to interval.
                        next_fire = TokioInstant::now() + interval_dur;
                        // Check if server paused the session
                        match handle_sleep_recovery(&app, &config).await {
                            Ok(true) => { /* continue */ }
                            Ok(false) => break,
                            Err(_) => {}
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("[capture-loop] screenshot failed: {e}");
                let _ = app.emit(
                    "capture-tick-error",
                    CaptureTickError {
                        message: e.clone(),
                    },
                );
                // Local capture failure — retry on the legacy cadence.
                next_fire = TokioInstant::now() + interval_dur;
            }
        }

        // Wait until next_fire or cancellation. sleep_until returns
        // immediately if next_fire is already in the past (catch-up).
        tokio::select! {
            _ = sleep_until(next_fire) => {}
            _ = cancel_rx.changed() => {
                eprintln!("[capture-loop] cancelled");
                break;
            }
        }
    }

    eprintln!("[capture-loop] stopped");
}

/// Parse an ISO-8601 timestamp like `2024-09-12T18:34:21.123Z` to milliseconds
/// since the Unix epoch. Returns None on any parse failure. Implementation
/// uses civil-date math (Howard Hinnant) so we don't pull in chrono just
/// for this one call site.
fn parse_iso_to_unix_ms(s: &str) -> Option<i64> {
    // Expected layout: YYYY-MM-DDTHH:MM:SS[.fff][Z|+HH:MM|-HH:MM]
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    let second: u32 = s.get(17..19)?.parse().ok()?;
    // Fractional seconds + timezone offset.
    let mut ms: i64 = 0;
    let mut idx = 19;
    if bytes.get(idx).copied() == Some(b'.') {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac = &s[frac_start..idx];
        // Take the first 3 digits as milliseconds, ignore the rest.
        let trimmed: String = frac.chars().take(3).collect();
        let padded = format!("{:0<3}", trimmed); // pad right to 3 chars
        ms = padded.parse().ok()?;
    }
    let mut tz_offset_min: i64 = 0;
    if let Some(&c) = bytes.get(idx) {
        if c == b'Z' {
            // UTC, no offset
        } else if c == b'+' || c == b'-' {
            let sign: i64 = if c == b'+' { 1 } else { -1 };
            let h: i64 = s.get(idx + 1..idx + 3)?.parse().ok()?;
            let m: i64 = if bytes.get(idx + 3) == Some(&b':') {
                s.get(idx + 4..idx + 6)?.parse().ok()?
            } else {
                s.get(idx + 3..idx + 5)?.parse().ok()?
            };
            tz_offset_min = sign * (h * 60 + m);
        }
    }

    // Civil date → days-since-epoch (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    let total_secs =
        days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64;
    Some((total_secs - tz_offset_min * 60) * 1000 + ms)
}

fn current_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Start the Rust-side capture loop. Replaces any existing loop.
/// For screen/window/pipewire sources only — camera sources stay JS-driven.
#[tauri::command]
async fn start_capture_loop(
    sources: Vec<CaptureSource>,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    // Stop any existing loop first
    {
        let mut guard = state.capture_loop.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = guard.take() {
            let _ = handle.cancel_tx.send(true);
            handle.join_handle.abort();
        }
    }

    // Start the tray timer (if not already running)
    start_tray_timer(&app, &state);

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let app_clone = app.clone();

    eprintln!(
        "[capture-loop] starting with {} sources, {}x{} q{}",
        sources.len(),
        max_width,
        max_height,
        jpeg_quality
    );

    let join_handle = tokio::spawn(async move {
        capture_loop_task(app_clone, sources, max_width, max_height, jpeg_quality, cancel_rx).await;
    });

    {
        let mut guard = state.capture_loop.lock().map_err(|e| e.to_string())?;
        *guard = Some(CaptureLoopHandle {
            cancel_tx,
            join_handle,
        });
    }

    Ok(())
}

/// Stop the Rust-side capture loop (if running).
#[tauri::command]
fn stop_capture_loop(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.capture_loop.lock().map_err(|e| e.to_string())?;
    if let Some(handle) = guard.take() {
        eprintln!("[capture-loop] stopping");
        let _ = handle.cancel_tx.send(true);
        handle.join_handle.abort();
    }
    // Stop the tray timer too
    stop_tray_timer(&state);
    Ok(())
}

/// Start the Rust-side tray title ticker (for camera sessions where
/// the capture loop runs in JS but we still want an accurate menu bar timer).
///
/// MUST be `async` — `start_tray_timer` calls `tokio::spawn` internally,
/// which panics if not invoked from inside a tokio runtime. Tauri runs
/// `async` commands on its own tokio runtime, but sync commands run on a
/// thread without one. Calling this command sync caused SIGABRT on camera
/// sessions in 0.2.0 + 0.2.1 (only camera path uses this; screen capture
/// goes through `start_capture_loop` which is already async).
#[tauri::command]
async fn start_tray_ticker(
    tracked_seconds: i64,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    let timer_state = start_tray_timer(&app, &state);
    timer_state.tracked_seconds.store(tracked_seconds, Ordering::Relaxed);
    let mut started = timer_state.started_at.lock().unwrap();
    *started = StdInstant::now();
    timer_state.is_running.store(true, Ordering::Relaxed);
    Ok(())
}

/// Pause the tray title ticker (freezes the displayed time).
#[tauri::command]
fn pause_tray_ticker(state: State<'_, AppState>) -> Result<(), String> {
    pause_tray_timer(&state);
    Ok(())
}

/// Resume the tray title ticker.
#[tauri::command]
fn resume_tray_ticker(
    tracked_seconds: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    {
        let guard = state.tray_timer.lock().unwrap();
        if let Some(ref handle) = *guard {
            handle.state.tracked_seconds.store(tracked_seconds, Ordering::Relaxed);
        }
    }
    resume_tray_timer(&state);
    Ok(())
}

/// Stop the tray title ticker.
#[tauri::command]
fn stop_tray_ticker(state: State<'_, AppState>) -> Result<(), String> {
    stop_tray_timer(&state);
    Ok(())
}

/// Sync the tray timer to an authoritative tracked_seconds value from JS.
#[tauri::command]
fn sync_tray_tracked_seconds(
    tracked_seconds: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    sync_tray_timer(&state, tracked_seconds);
    Ok(())
}

fn base64_decode(b64: &str) -> Result<Vec<u8>, String> {
    use base64_engine::*;
    ENGINE
        .decode(b64)
        .map_err(|e| format!("Base64 decode failed: {e}"))
}

mod base64_engine {
    pub use base64::engine::general_purpose::STANDARD as ENGINE;
    pub use base64::Engine;
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's accelerated compositing crashes on launch under Wayland with
    // some drivers (notably NVIDIA), bailing out with
    // "Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display."
    // Disabling compositing avoids the crash. Scope it to Wayland sessions so
    // X11 users keep GPU acceleration, set it before the webview is created,
    // and only when the user hasn't already chosen a value.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none()
    {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    // Keep _sentry_guard alive for the lifetime of the app so events flush on exit.
    let _sentry_guard = option_env!("SENTRY_DSN").map(|dsn| {
        sentry::init((dsn, sentry::ClientOptions {
            release: sentry::release_name!(),
            environment: Some("desktop-tauri".into()),
            send_default_pii: true,
            sample_rate: 1.0,
            ..Default::default()
        }))
    });

    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol("lookout-preview", |app_handle, request, responder| {
            #[allow(unused_variables)]
            let app_handle = app_handle.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let uri = request.uri().to_string();
                let parsed_url = match url::Url::parse(&uri) {
                    Ok(u) => u,
                    Err(_) => {
                        responder.respond(http::Response::builder().status(400).body(Vec::new()).unwrap());
                        return;
                    }
                };

                let path = parsed_url.path().trim_start_matches('/');
                let segments: Vec<&str> = path.split('/').collect();
                if segments.len() != 2 {
                    responder.respond(http::Response::builder().status(400).body(Vec::new()).unwrap());
                    return;
                }

                let source_type = segments[0];
                let source_id: u32 = match segments[1].parse() {
                    Ok(id) => id,
                    Err(_) => {
                        responder.respond(http::Response::builder().status(400).body(Vec::new()).unwrap());
                        return;
                    }
                };

                let source = match source_type {
                    "monitor" => crate::CaptureSource::Monitor { id: source_id },
                    "window" => crate::CaptureSource::Window { id: source_id },
                    "pipewire" => crate::CaptureSource::PipeWire { id: source_id },
                    _ => {
                        responder.respond(http::Response::builder().status(400).body(Vec::new()).unwrap());
                        return;
                    }
                };

                let mut max_width = 854;
                let mut max_height = 480;
                let mut jpeg_quality = 85;

                for (k, v) in parsed_url.query_pairs() {
                    match k.as_ref() {
                        "maxWidth" => max_width = v.parse().unwrap_or(max_width),
                        "maxHeight" => max_height = v.parse().unwrap_or(max_height),
                        "jpegQuality" => jpeg_quality = v.parse().unwrap_or(jpeg_quality),
                        _ => {}
                    }
                }

                #[allow(unused_mut, unused_assignments)]
                let mut pipewire_fds = std::collections::HashMap::new();
                #[cfg(target_os = "linux")]
                if let Some(app_state) = app_handle.try_state::<AppState>() {
                    if let Ok(guard) = app_state.pipewire_fds.lock() {
                        pipewire_fds = guard.clone();
                    }
                }

                // Read blacklisted apps for redaction
                let blacklisted: Vec<String> = app_handle
                    .try_state::<AppState>()
                    .and_then(|s| s.blacklisted_apps.lock().ok().map(|g| g.clone()))
                    .unwrap_or_default();

                match crate::capture::take_screenshot_raw_with_blacklist(source, max_width, max_height, jpeg_quality, &pipewire_fds, &blacklisted) {
                    Ok(res) => responder.respond(
                        http::Response::builder()
                            .header("Content-Type", "image/jpeg")
                            .header("Cache-Control", "no-store, no-cache, must-revalidate, max-age=0")
                            .header("Access-Control-Allow-Origin", "*")
                            .status(200)
                            .body(res.data)
                            .unwrap()
                    ),
                    Err(e) => {
                        eprintln!("Preview capture failed: {}", e);
                        responder.respond(
                            http::Response::builder()
                                .status(500)
                                .body(e.into_bytes())
                                .unwrap()
                        );
                    }
                }
            });
        })
        // Single-instance MUST be first: on Windows/Linux, when a second
        // instance is launched (e.g. deep link click while app is running),
        // this detects it, forwards args to the running instance, and exits
        // before initializing any other plugins.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // On Windows/Linux, deep-link URLs arrive as CLI args when a second
            // instance is launched. Search all args for a lookout:// URL rather
            // than assuming a fixed position — installers and protocol handlers
            // may pass extra flags.
            eprintln!("[single-instance] args: {args:?}");
            let urls: Vec<String> = args
                .iter()
                .filter(|arg| arg.starts_with("lookout://"))
                .cloned()
                .collect();
            handle_deep_link_urls(app, urls);
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_macos_permissions::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_liquid_glass::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            config: Mutex::new(None),
            cold_start_urls: Mutex::new(None),
            #[cfg(target_os = "linux")]
            pipewire_fds: Mutex::new(std::collections::HashMap::new()),
            blacklisted_apps: Mutex::new(Vec::new()),
            capture_loop: Mutex::new(None),
            tray_timer: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            list_capture_sources,
            configure,
            take_screenshot,
            capture_and_upload,
            upload_frame,
            start_capture_loop,
            stop_capture_loop,
            start_tray_ticker,
            pause_tray_ticker,
            resume_tray_ticker,
            stop_tray_ticker,
            sync_tray_tracked_seconds,
            get_cold_start_urls,
            enable_vibrancy,
            disable_vibrancy,
            is_wayland,
            request_screencast,
            add_screencast,
            set_blacklisted_apps,
            get_blacklisted_apps,
            list_running_apps,
            tray::show_tray,
            tray::update_tray_time,
            tray::hide_tray,
            tray::tray_action,
            tray::set_tray_state,
            tray::get_tray_state,
        ])
        .manage(tray::TrayStateMutex(std::sync::Mutex::new(tray::TrayState::default())))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                // Disable App Nap so macOS doesn't throttle WebView timers when
                // the window is occluded or Low Power Mode is on.  The capture
                // loop runs entirely in JS, so throttled timers = missed screenshots.
                // The returned activity token is intentionally leaked (never ended)
                // so the assertion lasts for the lifetime of the process.
                {
                    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
                    let info = NSProcessInfo::processInfo();
                    let reason = NSString::from_str("Periodic screenshot capture must not be throttled");
                    let opts = NSActivityOptions::LatencyCritical
                        | NSActivityOptions::IdleSystemSleepDisabled;
                    let _activity = info.beginActivityWithOptions_reason(opts, &reason);
                    // Leak the token so the activity assertion persists.
                    std::mem::forget(_activity);
                    eprintln!("[power] App Nap suppression enabled");
                }

                use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};

                let app_menu = Submenu::with_items(
                    app,
                    "Lookout",
                    true,
                    &[
                        &PredefinedMenuItem::about(
                            app,
                            Some("About Lookout"),
                            Some(AboutMetadata {
                                name: Some("Lookout".to_string()),
                                version: app.config().version.clone(),
                                authors: Some(vec!["Hack Club".to_string()]),
                                copyright: Some("© 2026 Hack Club, A 501(c)(3) nonprofit project for student makers.".to_string()),
                                license: Some("MIT".to_string()),
                                website: Some("https://fallout.hackclub.com".to_string()),
                                website_label: Some("Hack Club Fallout".to_string()),
                                ..Default::default()
                            }),
                        )?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::services(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::hide(app, Some("Hide Lookout"))?,
                        &PredefinedMenuItem::hide_others(app, None)?,
                        &PredefinedMenuItem::show_all(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::quit(app, Some("Quit Lookout"))?,
                    ],
                )?;

                let start_timelapse_item = MenuItem::with_id(app, "start_timelapse", "Start Timelapse", true, Some("CmdOrControl+N"))?;
                let file_menu = Submenu::with_items(
                    app,
                    "File",
                    true,
                    &[
                        &start_timelapse_item,
                    ],
                )?;

                let edit_menu = Submenu::with_items(
                    app,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(app, None)?,
                        &PredefinedMenuItem::redo(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::cut(app, None)?,
                        &PredefinedMenuItem::copy(app, None)?,
                        &PredefinedMenuItem::paste(app, None)?,
                        &PredefinedMenuItem::select_all(app, None)?,
                    ],
                )?;

                let window_menu = Submenu::with_items(
                    app,
                    "Window",
                    true,
                    &[
                        &PredefinedMenuItem::minimize(app, None)?,
                        &PredefinedMenuItem::maximize(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::close_window(app, None)?,
                    ],
                )?;

                let docs_item = MenuItem::with_id(app, "docs", "Fallout Docs", true, None::<&str>)?;
                let guide_item = MenuItem::with_id(app, "guide", "How to Timelapse?", true, None::<&str>)?;
                let gh_item = MenuItem::with_id(app, "github", "GitHub Repo", true, None::<&str>)?;
                let help_menu = Submenu::with_items(app, "Help", true, &[&docs_item, &guide_item, &gh_item])?;

                let menu = Menu::with_items(app, &[&app_menu, &file_menu, &edit_menu, &window_menu, &help_menu])?;
                app.set_menu(menu)?;

                app.on_menu_event(move |app_handle, event| {
                    if event.id().0 == "start_timelapse" {
                        let _ = app_handle.emit("lookout-navigate", "/add");
                        if let Some(w) = app_handle.get_webview_window("main") {
                            let _ = w.set_focus();
                        }
                    }
                    if event.id().0 == "docs" {
                        use tauri_plugin_opener::OpenerExt;
                        let _ = app_handle
                            .opener()
                            .open_url("https://fallout.hackclub.com/docs", None::<&str>);
                    }
                    if event.id().0 == "guide" {
                        use tauri_plugin_opener::OpenerExt;
                        let _ = app_handle
                            .opener()
                            .open_url("https://fallout.hackclub.com/docs/project-resources/how-to-timelapse", None::<&str>);
                    }
                    if event.id().0 == "github" {
                        use tauri_plugin_opener::OpenerExt;
                        let _ = app_handle
                            .opener()
                            .open_url("https://github.com/hackclub/lookout/", None::<&str>);
                    }
                });

            }

            // On Windows/Linux, ensure the lookout:// protocol handler is
            // registered even if the installer didn't do it (dev builds,
            // portable installs, AppImages).
            #[cfg(desktop)]
            {
                let _ = app.deep_link().register_all();
                eprintln!("[deep-link] registered protocol handler");
            }

            // Cold start: check if the app was launched via a deep link
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                let url_strings: Vec<String> = urls.into_iter().map(|u| u.to_string()).collect();
                handle_deep_link_urls(app.handle(), url_strings);
            }

            // macOS: Apple Events can deliver deep links after setup completes
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                let url_strings: Vec<String> = event.urls().iter().map(|u| u.to_string()).collect();
                handle_deep_link_urls(&handle, url_strings);
            });

            // Disable maximize/fullscreen controls on all platforms.
            if let Some(window) = app.get_webview_window("main") {
                window.set_maximizable(false)?;
                window.set_fullscreen(false)?;

                // Auto-grant camera/microphone permissions on Windows so the
                // WebView2 native prompt never appears.
                #[cfg(target_os = "windows")]
                windows_permissions::register_permission_handler(&window);

                // When the main window is closed during an active recording,
                // clean up the capture loop and tray immediately. The session
                // pause is handled in the RunEvent::ExitRequested handler below
                // to ensure it completes before the process exits.
                let app_handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Destroyed = event {
                        let state = app_handle.state::<AppState>();

                        // Stop capture loop
                        if let Ok(mut guard) = state.capture_loop.lock() {
                            if let Some(handle) = guard.take() {
                                eprintln!("[window-close] stopping capture loop");
                                let _ = handle.cancel_tx.send(true);
                                handle.join_handle.abort();
                            }
                        }

                        // Stop tray timer
                        stop_tray_timer(&state);

                        // Remove tray icon
                        app_handle.remove_tray_by_id("timelapse_tray");
                        if let Some(w) = app_handle.get_webview_window("tray") {
                            let _ = w.close();
                        }
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                // If there's an active capture loop, pause the session before
                // allowing exit so it doesn't sit "active" until the server
                // auto-pauses (5 min timeout).
                let state = app.state::<AppState>();
                let has_active_loop = state
                    .capture_loop
                    .lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false);
                let config = state.config.lock().ok().and_then(|g| g.clone());

                if has_active_loop {
                    if let Some(config) = config {
                        // Prevent immediate exit — we need to send the pause request first.
                        api.prevent_exit();
                        eprintln!("[exit] pausing session before exit");
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let client = reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(5))
                                .build()
                                .unwrap_or_default();
                            let url = format!(
                                "{}/api/sessions/{}/pause",
                                config.api_base_url, config.token
                            );
                            match client.post(&url).send().await {
                                Ok(res) => eprintln!("[exit] pause response: {}", res.status()),
                                Err(e) => eprintln!("[exit] pause failed (best-effort): {e}"),
                            }
                            // Now allow the app to exit
                            app_handle.exit(0);
                        });
                    }
                }
            }
        });
}

// ──────────────────────────────────────────────────────────────────
// Compat tests for the wire format.
//
// Cross-checks that:
//   1. The CURRENT response structs accept both legacy and new JSON.
//   2. The LEGACY response structs (copied verbatim from the pre-credit-mode
//      `lib.rs`) still accept the new server's JSON. This is the load-bearing
//      compat guarantee: an unupgraded user's binary in the wild keeps working.
// ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod compat_tests {
    use super::{
        captured_at_now, parse_iso_to_unix_ms, ConfirmResponse, UploadUrlResponse,
    };

    // Snapshot of the pre-credit-mode struct definitions, byte-for-byte from
    // git history. If a future change accidentally breaks shape compat with
    // shipped binaries, the relevant test below will fail.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct LegacyUploadUrlResponse {
        #[serde(rename = "uploadUrl")]
        upload_url: String,
        #[serde(rename = "r2Key")]
        r2_key: String,
        #[serde(rename = "screenshotId")]
        screenshot_id: String,
        #[serde(rename = "minuteBucket")]
        minute_bucket: i32,
        #[serde(rename = "nextExpectedAt")]
        next_expected_at: String,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct LegacyConfirmResponse {
        confirmed: bool,
        #[serde(rename = "trackedSeconds")]
        tracked_seconds: i64,
        #[serde(rename = "nextExpectedAt")]
        next_expected_at: String,
    }

    const LEGACY_UPLOAD_JSON: &str = r#"{
        "uploadUrl": "https://r2.example.com/upload",
        "r2Key": "screenshots/abc/def.jpg",
        "screenshotId": "11111111-2222-3333-4444-555555555555",
        "minuteBucket": 7,
        "nextExpectedAt": "2025-06-01T12:01:00.000Z"
    }"#;

    const NEW_UPLOAD_JSON: &str = r#"{
        "uploadUrl": "https://r2.example.com/upload",
        "r2Key": "screenshots/abc/def.jpg",
        "screenshotId": "11111111-2222-3333-4444-555555555555",
        "minuteBucket": 7,
        "nextExpectedAt": "2025-06-01T12:01:00.000Z",
        "serverTime": "2025-06-01T12:00:00.000Z",
        "trackingMode": "credit"
    }"#;

    const LEGACY_CONFIRM_JSON: &str = r#"{
        "confirmed": true,
        "trackedSeconds": 60,
        "nextExpectedAt": "2025-06-01T12:01:00.000Z"
    }"#;

    const NEW_CONFIRM_JSON: &str = r#"{
        "confirmed": true,
        "trackedSeconds": 60,
        "nextExpectedAt": "2025-06-01T12:01:00.000Z",
        "serverTime": "2025-06-01T12:00:00.500Z"
    }"#;

    // ── 4-way matrix: {legacy, new} struct × {legacy, new} JSON ───────

    #[test]
    fn new_struct_parses_legacy_json_upload_url() {
        // New binary hitting an OLD server (rollout window): the new struct
        // must accept the legacy JSON (missing serverTime / trackingMode).
        let r: UploadUrlResponse = serde_json::from_str(LEGACY_UPLOAD_JSON).unwrap();
        assert_eq!(r.screenshot_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(r.minute_bucket, 7);
        assert!(r.server_time.is_none());
        assert!(r.tracking_mode.is_none());
    }

    #[test]
    fn new_struct_parses_new_json_upload_url() {
        let r: UploadUrlResponse = serde_json::from_str(NEW_UPLOAD_JSON).unwrap();
        assert_eq!(r.server_time.as_deref(), Some("2025-06-01T12:00:00.000Z"));
        assert_eq!(r.tracking_mode.as_deref(), Some("credit"));
    }

    #[test]
    fn legacy_struct_parses_new_json_upload_url() {
        // *** Load-bearing compat guarantee ***
        // The struct shape as it exists in the currently-shipped binary
        // must continue to deserialize the new server's responses. If
        // serde's default behavior (ignore unknown fields) ever changes
        // — or if someone adds `deny_unknown_fields` later — this fails.
        let r: LegacyUploadUrlResponse = serde_json::from_str(NEW_UPLOAD_JSON).unwrap();
        assert_eq!(r.screenshot_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(r.minute_bucket, 7);
        assert_eq!(r.next_expected_at, "2025-06-01T12:01:00.000Z");
    }

    #[test]
    fn legacy_struct_parses_legacy_json_upload_url() {
        let r: LegacyUploadUrlResponse = serde_json::from_str(LEGACY_UPLOAD_JSON).unwrap();
        assert_eq!(r.minute_bucket, 7);
    }

    #[test]
    fn new_struct_parses_legacy_json_confirm() {
        let r: ConfirmResponse = serde_json::from_str(LEGACY_CONFIRM_JSON).unwrap();
        assert_eq!(r.tracked_seconds, 60);
        assert!(r.server_time.is_none());
    }

    #[test]
    fn new_struct_parses_new_json_confirm() {
        let r: ConfirmResponse = serde_json::from_str(NEW_CONFIRM_JSON).unwrap();
        assert_eq!(r.tracked_seconds, 60);
        assert!(r.server_time.is_some());
    }

    #[test]
    fn legacy_struct_parses_new_json_confirm() {
        // *** Load-bearing compat guarantee *** for /screenshots responses.
        let r: LegacyConfirmResponse = serde_json::from_str(NEW_CONFIRM_JSON).unwrap();
        assert!(r.confirmed);
        assert_eq!(r.tracked_seconds, 60);
        assert_eq!(r.next_expected_at, "2025-06-01T12:01:00.000Z");
    }

    #[test]
    fn legacy_struct_parses_legacy_json_confirm() {
        let r: LegacyConfirmResponse = serde_json::from_str(LEGACY_CONFIRM_JSON).unwrap();
        assert_eq!(r.tracked_seconds, 60);
    }

    // ── captured_at helpers ───────────────────────────────────────────

    #[test]
    fn captured_at_now_is_iso8601_utc() {
        let s = captured_at_now();
        // YYYY-MM-DDTHH:MM:SS.sssZ — 24 chars total
        assert_eq!(s.len(), 24);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[13..14], ":");
        assert_eq!(&s[16..17], ":");
        assert_eq!(&s[19..20], ".");
        assert_eq!(&s[23..24], "Z");
    }

    #[test]
    fn parse_iso_to_unix_ms_round_trips_captured_at_now() {
        let s = captured_at_now();
        let parsed = parse_iso_to_unix_ms(&s).expect("parses");
        let actual = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Round-trip within 5s of wall clock.
        assert!((parsed - actual).abs() < 5_000, "parsed={parsed} actual={actual}");
    }

    #[test]
    fn parse_iso_to_unix_ms_handles_known_values() {
        // 2025-01-01T00:00:00.000Z = 1735689600000
        assert_eq!(
            parse_iso_to_unix_ms("2025-01-01T00:00:00.000Z"),
            Some(1735689600000)
        );
        // 2025-06-01T12:00:00.000Z = 1748779200000
        assert_eq!(
            parse_iso_to_unix_ms("2025-06-01T12:00:00.000Z"),
            Some(1748779200000)
        );
        // 1970-01-01T00:00:00.000Z = 0
        assert_eq!(parse_iso_to_unix_ms("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn parse_iso_to_unix_ms_handles_timezone_offset() {
        // 12:00 at +05:00 == 07:00 UTC
        let a = parse_iso_to_unix_ms("2025-06-01T12:00:00.000+05:00");
        let b = parse_iso_to_unix_ms("2025-06-01T07:00:00.000Z");
        assert_eq!(a, b);
    }

    #[test]
    fn parse_iso_to_unix_ms_rejects_garbage() {
        assert_eq!(parse_iso_to_unix_ms(""), None);
        assert_eq!(parse_iso_to_unix_ms("not-a-date"), None);
        assert_eq!(parse_iso_to_unix_ms("2025/06/01"), None);
    }

    // ──────────────────────────────────────────────────────────────────
    // Regression test for the camera-session crash (SIGABRT) in 0.2.0/0.2.1.
    //
    // `start_tray_ticker` was a sync `#[tauri::command]` that called
    // `tokio::spawn` via `start_tray_timer`. Sync Tauri commands run on a
    // thread with no tokio runtime in context, so `tokio::spawn` panicked
    // and the app aborted. The fix is making the command `async` so Tauri
    // hosts it on its async runtime.
    //
    // We can't easily instantiate Tauri's AppHandle in a unit test, so we
    // reproduce the underlying invariant: `tokio::spawn` must run inside a
    // runtime. If this assertion ever weakens (e.g. tokio adds an
    // ambient-runtime fallback), revisit whether the `async fn` is still
    // load-bearing on the command.
    // ──────────────────────────────────────────────────────────────────

    #[test]
    fn tokio_spawn_panics_without_runtime() {
        // The exact failure mode that crashed 0.2.0/0.2.1 camera sessions.
        let result = std::panic::catch_unwind(|| {
            let _ = tokio::spawn(async {});
        });
        assert!(
            result.is_err(),
            "tokio::spawn outside a runtime should panic — if this now succeeds, \
             tokio's behavior changed and the async-fn fix may no longer be required."
        );
    }

    #[test]
    fn tokio_spawn_succeeds_inside_runtime() {
        // Mirrors what Tauri's async runtime does for `async` commands.
        let rt = tokio::runtime::Runtime::new().expect("build runtime");
        rt.block_on(async {
            let h = tokio::spawn(async { 42i32 });
            assert_eq!(h.await.unwrap(), 42);
        });
    }
}
