//! The Tauri shell over `lookout-core`.
//!
//! Everything capture-, clip-, upload- and server-API-related lives in the
//! core crate (`crates/lookout-core`); this crate is the window, the tray,
//! the menus, deep links, platform chrome, and a set of thin
//! `#[tauri::command]` wrappers that hand straight through to [`Core`]. Core
//! events reach the webview via [`TauriFrontend`], which forwards each
//! [`CoreEvent`] onto the Tauri event bus under its usual name.

mod background_blur;
mod desktop_appearance;
#[cfg(target_os = "linux")]
mod gbm_probe;
#[cfg(target_os = "linux")]
mod gnome_indicator;
mod native_menu;
#[cfg(target_os = "macos")]
mod native_tray;
mod secret_store;
mod tray;
mod window_shape;
#[cfg(target_os = "windows")]
mod windows_permissions;

use lookout_core::apps::AppEntry;
use lookout_core::capture_diagnostics::CaptureEnvironment;
use lookout_core::screencast::StreamInfo;
use lookout_core::sources::CaptureSourceList;
use lookout_core::{
    ApiError, CaptureResult, CaptureSource, CaptureUploadResult, Core, CoreEvent, Frontend,
    SessionConfig,
};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::http;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_deep_link::DeepLinkExt;

/// App state shared across commands.
pub struct AppState {
    /// The capture engine. Everything session-related goes through it.
    pub core: Arc<Core>,
    pub cold_start_urls: Mutex<Option<Vec<String>>>,
}

/// The core's view of this shell: events go onto the Tauri event bus, the
/// preview is wanted while the main window is focused, and the clock text
/// goes to the menu-bar item / tray (and the GNOME pill on Linux).
///
/// Constructed before the Tauri app exists (the core is managed state, so
/// it has to be built first) and bound to the [`AppHandle`] in `setup`.
/// Nothing can reach the core before then — commands only flow once the
/// event loop runs — so the unbound window is never observed.
#[derive(Default)]
struct TauriFrontend {
    app: OnceLock<AppHandle>,
}

impl TauriFrontend {
    fn bind(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }
}

impl Frontend for TauriFrontend {
    fn emit(&self, event: CoreEvent) {
        let Some(app) = self.app.get() else {
            return;
        };
        let name = event.name();
        let _ = match event {
            CoreEvent::Progress(message) => app.emit(name, message),
            CoreEvent::TrackedSeconds(seconds) => app.emit(name, seconds),
            CoreEvent::SessionTerminated(payload) => app.emit(name, payload),
            CoreEvent::TickResult(payload) => app.emit(name, payload),
            CoreEvent::TickError(payload) => app.emit(name, payload),
            CoreEvent::SourceLost(payload) => app.emit(name, payload),
            CoreEvent::PreviewFrame(payload) => app.emit(name, payload),
            CoreEvent::ScreencastRevoked(payload) => app.emit(name, payload),
        };
    }

    fn wants_preview_frames(&self) -> bool {
        self.app
            .get()
            .and_then(|app| app.get_webview_window("main"))
            .map(|w| w.is_focused().unwrap_or(false))
            .unwrap_or(false)
    }

