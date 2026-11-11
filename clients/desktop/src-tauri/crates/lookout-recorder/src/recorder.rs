//! The recorder: the recording session as a state machine and the actions
//! that drive it, with every rule the field taught about ordering.
//!
//! What the TypeScript recorder used to do — and every patch it grew —
//! lives here, once, for every client:
//!
//! - **Pause flushes before it pauses.** The loop is stopped and awaited
//!   first; on cancel it uploads the partial minute flagged `final`, and
//!   only then does the pause POST go out (a paused session rejects
//!   confirms). The clock is held at the click so nothing ticks through
//!   the round trip; the flush credit lands underneath the held value.
//! - **A failed pause puts the loop back.** The session is still active
//!   server-side; leaving the user with recording controls over no loop
//!   was the bug.
//! - **409s are read as "look again".** Pause/resume/stop on a session the
//!   server already moved are not errors: the status is read back and the
//!   recorder lands wherever the server is. A 409 on a session that reads
//!   back still live/paused is a lost optimistic-locking race and gets one
//!   retry.
//! - **Resume starts the loop only after the server says active**, and the
//!   seed capture is stamped with that instant.
//! - **A lost source parks the controls.** The recording stays open and
//!   paused; the shell sends the user to the picker.
//! - **A session the server ended finishes the recording.**
//! - **The clock is one clock.** The main window, the tray and the tray
//!   popup derive their text from the same `RecordingClock`.
//!
//! Every action is serialized (one at a time) and refused outside the
//! phase it applies to, so a double-click or a tray action landing mid-way
//! through another cannot interleave.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lookout_protocol::api;
use lookout_protocol::upload::UploadHost;
use lookout_protocol::{ApiError, Transport};
use lookout_session::clock_offset::ClockOffset;
use lookout_session::config::{SessionClipCapabilities, SessionConfig};
use lookout_session::source::RecordingSource;
use lookout_session::state::*;
use lookout_session::timing::Timing;

use crate::capture_loop::{CaptureLoopHandle, CaptureLoopSpec};
use crate::events::{CapturePreviewFrame, CoreEvent};
use crate::host::{timeout, Host, TaskHandle};
use crate::ticker::TrayTickerHandle;
use crate::uploader::UploadJob;

/// Backoff between stop attempts on a 5xx. Mirrors `useSession.stop`.
const STOP_RETRY_DELAYS_MS: [u64; 3] = [1_000, 2_000, 4_000];

/// How long the host has to open the camera and produce a first frame.
const CAMERA_START_TIMEOUT_MS: u64 = 20_000;

/// Housekeeping cadence: the stale check and the loop-liveness check.
const HOUSEKEEPING_INTERVAL_MS: u64 = 10_000;

/// The two long waits above, overridable by tests so a hung camera or a
/// dead loop can be exercised in seconds rather than minutes.
static CAMERA_START_TIMEOUT_OVERRIDE_MS: AtomicU64 = AtomicU64::new(0);
static HOUSEKEEPING_INTERVAL_OVERRIDE_MS: AtomicU64 = AtomicU64::new(0);

fn camera_start_timeout() -> Duration {
    match CAMERA_START_TIMEOUT_OVERRIDE_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => Duration::from_millis(CAMERA_START_TIMEOUT_MS),
        ms => Duration::from_millis(ms),
    }
}

fn housekeeping_interval() -> Duration {
    match HOUSEKEEPING_INTERVAL_OVERRIDE_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => Duration::from_millis(HOUSEKEEPING_INTERVAL_MS),
        ms => Duration::from_millis(ms),
    }
}

/// Shorten the camera-start and housekeeping waits — for tests that
/// exercise a hung camera or a dead loop.
pub fn set_test_timeouts(camera_start_ms: u64, housekeeping_ms: u64) {
    CAMERA_START_TIMEOUT_OVERRIDE_MS.store(camera_start_ms, std::sync::atomic::Ordering::Relaxed);
    HOUSEKEEPING_INTERVAL_OVERRIDE_MS.store(housekeeping_ms, std::sync::atomic::Ordering::Relaxed);
}

/// The tasks that run while a recording is open.
pub struct RecorderTasks {
    housekeeping: TaskHandle,
}

/// `trackedSeconds` / `screenshotCount` etc. off a session or status JSON.
fn json_i64(v: &serde_json::Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64())
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// The recorder. One per process, shared behind an [`Arc`]: the capture
/// loop, the uploader, the clock task hold clones while they run.
pub struct Recorder {
    pub(crate) host: Arc<dyn Host>,
    /// Where uploads go: session token + API base URL. `None` until a
    /// recording is opened (or [`Recorder::configure`] is called).
    config: Mutex<Option<SessionConfig>>,
    /// Active capture loop (if running).
    pub(crate) capture_loop: Mutex<Option<CaptureLoopHandle>>,
    /// Counts capture-loop starts. Each loop tags its events with the
    /// generation it was started as, so the recorder can tell a flush from
    /// a loop it already replaced apart from the live one.
    pub(crate) loop_generation: AtomicU64,
    /// The 1s tray title ticker.
    pub(crate) tray_ticker: Mutex<Option<TrayTickerHandle>>,
    /// The open recording, if any.
    recording: Mutex<Option<Recording>>,
    /// Serializes recorder actions (open/pause/resume/stop/close): each one
    /// is a multi-step sequence with awaits in it, and two interleaving —
    /// a tray Pause landing while a Stop is mid-flush — must not be
    /// possible. Held across awaits, so it is an async mutex.
    action_lock: tokio::sync::Mutex<()>,
    /// The ticker and housekeeping tasks that run while a recording is open.
    tasks: Mutex<Option<RecorderTasks>>,
    /// Running estimate of `serverNow - clientNow`. Fed by the `serverTime`
    /// on every upload response; read wherever we stamp a capture or turn a
    /// server wall-clock target into a local delay. This is what makes a
    /// wrong system clock cost nothing.
    pub clock_offset: Mutex<ClockOffset>,
    /// The pipeline's timing (cadence, retries, timeouts). Production
    /// values unless a simulation scaled them.
    timing: Mutex<Timing>,
    /// Captured intervals not yet delivered, oldest first. The uploader
    /// of whichever capture loop is running works through it; it outlives
    /// any one loop so a restart (failed pause, re-picked source) loses
    /// nothing.
    pub upload_backlog: Mutex<std::collections::VecDeque<UploadJob>>,
    /// The `Arc` this recorder lives in, for code reached through a plain
    /// `&Recorder` (an event observer) that needs to spawn a task holding one.
    weak_self: std::sync::Weak<Recorder>,
}

