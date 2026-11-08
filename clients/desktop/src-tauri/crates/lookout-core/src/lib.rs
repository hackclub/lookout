//! Lookout's capture engine, independent of any UI toolkit.
//!
//! Everything a Lookout desktop client has to do that is *not* drawing a
//! window lives here: enumerating monitors and windows, grabbing and
//! redacting frames, encoding per-minute H.264 clips, the self-scheduling
//! capture loop with its sleep/pause recovery and final flush, the ordered
//! uploader with its backlog (captures taken during an outage are
//! delivered when the link is back, stamped with when they were taken),
//! the presigned-URL upload pipeline with its retry policy and clock-offset
//! estimate, the menu-bar recording clock, and every call to the server
//! API. The Tauri app in `src-tauri/src` is one shell over this crate; a GTK
//! or Qt shell would link the same crate and implement [`Frontend`].
//!
//! This crate is the desktop host of three platform-free crates it
//! re-exports: `lookout-session` (the recording's state, clock, schedule
//! and timing as pure logic), `lookout-protocol` (the server API and upload
//! pipeline over a `Transport` the host supplies) and `lookout-recorder`
//! (the recorder, the capture loop and the uploader over a `Host`). A
//! browser or device client links those three and brings its own capture
//! and HTTP — `lookout-web` is the browser host.
//!
//! The contract is deliberately small:
//!
//! - Construct a [`Core`] with a [`Frontend`] implementation and a client
//!   name/version (the telemetry string sent to the server).
//! - Open a recording with [`Core::recorder_open`] and drive it with the
//!   `recorder_*` actions; draw the [`RecorderState`] published as
//!   [`CoreEvent::RecorderState`]. The capture loop, the session's status,
//!   the clock and the order of operations behind every button are the
//!   core's — see [`recorder`].
//! - Receive previews, notices and the raw loop events as [`CoreEvent`]s,
//!   and the clock text through [`Frontend::set_tray_title`].
//! - Read the program registry / announcement, and drive sessions that
//!   aren't being recorded here, through the [`api`] methods, so no shell
//!   speaks HTTP to the server on its own. Session *creation* happens on
//!   the program's website and reaches a shell as a `lookout://` deep link
//!   with the token.
//!
//! Anything that spawns (the recorder actions, `start_capture_loop`, the
//! screencast portal) uses `tokio::spawn` and so must be called from inside
//! a tokio runtime.

mod activity;
pub mod api;
pub mod apps;
pub mod capture;
pub mod capture_diagnostics;
pub mod capture_loop;
pub mod clips;
mod crop;
pub mod host;
#[cfg(test)]
pub(crate) mod mock_server;
mod pipewire;
#[cfg(target_os = "linux")]
mod pw_cursor;
#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "macos")]
pub mod power;
pub mod recorder;
#[cfg(test)]
mod recorder_stress;
#[cfg(test)]
mod credit_sim;
pub mod screencast;
pub mod sources;
pub mod timer;
pub mod transport;
pub mod upload;

// The pure session logic lives in `lookout-session`, the orchestration in
// `lookout-recorder`; these aliases keep every `lookout_core::` path a
// shell or a test has always used.
pub use lookout_session::clock;
pub use lookout_session::clock_offset;
pub use lookout_session::timing as tuning;
pub use lookout_session::{CaptureSource, SessionConfig};

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

use serde::Serialize;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

pub use api::{ApiError, ApiResult};
pub use capture_loop::{
    CaptureLoopHandle, CaptureLoopSpec, CapturePreviewFrame, CaptureSessionTerminated,
    CaptureSourceLost, CaptureTickError, CaptureTickResult, FrameSource,
    SessionClipCapabilities,
};
pub use clock::{ClockState, MAX_INTERPOLATION_SECS, TIMER_INTERPOLATION_SLACK_SECS};
pub use lookout_recorder::{CoreEvent, Recorder, ScreencastRevoked};
pub use recorder::{
    ControlMode, FinishOutcome, PickedSource, RecorderError, RecorderNotice, RecorderPhase,
    RecorderState, RecordingSource, SourceLoss, SourceLossReason, StopOutcome,
};
pub use upload::CaptureUploadResult;

/// A boxed, sendable future — what the camera hooks on [`Frontend`] return,
/// so the trait stays object-safe without an async-trait dependency.
pub type BoxFuture<'a, T> = lookout_protocol::transport::BoxFuture<'a, T>;