    /// Write the menu-bar time text (and, off macOS, the hover tooltip).
    fn set_tray_title(&self, time_text: &str, paused: bool) {
        #[cfg(target_os = "linux")]
        crate::gnome_indicator::publish_tick(time_text, paused);

        #[cfg(not(target_os = "linux"))]
        let _ = paused;

        #[cfg(target_os = "macos")]
        {
            // None = keep the current pause state; the Swift side renders the
            // tooltip and the numericText digit roll.
            let _ = crate::native_tray::update(time_text, None);
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(tray) = self
            .app
            .get()
            .and_then(|app| app.tray_by_id("timelapse_tray"))
        {
            let _ = tray.set_title(Some(time_text));
            // Windows doesn't render tray titles — the hover tooltip is the
            // only way to see the recorded time there.
            let _ = tray.set_tooltip(Some(format!("Lookout — {time_text} recorded")));
        }
    }
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
    // The binary is `lookout` now; a machine that ran a build from when it was
    // `lookout-desktop` still has that one's handler sitting there.
    let exe_name = exe_name.to_string_lossy().to_string();
    let mut removed = false;
    for name in [exe_name.as_str(), "lookout-desktop"] {
        let handler_file = applications_dir.join(format!("{name}-handler.desktop"));
        if handler_file.exists() && std::fs::remove_file(&handler_file).is_ok() {
            eprintln!("[deep-link] removed stale handler {}", handler_file.display());
            removed = true;
        }
    }
    if removed {
        // Also drop the mimeapps.list default the old register_all() set,
        // which points at the file just deleted. GIO would skip a missing
        // default anyway, but a KDE/XFCE launcher might not.
        let _ = app.deep_link().unregister("lookout");
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&applications_dir)
            .status();
    }
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

// ── Thin command wrappers over the core ──────────────────────────
// Names and argument shapes are the JS-facing contract; keep them stable.

/// Set the list of blacklisted app names (replaces current list).
#[tauri::command]
fn set_blacklisted_apps(apps: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    state.core.set_blacklisted_apps(apps)
}

/// Get the current list of blacklisted app names.
#[tauri::command]
fn get_blacklisted_apps(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    state.core.get_blacklisted_apps()
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
    tauri::async_runtime::spawn_blocking(lookout_core::apps::list_installed_apps_blocking)
        .await
        .unwrap_or_default()
}

/// Return a small PNG (base64) of an app's icon. `path` is the icon lookup
/// key from `AppEntry.path`. Cached per key; async so lookups run off the
/// main thread (a sync command here froze the UI while icons rasterized).
#[tauri::command]
async fn get_app_icon(path: String) -> Option<String> {
    lookout_core::apps::app_icon(path)
}

/// List available capture sources (monitors + windows).
#[tauri::command]
fn list_capture_sources() -> Result<CaptureSourceList, String> {
    lookout_core::sources::list_capture_sources()
}

/// Everything we know about why capture might be failing on this machine.
/// Only called when something has already gone wrong, so the D-Bus round
/// trips cost nothing on the happy path.
#[tauri::command]
async fn capture_environment() -> CaptureEnvironment {
    lookout_core::capture_diagnostics::capture_environment().await
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
///
/// Linux can't honour "foreground" for the redirect hook, and we no longer try.
/// Wayland compositors only raise a window for an activation token minted from
/// a focused surface and a recent input serial; the redirect fires off the
/// timelapse finishing, so there is no such interaction to borrow from and
/// GNOME rejects the request. Routing the launch through GTK to attach a token
/// anyway made it worse: it satisfied enough of the protocol that GNOME stopped
/// posting its "<browser> is ready" notification, and the page opened behind
/// everything with nothing to click. The plain spawn keeps that notification,
/// which is at least an affordance. See `fireRedirect` in App.tsx.
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

/// How Lookout was installed, so the update sheet can print a command that
/// actually works rather than a plausible one that fails.
#[derive(serde::Serialize)]
pub struct LinuxInstall {
    /// "apt", "rpm", "pacman", or "unknown".
    manager: String,
    /// Whether our repository is configured for that manager. A package
    /// installed by hand before the postinst existed is owned by dpkg or rpm
    /// but enrolled nowhere, and for it `apt install lookout` finds no
    /// candidate.
    enrolled: bool,
}

#[tauri::command]
fn linux_install_kind() -> LinuxInstall {
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};

        // Ask each package manager whether it owns our executable. A missing
        // manager fails to spawn, which is the same answer as "not mine".
        let owns = |program: &str, args: &[&str]| {
            Command::new(program)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };

        let exe = std::env::current_exe().ok();
        let manager = match exe.as_ref().and_then(|p| p.to_str()) {
            Some(path) if owns("dpkg", &["-S", path]) => "apt",
            Some(path) if owns("rpm", &["-qf", path]) => "rpm",
            Some(path) if owns("pacman", &["-Qo", path]) => "pacman",
            _ => "unknown",
        };

        let enrolled = match manager {
            "apt" => std::path::Path::new("/etc/apt/sources.list.d/lookout.sources").exists(),
            "rpm" => std::path::Path::new("/etc/yum.repos.d/lookout.repo").exists(),
            // pacman has no drop-in directory by default, so the section lives
            // in pacman.conf itself.
            "pacman" => std::fs::read_to_string("/etc/pacman.conf")
                .map(|c| c.lines().any(|l| l.trim() == "[lookout]"))
                .unwrap_or(false),
            _ => false,
        };

        LinuxInstall {
            manager: manager.into(),
            enrolled,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        LinuxInstall { manager: "unknown".into(), enrolled: false }
    }
}

#[tauri::command]
async fn request_screencast(state: State<'_, AppState>) -> Result<Vec<StreamInfo>, String> {
    state.core.request_screencast().await
}

#[tauri::command]
async fn add_screencast(state: State<'_, AppState>) -> Result<Vec<StreamInfo>, String> {
    state.core.add_screencast().await
}

/// Hand the screen back: close every portal session and drop its PipeWire fds.
///
/// The frontend calls this when a recording session ends or the user returns
/// to the source picker. It is deliberately NOT wired into `stop_capture_loop`
/// — pause goes through that same path, and a paused session has to keep its
/// cast so resuming doesn't re-prompt the portal.
#[tauri::command]
fn release_screencast(state: State<'_, AppState>) -> Result<(), String> {
    state.core.release_screencast();
    Ok(())
}

/// Initialize the session config so Rust knows where the server is.
#[tauri::command]
fn configure(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.core.configure(token, api_base_url)
}

/// Take a native screenshot, encode as JPEG, return base64.
#[tauri::command]
fn take_screenshot(
    source: CaptureSource,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    state: State<'_, AppState>,
) -> Result<CaptureResult, String> {
    state
        .core
        .take_screenshot(source, max_width, max_height, jpeg_quality)
}

/// Full capture-upload-confirm pipeline in Rust (no browser CORS issues).
/// Returns the confirm data AND the screenshot preview (base64) so the
/// frontend can display the captured frame without a separate IPC call.
#[tauri::command]
async fn capture_and_upload(
    sources: Vec<CaptureSource>,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    state: State<'_, AppState>,
) -> Result<CaptureUploadResult, String> {
    state
        .core
        .capture_and_upload(sources, max_width, max_height, jpeg_quality)
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
) -> Result<CaptureUploadResult, String> {
    state.core.upload_frame(base64, width, height).await
}

/// Start the Rust-side capture loop. Replaces any existing loop.
/// For screen/window/pipewire sources only — camera sources stay JS-driven.
///
/// `async` so it runs on Tauri's tokio runtime: the core spawns tasks.
#[tauri::command]
async fn start_capture_loop(
    sources: Vec<CaptureSource>,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .core
        .start_capture_loop(sources, max_width, max_height, jpeg_quality)
}

/// Stop the Rust-side capture loop (if running).
///
/// Async, and AWAITED by the UI before it sends the pause/stop POST: on
/// cancellation the loop flushes the in-progress partial minute as a
/// `final` capture (see the end of `capture_loop_task`), and that confirm
/// must land while the session is still active — a paused session rejects
/// it. Bounded so a dead network can't wedge the pause button; on timeout
/// the flush is abandoned (the partial minute is lost, exactly as it was
/// before the flush existed).
#[tauri::command]
async fn stop_capture_loop(state: State<'_, AppState>) -> Result<(), String> {
    state.core.stop_capture_loop().await
}

/// Start the Rust-side tray title ticker (for camera sessions where
/// the capture loop runs in JS but we still want an accurate menu bar timer).
///
/// MUST be `async` — `Core::start_tray_timer` calls `tokio::spawn` internally,
/// which panics if not invoked from inside a tokio runtime. Tauri runs
/// `async` commands on its own tokio runtime, but sync commands run on a
/// thread without one. Calling this command sync caused SIGABRT on camera
/// sessions in 0.2.0 + 0.2.1 (only camera path uses this; screen capture
/// goes through `start_capture_loop` which is already async).
#[tauri::command]
async fn start_tray_ticker(tracked_seconds: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.core.start_tray_ticker(tracked_seconds)
}

/// Pause the tray title ticker (freezes the displayed time).
#[tauri::command]
fn pause_tray_ticker(state: State<'_, AppState>) -> Result<(), String> {
    state.core.pause_tray_timer();
    Ok(())
}

/// Resume the tray title ticker.
#[tauri::command]
fn resume_tray_ticker(tracked_seconds: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.core.sync_tray_timer(tracked_seconds);
    state.core.resume_tray_timer();
    Ok(())
}

/// Stop the tray title ticker.
#[tauri::command]
fn stop_tray_ticker(state: State<'_, AppState>) -> Result<(), String> {
    state.core.stop_tray_timer();
    Ok(())
}

/// Sync the tray timer to an authoritative tracked_seconds value from JS.
#[tauri::command]
fn sync_tray_tracked_seconds(
    tracked_seconds: i64,
    anchor_at_ms: Option<i64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    match anchor_at_ms {
        // Align the ticker with the main window's interpolation anchor so
        // the two surfaces show the same seconds.
        Some(anchor) => state.core.sync_tray_timer_anchored(tracked_seconds, anchor),
        None => state.core.sync_tray_timer(tracked_seconds),
    }
    Ok(())
}

// ── Server API, through the core ────────────────────────────────────
// The webview never speaks HTTP to the Lookout server itself; each of these
// hands the call to `lookout_core::api` and returns the server's JSON as is.
// Errors come back as `ApiError { status, message }`, which the TS side
// turns back into its `HttpError`.

fn session_ref(token: String, api_base_url: String) -> SessionConfig {
    SessionConfig {
        token,
        api_base_url,
    }
}

#[tauri::command]
async fn api_session_get(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_get(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_status(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_status(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_pause(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_pause(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_resume(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_resume(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_stop(
    token: String,
    api_base_url: String,
    edit: bool,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_stop(&session_ref(token, api_base_url), edit).await
}

#[tauri::command]
async fn api_session_rename(
    token: String,
    api_base_url: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_rename(&session_ref(token, api_base_url), &name).await
}

#[tauri::command]
async fn api_session_video(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_video(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_units(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_units(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_set_cuts(
    token: String,
    api_base_url: String,
    cuts: Value,
    masks: Option<Value>,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state
        .core
        .session_set_cuts(&session_ref(token, api_base_url), cuts, masks)
        .await
}

#[tauri::command]
async fn api_session_apply_cuts(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_apply_cuts(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_heartbeat_editing(
    token: String,
    api_base_url: String,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_heartbeat_editing(&session_ref(token, api_base_url)).await
}

#[tauri::command]
async fn api_session_upload_url(
    token: String,
    api_base_url: String,
    captured_at: Option<String>,
    format: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state
        .core
        .session_upload_url(
            &session_ref(token, api_base_url),
            captured_at.as_deref(),
            format.as_deref(),
        )
        .await
}

#[tauri::command]
async fn api_session_confirm_screenshot(
    token: String,
    api_base_url: String,
    body: Value,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state
        .core
        .session_confirm_screenshot(&session_ref(token, api_base_url), body)
        .await
}

/// `bytes_base64` because Tauri serializes a `Vec<u8>` argument as a JSON
/// array of numbers, which is far larger and slower than base64 for a
/// multi-MB clip.
#[tauri::command]
async fn api_upload_to_r2(
    upload_url: String,
    bytes_base64: String,
    content_type: String,
    state: State<'_, AppState>,
) -> Result<(), ApiError> {
    let bytes = lookout_core::base64_decode(&bytes_base64).map_err(|message| ApiError {
        status: None,
        message,
    })?;
    state.core.upload_to_r2(&upload_url, bytes, &content_type).await
}

#[tauri::command]
async fn api_programs(
    api_base_url: String,
    timeout_ms: Option<u64>,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state
        .core
        .programs(
            &api_base_url,
            timeout_ms.map(std::time::Duration::from_millis),
        )
        .await
}

#[tauri::command]
async fn api_announcement(
    api_base_url: String,
    client: Option<String>,
    version: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state
        .core
        .announcement(&api_base_url, client.as_deref(), version.as_deref())
        .await
}

#[tauri::command]
async fn api_tip(
    api_base_url: String,
    client: Option<String>,
    version: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state
        .core
        .tip(&api_base_url, client.as_deref(), version.as_deref())
        .await
}

#[tauri::command]
async fn api_sessions_batch(
    api_base_url: String,
    tokens: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.sessions_batch(&api_base_url, &tokens).await
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

    // The NVIDIA check above is a guess made from a driver name. This one
    // is a measurement: ask GBM for a buffer and see whether it comes back.
    // A machine that cannot allocate one cannot run WebKitGTK's DMA-BUF
    // renderer, and every decoded video frame goes through that renderer —
    // which is why the failure tends to show up on the first <video> rather
    // than at launch. Runs second on purpose: the probe leaves an already
    // set WEBKIT_DISABLE_COMPOSITING_MODE alone, so the NVIDIA case and a
    // value the user exported both win over it.
    #[cfg(target_os = "linux")]
    gbm_probe::ensure_webview_can_render();

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

    // The core is managed state, so it exists before the Tauri app does; the
    // frontend it reports to gets its AppHandle in `setup`.
    let frontend = Arc::new(TauriFrontend::default());
    let core = Core::new(
        Arc::clone(&frontend) as Arc<dyn Frontend>,
        "Lookout Desktop",
        env!("CARGO_PKG_VERSION"),
    );

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
                    "monitor" => CaptureSource::Monitor { id: source_id },
                    "window" => CaptureSource::Window { id: source_id },
                    "pipewire" => CaptureSource::PipeWire { id: source_id },
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
                    if let Ok(guard) = app_state.core.pipewire_fds.lock() {
                        pipewire_fds = guard.clone();
                    }
                }

                // Read blacklisted apps for redaction
                let blacklisted: Vec<String> = app_handle
                    .try_state::<AppState>()
                    .and_then(|s| s.core.blacklisted_apps.lock().ok().map(|g| g.clone()))
                    .unwrap_or_default();

                match lookout_core::capture::take_screenshot_raw_with_blacklist(source, max_width, max_height, jpeg_quality, &pipewire_fds, &blacklisted, lookout_core::capture::ResizeQuality::Preview) {
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
            // Both parameters spelled out: `C` defaults to `()`, but a default
            // is not used for inference in an expression, and nothing here
            // pins it — so without this it's E0283.
            tauri::plugin::Builder::<tauri::Wry, ()>::new("lookout-window-frame")
                .js_init_script(format!(
                    "window.__LOOKOUT_SHELL_DRAWS_FRAME__ = {};",
                    desktop_appearance::shell_draws_window_frame(),
                ))
                .build(),
        )
        .manage(AppState {
            core: Arc::clone(&core),
            cold_start_urls: Mutex::new(None),
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
            capture_environment,
            desktop_appearance::desktop_appearance,
            window_shape::sync_window_frame,
            background_blur::sync_background_blur,
            open_external_url,
            linux_install_kind,
            secret_store::secret_set,
            secret_store::secret_get,
            secret_store::secret_delete,
            native_menu::show_add_menu,
            native_menu::prefetch_add_menu_icons,
            request_screencast,
            add_screencast,
            release_screencast,
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
            api_session_get,
            api_session_status,
            api_session_pause,
            api_session_resume,
            api_session_stop,
            api_session_rename,
            api_session_video,
            api_session_units,
            api_session_set_cuts,
            api_session_apply_cuts,
            api_session_heartbeat_editing,
            api_session_upload_url,
            api_session_confirm_screenshot,
            api_upload_to_r2,
            api_programs,
            api_announcement,
            api_tip,
            api_sessions_batch,
        ])
        .manage(tray::TrayStateMutex(std::sync::Mutex::new(tray::TrayState::default())))
        .setup(move |app| {
            // From here on the core can reach the webview.
            frontend.bind(app.handle().clone());

            // Warm the installed-app cache off-thread so the first visit to
            // Filtered Apps is instant rather than paying for the scan.
            lookout_core::apps::prewarm_installed_apps();

            // The GNOME top-bar pill. Installing the extension is not a
            // setting to go and find: on GNOME the pill *is* how a recording
            // is indicated, so put it in place and switch it on, then export
            // the state it reads. Both no-op off GNOME, and the install is a
            // no-op once the shipped version is already there.
            #[cfg(target_os = "linux")]
            {
                gnome_indicator::ensure_installed();
                gnome_indicator::start(app.handle().clone());
            }

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
            // chooser then lists two indistinguishable Lookouts. Only dev
            // builds, which install no desktop file, need runtime
            // registration.
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

                    // Pin the window to an exact size, reserving the
                    // transparent frame the webview draws its outer border and
                    // shadow into (see WINDOW_MARGIN in linuxChrome.ts). Both
                    // paint outside the content box, so without the extra room
                    // the compositor simply clips them away.
                    //
                    // The margin drops to zero when the compositor or a shell
                    // extension is already rounding and shading every window —
                    // it would draw around the grown window, 40px out from the
                    // app, and the frame ends up decorated twice. See
                    // `shell_draws_window_frame`.
                    //
                    // The size is NOT read back from the window, and that is
                    // the whole point of this block. `inner_size()` here
                    // reports a size GTK has already had its way with: on Yaru
                    // it came back 47px short (exactly a titlebar), which is why
                    // the main window shipped as 480x593 instead of the 480x640
                    // it is configured for. And with no explicit `set_size` at
                    // all, GTK leaves its CSD shadow extents in the surface,
                    // which measured 52px over on the same theme. Two opposite
                    // errors, one cause: trusting that query. So don't.
                    //
                    // The intended size is read from tauri.conf.json rather
                    // than repeated here. The frontend's layout is built
                    // against those same numbers, and a second copy in Rust
                    // would drift from them silently.
                    let configured = app
                        .config()
                        .app
                        .windows
                        .iter()
                        .find(|w| w.label == window.label())
                        .map(|w| (w.width, w.height));

                    match configured {
                        Some((content_w, content_h)) => {
                            let margin: f64 =
                                if desktop_appearance::shell_draws_window_frame() {
                                    0.0
                                } else {
                                    40.0
                                };
                            let target = tauri::LogicalSize::new(
                                content_w + margin * 2.0,
                                content_h + margin * 2.0,
                            );

                            // Unconditional, including when the margin is zero:
                            // the set_size is not only about growing the window,
                            // it is what pins the surface to an exact size now
                            // the decorations are off. Skipping it is what left
                            // the window oversized.
                            //
                            // The bounds have to move first: this window is
                            // fixed by min == max, and a set_size past the
                            // maximum would just be clamped back. Widen the
                            // limits, then resize, then re-centre — the window
                            // grows around its old top-left otherwise.
                            window.set_min_size(Some(target))?;
                            window.set_max_size(Some(target))?;
                            window.set_size(target)?;
                            window.center()?;
                        }
                        None => {
                            // Unreachable for a window that came from the
                            // config, which this one did. Leaving the size alone
                            // beats inventing one: a clipped shadow is a smaller
                            // wrong than a window the wrong size.
                            eprintln!(
                                "[linux-chrome] no config for window {:?}; leaving its size alone",
                                window.label()
                            );
                        }
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
                        if let Some(handle) = state.core.take_capture_loop() {
                            eprintln!("[window-close] stopping capture loop");
                            handle.cancel();
                        }

                        // Stop tray timer
                        state.core.stop_tray_timer();

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
                let has_active_loop = state.core.has_capture_loop();
                let config = state.core.config.lock().ok().and_then(|g| g.clone());

                if has_active_loop {
                    if let Some(config) = config {
                        // Prevent immediate exit — we need to send the pause request first.
                        api.prevent_exit();
                        eprintln!("[exit] pausing session before exit");
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            lookout_core::upload::pause_session_before_exit(&config).await;
                            // Now allow the app to exit
                            app_handle.exit(0);
                        });
                    }
                }
            }
        });
}
