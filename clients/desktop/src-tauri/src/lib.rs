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
use lookout_core::timer::format_tray_time;
use lookout_core::{
    ApiError, BoxFuture, CameraFrame, CaptureSource, ControlMode, Core, CoreEvent, FinishOutcome,
    Frontend, PickedSource, RecorderError, RecorderPhase, RecorderState, RecordingSource,
    SessionConfig, StopOutcome,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// What the webview answers a camera request with.
enum CameraReply {
    Started,
    Frame(CameraFrame),
}

/// The core's view of this shell: events go onto the Tauri event bus, the
/// preview is wanted while the main window is focused, the clock text goes
/// to the menu-bar item / tray (and the GNOME pill on Linux), the tray's
/// presence follows the recorder's state, and camera frames come from the
/// webview's getUserMedia stream over a request/reply pair of events.
///
/// Constructed before the Tauri app exists (the core is managed state, so
/// it has to be built first) and bound to the [`AppHandle`] in `setup`.
/// Nothing can reach the core before then — commands only flow once the
/// event loop runs — so the unbound window is never observed.
#[derive(Default)]
struct TauriFrontend {
    app: OnceLock<AppHandle>,
    /// Outstanding camera requests, by id, waiting for the webview to call
    /// `camera_request_done` / `camera_frame_delivered`.
    camera_waiters: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Result<CameraReply, String>>>>,
    camera_next_id: AtomicU64,
    /// The tray item's state as last shown: `None` while hidden.
    tray_shown: Mutex<Option<ControlMode>>,
}

/// What the webview is asked for. `requestId` comes back on the reply.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CameraRequest {
    request_id: u64,
    device_id: String,
}

impl TauriFrontend {
    fn bind(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }

    /// Ask the webview for something camera-related and hand back the
    /// future that resolves when it answers. A webview that never answers
    /// is the core's problem to time out; a reply for an id nobody is
    /// waiting on is dropped.
    fn camera_request(
        &self,
        event: &'static str,
        device_id: &str,
    ) -> BoxFuture<'static, Result<CameraReply, String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let Some(app) = self.app.get() else {
            return Box::pin(async { Err("no window to ask for the camera".to_string()) });
        };
        let request_id = self.camera_next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.camera_waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        let payload = CameraRequest {
            request_id,
            device_id: device_id.to_string(),
        };
        if let Err(e) = app.emit(event, payload) {
            self.camera_waiters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&request_id);
            return Box::pin(async move { Err(format!("couldn't reach the window: {e}")) });
        }
        Box::pin(async move {
            rx.await
                .unwrap_or_else(|_| Err("the window went away before answering".to_string()))
        })
    }

    /// Resolve a camera request. Returns whether anyone was waiting.
    fn camera_reply(&self, request_id: u64, reply: Result<CameraReply, String>) -> bool {
        let waiter = self
            .camera_waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        match waiter {
            Some(tx) => tx.send(reply).is_ok(),
            None => false,
        }
    }

    /// Keep the tray item in step with the recorder: shown while a
    /// recording is open, with the paused glyph following the control
    /// mode, hidden once it is finished or closed. The clock text itself is
    /// the ticker's job (`set_tray_title`).
    ///
    /// Called from the core's tasks, never the main thread — and the tray
    /// (GTK's status item on Linux, the NSStatusItem on macOS) is main-thread
    /// only, so every touch is dispatched there.
    fn sync_tray(&self, state: Option<&RecorderState>) {
        let Some(app) = self.app.get() else {
            return;
        };
        let visible = state
            .filter(|s| !matches!(s.phase, RecorderPhase::LoadError | RecorderPhase::Finished));
        let mut shown = self.tray_shown.lock().unwrap_or_else(|e| e.into_inner());
        match visible {
            None => {
                if shown.take().is_some() {
                    on_main_thread(app, |app| tray::hide(&app));
                }
            }
            Some(state) => {
                let text = format_tray_time(state.display_seconds);
                let paused = state.control_mode == ControlMode::Paused;
                match *shown {
                    None => {
                        on_main_thread(app, move |app| {
                            if let Err(e) = tray::show(&app, &text, paused) {
                                eprintln!("[tray] show failed: {e}");
                            }
                        });
                        *shown = Some(state.control_mode);
                    }
                    Some(mode) if mode != state.control_mode => {
                        // The glyph has to flip now, not on the ticker's
                        // next second.
                        on_main_thread(app, move |app| {
                            let _ = tray::update(&app, &text, paused);
                        });
                        *shown = Some(state.control_mode);
                    }
                    Some(_) => {}
                }
            }
        }
    }
}