/// One frame off the shell's camera pipeline: a JPEG already scaled to fit
/// the capture bounds, plus its dimensions. The core decodes it when it
/// needs pixels (clip encoding, the live preview) and uploads the bytes as
/// they are otherwise.
#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize)]
pub struct CaptureResult {
    /// Base64-encoded JPEG bytes
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: usize,
}

/// What the core needs from whatever is drawing the UI.
///
/// Implementations must be cheap and non-blocking: these are called from
/// the capture loop and the once-a-second clock task.
pub trait Frontend: Send + Sync + 'static {
    /// Deliver a capture-loop / upload / screencast event to the UI.
    fn emit(&self, event: CoreEvent);

    /// Whether anyone is looking at the live preview right now. When false
    /// the loop skips the per-frame preview JPEG (and, outside clips mode,
    /// the in-between frame grab entirely). The Tauri shell answers with
    /// "is the main window focused".
    fn wants_preview_frames(&self) -> bool;

    /// Show the recording clock text (`mm:ss` or `h:mm:ss`) in the menu bar
    /// / tray, and whether the recording is paused. Called once a second
    /// while a recording is open, but only when the text or the paused flag
    /// changed. This is the ONLY writer of the tray title: two writers
    /// interpolating to different seconds is what flickered the menu bar
    /// between "0m"/"1m" at every minute boundary.
    fn set_tray_title(&self, time_text: &str, paused: bool);

    // ── Camera (optional) ─────────────────────────────────────────────
    //
    // Camera sessions run through the same capture loop as screen sessions
    // — the cadence, sleep recovery, pause flush, clips and upload pipeline
    // — with only the *frame* coming from the shell. The Tauri shell answers
    // these from the webview's getUserMedia stream; a shell with a native
    // camera stack answers them itself; a headless shell leaves the
    // defaults, which refuse camera sources.

    /// Open the camera and wait until it is producing frames. Called before
    /// the capture loop starts (and again on resume), so the very first tick
    /// has a frame to upload: the seed capture is what activates the session
    /// and anchors its credit schedule, and a tick that finds no frame would
    /// push that a full interval out.
    fn camera_start(&self, device_id: &str) -> BoxFuture<'static, Result<(), String>> {
        let _ = device_id;
        Box::pin(async { Err("this shell has no camera backend".to_string()) })
    }

    /// Grab one frame from the open camera.
    fn camera_grab_frame(&self, device_id: &str) -> BoxFuture<'static, Result<CameraFrame, String>> {
        let _ = device_id;
        Box::pin(async { Err("this shell has no camera backend".to_string()) })
    }

    /// Release the camera. Called on pause, stop and close — the light must
    /// go off when the recording isn't recording.
    fn camera_stop(&self) {}
}

/// The engine. One per process, shared behind an [`Arc`]. Everything about
/// the recording is the [`Recorder`] it derefs to; what is here is the
/// desktop's own: the screencast sessions, the Filtered Apps list, the host.
pub struct Core {
    /// Maps PipeWire node_id -> the RawFd of the screencast session that owns it.
    /// This allows streams from different portal sessions to coexist (e.g. when
    /// the user incrementally adds sources via the "+" button).
    #[cfg(target_os = "linux")]
    pub pipewire_fds: Mutex<std::collections::HashMap<u32, std::os::fd::RawFd>>,
    /// Live XDG screencast sessions, one per trip through the portal picker.
    /// Holding them is what lets us CLOSE them: the portal keeps casting until
    /// `Close()` is called, so a session we forget about streams (and shows in
    /// the system's screen-sharing indicator) until the process exits.
    /// `pipewire_fds` above is a derived view of this list — see
    /// `screencast::rebuild_fd_map`.
    #[cfg(target_os = "linux")]
    pub screencast_sessions: Mutex<Vec<screencast::ScreencastSession>>,
    /// App names whose windows should be blacked out in monitor captures.
    pub blacklisted_apps: Mutex<Vec<String>>,
    recorder: Arc<Recorder>,
    host: Arc<host::DesktopHost>,
}

impl Deref for Core {
    type Target = Recorder;
    fn deref(&self) -> &Recorder {
        &self.recorder
    }
}

