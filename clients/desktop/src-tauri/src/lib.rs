mod capture;
mod capture_diagnostics;
mod clips;
mod clock_offset;
mod crop;
mod desktop_appearance;
#[cfg(target_os = "linux")]
mod gnome_indicator;
mod native_menu;
#[cfg(target_os = "macos")]
mod native_tray;
mod pipewire;
mod screencast;
mod tray;
mod window_shape;
#[cfg(target_os = "windows")]
mod windows_permissions;

/// Test-only helpers shared by the per-module leak tests.
#[cfg(test)]
pub(crate) mod test_support {
    /// Resident-set size of this process, in KB. Crude on purpose — good
    /// enough to tell a leak from steady state, and needs no dependency.
    ///
    /// The Unix arm shells out to `ps`; Windows (where the worst leaks have
    /// actually shipped — the GDI bitmap and MF sink-writer ones) asks
    /// PowerShell for the working set, so the leak tests finally RUN there
    /// instead of panicking on a missing `ps`.
    pub fn rss_kb() -> u64 {
        #[cfg(windows)]
        {
            let out = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command"])
                .arg(format!(
                    "(Get-Process -Id {}).WorkingSet64",
                    std::process::id()
                ))
                .output()
                .expect("powershell");
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u64>()
                .unwrap_or(0)
                / 1024
        }
        #[cfg(not(windows))]
        {
            let out = std::process::Command::new("ps")
                .args(["-o", "rss=", "-p"])
                .arg(std::process::id().to_string())
                .output()
                .expect("ps");
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        }
    }

    /// Live GDI object count for this process — the precise leak signal on
    /// Windows: orphaning a bitmap costs exactly one GDI handle, and the
    /// per-process cap is 10k, after which every capture fails.
    #[cfg(windows)]
    pub fn gdi_object_count() -> u32 {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, GetGuiResources, GR_GDIOBJECTS,
        };
        unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) }
    }
}

/// Scoped App Nap / idle-system-sleep suppression (macOS).
///
/// The assertion must be held while a session is recording (or paused
/// mid-session) so macOS never throttles the capture cadence or lets the
/// machine idle-sleep out from under an active recording. It must NOT be
/// held for the whole process lifetime — that kept the user's Mac from ever
/// idle-sleeping just because Lookout sat open on the gallery.
#[cfg(target_os = "macos")]
mod power {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
    use std::sync::Mutex;

    struct ActivityToken(Retained<ProtocolObject<dyn NSObjectProtocol>>);
    // SAFETY: the token is an opaque handle whose only use is being handed
    // back to `NSProcessInfo::endActivity`, which is documented thread-safe.
    unsafe impl Send for ActivityToken {}

    static ACTIVITY: Mutex<Option<ActivityToken>> = Mutex::new(None);

    /// Begin the recording assertion. Idempotent — a second call while one
    /// is already held is a no-op.
    pub fn begin_recording_assertion() {
        let mut guard = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let info = NSProcessInfo::processInfo();
        let reason = NSString::from_str("Periodic screenshot capture must not be throttled");
        let opts =
            NSActivityOptions::LatencyCritical | NSActivityOptions::IdleSystemSleepDisabled;
        *guard = Some(ActivityToken(
            info.beginActivityWithOptions_reason(opts, &reason),
        ));
        eprintln!("[power] recording sleep/App Nap suppression ON");
    }

    /// End the recording assertion (no-op if none is held).
    pub fn end_recording_assertion() {
        let mut guard = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(token) = guard.take() {
            // SAFETY: `token.0` came from `beginActivityWithOptions_reason`,
            // so it is the correct activity type.
            unsafe { NSProcessInfo::processInfo().endActivity(&token.0) };
            eprintln!("[power] recording sleep/App Nap suppression OFF");
        }
    }
}

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
///
/// This mirrors `useSessionTimerState` in @lookout/react. The menu bar,
/// the tray popup and the main window each tick their own clock (so a
/// throttled WebView can't stall the menu bar), which only works if all
/// three apply the *same* rules to the same anchor: ratchet the base
/// forward, cap interpolation at one capture interval, and drop the
/// interpolated remainder while paused. Diverge on any of those and the
/// menu bar visibly disagrees with the main window.
struct TrayTimerState {
    /// Authoritative tracked seconds from the last server response.
    /// Ratchets forward only — see `sync_tray_timer`.
    tracked_seconds: AtomicI64,
    /// Wall-clock instant `tracked_seconds` last advanced (the
    /// interpolation anchor).
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
    /// Running estimate of `serverNow - clientNow`. Fed by the `serverTime`
    /// on every upload response; read wherever we stamp a capture or turn a
    /// server wall-clock target into a local delay. This is what makes a
    /// wrong system clock cost nothing: without it, any skew past ±30s
    /// zeroed the credit and past 60s halved the capture rate (the skew
    /// leaked into every `nextExpectedAt - now` delay and hit the
    /// 2x-interval clamp).
    pub clock_offset: Mutex<clock_offset::ClockOffset>,
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

/// Earlier releases called `register_all()` on every Linux launch, which
/// wrote a `lookout-desktop-handler.desktop` into the user's applications
/// dir. On deb/rpm installs that duplicated the packaged desktop file, so
/// the "Open with" chooser offered two Lookouts. Delete the leftover so
/// only the packaged entry claims lookout://.
#[cfg(target_os = "linux")]
fn remove_stale_deep_link_handler(app: &AppHandle) {
    let Ok(exe) = tauri::utils::platform::current_exe() else {
        return;
    };
    let Some(exe_name) = exe.file_name() else {
        return;
    };
    let Ok(data_dir) = app.path().data_dir() else {
        return;
    };
    let applications_dir = data_dir.join("applications");
    let handler_file =
        applications_dir.join(format!("{}-handler.desktop", exe_name.to_string_lossy()));
    if handler_file.exists() && std::fs::remove_file(&handler_file).is_ok() {
        eprintln!("[deep-link] removed stale handler {}", handler_file.display());
        // Also drop the mimeapps.list default the old register_all() set,
        // which points at the file just deleted. GIO would skip a missing
        // default anyway, but a KDE/XFCE launcher might not.
        let _ = app.deep_link().unregister("lookout");
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&applications_dir)
            .status();
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
    /// True when the server replaced this capture's timestamp with its own
    /// because ours was outside the ±5min trust envelope. The upload still
    /// succeeded; seeing this means the clock-offset estimate is about to
    /// matter, so it's worth telling the user their clock is wrong.
    #[serde(rename = "capturedAtAdopted", default)]
    pub captured_at_adopted: bool,
    /// Sticky tracking mode for the session. Absent on pre-credit-mode servers.
    #[serde(rename = "trackingMode", default)]
    pub tracking_mode: Option<String>,
    /// GRANTED payload format — may differ from the requested one (the
    /// server downgrades clip formats to "jpeg" on sessions without clips).
    /// Absent on pre-clips servers.
    #[serde(rename = "format", default)]
    pub format: Option<String>,
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

/// One entry in the app list shown on the Filtered Apps page.
#[derive(Clone, Serialize)]
pub struct AppEntry {
    pub name: String,
    /// Platform-specific icon lookup key, passed back to `get_app_icon`:
    /// macOS = .app bundle path, Windows = Start Menu .lnk path,
    /// Linux = the .desktop entry's Icon= value.
    pub path: Option<String>,
    /// Whether the app is currently running (used to sort open apps first).
    pub running: bool,
}

/// Read an app bundle's display name (CFBundleDisplayName, falling back to
/// CFBundleName). These are what `kCGWindowOwnerName` reports for the app's
/// windows, so blacklist entries created from this list match redaction.
#[cfg(target_os = "macos")]
fn bundle_display_name(path: &std::path::Path) -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};

    let ns_path = NSString::from_str(path.to_str()?);
    let bundle = NSBundle::bundleWithPath(&ns_path)?;
    for key in ["CFBundleDisplayName", "CFBundleName"] {
        let key = NSString::from_str(key);
        if let Some(value) = bundle.objectForInfoDictionaryKey(&key) {
            if let Ok(s) = value.downcast::<NSString>() {
                let s = s.to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Scan the standard application folders for installed .app bundles.
/// Slow-ish (reads each bundle's Info.plist), so callers cache the result.
#[cfg(target_os = "macos")]
fn scan_installed_apps() -> Vec<AppEntry> {
    let mut queue: Vec<(std::path::PathBuf, u8)> = vec![
        ("/Applications".into(), 0),
        ("/System/Applications".into(), 0),
    ];
    if let Ok(home) = std::env::var("HOME") {
        queue.push((std::path::Path::new(&home).join("Applications"), 0));
    }

    let mut apps = Vec::new();
    // Scan one folder level deep: /Applications/Utilities/X.app and vendor
    // folders like /Applications/Adobe .../X.app are common.
    while let Some((dir, depth)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if file_name.starts_with('.') {
                continue;
            }
            if file_name.ends_with(".app") {
                let name = bundle_display_name(&path)
                    .unwrap_or_else(|| file_name.trim_end_matches(".app").to_string());
                if name.is_empty() || name == "Lookout" || should_exclude_window(&name, "") {
                    continue;
                }
                apps.push(AppEntry {
                    name,
                    path: Some(path.to_string_lossy().into_owned()),
                    running: false,
                });
            } else if depth < 1 && path.is_dir() {
                queue.push((path, depth + 1));
            }
        }
    }
    apps
}

/// Scan Start Menu shortcuts — the canonical "installed apps" on Windows.
#[cfg(target_os = "windows")]
fn scan_installed_apps() -> Vec<AppEntry> {
    let mut queue: Vec<(std::path::PathBuf, u8)> = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        queue.push((
            std::path::Path::new(&program_data).join(r"Microsoft\Windows\Start Menu\Programs"),
            0,
        ));
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        queue.push((
            std::path::Path::new(&app_data).join(r"Microsoft\Windows\Start Menu\Programs"),
            0,
        ));
    }

    let mut apps = Vec::new();
    while let Some((dir, depth)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `entry.file_type()` reads the attributes the directory
            // enumeration already returned; `entry.path().is_dir()` would stat
            // each entry again. On Windows that is a real syscall per shortcut,
            // through whatever filter drivers and AV hooks are installed, and
            // the Start Menu tree has hundreds of entries.
            // ...but file_type() does NOT follow symlinks where is_dir() did,
            // so a directory junction in the Start Menu would stop being
            // traversed. Fall back to the stat only for that rare case.
            let path = entry.path();
            let is_dir = match entry.file_type() {
                Ok(t) if t.is_symlink() => path.is_dir(),
                Ok(t) => t.is_dir(),
                Err(_) => path.is_dir(),
            };
            if is_dir {
                if depth < 3 {
                    queue.push((path, depth + 1));
                }
                continue;
            }
            let is_lnk = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
            if !is_lnk {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let name = name.to_string();
            let lower = name.to_lowercase();
            if name.is_empty()
                || name == "Lookout"
                || should_exclude_window(&name, "")
                || lower.starts_with("uninstall")
                || lower.contains("uninstaller")
            {
                continue;
            }
            apps.push(AppEntry {
                name,
                path: Some(path.to_string_lossy().into_owned()),
                running: false,
            });
        }
    }
    apps
}

/// Parse the fields we need from a .desktop file's [Desktop Entry] section.
/// Returns (name, icon) or None if the entry isn't a visible application.
#[cfg(target_os = "linux")]
fn parse_desktop_entry(content: &str) -> Option<(String, Option<String>)> {
    let mut in_section = false;
    let mut name = None;
    let mut icon = None;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_section {
                break; // end of [Desktop Entry]
            }
            in_section = line == "[Desktop Entry]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(value) = line.strip_prefix("NoDisplay=") {
            if value.trim() == "true" {
                return None;
            }
        } else if let Some(value) = line.strip_prefix("Type=") {
            if value.trim() != "Application" {
                return None;
            }
        } else if let Some(value) = line.strip_prefix("Name=") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Icon=") {
            icon = Some(value.trim().to_string());
        }
    }
    Some((name.filter(|n| !n.is_empty())?, icon))
}