/// Run `f` on the main thread with a clone of the app handle. Tray and
/// menu work is main-thread only on every platform; the core's tasks that
/// drive it are not.
fn on_main_thread(app: &AppHandle, f: impl FnOnce(AppHandle) + Send + 'static) {
    let handle = app.clone();
    if let Err(e) = app.run_on_main_thread(move || f(handle)) {
        eprintln!("[tray] main-thread dispatch failed: {e}");
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
            // The recorder folds this into its state and re-emits the
            // preview; the raw payload (a full-size base64 JPEG) would only
            // cross the bridge a second time for nobody.
            CoreEvent::TickResult(_) => Ok(()),
            CoreEvent::TickError(payload) => app.emit(name, payload),
            CoreEvent::SourceLost(payload) => app.emit(name, payload),
            CoreEvent::PreviewFrame(payload) => app.emit(name, payload),
            CoreEvent::ScreencastRevoked(payload) => app.emit(name, payload),
            CoreEvent::RecorderState(state) => {
                self.sync_tray(state.as_ref());
                app.emit(name, state)
            }
            CoreEvent::RecorderNotice(notice) => app.emit(name, notice),
        };
    }

    fn wants_preview_frames(&self) -> bool {
        self.app
            .get()
            .and_then(|app| app.get_webview_window("main"))
            .map(|w| w.is_focused().unwrap_or(false))
            .unwrap_or(false)
    }

    /// Write the menu-bar time text, the paused glyph, and (off macOS) the
    /// hover tooltip. The one writer of the tray title.
    fn set_tray_title(&self, time_text: &str, paused: bool) {
        if let Some(app) = self.app.get() {
            let text = time_text.to_string();
            on_main_thread(app, move |app| {
                let _ = tray::update(&app, &text, paused);
            });
        }
    }

    fn camera_start(&self, device_id: &str) -> BoxFuture<'static, Result<(), String>> {
        let fut = self.camera_request("camera-start-request", device_id);
        Box::pin(async move { fut.await.map(|_| ()) })
    }

    fn camera_grab_frame(&self, device_id: &str) -> BoxFuture<'static, Result<CameraFrame, String>> {
        let fut = self.camera_request("camera-frame-request", device_id);
        Box::pin(async move {
            match fut.await? {
                CameraReply::Frame(frame) => Ok(frame),
                CameraReply::Started => Err("the window answered a frame request with 'started'".into()),
            }
        })
    }

    fn camera_stop(&self) {
        if let Some(app) = self.app.get() {
            let _ = app.emit("camera-stop", ());
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

// ── The recorder ─────────────────────────────────────────────────────
// The recording session lives in the core (`lookout_core::recorder`); the
// webview opens it, issues actions, and draws the `RecorderState` it is
// handed on `recorder-state`. Every command here is `async` so it runs on
// Tauri's tokio runtime — the core spawns tasks, and a sync command has no
// runtime in context (which is what aborted camera sessions in 0.2.0).

/// Open a recording for a session on the picked sources: load it, and
/// start capturing if it is recordable. Replaces any recording already
/// open. The returned state is also broadcast on `recorder-state`.
#[tauri::command]
async fn recorder_open(
    token: String,
    api_base_url: String,
    sources: Vec<PickedSource>,
    state: State<'_, AppState>,
) -> Result<RecorderState, RecorderError> {
    let source = RecordingSource::from_picked(sources)
        .map_err(|message| RecorderError::Other { message })?;
    state
        .core
        .recorder_open(SessionConfig { token, api_base_url }, source)
        .await
}

/// Close the recording: flush the partial minute, release the camera, and
/// leave the session itself alone (it stays open, keeping its time).
#[tauri::command]
async fn recorder_close(state: State<'_, AppState>) -> Result<(), String> {
    state.core.recorder_close().await;
    Ok(())
}

#[tauri::command]
async fn recorder_pause(state: State<'_, AppState>) -> Result<RecorderState, RecorderError> {
    state.core.recorder_pause().await
}

#[tauri::command]
async fn recorder_resume(state: State<'_, AppState>) -> Result<RecorderState, RecorderError> {
    state.core.recorder_resume().await
}

#[tauri::command]
async fn recorder_begin_stop(state: State<'_, AppState>) -> Result<RecorderState, RecorderError> {
    state.core.recorder_begin_stop().await
}

#[tauri::command]
async fn recorder_cancel_stop(state: State<'_, AppState>) -> Result<RecorderState, RecorderError> {
    state.core.recorder_cancel_stop().await
}

#[tauri::command]
async fn recorder_finish_stop(
    name: Option<String>,
    edit: bool,
    state: State<'_, AppState>,
) -> Result<StopOutcome, RecorderError> {
    state.core.recorder_finish_stop(name, edit).await
}

/// The open recording's state, for a window that just mounted (the tray
/// popup, a reloaded main window).
#[tauri::command]
fn recorder_state(state: State<'_, AppState>) -> Option<RecorderState> {
    state.core.recorder_state()
}

/// Rename (optional) and stop a session the way the recorder does, for the
/// source picker's Stop button — no recording is open there.
#[tauri::command]
async fn session_finish(
    token: String,
    api_base_url: String,
    name: Option<String>,
    edit: bool,
    state: State<'_, AppState>,
) -> Result<FinishOutcome, ApiError> {
    state
        .core
        .session_finish(&session_ref(token, api_base_url), name.as_deref(), edit)
        .await
}

// ── Camera, answered by the webview ──────────────────────────────────

/// The webview's answer to `camera-start-request`: the stream is up and
/// producing frames, or why not.
#[tauri::command]
fn camera_request_done(request_id: u64, error: Option<String>) {
    let reply = match error {
        None => Ok(CameraReply::Started),
        Some(e) => Err(e),
    };
    if !frontend().camera_reply(request_id, reply) {
        eprintln!("[camera] stray start reply for request {request_id}");
    }
}

/// The webview's answer to `camera-frame-request`: a JPEG (base64) with
/// its dimensions, or why there isn't one.
#[tauri::command]
fn camera_frame_delivered(
    request_id: u64,
    base64: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    error: Option<String>,
) {
    let reply = match (base64, error) {
        (_, Some(e)) => Err(e),
        (Some(b64), None) => lookout_core::base64_decode(&b64).map(|jpeg| {
            CameraReply::Frame(CameraFrame {
                jpeg,
                width: width.unwrap_or(0),
                height: height.unwrap_or(0),
            })
        }),
        (None, None) => Err("no frame".into()),
    };
    if !frontend().camera_reply(request_id, reply) {
        eprintln!("[camera] stray frame reply for request {request_id}");
    }
}

/// The shell behind the core — the `TauriFrontend` handed to `Core::new`.
fn frontend() -> &'static TauriFrontend {
    FRONTEND.get().expect("frontend is set before the app runs")
}

/// The one `TauriFrontend`, reachable from commands that answer it.
static FRONTEND: OnceLock<Arc<TauriFrontend>> = OnceLock::new();

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
    state: State<'_, AppState>,
) -> Result<Value, ApiError> {
    state.core.session_set_cuts(&session_ref(token, api_base_url), cuts).await
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
        // `ClientOptions` is non-exhaustive as of sentry 0.49, so it is
        // built up rather than written out.
        let mut options = sentry::ClientOptions::new();
        options.release = sentry::release_name!();
        options.environment = Some("desktop-tauri".into());
        options.send_default_pii = true;
        sentry::init((dsn, options.sample_rate(1.0)))
    });

    // The core is managed state, so it exists before the Tauri app does; the
    // frontend it reports to gets its AppHandle in `setup`.
    let frontend = Arc::new(TauriFrontend::default());
    let _ = FRONTEND.set(Arc::clone(&frontend));
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
            recorder_open,
            recorder_close,
            recorder_pause,
            recorder_resume,
            recorder_begin_stop,
            recorder_cancel_stop,
            recorder_finish_stop,
            recorder_state,
            session_finish,
            camera_request_done,
            camera_frame_delivered,
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
            tray::tray_action,
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
                // tear the recording down immediately — the window that
                // answers camera requests and draws the state is gone. The
                // session pause is handled in the RunEvent::ExitRequested
                // handler below to ensure it completes before the process
                // exits.
                let app_handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Destroyed = event {
                        let state = app_handle.state::<AppState>();
                        eprintln!("[window-close] aborting the recording");
                        state.core.recorder_abort();
                        tray::hide(&app_handle);
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
                let config = state
                    .core
                    .recorder_session()
                    .or_else(|| state.core.config());

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