impl Core {
    /// `client_name`/`client_version` make up the telemetry string sent with
    /// every upload-url request (`"Lookout Desktop/0.4.0 (macOS 14.3)"`) —
    /// pass the shipping app's name and version, not this crate's.
    pub fn new(
        frontend: Arc<dyn Frontend>,
        client_name: impl Into<String>,
        client_version: impl Into<String>,
    ) -> Arc<Self> {
        let client_name = client_name.into();
        let client_version = client_version.into();
        Arc::new_cyclic(|weak| {
            let host = Arc::new(host::DesktopHost::new(
                frontend,
                weak.clone(),
                client_name,
                client_version,
            ));
            let recorder = Recorder::new(host.clone());
            Self {
                #[cfg(target_os = "linux")]
                pipewire_fds: Mutex::new(std::collections::HashMap::new()),
                #[cfg(target_os = "linux")]
                screencast_sessions: Mutex::new(Vec::new()),
                blacklisted_apps: Mutex::new(Vec::new()),
                recorder,
                host,
            }
        })
    }

    /// The shell this core reports to.
    pub fn frontend(&self) -> &dyn Frontend {
        &*self.host.frontend
    }

    /// The recorder behind this core (also reachable by deref).
    pub fn recorder(&self) -> &Arc<Recorder> {
        &self.recorder
    }

    /// This core's HTTP transport, for the protocol crate's calls.
    pub fn transport(&self) -> &dyn lookout_protocol::Transport {
        lookout_recorder::host::Host::transport(&*self.host)
    }

    /// The telemetry string sent with every upload-url request.
    pub fn client_info(&self) -> &str {
        lookout_recorder::host::Host::client_info(&*self.host)
    }

    // ── Recorder actions (the `Arc<Recorder>` receivers, forwarded) ──

    pub async fn recorder_open(
        self: &Arc<Self>,
        cfg: SessionConfig,
        source: RecordingSource,
    ) -> Result<RecorderState, RecorderError> {
        self.recorder.recorder_open(cfg, source).await
    }

    pub async fn recorder_pause(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        self.recorder.recorder_pause().await
    }

    pub async fn recorder_resume(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        self.recorder.recorder_resume().await
    }

    pub async fn recorder_begin_stop(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        self.recorder.recorder_begin_stop().await
    }

    pub async fn recorder_cancel_stop(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        self.recorder.recorder_cancel_stop().await
    }

    pub async fn recorder_finish_stop(
        self: &Arc<Self>,
        name: Option<String>,
        edit: bool,
    ) -> Result<StopOutcome, RecorderError> {
        self.recorder.recorder_finish_stop(name, edit).await
    }

    pub async fn recorder_close(self: &Arc<Self>) {
        self.recorder.recorder_close().await
    }

    pub fn start_capture_loop(self: &Arc<Self>, spec: CaptureLoopSpec) -> Result<u64, String> {
        self.recorder.start_capture_loop(spec)
    }

    /// Initialize the session config so the core knows where the server is.
    pub fn configure(&self, token: String, api_base_url: String) -> Result<(), String> {
        self.recorder.configure(token, api_base_url);
        Ok(())
    }

    /// Set the list of blacklisted app names (replaces current list).
    pub fn set_blacklisted_apps(&self, apps: Vec<String>) -> Result<(), String> {
        let mut blacklist = self.blacklisted_apps.lock().map_err(|e| e.to_string())?;
        *blacklist = apps;
        Ok(())
    }

    /// Get the current list of blacklisted app names.
    pub fn get_blacklisted_apps(&self) -> Result<Vec<String>, String> {
        let blacklist = self.blacklisted_apps.lock().map_err(|e| e.to_string())?;
        Ok(blacklist.clone())
    }

    /// Take a native screenshot, encode as JPEG, return base64.
    pub fn take_screenshot(
        &self,
        source: CaptureSource,
        max_width: u32,
        max_height: u32,
        jpeg_quality: u8,
    ) -> Result<CaptureResult, String> {
        #[allow(unused_mut, unused_assignments)]
        let mut pipewire_fds = std::collections::HashMap::new();
        #[cfg(target_os = "linux")]
        if let Ok(guard) = self.pipewire_fds.lock() {
            pipewire_fds = guard.clone();
        }
        capture::take_screenshot(source, max_width, max_height, jpeg_quality, &pipewire_fds)
    }
}

pub fn base64_decode(b64: &str) -> Result<Vec<u8>, String> {
    use base64_engine::*;
    ENGINE
        .decode(b64)
        .map_err(|e| format!("Base64 decode failed: {e}"))
}

pub fn base64_encode(data: &[u8]) -> String {
    lookout_recorder::base64_encode(data)
}

mod base64_engine {
    pub use base64::engine::general_purpose::STANDARD as ENGINE;
    pub use base64::Engine;
}