/// Scan .desktop entries — the canonical "installed apps" on Linux.
#[cfg(target_os = "linux")]
fn scan_installed_apps() -> Vec<AppEntry> {
    let mut dirs: Vec<std::path::PathBuf> = vec![
        "/usr/share/applications".into(),
        "/usr/local/share/applications".into(),
        "/var/lib/flatpak/exports/share/applications".into(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::Path::new(&home);
        dirs.push(home.join(".local/share/applications"));
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }

    let mut apps = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some((name, icon)) = parse_desktop_entry(&content) else {
                continue;
            };
            if name == "Lookout" || should_exclude_window(&name, "") {
                continue;
            }
            apps.push(AppEntry {
                name,
                path: icon,
                running: false,
            });
        }
    }
    apps
}

fn installed_apps_cached() -> &'static [AppEntry] {
    static CACHE: std::sync::OnceLock<Vec<AppEntry>> = std::sync::OnceLock::new();
    CACHE.get_or_init(scan_installed_apps)
}

/// (name, icon-lookup key) pairs for currently running apps. Names come from
/// the same source redaction matches against (kCGWindowOwnerName on macOS,
/// xcap `app_name` elsewhere), so a running app always blacklists correctly
/// even when its installed entry is named differently.
fn running_apps() -> Vec<(String, Option<String>)> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

        let workspace = NSWorkspace::sharedWorkspace();
        workspace
            .runningApplications()
            .iter()
            .filter(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .filter_map(|app| {
                let name = app.localizedName()?.to_string();
                let path = app
                    .bundleURL()
                    .and_then(|url| url.path())
                    .map(|p| p.to_string());
                Some((name, path))
            })
            .collect()
    }

    #[cfg(not(target_os = "macos"))]
    {
        use xcap::Window;
        let mut names = std::collections::BTreeSet::new();
        if let Ok(windows) = Window::all() {
            for w in windows {
                if let Ok(name) = w.app_name() {
                    if !name.is_empty() && !should_exclude_window(&name, &w.title().unwrap_or_default()) {
                        names.insert(name);
                    }
                }
            }
        }
        names.into_iter().map(|name| (name, None)).collect()
    }
}

/// List apps for the Filtered Apps page, sorted by name: every installed app
/// (scanned once per process and cached) merged with currently running apps.
/// Only real applications appear — helper/XPC processes that merely own
/// windows (e.g. "CursorUIViewService") don't.
///
/// The work is BLOCKING — a Start Menu tree walk on the first call, and a
/// window enumeration on every call — so it runs on the blocking pool rather
/// than on the async runtime. `async fn` alone was not enough: the body never
/// yields, so it occupied a tokio worker for its whole duration, and the
/// capture loop lives on those same workers. A slow enumeration could
/// therefore delay a capture tick, not just the Settings page.
#[tauri::command]
async fn list_installed_apps() -> Vec<AppEntry> {
    tauri::async_runtime::spawn_blocking(list_installed_apps_blocking)
        .await
        .unwrap_or_default()
}

/// Pre-warm the installed-app cache so the first visit to Filtered Apps doesn't
/// pay for the app scan while the user waits.
///
/// DEFERRED on purpose. The scan is disk-bound — a Start Menu tree walk on
/// Windows, /Applications on macOS, .desktop files on Linux — and launch is
/// already the most I/O-contended moment in the process's life: the webview is
/// loading its own assets at the same time. Starting the scan immediately would
/// trade a faster Settings page for a slower app open, which is the wrong way
/// round. A few seconds' delay is still far earlier than anyone navigates to
/// Filtered Apps, and by then the launch I/O has settled.
fn prewarm_installed_apps() {
    std::thread::Builder::new()
        .name("app-scan-prewarm".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let _ = installed_apps_cached();
        })
        // A failed prewarm is not worth failing startup over: the cache just
        // fills lazily on first use, exactly as it did before.
        .ok();
}

fn list_installed_apps_blocking() -> Vec<AppEntry> {
    // name -> (path, running); BTreeMap keeps the result sorted by name.
    let mut apps: std::collections::BTreeMap<String, (Option<String>, bool)> =
        installed_apps_cached()
            .iter()
            .map(|a| (a.name.clone(), (a.path.clone(), false)))
            .collect();

    for (name, path) in running_apps() {
        if name.is_empty() || name == "Lookout" || should_exclude_window(&name, "") {
            continue;
        }
        match apps.entry(name) {
            std::collections::btree_map::Entry::Occupied(mut e) => {
                let (existing_path, running) = e.get_mut();
                if existing_path.is_none() {
                    *existing_path = path;
                }
                *running = true;
            }
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((path, true));
            }
        }
    }

    apps.into_iter()
        .map(|(name, (path, running))| AppEntry {
            name,
            path,
            running,
        })
        .collect()
}

/// Return a small PNG (base64) of an app's icon. `path` is the icon lookup
/// key from `AppEntry.path`. Cached per key; async so lookups run off the
/// main thread (a sync command here froze the UI while icons rasterized).
#[tauri::command]
async fn get_app_icon(path: String) -> Option<String> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<String>>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path)
    {
        return hit.clone();
    }

    let result = compute_app_icon(&path);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path, result.clone());
    result
}

#[cfg(target_os = "macos")]
fn compute_app_icon(path: &str) -> Option<String> {
    use base64::Engine as _;
    use objc2::AnyThread as _;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_core_foundation::{CGPoint, CGSize};
    use objc2_foundation::{NSDictionary, NSString};

    let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
    // Ask for a small rect so IconServices hands back the small icon
    // representation instead of rasterizing the full 1024px artwork.
    let mut rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 32.0,
            height: 32.0,
        },
    };
    unsafe { icon.CGImageForProposedRect_context_hints(&mut rect, None, None) }
        .and_then(|cg| {
            let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
            unsafe {
                rep.representationUsingType_properties(
                    NSBitmapImageFileType::PNG,
                    &NSDictionary::new(),
                )
            }
        })
        .map(|png| base64::engine::general_purpose::STANDARD.encode(png.to_vec()))
}