impl UploadHost for Recorder {
    fn transport(&self) -> &dyn Transport {
        self.host.transport()
    }
    fn timing(&self) -> Timing {
        Recorder::timing(self)
    }
    fn client_info(&self) -> &str {
        self.host.client_info()
    }
    fn clock_offset(&self) -> &Mutex<ClockOffset> {
        &self.clock_offset
    }
    fn progress(&self, message: String) {
        self.emit(CoreEvent::Progress(message));
    }
}

impl Recorder {
    pub fn new(host: Arc<dyn Host>) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            host,
            config: Mutex::new(None),
            capture_loop: Mutex::new(None),
            loop_generation: AtomicU64::new(0),
            tray_ticker: Mutex::new(None),
            recording: Mutex::new(None),
            action_lock: tokio::sync::Mutex::new(()),
            tasks: Mutex::new(None),
            clock_offset: Mutex::new(ClockOffset::new()),
            timing: Mutex::new(Timing::default()),
            upload_backlog: Mutex::new(std::collections::VecDeque::new()),
            weak_self: weak.clone(),
        })
    }

    pub fn host(&self) -> &dyn Host {
        &*self.host
    }

    /// The pipeline's timing.
    pub fn timing(&self) -> Timing {
        *self.timing.lock().unwrap()
    }

    /// Scale the pipeline for a simulation. Only before any loop starts.
    pub fn set_timing(&self, timing: Timing) {
        *self.timing.lock().unwrap() = timing;
    }

    /// The host's wall clock.
    pub fn now_ms(&self) -> i64 {
        self.host.runtime().now_unix_ms()
    }

    /// The host's wall clock corrected into server time.
    pub fn corrected_now_ms(&self) -> i64 {
        let offset = self.clock_offset.lock().unwrap();
        offset.correct(self.now_ms())
    }

    /// The session uploads go to, if configured.
    pub fn config(&self) -> Option<SessionConfig> {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Set the session config so the recorder knows where the server is.
    pub fn configure(&self, token: String, api_base_url: String) {
        let mut config = self.config.lock().unwrap_or_else(|e| e.into_inner());
        *config = Some(SessionConfig { token, api_base_url });
    }

    /// An `Arc` to this recorder, if it is still alive.
    pub(crate) fn self_arc(&self) -> Option<Arc<Recorder>> {
        self.weak_self.upgrade()
    }

    /// Deliver an event to the host. Every event passes the recorder first,
    /// which folds it into the recording's state (a confirm credits the
    /// clock, a lost source parks the controls) and publishes the result;
    /// the raw event then goes out as it always has.
    pub fn emit(&self, event: CoreEvent) {
        self.emit_tagged(0, event);
    }

    /// [`Recorder::emit`] for events from a capture loop, tagged with the
    /// loop's generation so the recorder can ignore a loop it has already
    /// moved on from. Generation 0 is "not from a loop".
    pub fn emit_tagged(&self, generation: u64, event: CoreEvent) {
        self.observe(generation, &event);
        self.host.emit(event);
    }

    // ── Session API, with this recorder's timing ─────────────────────

    pub async fn session_get(&self, s: &SessionConfig) -> Result<serde_json::Value, ApiError> {
        api::session_get(self.host.transport(), s, self.timing().api_timeout()).await
    }

    pub async fn session_status(&self, s: &SessionConfig) -> Result<serde_json::Value, ApiError> {
        api::session_status(self.host.transport(), s, self.timing().api_timeout()).await
    }

    pub async fn session_pause(&self, s: &SessionConfig) -> Result<serde_json::Value, ApiError> {
        api::session_pause(self.host.transport(), s, self.timing().api_timeout()).await
    }

    pub async fn session_resume(&self, s: &SessionConfig) -> Result<serde_json::Value, ApiError> {
        api::session_resume(self.host.transport(), s, self.timing().api_timeout()).await
    }

    pub async fn session_stop(&self, s: &SessionConfig, edit: bool) -> Result<serde_json::Value, ApiError> {
        api::session_stop(self.host.transport(), s, edit, self.timing().api_timeout()).await
    }

    pub async fn session_rename(&self, s: &SessionConfig, name: &str) -> Result<serde_json::Value, ApiError> {
        api::session_rename(self.host.transport(), s, name, self.timing().api_timeout()).await
    }

    // ── State ────────────────────────────────────────────────────────

    /// The open recording's state, if any.
    pub fn recorder_state(&self) -> Option<RecorderState> {
        let now_ms = self.now_ms();
        let mut state = {
            let guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map(|rec| rec.snapshot(now_ms))?
        };
        state.pending_uploads = self
            .upload_backlog
            .lock()
            .map(|q| q.iter().filter(|j| !j.is_final()).count() as u32)
            .unwrap_or(0);
        Some(state)
    }

    /// Mutate the recording, if there is one, under the lock.
    pub fn with_recording<R>(&self, f: impl FnOnce(&mut Recording) -> R) -> Option<R> {
        let mut guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_mut().map(f)
    }

    /// Publish the current state (and any notices it earns) to the host.
    /// Never called with the recorder lock held: the host may call back.
    pub fn recorder_publish(&self) {
        let now_ms = self.now_ms();
        let (state, notices) = {
            let mut guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_mut() {
                Some(rec) => {
                    let mut state = rec.snapshot(now_ms);
                    state.pending_uploads = self
                        .upload_backlog
                        .lock()
                        .map(|q| q.iter().filter(|j| !j.is_final()).count() as u32)
                        .unwrap_or(0);
                    rec.published_stale = state.stale;
                    (Some(state), rec.take_notices())
                }
                None => (None, Vec::new()),
            }
        };
        self.host.emit(CoreEvent::RecorderState(state));
        for notice in notices {
            self.host.emit(CoreEvent::RecorderNotice(notice));
        }
    }

    /// Fold an event into the recording. Loop events carry the generation
    /// of the loop that produced them, and a loop this recording has moved
    /// on from is ignored (its flush may still be landing).
    fn observe(&self, generation: u64, event: &CoreEvent) {
        let now_ms = self.now_ms();
        let mut preview: Option<CoreEvent> = None;
        let mut follow_ups: Vec<FollowUp> = Vec::new();
        let mut release_source: Option<RecordingSource> = None;
        let changed = self
            .with_recording(|rec| {
                let from_live_loop = generation == 0 || generation == rec.loop_generation;
                match event {
                    CoreEvent::TickResult(r) if from_live_loop => {
                        // The first confirm is what activates a pending
                        // session.
                        if rec.session_status.as_deref() == Some("pending") {
                            rec.set_status("active");
                        }
                        rec.credit(Some(r.tracked_seconds), now_ms);
                        rec.ticks += 1;
                        rec.last_capture_at = Some(now_ms);
                        rec.capture_error = None;
                        if !r.preview_base64.is_empty() {
                            preview = Some(CoreEvent::PreviewFrame(CapturePreviewFrame {
                                preview_base64: r.preview_base64.clone(),
                                preview_width: r.preview_width,
                                preview_height: r.preview_height,
                            }));
                        }
                        true
                    }
                    CoreEvent::TickError(e) if from_live_loop => {
                        rec.capture_error = Some(e.message.clone());
                        true
                    }
                    CoreEvent::TrackedSeconds(ts) if from_live_loop => {
                        rec.credit(Some(*ts), now_ms);
                        true
                    }
                    CoreEvent::SessionTerminated(t) if from_live_loop => {
                        // The loop confirmed the terminal status with the
                        // server before stopping; finish now, then refresh
                        // the credited seconds behind it.
                        rec.finish(&t.status);
                        if std::mem::take(&mut rec.camera_open) {
                            release_source = Some(rec.source.clone());
                        }
                        // The loop's task has ended; reap its handle (and
                        // with it the keep-awake assertion) now rather than
                        // at the next action.
                        follow_ups.push(FollowUp::StopLoop);
                        follow_ups.push(FollowUp::SyncStatus);
                        true
                    }
                    CoreEvent::SourceLost(l) if from_live_loop => {
                        // The loop has already given up and exited — no
                        // flush (there is no frame to flush). Reap it.
                        follow_ups.push(FollowUp::StopLoop);
                        rec.lose_source(SourceLoss {
                            reason: SourceLossReason::Failed,
                            message: "Lookout lost the screen it was capturing — the stream \
                                      stopped sending frames. This usually means PipeWire \
                                      restarted."
                                .into(),
                            detail: Some(l.message.clone()),
                        });
                        true
                    }
                    CoreEvent::ScreencastRevoked(r) => {
                        if !rec.source.uses_pipewire_node(&r.node_ids) {
                            return false;
                        }
                        // The portal session is gone, so every further
                        // capture would come back empty — stop rather than
                        // tick away over a dead stream. The stop is the
                        // orderly one: the last good frame is still in
                        // memory, so the partial minute flushes.
                        let was_capturing = rec.capturing();
                        rec.lose_source(SourceLoss {
                            reason: SourceLossReason::Revoked,
                            message: "Screen sharing was stopped from your system's sharing \
                                      indicator, so nothing is being captured any more."
                                .into(),
                            detail: None,
                        });
                        if was_capturing {
                            follow_ups.push(FollowUp::StopLoop);
                        }
                        true
                    }
                    _ => false,
                }
            })
            .unwrap_or(false);
        if let Some(source) = release_source {
            self.host.source_close(&source);
        }
        if changed {
            self.recorder_publish();
        }
        if let Some(preview) = preview {
            self.host.emit(preview);
        }
        for follow_up in follow_ups {
            self.spawn_follow_up(follow_up);
        }
    }

    /// Async reactions to observed events, run off the event path.
    fn spawn_follow_up(&self, follow_up: FollowUp) {
        let Some(recorder) = self.self_arc() else {
            return;
        };
        let rt = self.host.runtime();
        TaskHandle::spawn(rt, async move {
            match follow_up {
                FollowUp::StopLoop => {
                    let _ = recorder.stop_capture_loop().await;
                }
                FollowUp::SyncStatus => {
                    let cfg = recorder.with_recording(|rec| rec.cfg.clone());
                    if let Some(cfg) = cfg {
                        if let Ok(v) = recorder.session_status(&cfg).await {
                            let now_ms = recorder.now_ms();
                            recorder.with_recording(|rec| {
                                rec.credit(json_i64(&v, "trackedSeconds"), now_ms);
                                if let Some(s) = json_str(&v, "status") {
                                    rec.set_status(s);
                                }
                            });
                            recorder.recorder_publish();
                        }
                    }
                }
            }
        });
    }

    /// Housekeeping, every `HOUSEKEEPING_INTERVAL`: publish when the stale
    /// flag flips, and notice a loop that died without saying so.
    pub fn recorder_housekeeping(&self) {
        let loop_finished = self.capture_loop_finished();
        let now_ms = self.now_ms();
        let changed = self
            .with_recording(|rec| {
                let mut changed = false;
                if rec.phase == RecorderPhase::Recording && loop_finished && rec.capture_error.is_none()
                {
                    // Defensive: every expected exit (terminated, source
                    // lost, cancelled) is reported through an event first.
                    rec.capture_error =
                        Some("The capture loop stopped unexpectedly. Pause and resume to restart it.".into());
                    changed = true;
                }
                if rec.stale(now_ms) != rec.published_stale {
                    changed = true;
                }
                changed
            })
            .unwrap_or(false);
        if changed {
            self.recorder_publish();
        }
    }

    fn start_tasks(self: &Arc<Self>) {
        let mut guard = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let recorder = Arc::clone(self);
        let rt = self.host.runtime();
        let housekeeping = TaskHandle::spawn(rt, async move {
            loop {
                recorder.host.runtime().sleep(housekeeping_interval()).await;
                recorder.recorder_housekeeping();
            }
        });
        *guard = Some(RecorderTasks { housekeeping });
        self.start_tray_ticker();
    }

    fn stop_tasks(&self) {
        let mut guard = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tasks) = guard.take() {
            tasks.housekeeping.abort();
        }
        self.stop_tray_ticker();
    }

    // ── Actions ──────────────────────────────────────────────────────

    /// Open a recording for a session: load it, and if it is recordable
    /// right now (`active` or `pending`), start capturing. A `paused`
    /// session opens paused and waits for Resume. Replaces any recording
    /// already open — after its loop has flushed.
    ///
    /// The returned state is also published; a load failure is a state
    /// (`LoadError`), not an `Err`.
    pub async fn recorder_open(
        self: &Arc<Self>,
        cfg: SessionConfig,
        source: RecordingSource,
    ) -> Result<RecorderState, RecorderError> {
        self.open_inner(cfg, source, true).await
    }

    /// Open a recording without capturing yet, for a host that has nothing
    /// to capture until the user acts (a web page before the screen share).
    /// A pending session stays pending, parked, until `recorder_resume`
    /// starts it — its first upload is what activates it. An active one is
    /// paused server-side so it doesn't accrue time no one is recording.
    pub async fn recorder_open_parked(
        self: &Arc<Self>,
        cfg: SessionConfig,
        source: RecordingSource,
    ) -> Result<RecorderState, RecorderError> {
        self.open_inner(cfg, source, false).await
    }

    async fn open_inner(
        self: &Arc<Self>,
        cfg: SessionConfig,
        source: RecordingSource,
        autostart: bool,
    ) -> Result<RecorderState, RecorderError> {
        let _action = self.action_lock.lock().await;
        self.close_inner().await;

        {
            let mut guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(Recording::new(cfg.clone(), source, self.now_ms()));
        }
        self.start_tasks();
        self.recorder_publish();

        self.configure(cfg.token.clone(), cfg.api_base_url.clone());

        let loaded = self.session_get(&cfg).await;
        let now_ms = self.now_ms();
        let start = match loaded {
            Err(e) => {
                self.with_recording(|rec| {
                    rec.phase = RecorderPhase::LoadError;
                    rec.session_error = Some(e.message);
                });
                false
            }
            Ok(v) => self
                .with_recording(|rec| {
                    let status = json_str(&v, "status").unwrap_or("").to_string();
                    rec.set_status(&status);
                    rec.session_name = json_str(&v, "name").unwrap_or("").to_string();
                    rec.base_capture_count = json_i64(&v, "screenshotCount").unwrap_or(0).max(0) as u32;
                    rec.credit(json_i64(&v, "trackedSeconds"), now_ms);
                    rec.clip_capabilities = Some(SessionClipCapabilities {
                        clips_enabled: v.get("clipsEnabled").and_then(|c| c.as_bool()) == Some(true),
                        frame_interval_ms: v.get("frameIntervalMs").and_then(|f| f.as_u64()),
                    });
                    if is_terminal(&status) {
                        rec.finish(&status);
                        false
                    } else if is_live(&status) {
                        true
                    } else {
                        rec.phase = RecorderPhase::Paused;
                        false
                    }
                })
                .unwrap_or(false),
        };

        if start && autostart {
            self.start_capturing().await;
        } else if start {
            let pending = self
                .with_recording(|rec| rec.session_status.as_deref() == Some("pending"))
                .unwrap_or(false);
            if pending {
                self.with_recording(|rec| rec.phase = RecorderPhase::Paused);
            } else {
                // A failed pause is in the state's actionError; the
                // recording is left paused here either way, since nothing
                // is capturing.
                let _ = self.pause_inner(RecorderPhase::Pausing).await;
            }
        }
        self.recorder_publish();
        self.recorder_state().ok_or(RecorderError::NoRecording)
    }

    /// Start the capture loop for the open recording (camera first, when
    /// there is one) and mark it recording. Holds no lock across awaits.
    async fn start_capturing(self: &Arc<Self>) {
        // The recording runs from THIS instant — the record/resume press,
        // as far as the user is concerned, and the moment the server has
        // the session active from. The seed capture is stamped with it so
        // the credit grid starts here, not after the camera has warmed up
        // or the screencast has negotiated (seconds, every resume).
        let started_ms = self.corrected_now_ms();
        let Some((source, caps)) = self.with_recording(|rec| {
            // Starting over is the only thing that clears a loss or a stale
            // error: the loop that would have reported a success is the one
            // that stopped.
            rec.capture_error = None;
            rec.source_lost = None;
            (rec.source.clone(), rec.clip_capabilities.clone())
        }) else {
            return;
        };

        if source.is_camera() {
            // Ours to release from here on, whether or not the open succeeds
            // (a failed getUserMedia can still hold the device).
            self.with_recording(|rec| rec.camera_open = true);
            // Open the camera and wait for frames BEFORE the loop starts,
            // so the seed tick — the capture that activates the session
            // and anchors its credit schedule — has something to upload.
            let budget = camera_start_timeout();
            let started = timeout(self.host.runtime(), budget, self.host.source_open(&source)).await;
            let error = match started {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("Camera failed to start: {e}")),
                Err(()) => Some(format!(
                    "Camera failed to start: no frames after {}s",
                    budget.as_secs()
                )),
            };
            if let Some(error) = error {
                lookout_session::log_line!("[recorder] {error}");
                // The loop still starts: its ticks keep trying (and keep
                // the error honest), and a camera that shows up later is
                // reopened by the tick path.
                self.with_recording(|rec| rec.capture_error = Some(error));
            }
        }

        let spec = CaptureLoopSpec {
            source,
            max_width: MAX_CAPTURE_WIDTH,
            max_height: MAX_CAPTURE_HEIGHT,
            jpeg_quality: CAPTURE_JPEG_QUALITY,
            clip_capabilities: caps,
            seed_captured_at_ms: Some(started_ms),
        };
        let now_ms = self.now_ms();
        match self.start_capture_loop(spec) {
            Ok(generation) => {
                self.with_recording(|rec| {
                    rec.loop_generation = generation;
                    rec.clock.start(now_ms);
                    rec.phase = RecorderPhase::Recording;
                });
            }
            Err(e) => {
                self.with_recording(|rec| {
                    rec.capture_error = Some(format!("Couldn't start capturing: {e}"));
                    rec.phase = RecorderPhase::Paused;
                });
            }
        }
    }

    /// Stop the loop (flushing the partial minute) and release the camera.
    /// The clock stops the moment the loop is gone: nothing is capturing,
    /// so nothing should be ticking.
    async fn stop_capturing(&self) {
        let _ = self.stop_capture_loop().await;
        self.with_recording(|rec| rec.clock.stop());
        self.release_source();
    }

    /// Tell the host to close the camera, once, if this recording opened it.
    fn release_source(&self) {
        let source = self
            .with_recording(|rec| std::mem::take(&mut rec.camera_open).then(|| rec.source.clone()))
            .flatten();
        if let Some(source) = source {
            self.host.source_close(&source);
        }
    }

    /// `GET status`, folding the credited seconds into the clock.
    async fn sync_status(&self, cfg: &SessionConfig) -> Result<String, ApiError> {
        let v = self.session_status(cfg).await?;
        let now_ms = self.now_ms();
        self.with_recording(|rec| rec.credit(json_i64(&v, "trackedSeconds"), now_ms));
        Ok(json_str(&v, "status").unwrap_or("").to_string())
    }

    /// `POST pause`, with a 409 read as "read the status back". A 409 means
    /// either the session is terminal (nothing to pause) or the server lost
    /// an optimistic-locking race ("state changed concurrently, please
    /// retry"); if the status comes back still live, the pause is retried
    /// once. Returns the status the session is now in.
    async fn pause_session(&self, cfg: &SessionConfig) -> Result<String, ApiError> {
        match self.session_pause(cfg).await {
            Ok(v) => Ok(json_str(&v, "status").unwrap_or("paused").to_string()),
            Err(e) if e.status == Some(409) => {
                lookout_session::log_line!("[recorder] pause returned 409, syncing status");
                let status = self.sync_status(cfg).await?;
                if !is_live(&status) {
                    return Ok(status);
                }
                lookout_session::log_line!("[recorder] session still {status} after a 409 — retrying the pause");
                match self.session_pause(cfg).await {
                    Ok(v) => Ok(json_str(&v, "status").unwrap_or("paused").to_string()),
                    Err(e) if e.status == Some(409) => self.sync_status(cfg).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// `POST resume`, with the same 409 handling: a session that reads back
    /// as still paused gets one more try.
    async fn resume_session(&self, cfg: &SessionConfig) -> Result<String, ApiError> {
        match self.session_resume(cfg).await {
            Ok(v) => Ok(json_str(&v, "status").unwrap_or("active").to_string()),
            Err(e) if e.status == Some(409) => {
                lookout_session::log_line!("[recorder] resume returned 409, syncing status");
                let status = self.sync_status(cfg).await?;
                if status != "paused" {
                    return Ok(status);
                }
                lookout_session::log_line!("[recorder] session still paused after a 409 — retrying the resume");
                match self.session_resume(cfg).await {
                    Ok(v) => Ok(json_str(&v, "status").unwrap_or("active").to_string()),
                    Err(e) if e.status == Some(409) => self.sync_status(cfg).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// The shared body of Pause and Stop: freeze the clock, flush and stop
    /// the loop, pause the session. `in_flight` is the phase to show while
    /// it happens. On success the recording is paused (or finished, if the
    /// server turned out to have ended it); on failure the loop is put
    /// back — the session is still active and must not be left with
    /// recording controls over no loop.
    async fn pause_inner(self: &Arc<Self>, in_flight: RecorderPhase) -> Result<(), RecorderError> {
        let now_ms = self.now_ms();
        let Some((cfg, was_recording)) = self.with_recording(|rec| {
            rec.action_error = None;
            let was_recording = rec.phase == RecorderPhase::Recording;
            rec.phase = in_flight;
            if was_recording {
                // Freeze every clock surface at the CLICK, before any
                // awaits: the flush credit lands underneath the held value.
                rec.clock.hold(now_ms);
            }
            (rec.cfg.clone(), was_recording)
        }) else {
            return Err(RecorderError::NoRecording);
        };
        self.recorder_publish();

        if was_recording {
            // Stop AND AWAIT the loop before the pause POST: on cancel it
            // flushes the in-progress partial minute as a `final` capture,
            // and that confirm must land while the session is still
            // active — a paused session rejects it.
            self.stop_capturing().await;
        }

        match self.pause_session(&cfg).await {
            Ok(status) => {
                self.with_recording(|rec| {
                    if rec.phase == RecorderPhase::Finished {
                        return;
                    }
                    if is_terminal(&status) {
                        rec.finish(&status);
                    } else {
                        rec.set_status(&status);
                        rec.clock.stop();
                        rec.phase = RecorderPhase::Paused;
                    }
                });
                self.recorder_publish();
                Ok(())
            }
            Err(e) => {
                lookout_session::log_line!("[recorder] pause failed: {e}");
                // The session is still recording server-side. Release the
                // hold and let the clock catch back up rather than lie
                // frozen — and put the loop back, since we stopped it.
                let still_live = self
                    .with_recording(|rec| {
                        rec.action_error = Some(e.message.clone());
                        rec.clock.release_hold();
                        rec.phase != RecorderPhase::Finished
                    })
                    .unwrap_or(false);
                if still_live {
                    if was_recording {
                        self.start_capturing().await;
                    } else {
                        self.with_recording(|rec| rec.phase = RecorderPhase::Paused);
                    }
                }
                self.recorder_publish();
                Err(e.into())
            }
        }
    }

    /// The shared body of Resume: resume the session, then start capturing.
    async fn resume_inner(self: &Arc<Self>) -> Result<(), RecorderError> {
        let Some(cfg) = self.with_recording(|rec| {
            rec.action_error = None;
            rec.phase = RecorderPhase::Resuming;
            rec.cfg.clone()
        }) else {
            return Err(RecorderError::NoRecording);
        };
        self.recorder_publish();

        // A pending session has nothing to resume: the first upload
        // activates it, and the server refuses a resume of it with a 409.
        let pending = self
            .with_recording(|rec| rec.session_status.as_deref() == Some("pending"))
            .unwrap_or(false);
        let resumed = if pending {
            Ok("pending".to_string())
        } else {
            self.resume_session(&cfg).await
        };
        match resumed {
            Ok(status) => {
                let finished = self
                    .with_recording(|rec| {
                        if is_terminal(&status) {
                            rec.finish(&status);
                            true
                        } else {
                            rec.set_status(&status);
                            rec.phase == RecorderPhase::Finished
                        }
                    })
                    .unwrap_or(true);
                if !finished {
                    self.start_capturing().await;
                }
                self.recorder_publish();
                Ok(())
            }
            Err(e) => {
                lookout_session::log_line!("[recorder] resume failed: {e}");
                self.with_recording(|rec| {
                    rec.action_error = Some(e.message.clone());
                    if rec.phase != RecorderPhase::Finished {
                        rec.phase = RecorderPhase::Paused;
                    }
                });
                self.recorder_publish();
                Err(e.into())
            }
        }
    }

    fn require_phase(&self, allowed: &[RecorderPhase]) -> Result<RecorderPhase, RecorderError> {
        let phase = self
            .with_recording(|rec| rec.phase)
            .ok_or(RecorderError::NoRecording)?;
        if allowed.contains(&phase) {
            Ok(phase)
        } else {
            Err(RecorderError::InvalidPhase { phase })
        }
    }

    /// Pause the recording: flush the partial minute, then pause the
    /// session. Only while `Recording`.
    pub async fn recorder_pause(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        let _action = self.action_lock.lock().await;
        self.require_phase(&[RecorderPhase::Recording])?;
        lookout_session::log_line!("[recorder] pausing");
        self.pause_inner(RecorderPhase::Pausing).await?;
        self.recorder_state().ok_or(RecorderError::NoRecording)
    }

    /// Resume a paused recording. Refused with [`RecorderError::SourceLost`]
    /// when there is nothing to resume onto — the shell should send the
    /// user to the picker (and, from the tray, surface the window).
    pub async fn recorder_resume(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        let _action = self.action_lock.lock().await;
        self.require_phase(&[RecorderPhase::Paused])?;
        if self.with_recording(|rec| rec.source_lost.is_some()) == Some(true) {
            return Err(RecorderError::SourceLost);
        }
        lookout_session::log_line!("[recorder] resuming");
        self.resume_inner().await?;
        self.recorder_state().ok_or(RecorderError::NoRecording)
    }

    /// Stop clicked: pause the session (flushing first, exactly like Pause)
    /// and move to the naming prompt. From `Recording` or `Paused` — a
    /// paused session is paused again, idempotently.
    pub async fn recorder_begin_stop(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        let _action = self.action_lock.lock().await;
        self.require_phase(&[RecorderPhase::Recording, RecorderPhase::Paused])?;
        lookout_session::log_line!("[recorder] stop clicked, pausing and opening the prompt");
        self.pause_inner(RecorderPhase::StopPausing).await?;
        self.with_recording(|rec| {
            if rec.phase == RecorderPhase::Paused {
                rec.phase = RecorderPhase::StopPrompting;
            }
        });
        self.recorder_publish();
        self.recorder_state().ok_or(RecorderError::NoRecording)
    }

    /// "Keep recording" from the prompt: close it and resume. With the
    /// source lost there is nothing to resume onto, so the prompt just
    /// closes (the recording stays paused) and the shell asks for a source.
    pub async fn recorder_cancel_stop(self: &Arc<Self>) -> Result<RecorderState, RecorderError> {
        let _action = self.action_lock.lock().await;
        self.require_phase(&[RecorderPhase::StopPrompting])?;
        let lost = self
            .with_recording(|rec| {
                rec.phase = RecorderPhase::Paused;
                rec.source_lost.is_some()
            })
            .unwrap_or(false);
        if lost {
            lookout_session::log_line!("[recorder] keep recording with no source — back to the picker");
            self.recorder_publish();
        } else {
            lookout_session::log_line!("[recorder] keep recording: resuming");
            self.resume_inner().await?;
        }
        self.recorder_state().ok_or(RecorderError::NoRecording)
    }

    /// Record from a different source without closing the recording, so
    /// the session keeps its clock, its counts and its upload backlog
    /// (closing drops the backlog, and with it any minutes an outage was
    /// still holding).
    ///
    /// From `Recording`: the old source's loop is stopped AND AWAITED, so
    /// its partial minute flushes as a `final` capture while the session
    /// is still active, then the new source's loop starts. No pause or
    /// resume round trip: the session stays active throughout, and only
    /// the seconds between the flush and the new seed go uncredited.
    ///
    /// From `Paused` (a lost source included): the recording resumes on
    /// the new source. Picking a source is the go-ahead to record it.
    pub async fn recorder_switch_source(
        self: &Arc<Self>,
        source: RecordingSource,
    ) -> Result<RecorderState, RecorderError> {
        let _action = self.action_lock.lock().await;
        let phase = self.require_phase(&[RecorderPhase::Recording, RecorderPhase::Paused])?;
        lookout_session::log_line!("[recorder] switching source");
        if phase == RecorderPhase::Recording {
            let now_ms = self.now_ms();
            self.with_recording(|rec| {
                rec.action_error = None;
                // The flush's credit lands under the clock as it was at
                // the click, as with a pause.
                rec.clock.hold(now_ms);
            });
            self.recorder_publish();
            self.stop_capturing().await;
            let finished = self
                .with_recording(|rec| {
                    if rec.phase != RecorderPhase::Finished {
                        rec.switch_source(source);
                    }
                    rec.phase == RecorderPhase::Finished
                })
                .unwrap_or(true);
            // The server may have ended the session while the old loop
            // flushed; there is nothing to switch to then.
            if !finished {
                self.start_capturing().await;
            }
            self.recorder_publish();
        } else {
            // Paused means nothing is capturing; a camera left open by a
            // loss is released here, once.
            self.release_source();
            self.with_recording(|rec| rec.switch_source(source));
            self.recorder_publish();
            self.resume_inner().await?;
        }
        self.recorder_state().ok_or(RecorderError::NoRecording)
    }

    /// Finish the stop: name the session (optional, non-fatal), stop it
    /// (retrying on 5xx), and finish the recording. With `edit`, the
    /// timelapse is held unpublished after compiling so the user can cut it
    /// first; the shell opens its editor on the returned outcome.
    pub async fn recorder_finish_stop(
        self: &Arc<Self>,
        name: Option<String>,
        edit: bool,
    ) -> Result<StopOutcome, RecorderError> {
        let _action = self.action_lock.lock().await;
        self.require_phase(&[RecorderPhase::StopPrompting])?;
        let Some(cfg) = self.with_recording(|rec| {
            rec.action_error = None;
            rec.phase = RecorderPhase::StopFinishing;
            rec.cfg.clone()
        }) else {
            return Err(RecorderError::NoRecording);
        };
        self.recorder_publish();
        lookout_session::log_line!(
            "[recorder] stopping, name: {}, edit: {edit}",
            name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("(none)")
        );

        match self.session_finish(&cfg, name.as_deref(), edit).await {
            Ok(outcome) => {
                let now_ms = self.now_ms();
                self.with_recording(|rec| {
                    rec.credit(outcome.tracked_seconds, now_ms);
                    rec.finish(&outcome.status);
                });
                self.release_source();
                self.recorder_publish();
                Ok(StopOutcome {
                    status: outcome.status,
                    edit,
                })
            }
            Err(e) => {
                lookout_session::log_line!("[recorder] stop failed: {e}");
                self.with_recording(|rec| {
                    rec.action_error = Some(e.message.clone());
                    if rec.phase != RecorderPhase::Finished {
                        rec.phase = RecorderPhase::StopPrompting;
                    }
                });
                self.recorder_publish();
                Err(e.into())
            }
        }
    }

    /// Rename (optional, non-fatal) and stop a session, the way the
    /// recorder does it, for a shell that has no recording open — the
    /// source picker's Stop button. Retries the stop on 5xx with backoff;
    /// a 409 reads the status back and counts as stopped if the session is
    /// in any terminal state.
    pub async fn session_finish(
        &self,
        cfg: &SessionConfig,
        name: Option<&str>,
        edit: bool,
    ) -> Result<FinishOutcome, ApiError> {
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            if let Err(e) = self.session_rename(cfg, name).await {
                lookout_session::log_line!("[recorder] rename failed (non-fatal): {e}");
            }
        }
        let mut attempt = 0usize;
        loop {
            match self.session_stop(cfg, edit).await {
                Ok(v) => {
                    return Ok(FinishOutcome {
                        status: json_str(&v, "status").unwrap_or("stopped").to_string(),
                        tracked_seconds: json_i64(&v, "trackedSeconds"),
                        total_active_seconds: json_i64(&v, "totalActiveSeconds"),
                    })
                }
                Err(e) if e.status == Some(409) => {
                    lookout_session::log_line!("[recorder] stop returned 409, syncing status");
                    let v = self.session_status(cfg).await?;
                    let status = json_str(&v, "status").unwrap_or("").to_string();
                    if is_terminal(&status) {
                        return Ok(FinishOutcome {
                            status,
                            tracked_seconds: json_i64(&v, "trackedSeconds"),
                            total_active_seconds: None,
                        });
                    }
                    return Err(ApiError {
                        status: Some(409),
                        message: format!("Session is {status}, cannot stop"),
                    });
                }
                Err(e)
                    if e.status.is_some_and(|s| s >= 500)
                        && attempt < STOP_RETRY_DELAYS_MS.len() =>
                {
                    lookout_session::log_line!(
                        "[recorder] stop failed ({}), retrying in {}ms (attempt {}/{})",
                        e.status.unwrap_or(0),
                        STOP_RETRY_DELAYS_MS[attempt],
                        attempt + 1,
                        STOP_RETRY_DELAYS_MS.len()
                    );
                    self.host
                        .runtime()
                        .sleep(Duration::from_millis(STOP_RETRY_DELAYS_MS[attempt]))
                        .await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Close the recording: stop capturing (flushing the partial minute),
    /// release the camera, and forget it. The session itself is left alone
    /// — still open, still holding its time — so the next source pick
    /// drops straight back into recording. Publishes `None`.
    pub async fn recorder_close(self: &Arc<Self>) {
        let _action = self.action_lock.lock().await;
        self.close_inner().await;
    }

    async fn close_inner(&self) {
        let open = {
            let guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
            guard.is_some()
        };
        if !open {
            return;
        }
        lookout_session::log_line!("[recorder] closing");
        self.stop_capturing().await;
        {
            let mut guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
            *guard = None;
        }
        // Whatever the uploader still owed is this recording's; nothing
        // will deliver it now.
        self.drop_upload_backlog();
        self.stop_tasks();
        self.host.emit(CoreEvent::RecorderState(None));
    }

    /// Tear the recording down without flushing — the window is gone.
    /// Synchronous, for a shell's destroy handler. Publishes nothing: there
    /// is no window to publish to.
    pub fn recorder_abort(&self) {
        if let Some(handle) = self.take_capture_loop() {
            lookout_session::log_line!("[recorder] aborting the capture loop");
            handle.cancel();
            self.host.on_capturing(false);
        }
        let source = {
            let mut guard = self.recording.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .take()
                .and_then(|rec| rec.camera_open.then_some(rec.source))
        };
        if let Some(source) = source {
            self.host.source_close(&source);
        }
        self.drop_upload_backlog();
        self.stop_tasks();
    }

    fn drop_upload_backlog(&self) {
        let left = {
            let mut queue = self.upload_backlog.lock().unwrap_or_else(|e| e.into_inner());
            let n = queue.iter().filter(|j| j.done.is_none()).count();
            queue.clear();
            n
        };
        if left > 0 {
            lookout_session::log_line!("[recorder] {left} undelivered capture(s) dropped with the recording");
        }
    }

    /// The `SessionConfig` of the open recording, for a shell that needs to
    /// pause it on the way out of the process.
    pub fn recorder_session(&self) -> Option<SessionConfig> {
        self.with_recording(|rec| rec.cfg.clone())
    }
}

#[derive(Debug)]
enum FollowUp {
    StopLoop,
    SyncStatus,
}