/// Windows: shell icon for the Start Menu .lnk (resolves to the target
/// exe's icon), converted HICON -> RGBA -> PNG.
#[cfg(target_os = "windows")]
fn compute_app_icon(path: &str) -> Option<String> {
    use base64::Engine as _;
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    // SHGetFileInfoW needs COM for .lnk resolution; commands run on worker
    // threads, so initialize per call (no-op if already initialized).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }

    let png = (|| {
        let mut icon_info = ICONINFO::default();
        unsafe { GetIconInfo(info.hIcon, &mut icon_info) }.ok()?;

        let result = (|| {
            let mut bmp = BITMAP::default();
            let got = unsafe {
                GetObjectW(
                    icon_info.hbmColor.into(),
                    std::mem::size_of::<BITMAP>() as i32,
                    Some(&mut bmp as *mut _ as *mut _),
                )
            };
            if got == 0 || bmp.bmWidth <= 0 || bmp.bmHeight <= 0 {
                return None;
            }
            let (w, h) = (bmp.bmWidth, bmp.bmHeight);

            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // negative = top-down rows
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB.0;

            let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
            let hdc = unsafe { GetDC(None) };
            let lines = unsafe {
                GetDIBits(
                    hdc,
                    icon_info.hbmColor,
                    0,
                    h as u32,
                    Some(buf.as_mut_ptr() as *mut _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                )
            };
            unsafe { ReleaseDC(None, hdc) };
            if lines == 0 {
                return None;
            }

            // BGRA -> RGBA; some icons come back with an empty alpha
            // channel, which would render as fully transparent.
            for px in buf.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            if buf.chunks_exact(4).all(|px| px[3] == 0) {
                for px in buf.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }

            let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)?;
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(img)
                .write_to(&mut out, image::ImageFormat::Png)
                .ok()?;
            Some(out.into_inner())
        })();

        unsafe {
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
        }
        result
    })();

    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    png.map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Linux: resolve the .desktop Icon= value against the hicolor theme and
/// pixmaps dirs (PNG only) and return the file as-is.
#[cfg(target_os = "linux")]
fn compute_app_icon(icon: &str) -> Option<String> {
    use base64::Engine as _;

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if icon.starts_with('/') {
        candidates.push(icon.into());
    } else {
        let mut base_dirs: Vec<String> = vec![
            "/usr/share".into(),
            "/usr/local/share".into(),
            "/var/lib/flatpak/exports/share".into(),
        ];
        if let Ok(home) = std::env::var("HOME") {
            base_dirs.push(format!("{home}/.local/share"));
            base_dirs.push(format!("{home}/.local/share/flatpak/exports/share"));
        }
        for base in &base_dirs {
            for size in ["48x48", "64x64", "32x32", "128x128", "256x256"] {
                candidates.push(format!("{base}/icons/hicolor/{size}/apps/{icon}.png").into());
            }
            candidates.push(format!("{base}/pixmaps/{icon}.png").into());
        }
    }

    for path in candidates {
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    None
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

/// Open a URL in the user's default browser (foreground). Used by the program
/// picker to start a session on the program's website, where the user is
/// already logged in. Foreground is required: the lookout:// deep-link handoff
/// shows a browser confirmation prompt the user must click. Restricted to
/// http(s) so the frontend can't open arbitrary schemes.
#[tauri::command]
fn open_external_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("only http(s) URLs are allowed".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
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

/// Shared HTTP client for all server/R2 traffic. Building a `reqwest::Client`
/// allocates a fresh connection pool + TLS config, so constructing one per
/// request (as each capture tick used to) both wastes CPU and forces a new
/// TCP/TLS handshake every 60 seconds. One shared client keeps connections
/// alive between ticks. Timeouts differ per call site, so they're applied
/// per-request via `RequestBuilder::timeout` instead of on the client.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
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

/// Compact, consistent message for an HTTP error response: the status code
/// (+ reason phrase, which `StatusCode` Displays) and the response body when
/// there is one. No filler. The failing step is supplied by the retry
/// wrapper's label, so it isn't repeated here.
fn http_error(status: reqwest::StatusCode, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {body}")
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

/// Outcome of a single upload-pipeline attempt, used by [`retry_upload_step`]
/// to decide whether a failure is worth retrying.
enum StepError {
    /// Transient failure (timeout, connection error, 5xx, …) — retry with
    /// backoff.
    Retryable(String),
    /// Permanent failure — fail fast, no retry. Currently only HTTP 409
    /// (session paused/stopped server-side): retrying would just burn the
    /// backoff window before the capture loop runs sleep-recovery.
    Terminal(String),
}

/// Classify an HTTP error response into a [`StepError`]. Mirrors the react
/// client's special-case (clients/react/src/hooks/useUploader.ts): 409 is
/// terminal, everything else is retryable.
fn classify_http(status: reqwest::StatusCode, msg: String) -> StepError {
    if status == reqwest::StatusCode::CONFLICT {
        StepError::Terminal(msg)
    } else {
        StepError::Retryable(msg)
    }
}

/// Collapse the per-attempt failure history into one diagnostic string, led by
/// the step `label` (e.g. `r2-upload 84KB → acct.r2.cloudflarestorage.com`).
/// The goal: make an intermittent failure legible at a glance.
///
/// - If every attempt failed the same way, report the cause once with all the
///   elapsed times (a steady outage): `… failed after 3 attempts: timed out
///   (30.0s, 30.0s, 30.0s)`.
/// - If the causes differ, list each attempt (flapping connectivity): `…
///   failed after 3 attempts: #1 timed out (30.0s); #2 connection refused
///   (1.2s); #3 timed out (30.0s)`.
///
/// Each entry is `(attempt_number, cause, elapsed_seconds)`. Per-attempt
/// timing is the one thing the breadcrumb log can't reconstruct — it tells a
/// read-timeout (R2 stalled mid-transfer) apart from a connect-timeout
/// (couldn't reach it at all).
fn summarize_attempts(label: &str, history: &[(usize, String, f64)]) -> String {
    match history {
        [] => format!("{label}: unknown error"),
        [(_, cause, secs)] => format!("{label}: {cause} ({secs:.1}s)"),
        [(_, first_cause, _), rest @ ..] => {
            let n = history.len();
            if rest.iter().all(|(_, c, _)| c == first_cause) {
                let times = history
                    .iter()
                    .map(|(_, _, s)| format!("{s:.1}s"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{label} failed after {n} attempts: {first_cause} ({times})")
            } else {
                let parts = history
                    .iter()
                    .map(|(i, c, s)| format!("#{i} {c} ({s:.1}s)"))
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("{label} failed after {n} attempts: {parts}")
            }
        }
    }
}

/// Retry an upload step with exponential backoff, mirroring the web client's
/// `retry()` (clients/web/src/hooks/useUploader.ts): up to `MAX_RETRIES`
/// attempts, sleeping `RETRY_DELAYS_MS[i]` between them. The body must
/// evaluate to `Result<T, StepError>`; a `StepError::Terminal` short-circuits
/// without retrying, and the macro yields `Result<T, String>`.
///
/// Takes a `label` describing the step (carried into the final error) and the
/// attempt `block`. On exhaustion the error is the full per-attempt history
/// with timing (via [`summarize_attempts`]) rather than just the last failure
/// — that's what tells a steady outage apart from flapping connectivity. A
/// `Terminal` error is labelled (`{label}: {msg}`) and returned immediately.
///
/// Expanded inline (rather than a generic async helper) so the body can
/// freely borrow locals — an `FnMut` returning a borrowing future runs into
/// lifetime gymnastics that aren't worth it here.
macro_rules! retry_upload_step {
    ($label:expr, $attempt:block) => {{
        const MAX_RETRIES: usize = 3;
        const RETRY_DELAYS_MS: [u64; 3] = [2_000, 4_000, 8_000];
        let __label: String = ($label).to_string();
        let mut __attempt: usize = 0;
        let mut __history: Vec<(usize, String, f64)> = Vec::new();
        loop {
            let __start = tokio::time::Instant::now();
            match (async $attempt).await {
                Ok(__v) => break Ok::<_, String>(__v),
                Err(StepError::Terminal(__msg)) => break Err(format!("{__label}: {__msg}")),
                Err(StepError::Retryable(__msg)) => {
                    __history.push((__attempt + 1, __msg, __start.elapsed().as_secs_f64()));
                    if __attempt + 1 >= MAX_RETRIES {
                        break Err(summarize_attempts(&__label, &__history));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(
                        RETRY_DELAYS_MS[__attempt],
                    ))
                    .await;
                    __attempt += 1;
                }
            }
        }
    }};
}

/// Shared upload-and-confirm pipeline: get presigned URL, PUT to R2, POST
/// confirmation. Used by both `capture_and_upload` (screen/window) and
/// `upload_frame` (camera). Each network step is retried with exponential
/// backoff (see [`retry_upload_step`]).
///
/// `captured_at` is the ISO-8601 timestamp (in client clock) of when the
/// screenshot was actually taken. Optional — when `None`, the request
/// matches the legacy bucket-mode payload byte-for-byte. When `Some`, it
/// opts the session into credit-mode tracking on the first request.
///
/// Takes the JPEG as `bytes::Bytes` (cheap refcounted clones for retries —
/// no full-buffer copy per attempt) plus its base64 form, which is only
/// carried through for the JS preview. Callers that capture natively encode
/// base64 exactly once; nothing here decodes it back.
/// One capture unit ready for upload: the legacy single JPEG or an H.264
/// MP4 clip. The content type must match the granted format — the
/// presigned URL is signed with it.
struct UploadPayload {
    bytes: bytes::Bytes,
    content_type: &'static str,
    /// `format` query value for upload-url. None = legacy JPEG request.
    format: Option<&'static str>,
    /// Frames inside a clip (confirm-body telemetry). None for JPEG.
    frame_count: Option<u32>,
    width: u32,
    height: u32,
    /// JPEG preview (base64) of the unit's last frame, for the UI event.
    preview_base64: String,
}

impl UploadPayload {
    fn jpeg(bytes: bytes::Bytes, base64: String, width: u32, height: u32) -> Self {
        Self {
            bytes,
            content_type: "image/jpeg",
            format: None,
            frame_count: None,
            width,
            height,
            preview_base64: base64,
        }
    }

    fn mp4(clip: clips::FinishedClip, preview_base64: String) -> Self {
        Self {
            bytes: bytes::Bytes::from(clip.mp4),
            content_type: "video/mp4",
            format: Some("mp4"),
            frame_count: Some(clip.frame_count),
            width: clip.width,
            height: clip.height,
            preview_base64,
        }
    }
}

async fn upload_and_confirm(
    payload: UploadPayload,
    captured_at: Option<&str>,
    config: &SessionConfig,
    app: &AppHandle,
) -> Result<CaptureUploadResult, String> {
    let size_bytes = payload.bytes.len();
    const STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    // Step 1: Get presigned URL from server
    let _ = app.emit("capture-progress", "getting upload url from server...");
    let client = http_client();
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
    if let Some(f) = payload.format {
        query.push(("format", f));
    }
    // Each attempt re-requests a FRESH presigned URL (it has a 120s expiry).
    // Bracket the request on the local clock for the offset estimate below.
    // The bracket spans the whole retry block (matching the web SDK) — a
    // retried attempt inflates the window and the midpoint with it, but the
    // estimator smooths samples and only ever needs ±30s accuracy.
    let url_sent_at_ms = current_unix_ms();
    let upload_url_resp: UploadUrlResponse = retry_upload_step!("upload-url", {
        let url_response = client
            .get(upload_url_url.as_str())
            .query(&query)
            .timeout(STEP_TIMEOUT)
            .send()
            .await
            .map_err(|e| StepError::Retryable(describe_reqwest_error(&e)))?;
        let url_status = url_response.status();
        if !url_status.is_success() {
            let body = url_response.text().await.unwrap_or_default();
            Err(classify_http(url_status, http_error(url_status, &body)))
        } else {
            url_response
                .json::<UploadUrlResponse>()
                .await
                .map_err(|e| StepError::Retryable(describe_reqwest_error(&e)))
        }
    })?;
    let _ = app.emit(
        "capture-progress",
        format!(
            "got upload url, screenshot id: {}",
            upload_url_resp.screenshot_id
        ),
    );

    // Fold the server's clock into the offset estimate. One sample per
    // upload, for free — this is what keeps stamps and the tick schedule
    // honest on a machine whose system clock is wrong.
    if let Some(server_ms) = upload_url_resp
        .server_time
        .as_deref()
        .and_then(parse_iso_to_unix_ms)
    {
        let state = app.state::<AppState>();
        let mut offset = state.clock_offset.lock().unwrap();
        offset.observe(server_ms, url_sent_at_ms, current_unix_ms());
        if upload_url_resp.captured_at_adopted {
            // The server stamped that capture on arrival because our clock
            // was outside the trust envelope. Recording is intact; from the
            // next capture on, the estimate above corrects our stamps.
            let skew_s = offset.offset_ms() / 1000;
            eprintln!(
                "[clock] this machine's clock is ~{skew_s}s off the server's — \
                 the capture was saved with server time, and later captures \
                 are corrected automatically"
            );
            let _ = app.emit(
                "capture-progress",
                format!("system clock is ~{skew_s}s off — corrected automatically"),
            );
        }
    }

    // The presigned URL is signed for the GRANTED format's content type —
    // uploading a clip against a jpeg grant would fail the signature. A
    // downgrade here (clips disabled server-side, pre-clips server) is a
    // terminal error for this payload; the capture loop retries the tick
    // with its JPEG fallback.
    if let Some(requested) = payload.format {
        let granted = upload_url_resp.format.as_deref().unwrap_or("jpeg");
        if granted != requested {
            return Err(format!(
                "server granted \"{granted}\" for a \"{requested}\" clip upload"
            ));
        }
    }

    // Step 2: Upload JPEG to R2
    let _ = app.emit(
        "capture-progress",
        format!("uploading {}KB to R2...", size_bytes / 1024),
    );
    // Retried against the same presigned URL (still valid within its expiry).
    // Label carries the payload size and target host so an "r2 timed out"
    // report isolates the bucket/account and flags oversized frames.
    let r2_host = reqwest::Url::parse(&upload_url_resp.upload_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "r2".to_string());
    let r2_label = format!("r2-upload {}KB → {}", size_bytes / 1024, r2_host);
    retry_upload_step!(r2_label, {
        client
            .put(upload_url_resp.upload_url.as_str())
            .header("Content-Type", payload.content_type)
            // Bytes::clone is a refcount bump, not a buffer copy.
            .body(payload.bytes.clone())
            .timeout(STEP_TIMEOUT)
            .send()
            .await
            .map_err(|e| StepError::Retryable(describe_reqwest_error(&e)))?
            .error_for_status()
            .map_err(|e| StepError::Retryable(describe_reqwest_error(&e)))?;
        Ok(())
    })?;
    let _ = app.emit("capture-progress", "uploaded to R2 successfully");

    // Step 3: Confirm upload with server
    let _ = app.emit("capture-progress", "confirming upload with server...");
    let mut confirm_body = serde_json::json!({
        "screenshotId": upload_url_resp.screenshot_id,
        "width": payload.width,
        "height": payload.height,
        "fileSize": size_bytes,
    });
    if let Some(fc) = payload.frame_count {
        confirm_body["frameCount"] = fc.into();
    }
    let confirm_sent_at_ms = current_unix_ms();
    let confirm_resp: ConfirmResponse = retry_upload_step!("confirm", {
        let confirm_response = client
            .post(format!(
                "{}/api/sessions/{}/screenshots",
                config.api_base_url, config.token
            ))
            .json(&confirm_body)
            .timeout(STEP_TIMEOUT)
            .send()
            .await
            .map_err(|e| StepError::Retryable(describe_reqwest_error(&e)))?;
        let confirm_status = confirm_response.status();
        if !confirm_status.is_success() {
            let body = confirm_response.text().await.unwrap_or_default();
            Err(classify_http(confirm_status, http_error(confirm_status, &body)))
        } else {
            confirm_response
                .json::<ConfirmResponse>()
                .await
                .map_err(|e| StepError::Retryable(describe_reqwest_error(&e)))
        }
    })?;
    // Second free offset sample per upload, same bracketing as upload-url.
    if let Some(server_ms) = confirm_resp
        .server_time
        .as_deref()
        .and_then(parse_iso_to_unix_ms)
    {
        let state = app.state::<AppState>();
        state
            .clock_offset
            .lock()
            .unwrap()
            .observe(server_ms, confirm_sent_at_ms, current_unix_ms());
    }

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
        preview_base64: payload.preview_base64,
        preview_width: payload.width,
        preview_height: payload.height,
    })
}

#[cfg(test)]
mod retry_tests {
    use super::{classify_http, summarize_attempts, StepError};
    use std::cell::Cell;

    #[test]
    fn summarize_handles_edge_cases() {
        assert_eq!(summarize_attempts("upload-url", &[]), "upload-url: unknown error");
        // A lone failure is labelled and timed, no "failed after N" wrapper.
        assert_eq!(
            summarize_attempts("r2-upload", &[(1, "timed out".into(), 30.0)]),
            "r2-upload: timed out (30.0s)"
        );
    }

    #[test]
    fn summarize_collapses_identical_causes_with_all_times() {
        // Steady outage: one cause, every attempt's elapsed time listed.
        let history = [
            (1, "timed out".to_string(), 30.0),
            (2, "timed out".to_string(), 30.0),
            (3, "timed out".to_string(), 30.0),
        ];
        assert_eq!(
            summarize_attempts("r2-upload 84KB → acct.r2.dev", &history),
            "r2-upload 84KB → acct.r2.dev failed after 3 attempts: timed out (30.0s, 30.0s, 30.0s)"
        );
    }

    #[test]
    fn summarize_lists_distinct_causes_with_times() {
        // Flapping: each attempt's cause and elapsed time preserved.
        let history = [
            (1, "timed out".to_string(), 30.0),
            (2, "connection refused".to_string(), 1.2),
        ];
        assert_eq!(
            summarize_attempts("r2-upload", &history),
            "r2-upload failed after 2 attempts: #1 timed out (30.0s); #2 connection refused (1.2s)"
        );
    }

    #[test]
    fn classify_409_conflict_is_terminal() {
        // 409 = session paused/stopped server-side → must not retry.
        let e = classify_http(reqwest::StatusCode::CONFLICT, "paused".into());
        assert!(matches!(e, StepError::Terminal(_)));
    }

    #[test]
    fn classify_5xx_is_retryable() {
        let e = classify_http(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom".into());
        assert!(matches!(e, StepError::Retryable(_)));
    }

    #[test]
    fn classify_other_4xx_is_retryable() {
        // Only 409 is special; everything else (incl. 404) retries, matching
        // the web client.
        let e = classify_http(reqwest::StatusCode::NOT_FOUND, "missing".into());
        assert!(matches!(e, StepError::Retryable(_)));
    }

    // `start_paused` makes tokio auto-advance virtual time, so the backoff
    // sleeps complete instantly and we still exercise the real sleep path.

    // Under the paused clock no real time elapses between an attempt's start
    // and its failure, so per-attempt timing renders as "0.0s".

    #[tokio::test(start_paused = true)]
    async fn first_attempt_success_does_not_retry() {
        let attempts = Cell::new(0usize);
        let result: Result<&str, String> = retry_upload_step!("test", {
            attempts.set(attempts.get() + 1);
            Ok("ok")
        });
        assert_eq!(attempts.get(), 1);
        assert_eq!(result.unwrap(), "ok");
    }

    #[tokio::test(start_paused = true)]
    async fn retryable_failure_recovers_within_max_attempts() {
        let attempts = Cell::new(0usize);
        let result: Result<u32, String> = retry_upload_step!("test", {
            let n = attempts.get() + 1;
            attempts.set(n);
            if n < 3 {
                Err(StepError::Retryable(format!("transient {n}")))
            } else {
                Ok(42u32)
            }
        });
        assert_eq!(attempts.get(), 3);
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test(start_paused = true)]
    async fn retryable_failure_exhausts_after_three_attempts() {
        let attempts = Cell::new(0usize);
        let result: Result<(), String> = retry_upload_step!("upload-url", {
            attempts.set(attempts.get() + 1);
            Err::<(), StepError>(StepError::Retryable("timed out".into()))
        });
        // MAX_RETRIES = 3 → three attempts; identical causes collapse to one
        // line, led by the label, with every elapsed time listed.
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            result.unwrap_err(),
            "upload-url failed after 3 attempts: timed out (0.0s, 0.0s, 0.0s)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn distinct_failures_are_all_listed() {
        // A flapping failure must stay distinguishable from a steady outage:
        // each attempt's cause is preserved in the summary.
        let attempts = Cell::new(0usize);
        let result: Result<(), String> = retry_upload_step!("r2-upload", {
            let n = attempts.get() + 1;
            attempts.set(n);
            Err::<(), StepError>(StepError::Retryable(format!("cause {n}")))
        });
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            result.unwrap_err(),
            "r2-upload failed after 3 attempts: #1 cause 1 (0.0s); #2 cause 2 (0.0s); #3 cause 3 (0.0s)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_failure_short_circuits_without_retry() {
        let attempts = Cell::new(0usize);
        let result: Result<(), String> = retry_upload_step!("confirm", {
            attempts.set(attempts.get() + 1);
            Err::<(), StepError>(StepError::Terminal("paused".into()))
        });
        assert_eq!(attempts.get(), 1);
        // Terminal errors are labelled too, so the step is never lost.
        assert_eq!(result.unwrap_err(), "confirm: paused");
    }
}

/// Build an ISO-8601 timestamp in UTC for the current instant, corrected
/// into server time by the running clock-offset estimate. Used as the
/// client-attested `capturedAt` on upload requests when credit mode is on.
/// A no-op for a healthy clock; for a skewed one it's the difference between
/// every capture landing in the ±30s credit window and none of them doing so.
fn captured_at_now(app: &AppHandle) -> String {
    let corrected_ms = {
        let state = app.state::<AppState>();
        let offset = state.clock_offset.lock().unwrap();
        offset.correct(current_unix_ms())
    };
    unix_ms_to_iso(corrected_ms)
}

/// Format milliseconds-since-epoch as `YYYY-MM-DDTHH:MM:SS.sssZ` — what
/// `Date.parse()` and Go's `time.Parse(time.RFC3339)` both accept without
/// surprises.
fn unix_ms_to_iso(unix_ms: i64) -> String {
    let total_secs = unix_ms.div_euclid(1_000);
    let millis = unix_ms.rem_euclid(1_000) as u32;

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

    // Screen capture + JPEG encode is heavy blocking work — keep it off the
    // async runtime's worker threads (same as the Rust capture loop does).
    let screenshot = tokio::task::spawn_blocking(move || {
        capture::take_stitched_screenshots_raw_with_blacklist(
            &sources,
            max_width,
            max_height,
            jpeg_quality,
            &pipewire_fds,
            &blacklisted,
        )
    })
    .await
    .map_err(|e| format!("spawn_blocking panicked: {e}"))??;
    let _ = app.emit(
        "capture-progress",
        format!(
            "captured {}x{} ({}KB jpeg)",
            screenshot.width,
            screenshot.height,
            screenshot.data.len() / 1024
        ),
    );

    let captured_at = if ENABLE_CREDIT_MODE {
        Some(captured_at_now(&app))
    } else {
        None
    };
    let jpeg_base64 = base64_encode(&screenshot.data);
    upload_and_confirm(
        UploadPayload::jpeg(
            bytes::Bytes::from(screenshot.data),
            jpeg_base64,
            screenshot.width,
            screenshot.height,
        ),
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
        Some(captured_at_now(&app))
    } else {
        None
    };
    let jpeg_bytes = bytes::Bytes::from(base64_decode(&base64)?);
    upload_and_confirm(
        UploadPayload::jpeg(jpeg_bytes, base64, width, height),
        captured_at.as_deref(),
        &config,
        &app,
    )
    .await
}

// ── Capture-loop interval (seconds) ─────────────────────────────
const CAPTURE_INTERVAL_SECS: u64 = 60;
/// If the wall-clock gap between ticks exceeds this, the machine
/// probably slept (or the WebView was throttled hard).
const SLEEP_THRESHOLD_SECS: u64 = CAPTURE_INTERVAL_SECS * 2 + 30; // 150s
/// Fallback frame cadence when the server doesn't advertise one (pre-clips
/// servers): every 10s = 6 frames/min. Mirrors CLIP_FRAME_INTERVAL_MS in
/// @lookout/shared. When the server sends `frameIntervalMs` on the session
/// GET, that value wins — the cadence is server-authoritative. Frames go
/// through the identical redaction-aware capture path as uploads; in clips
/// mode they're recorded into the clip, and the JPEG preview side is only
/// produced while the window is focused.
const DEFAULT_FRAME_INTERVAL_MS: u64 = 10_000;

/// Delay from capture start to the FIRST upload tick. Mirrors
/// CLIP_FIRST_CUT_DELAY_MS in @lookout/shared.
///
/// Deliberately not a multiple of the frame cadence: the opening clip is the
/// session's seed capture, which credits 0 seconds and which the compiler
/// drops from the video outright, so its frame density doesn't matter. What
/// this delay does control is how long the user stares at an unstarted
/// session — and tying it to the cadence turned every slower cadence into a
/// 20-second-plus wait.
const CLIP_FIRST_CUT_DELAY_MS: u64 = 8_000;

/// Consecutive clip-encoder failures tolerated before this capture run gives
/// up on clips and records plain JPEGs for the rest of the session.
///
/// A broken encoder is already survivable one interval at a time (each
/// failure falls back to a JPEG), but "survivable" was not the same as
/// "quiet": on a machine where the encoder can never initialize, the loop
/// retried it on every single frame — for hours — each attempt paying the
/// full cost of constructing and tearing down an OS encoder, and writing a
/// line to stderr. Latching off after a few consecutive failures keeps the
/// recording intact and stops the thrash.
const MAX_CLIP_ENCODER_FAILURES: u32 = 3;

/// Max seconds the menu-bar time may run ahead of the last server-credited
/// `tracked_seconds`. Must equal `MAX_INTERPOLATION_S` in
/// @lookout/react's useSessionTimer — one capture interval. Without the cap
/// the menu bar kept counting through a capture stall while the main window
/// froze at base + 60, and the two never reconverged.
const MAX_TRAY_INTERPOLATION_SECS: i64 = CAPTURE_INTERVAL_SECS as i64;

/// The Rust mirror of `deriveDisplaySeconds` in @lookout/react. Keep the two
/// in step: the menu bar and the main window each tick their own clock, so any
/// difference here is directly visible as the two showing different times.
fn tray_display_seconds(base_seconds: i64, elapsed_secs: i64, running: bool) -> i64 {
    if !running {
        // Paused drops the interpolated remainder rather than freezing it,
        // matching the main window's snap-down.
        return base_seconds;
    }
    base_seconds + elapsed_secs.clamp(0, MAX_TRAY_INTERPOLATION_SECS)
}

/// Format seconds into a clock-style tray title:
/// >0h: "{h}:{mm:02}:{ss:02}", else: "{mm:02}:{ss:02}"
fn format_tray_time(total_seconds: i64) -> String {
    let total = total_seconds.max(0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
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

    // The title only changes at minute granularity, so most 1s ticks would
    // rewrite the exact same string. Cache the last text and skip redundant
    // native tray updates. After a paused stretch the JS side may have
    // overwritten the title (paused indicator), so force one refresh on the
    // first running tick after a pause even if the text matches.
    let mut last_title: Option<String> = None;
    let mut was_paused = false;

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = cancel_rx.changed() => {
                eprintln!("[tray-timer] cancelled");
                break;
            }
        }

        let base_seconds = timer_state.tracked_seconds.load(Ordering::Relaxed);
        let running = timer_state.is_running.load(Ordering::Relaxed);

        let elapsed = {
            let started = timer_state.started_at.lock().unwrap();
            started.elapsed().as_secs() as i64
        };
        let display_seconds = tray_display_seconds(base_seconds, elapsed, running);

        let time_text = format_tray_time(display_seconds);

        if !running {
            // Write the frozen value once (a pause snaps the title down by
            // the dropped remainder), then idle until resume.
            if last_title.as_deref() != Some(time_text.as_str()) {
                set_tray_title(&app, &time_text, true);
                last_title = Some(time_text);
            }
            was_paused = true;
            continue;
        }

        if was_paused || last_title.as_deref() != Some(time_text.as_str()) {
            set_tray_title(&app, &time_text, false);
            last_title = Some(time_text);
        }
        was_paused = false;
    }
}

/// Write the menu-bar time text (and, off macOS, the hover tooltip).
fn set_tray_title(app: &AppHandle, time_text: &str, paused: bool) {
    #[cfg(target_os = "linux")]
    crate::gnome_indicator::publish_tick(time_text, paused);

    #[cfg(not(target_os = "linux"))]
    let _ = paused;

    #[cfg(target_os = "macos")]
    {
        let _ = app;
        // None = keep the current pause state; the Swift side renders the
        // tooltip and the numericText digit roll.
        let _ = crate::native_tray::update(time_text, None);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(tray) = app.tray_by_id("timelapse_tray") {
        let _ = tray.set_title(Some(time_text));
        // Windows doesn't render tray titles — the hover tooltip is the
        // only way to see the recorded time there.
        let _ = tray.set_tooltip(Some(format!("Lookout — {time_text} recorded")));
    }
}

/// State of the GNOME top-bar pill, for the Linux settings row. Registered on
/// every platform because the handler list is; reports `supported: false`
/// wherever the pill can't exist.
#[tauri::command]
fn gnome_indicator_status() -> serde_json::Value {
    #[cfg(target_os = "linux")]
    return serde_json::to_value(crate::gnome_indicator::status()).unwrap_or_default();

    #[cfg(not(target_os = "linux"))]
    serde_json::json!({
        "supported": false,
        "installed": false,
        "enabled": false,
        "attached": false,
    })
}

/// Install the shell extension that draws the pill, and enable it.
#[tauri::command]
fn install_gnome_indicator() -> Result<serde_json::Value, String> {
    #[cfg(target_os = "linux")]
    return crate::gnome_indicator::install()
        .map(|status| serde_json::to_value(status).unwrap_or_default());

    #[cfg(not(target_os = "linux"))]
    Err("the GNOME top-bar indicator is only available on Linux".into())
}

/// Start the tray timer (if not already running). Returns the shared state
/// so the capture loop can sync `tracked_seconds` into it.
fn start_tray_timer(app: &AppHandle, state: &AppState) -> Arc<TrayTimerState> {
    let mut guard = state.tray_timer.lock().unwrap();

    // If already running, just return the existing state handle
    if let Some(ref handle) = *guard {
        return Arc::clone(&handle.state);
    }

    // The tray timer lives exactly as long as a session is being recorded
    // (screen sessions via start_capture_loop, camera via start_tray_ticker),
    // so it's the right scope for the keep-awake assertion.
    #[cfg(target_os = "macos")]
    power::begin_recording_assertion();

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

        // Recording is over — let macOS nap/idle-sleep normally again.
        #[cfg(target_os = "macos")]
        power::end_recording_assertion();
    }
}

/// Ratchet `tracked_seconds` to a new authoritative value, re-anchoring the
/// elapsed counter **only if the value actually advanced**.
///
/// Both halves matter for staying in step with the main window:
///   - Ratchet: an idempotent retry can confirm against a stale read and
///     return a *lower* `trackedSeconds`. JS keeps the higher value, so
///     storing the lower one here made the menu bar jump backwards and sit
///     a minute behind until the next credit.
///   - Anchor only on advance: a repeated reading must not restart the
///     interpolation window, or the menu bar loses time the main window keeps.
fn ratchet_tray_tracked_seconds(timer_state: &TrayTimerState, tracked_seconds: i64) {
    let prev = timer_state
        .tracked_seconds
        .fetch_max(tracked_seconds, Ordering::Relaxed);
    if tracked_seconds > prev {
        let mut started = timer_state.started_at.lock().unwrap();
        *started = StdInstant::now();
    }
}

/// Sync the tray timer to a new authoritative tracked_seconds value
/// (typically from a capture result).
fn sync_tray_timer(state: &AppState, tracked_seconds: i64) {
    let guard = state.tray_timer.lock().unwrap();
    if let Some(ref handle) = *guard {
        ratchet_tray_tracked_seconds(&handle.state, tracked_seconds);
    }
}

/// Pause the tray timer. The next tick drops the interpolated remainder and
/// shows the bare `tracked_seconds`, matching the main window's snap-down.
fn pause_tray_timer(state: &AppState) {
    let guard = state.tray_timer.lock().unwrap();
    if let Some(ref handle) = *guard {
        handle.state.is_running.store(false, Ordering::Relaxed);
    }
}

/// Resume the tray timer. Re-anchors the elapsed counter so it continues
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

/// Event payload for an in-between live-preview frame from the capture
/// loop (one per frame interval while the window is focused).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapturePreviewFrame {
    preview_base64: String,
    preview_width: u32,
    preview_height: u32,
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

/// What JPEG (if any) a frame grab should produce alongside the raw image.
#[derive(Clone, Copy, PartialEq)]
enum GrabJpeg {
    /// No JPEG — clip frame while the window is unfocused.
    None,
    /// Preview-sized (≤854x480, q65) — matches the resolution the live
    /// preview always used, and keeps the per-frame IPC payload ~5x
    /// smaller than a full-res frame would be.
    Preview,
    /// Full capture resolution at upload quality — the tick frame, which
    /// doubles as the JPEG upload/fallback payload.
    Full,
}

/// Downscale bounds + quality for preview JPEGs (mirrors the values the
/// dedicated preview protocol always served).
const PREVIEW_MAX_W: u32 = 854;
const PREVIEW_MAX_H: u32 = 480;
const PREVIEW_JPEG_QUALITY: u8 = 65;

/// One frame off the capture pipeline: the raw (redacted, scaled) image
/// plus, when requested, its JPEG encoding.
struct FrameGrab {
    image: image::DynamicImage,
    jpeg: Option<capture::RawCaptureResult>,
}

/// Read the current blacklist (+ Linux PipeWire fds) and capture one
/// redaction-aware stitched frame on the blocking pool. Shared by the
/// upload tick, the clip frames, and the live preview, so everything goes
/// through the exact same capture path — Filtered Apps redaction included.
async fn grab_frame(
    app: &AppHandle,
    sources: &[CaptureSource],
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    jpeg: GrabJpeg,
) -> Result<FrameGrab, String> {
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

    let sources_clone = sources.to_vec();
    tokio::task::spawn_blocking(move || {
        let image = capture::take_stitched_screenshots_image_with_blacklist(
            &sources_clone,
            max_width,
            max_height,
            &pipewire_fds,
            &blacklisted,
        )?;
        let encoded = match jpeg {
            GrabJpeg::None => None,
            GrabJpeg::Full => Some(capture::encode_frame_jpeg(&image, jpeg_quality)?),
            GrabJpeg::Preview => {
                let (w, h) = (image.width(), image.height());
                if w > PREVIEW_MAX_W || h > PREVIEW_MAX_H {
                    let scale =
                        f64::min(PREVIEW_MAX_W as f64 / w as f64, PREVIEW_MAX_H as f64 / h as f64);
                    let pw = ((w as f64 * scale).round() as u32).max(2);
                    let ph = ((h as f64 * scale).round() as u32).max(2);
                    // Borrowing resize: the full-res frame stays untouched
                    // for the clip encoder.
                    image
                        .as_rgba8()
                        .and_then(|rgba| capture::fast_resize_buffer(rgba, pw, ph))
                        .map(|small| {
                            capture::encode_frame_jpeg(
                                &image::DynamicImage::ImageRgba8(small),
                                PREVIEW_JPEG_QUALITY,
                            )
                        })
                        .transpose()?
                } else {
                    Some(capture::encode_frame_jpeg(&image, PREVIEW_JPEG_QUALITY)?)
                }
            }
        };
        Ok(FrameGrab {
            image,
            jpeg: encoded,
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking panicked: {e}"))
    .and_then(|r| r)
}

/// Record one clip-encoder failure, and latch clips off for the rest of the
/// run once they stop looking transient.
///
/// The recording itself is never at risk either way — every clip failure
/// already falls back to a JPEG for that interval. This is about not
/// re-attempting a hopeless encoder several times a minute for hours. Any
/// clip that finalizes successfully resets the counter, so a one-off
/// hiccup (a display mode change, a busy GPU) never disables clips.
fn note_clip_failure(failures: &mut u32, clips_mode: &mut bool) {
    *failures += 1;
    if *failures >= MAX_CLIP_ENCODER_FAILURES && *clips_mode {
        *clips_mode = false;
        eprintln!(
            "[capture-loop] {} consecutive clip-encoder failures — disabling clips \
             for this session, continuing with one JPEG per minute",
            *failures
        );
    }
}

/// Clip capability the server advertises for a session (on the session
/// GET). Fetched once at capture-loop start; any failure means clips off,
/// i.e. legacy one-JPEG-per-minute behavior.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SessionClipCapabilities {
    #[serde(default)]
    clips_enabled: bool,
    #[serde(default)]
    frame_interval_ms: Option<u64>,
}

async fn fetch_clip_capabilities(config: &SessionConfig) -> SessionClipCapabilities {
    let url = format!("{}/api/sessions/{}", config.api_base_url, config.token);
    match http_client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => res.json().await.unwrap_or_default(),
        Ok(res) => {
            eprintln!(
                "[capture-loop] capability fetch returned HTTP {} — clips off",
                res.status()
            );
            SessionClipCapabilities::default()
        }
        Err(e) => {
            eprintln!("[capture-loop] capability fetch failed ({e}) — clips off");
            SessionClipCapabilities::default()
        }
    }
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
        let client = http_client();
        let status_timeout = std::time::Duration::from_secs(15);
        let url = format!("{}/api/sessions/{}/status", config.api_base_url, config.token);
        match client.get(&url).timeout(status_timeout).send().await {
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
                        let _ = client.post(&resume_url).timeout(status_timeout).send().await;
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

    /// Apply a finished upload's outcome: sync the tray timer, refine the
    /// next tick target from the server's `nextExpectedAt`, emit the UI
    /// events, and run pause/termination recovery on failure. Returns false
    /// when the capture loop should stop (terminal session state).
    async fn apply_upload_result(
        app: &AppHandle,
        config: &SessionConfig,
        result: Result<CaptureUploadResult, String>,
        next_fire: &mut tokio::time::Instant,
        interval_dur: tokio::time::Duration,
    ) -> bool {
        match result {
            Ok(result) => {
                // Sync tray timer to authoritative server time
                {
                    let state = app.state::<AppState>();
                    sync_tray_timer(&state, result.tracked_seconds);
                }
                // Compute next fire from the server-provided nextExpectedAt.
                // If parsing fails or the target is in the past, default to
                // "fire now" (catch-up). Upper-bounded at 2x interval as a
                // guard against malformed responses.
                //
                // The target is SERVER wall-clock, so subtract our estimate
                // of the server's now — not the raw local clock. Raw local
                // time baked the machine's clock skew into every delay:
                // >30s of skew pushed every capture out of the credit
                // window (trackedSeconds stuck at 0), and >60s pinned the
                // delay at the clamp below, halving the capture rate and
                // with it the compiled video's length.
                let parsed_target_ms = parse_iso_to_unix_ms(&result.next_expected_at);
                let now_ms = {
                    let state = app.state::<AppState>();
                    let offset = state.clock_offset.lock().unwrap();
                    offset.correct(current_unix_ms())
                };
                let delay_ms = match parsed_target_ms {
                    Some(target) => (target - now_ms).max(0) as u64,
                    None => CAPTURE_INTERVAL_SECS * 1000,
                };
                let clamp_ms = CAPTURE_INTERVAL_SECS * 2 * 1000;
                if delay_ms > clamp_ms {
                    // With the offset applied this should never bind for
                    // clock skew — if it fires, something else is feeding us
                    // bad targets, and silence here is how the skew bug ran
                    // unnoticed for three months.
                    eprintln!(
                        "[capture-loop] next-capture delay {delay_ms}ms exceeds \
                         the 2x-interval clamp — capping to {clamp_ms}ms"
                    );
                }
                let delay_ms = delay_ms.min(clamp_ms);
                *next_fire =
                    tokio::time::Instant::now() + tokio::time::Duration::from_millis(delay_ms);
                let _ = app.emit("capture-tick-result", CaptureTickResult::from(result));
                true
            }
            Err(e) => {
                eprintln!("[capture-loop] upload failed: {e}");
                let _ = app.emit(
                    "capture-tick-error",
                    CaptureTickError { message: e.clone() },
                );
                // No server target available — fall back to a full interval.
                *next_fire = tokio::time::Instant::now() + interval_dur;
                // Check if the server paused/stopped the session
                match handle_sleep_recovery(app, config).await {
                    Ok(true) => true,
                    Ok(false) => false,
                    Err(_) => true,
                }
            }
        }
    }

    // Clip capability comes from the server, once per loop run. Any fetch
    // failure (or clips off) means legacy JPEG mode, bit-for-bit.
    let initial_config = {
        let state = app.state::<AppState>();
        let guard = state.config.lock().unwrap();
        guard.clone()
    };
    let caps = match &initial_config {
        Some(c) => fetch_clip_capabilities(c).await,
        None => SessionClipCapabilities::default(),
    };
    // Mutable: latches off after MAX_CLIP_ENCODER_FAILURES consecutive
    // encoder failures, so a machine with a broken encoder settles into
    // plain JPEG mode instead of retrying forever.
    let mut clips_mode = caps.clips_enabled;
    let mut clip_encoder_failures: u32 = 0;
    // Server-authoritative cadence, clamped defensively against a
    // misbehaving server so the loop can't spin or stall.
    let frame_interval_ms = caps
        .frame_interval_ms
        .unwrap_or(DEFAULT_FRAME_INTERVAL_MS)
        .clamp(500, 30_000);
    let frame_dur = Duration::from_millis(frame_interval_ms);
    let mut recorder: Option<clips::ClipRecorder> = None;
    if clips_mode {
        eprintln!("[capture-loop] clips enabled (frame every {frame_interval_ms}ms)");
    }

    // Clips: hold the first upload back so the opening clip has a few frames
    // and the session activates promptly. Fixed delay, NOT a multiple of the
    // cadence — see CLIP_FIRST_CUT_DELAY_MS. JPEG mode keeps the legacy
    // immediate first tick.
    next_fire = TokioInstant::now()
        + if clips_mode {
            Duration::from_millis(CLIP_FIRST_CUT_DELAY_MS)
        } else {
            Duration::ZERO
        };

    // The opening window is shorter than one frame interval, so at the normal
    // cadence the first clip would hold a single frame. Capture it densely
    // enough to carry a handful; after the first upload the cadence returns
    // to the server's value.
    let opening_frame_dur = Duration::from_millis((CLIP_FIRST_CUT_DELAY_MS / 4).max(500));
    let mut first_upload_done = false;

    // The in-flight upload, if any. Uploads run CONCURRENTLY with frame
    // capture: a multi-second clip finalize+upload must not punch a hole in
    // the recording every minute — serially that compounds to minutes of
    // missing screen time per hour. Strictly one upload at a time: the next
    // tick settles the previous one before cutting, which preserves
    // capturedAt monotonicity and the per-session rate-limit assumptions.
    let mut upload_handle: Option<tokio::task::JoinHandle<Result<CaptureUploadResult, String>>> =
        None;
    let mut upload_cfg: Option<SessionConfig> = None;

    'outer: loop {
        // ── Wait until next_fire, collecting frames along the way ──
        // Frames run at the clip cadence (server-set, 6/min) through the
        // SAME redaction-aware capture path as uploads. In clips mode every
        // frame is recorded into the current clip; the JPEG preview side
        // is focus-gated either way (nobody can see it unfocused).
        // sleep_until returns immediately when next_fire is already past
        // (catch-up), which also skips frame collection.
        let cadence = if first_upload_done {
            frame_dur
        } else {
            opening_frame_dur
        };
        loop {
            let now = TokioInstant::now();
            if now >= next_fire {
                break;
            }
            let wake = std::cmp::min(now + cadence, next_fire);
            // Third arm: the in-flight upload finishing mid-wait. Its body
            // only records the outcome — applying it (which needs mutable
            // access to upload_handle/next_fire) happens after the select.
            let mut upload_outcome: Option<Result<CaptureUploadResult, String>> = None;
            tokio::select! {
                _ = sleep_until(wake) => {}
                _ = cancel_rx.changed() => {
                    eprintln!("[capture-loop] cancelled");
                    break 'outer;
                }
                res = async {
                    match upload_handle.as_mut() {
                        Some(h) => match h.await {
                            Ok(r) => r,
                            Err(e) => Err(format!("upload task panicked: {e}")),
                        },
                        None => unreachable!("guarded by select condition"),
                    }
                }, if upload_handle.is_some() => {
                    upload_outcome = Some(res);
                }
            }
            if let Some(res) = upload_outcome {
                upload_handle = None;
                let cfg = upload_cfg.take().expect("cfg tracks upload_handle");
                if !apply_upload_result(&app, &cfg, res, &mut next_fire, interval_dur).await {
                    break 'outer;
                }
                // next_fire was just refined by the confirm — recompute the
                // wake target instead of falling through with a stale one.
                continue;
            }
            // Woke for the upload tick, not a frame.
            if TokioInstant::now() >= next_fire {
                break;
            }

            let focused = app
                .get_webview_window("main")
                .map(|w| w.is_focused().unwrap_or(false))
                .unwrap_or(false);
            if !clips_mode && !focused {
                continue;
            }

            let jpeg_mode = if focused {
                GrabJpeg::Preview
            } else {
                GrabJpeg::None
            };
            match grab_frame(&app, &sources, max_width, max_height, jpeg_quality, jpeg_mode).await
            {
                Ok(grab) => {
                    if clips_mode {
                        if recorder.is_none() {
                            match clips::ClipRecorder::new(
                                grab.image.width(),
                                grab.image.height(),
                                frame_interval_ms,
                            ) {
                                Ok(r) => recorder = Some(r),
                                Err(e) => {
                                    eprintln!(
                                        "[capture-loop] clip encoder init failed: {e} — JPEG fallback this interval"
                                    );
                                    note_clip_failure(
                                        &mut clip_encoder_failures,
                                        &mut clips_mode,
                                    );
                                }
                            }
                        }
                        if let Some(r) = recorder.as_mut() {
                            if let Err(e) = r.push_frame(&grab.image) {
                                eprintln!(
                                    "[capture-loop] clip frame append failed: {e} — dropping clip, JPEG fallback"
                                );
                                if let Some(r) = recorder.take() {
                                    r.discard();
                                }
                                note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                            }
                        }
                    }
                    if focused {
                        if let Some(jpeg) = grab.jpeg {
                            let _ = app.emit(
                                "capture-preview-frame",
                                CapturePreviewFrame {
                                    preview_base64: base64_encode(&jpeg.data),
                                    preview_width: jpeg.width,
                                    preview_height: jpeg.height,
                                },
                            );
                        }
                    }
                }
                Err(e) => {
                    // Frame-level failure: log and keep going — the upload
                    // tick has its own error handling and retry cadence.
                    eprintln!("[capture-loop] frame capture failed: {e}");
                }
            }
        }

        // ── Upload tick ──
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
            // A clip spanning a sleep gap would carry an hours-long hole —
            // drop it and start fresh after recovery.
            if let Some(r) = recorder.take() {
                r.discard();
            }
            match handle_sleep_recovery(&app, &config).await {
                Ok(true) => { /* continue capturing */ }
                Ok(false) => break,
                Err(_) => { /* best effort, continue */ }
            }
        }

        // A previous upload still in flight (very slow network): settle it
        // before cutting the next clip so uploads stay strictly ordered —
        // capturedAt monotonicity and the per-session rate limits both
        // assume order.
        if let Some(handle) = upload_handle.take() {
            let cfg = upload_cfg.take().expect("cfg tracks upload_handle");
            let res = match handle.await {
                Ok(r) => r,
                Err(e) => Err(format!("upload task panicked: {e}")),
            };
            if !apply_upload_result(&app, &cfg, res, &mut next_fire, interval_dur).await {
                break;
            }
        }

        // Grab the tick frame — the clip's final frame, the UI preview,
        // and the JPEG fallback, all from one capture. Full-size JPEG:
        // this one may be uploaded.
        let grab_result =
            grab_frame(&app, &sources, max_width, max_height, jpeg_quality, GrabJpeg::Full).await;

        // Capture the wall-clock moment NOW — that's the value we'll send
        // as `capturedAt`, not when the upload eventually reaches the server.
        let captured_at = if ENABLE_CREDIT_MODE {
            Some(captured_at_now(&app))
        } else {
            None
        };

        match grab_result {
            Ok(grab) => {
                let capture::RawCaptureResult {
                    data: jpeg_data,
                    width: jpeg_w,
                    height: jpeg_h,
                } = grab.jpeg.expect("tick grab always requests jpeg");
                let jpeg_base64 = base64_encode(&jpeg_data);
                let jpeg_bytes = bytes::Bytes::from(jpeg_data);

                // Clips: append the final frame and cut this interval's clip.
                let clip = if clips_mode {
                    if recorder.is_none() {
                        recorder = clips::ClipRecorder::new(
                            grab.image.width(),
                            grab.image.height(),
                            frame_interval_ms,
                        )
                        .map_err(|e| {
                            eprintln!("[capture-loop] clip encoder init failed: {e}");
                            note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                        })
                        .ok();
                    }
                    if let Some(r) = recorder.as_mut() {
                        if let Err(e) = r.push_frame(&grab.image) {
                            eprintln!("[capture-loop] clip frame append failed: {e}");
                            if let Some(r) = recorder.take() {
                                r.discard();
                            }
                            note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                        }
                    }
                    match recorder.take().map(|r| r.finish()) {
                        Some(Ok(c)) => {
                            // A clip made it out whole — the encoder works,
                            // so earlier failures were transient.
                            clip_encoder_failures = 0;
                            Some(c)
                        }
                        Some(Err(e)) => {
                            eprintln!(
                                "[capture-loop] clip finalize failed: {e} — uploading JPEG instead"
                            );
                            note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                            None
                        }
                        None => None,
                    }
                } else {
                    None
                };

                // Spawn the upload as a background task — frame capture for
                // the NEXT clip resumes immediately instead of stalling for
                // the finalize+upload round trip (which would put a hole in
                // the recording every minute). Clip first; ANY clip-upload
                // failure (size cap, server downgrade, transient) retries
                // the tick as a JPEG so the credit streak never skips a
                // beat.
                let task_app = app.clone();
                let task_config = config.clone();
                let task_captured_at = captured_at.clone();
                upload_handle = Some(tokio::spawn(async move {
                    let jpeg_fallback =
                        UploadPayload::jpeg(jpeg_bytes, jpeg_base64.clone(), jpeg_w, jpeg_h);
                    match clip {
                        Some(c) => {
                            match upload_and_confirm(
                                UploadPayload::mp4(c, jpeg_base64),
                                task_captured_at.as_deref(),
                                &task_config,
                                &task_app,
                            )
                            .await
                            {
                                Ok(r) => Ok(r),
                                Err(e) => {
                                    eprintln!(
                                        "[capture-loop] clip upload failed ({e}) — retrying tick as JPEG"
                                    );
                                    upload_and_confirm(
                                        jpeg_fallback,
                                        task_captured_at.as_deref(),
                                        &task_config,
                                        &task_app,
                                    )
                                    .await
                                }
                            }
                        }
                        None => {
                            upload_and_confirm(
                                jpeg_fallback,
                                task_captured_at.as_deref(),
                                &task_config,
                                &task_app,
                            )
                            .await
                        }
                    }
                }));
                upload_cfg = Some(config.clone());
                // Provisional next tick one interval out; refined to the
                // server's nextExpectedAt when the confirm lands mid-wait
                // (see the wait-loop's third select arm).
                next_fire = TokioInstant::now() + interval_dur;
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

        // Whatever happened, the opening window is over — later intervals
        // are full-length, so the normal cadence applies (a failed first
        // upload must not run the fast cadence across a 60s retry window).
        first_upload_done = true;
    }

    // Never leave a half-recorded clip (or its temp file) behind on
    // pause/stop/cancel.
    if let Some(r) = recorder.take() {
        r.discard();
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
    // Ratchet, don't store: `start_tray_timer` returns the *existing* state
    // if a session is already being tracked, and a re-entrant call with a
    // stale (or zero) baseline would knock the menu bar backwards.
    ratchet_tray_tracked_seconds(&timer_state, tracked_seconds);
    {
        let mut started = timer_state.started_at.lock().unwrap();
        *started = StdInstant::now();
    }
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
    sync_tray_timer(&state, tracked_seconds);
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

fn base64_encode(data: &[u8]) -> String {
    use base64_engine::*;
    ENGINE.encode(data)
}

mod base64_engine {
    pub use base64::engine::general_purpose::STANDARD as ENGINE;
    pub use base64::Engine;
}

/// Whether NVIDIA's proprietary driver is loaded.
///
/// Both paths are created by that driver and by nothing else, so their
/// absence is a reliable "not NVIDIA" — nouveau, AMD and Intel never
/// produce them. Reading a path rather than shelling out keeps this cheap
/// enough to run on the startup path before the webview exists.
#[cfg(target_os = "linux")]
fn nvidia_proprietary_driver_loaded() -> bool {
    std::path::Path::new("/proc/driver/nvidia/version").exists()
        || std::path::Path::new("/sys/module/nvidia/version").exists()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's accelerated compositing crashes on launch under Wayland with
    // some drivers (notably NVIDIA), bailing out with
    // "Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display."
    // Disabling compositing avoids the crash.
    //
    // It is not free, though: without accelerated compositing WebKitGTK
    // falls back to a software path, and video goes through it too — which
    // is why a finished timelapse played back blocky and smeared on Linux
    // while every other surface looked fine. So this is narrowed to the
    // drivers that actually need it rather than all of Wayland; everyone
    // else keeps GPU compositing and gets video that looks like video.
    //
    // Anyone who does hit the crash on another driver still has the escape
    // hatch, since a value the user set is always left alone:
    //     WEBKIT_DISABLE_COMPOSITING_MODE=1 lookout
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none()
        && nvidia_proprietary_driver_loaded()
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
        // Tell the webview whether it should draw its own window frame, before
        // it paints anything. index.html keys its first-paint styles on this
        // (the frame has to be right on the very first frame, or every launch
        // flashes), so it cannot be an IPC call — the answer has to already be
        // in the page when our own scripts start. `js_init_script` runs at
        // document start, which is the one hook early enough.
        .plugin(
            tauri::plugin::Builder::new("lookout-window-frame")
                .js_init_script(format!(
                    "window.__LOOKOUT_SHELL_DRAWS_FRAME__ = {};",
                    desktop_appearance::shell_draws_window_frame(),
                ))
                .build(),
        )
        .manage(AppState {
            config: Mutex::new(None),
            cold_start_urls: Mutex::new(None),
            #[cfg(target_os = "linux")]
            pipewire_fds: Mutex::new(std::collections::HashMap::new()),
            blacklisted_apps: Mutex::new(Vec::new()),
            capture_loop: Mutex::new(None),
            tray_timer: Mutex::new(None),
            clock_offset: Mutex::new(clock_offset::ClockOffset::new()),
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
            capture_diagnostics::capture_environment,
            desktop_appearance::desktop_appearance,
            window_shape::sync_window_frame,
            open_external_url,
            native_menu::show_add_menu,
            native_menu::prefetch_add_menu_icons,
            request_screencast,
            add_screencast,
            set_blacklisted_apps,
            get_blacklisted_apps,
            list_installed_apps,
            get_app_icon,
            tray::show_tray,
            tray::update_tray_time,
            tray::hide_tray,
            tray::tray_action,
            tray::set_tray_state,
            tray::get_tray_state,
            gnome_indicator_status,
            install_gnome_indicator,
        ])
        .manage(tray::TrayStateMutex(std::sync::Mutex::new(tray::TrayState::default())))
        .setup(|app| {
            // Warm the installed-app cache off-thread so the first visit to
            // Filtered Apps is instant rather than paying for the scan.
            prewarm_installed_apps();

            // Export the recording state for the GNOME pill extension. Starts
            // regardless of whether the extension is installed — it connects
            // whenever it appears, and nothing else reads this.
            #[cfg(target_os = "linux")]
            gnome_indicator::start(app.handle().clone());

            #[cfg(target_os = "macos")]
            {
                // NOTE: App Nap / idle-sleep suppression is scoped to active
                // recordings — see the `power` module. It is deliberately NOT
                // asserted here for the whole process lifetime.
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

            // On Windows, ensure the lookout:// protocol handler is
            // registered even if the installer didn't do it (dev builds,
            // portable installs). Registry writes are idempotent.
            #[cfg(windows)]
            {
                let _ = app.deep_link().register_all();
                eprintln!("[deep-link] registered protocol handler");
            }

            // On Linux, deb/rpm installs already ship a desktop file that
            // claims lookout:// AND forwards the URL (%u comes from our
            // custom template, linux/lookout.desktop — Tauri's stock one
            // drops the URL). Registering again at runtime writes a second
            // lookout-desktop-handler.desktop, and the system's "Open with"
            // chooser then lists two indistinguishable Lookouts. Only
            // AppImages and dev builds, which install no desktop file,
            // need runtime registration.
            #[cfg(target_os = "linux")]
            {
                if app.env().appimage.is_some() || cfg!(debug_assertions) {
                    let _ = app.deep_link().register_all();
                    eprintln!("[deep-link] registered protocol handler");
                } else {
                    remove_stale_deep_link_handler(app.handle());
                }
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

                // Linux: drop the server-side titlebar and let the webview
                // draw a client-side header bar instead (see HeaderBar.tsx).
                // A GTK titlebar stacked on top of the app's own chrome is
                // two visual systems in one window, which is most of why
                // Lookout read as a visitor on the desktop.
                #[cfg(target_os = "linux")]
                {
                    window.set_decorations(false)?;

                    // Grow the window by the transparent frame the webview
                    // draws its outer border and shadow into (see
                    // WINDOW_MARGIN in linuxChrome.ts). Both paint outside
                    // the content box, so without the extra room the
                    // compositor simply clips them away.
                    //
                    // Skipped when a shell extension is already rounding and
                    // shading every window: it would draw around the grown
                    // window, i.e. 40px out from the app, and the frame ends
                    // up decorated twice. See `shell_draws_window_frame`.
                    //
                    // The bounds have to move with it: this window is fixed
                    // at 480x640 by min == max, and a set_size past the
                    // maximum would just be clamped back. Widen the limits
                    // first, then resize, then re-centre — the window grew
                    // around its old top-left otherwise.
                    if !desktop_appearance::shell_draws_window_frame() {
                        const MARGIN: f64 = 40.0;
                        let scale = window.scale_factor()?;
                        let inner: tauri::LogicalSize<f64> =
                            window.inner_size()?.to_logical(scale);
                        let grown = tauri::LogicalSize::new(
                            inner.width + MARGIN * 2.0,
                            inner.height + MARGIN * 2.0,
                        );
                        window.set_min_size(Some(grown))?;
                        window.set_max_size(Some(grown))?;
                        window.set_size(grown)?;
                        window.center()?;
                    }
                }

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
                            let client = http_client();
                            let url = format!(
                                "{}/api/sessions/{}/pause",
                                config.api_base_url, config.token
                            );
                            match client
                                .post(&url)
                                .timeout(std::time::Duration::from_secs(5))
                                .send()
                                .await
                            {
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

/// The menu-bar clock must agree with the main window's clock. Both tick
/// independently, so they only stay together if these rules match
/// `deriveDisplaySeconds` / `useSessionTimerState` in @lookout/react.
#[cfg(test)]
mod tray_timer_tests {
    use super::{
        format_tray_time, ratchet_tray_tracked_seconds, tray_display_seconds, TrayTimerState,
        MAX_TRAY_INTERPOLATION_SECS,
    };
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    use std::sync::Mutex;
    use std::time::Instant;

    fn state(tracked: i64) -> TrayTimerState {
        TrayTimerState {
            tracked_seconds: AtomicI64::new(tracked),
            started_at: Mutex::new(Instant::now()),
            is_running: AtomicBool::new(true),
        }
    }

    #[test]
    fn cap_matches_the_js_side() {
        // MAX_INTERPOLATION_S in useSessionTimer.ts is
        // SCREENSHOT_INTERVAL_MS / 1000 = 60.
        assert_eq!(MAX_TRAY_INTERPOLATION_SECS, 60);
    }

    #[test]
    fn interpolates_at_wall_clock_rate() {
        assert_eq!(tray_display_seconds(120, 0, true), 120);
        assert_eq!(tray_display_seconds(120, 30, true), 150);
    }

    #[test]
    fn interpolation_is_capped_at_one_interval() {
        // Without the cap the menu bar kept counting through a capture stall
        // while the main window froze at base + 60, and the two never
        // reconverged — the reported "menu bar shows a different time".
        assert_eq!(tray_display_seconds(120, 90, true), 180);
        assert_eq!(tray_display_seconds(120, 600, true), 180);
    }

    #[test]
    fn pause_drops_the_interpolated_remainder() {
        // The main window snaps down to the base on pause. Freezing at the
        // interpolated value here left the menu bar up to a minute ahead for
        // the whole pause.
        assert_eq!(tray_display_seconds(120, 45, false), 120);
        // Clock-style title: the paused value is the base, formatted exactly —
        // 299s is 04:59, not the 4m the minute-granularity title used to show.
        assert_eq!(
            format_tray_time(tray_display_seconds(299, 59, false)),
            "04:59"
        );
    }

    #[test]
    fn ratchet_ignores_a_stale_lower_reading() {
        // An idempotent retry can confirm against a stale read and return a
        // lower trackedSeconds. JS keeps the higher value; storing the lower
        // one here made the menu bar jump backwards and sit behind.
        let s = state(120);
        ratchet_tray_tracked_seconds(&s, 60);
        assert_eq!(s.tracked_seconds.load(Ordering::Relaxed), 120);
        ratchet_tray_tracked_seconds(&s, 180);
        assert_eq!(s.tracked_seconds.load(Ordering::Relaxed), 180);
    }

    #[test]
    fn ratchet_re_anchors_only_on_advance() {
        let s = state(120);
        let before = *s.started_at.lock().unwrap();

        // A repeated reading must not restart the interpolation window, or
        // the menu bar loses time the main window is still counting.
        ratchet_tray_tracked_seconds(&s, 120);
        assert_eq!(*s.started_at.lock().unwrap(), before);
        ratchet_tray_tracked_seconds(&s, 60);
        assert_eq!(*s.started_at.lock().unwrap(), before);

        // A real advance re-anchors.
        ratchet_tray_tracked_seconds(&s, 180);
        assert!(*s.started_at.lock().unwrap() > before);
    }
}

#[cfg(test)]
mod compat_tests {
    use super::{
        current_unix_ms, parse_iso_to_unix_ms, unix_ms_to_iso, ConfirmResponse,
        UploadUrlResponse,
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
        "capturedAtAdopted": true,
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
        assert!(!r.captured_at_adopted);
        assert!(r.tracking_mode.is_none());
    }

    #[test]
    fn new_struct_parses_new_json_upload_url() {
        let r: UploadUrlResponse = serde_json::from_str(NEW_UPLOAD_JSON).unwrap();
        assert_eq!(r.server_time.as_deref(), Some("2025-06-01T12:00:00.000Z"));
        assert!(r.captured_at_adopted);
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
        let s = unix_ms_to_iso(current_unix_ms());
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
        let s = unix_ms_to_iso(current_unix_ms());
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
