//! The capture loop: frame cadence, clip cutting, sleep/pause recovery,
//! the pause/stop flush and the self-scheduling upload tick. Runs on the
//! host's runtime, immune to any UI timer throttling; the UI only receives
//! [`CoreEvent`]s.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use lookout_protocol::api;
use lookout_protocol::upload::{UploadPayload, ENABLE_CREDIT_MODE};
use lookout_session::config::SessionClipCapabilities;
use lookout_session::schedule::next_capture_delay_ms;
use lookout_session::source::RecordingSource;
use lookout_session::time::{parse_iso_to_unix_ms, unix_ms_to_iso};
use lookout_session::timing::CAPTURE_INTERVAL_SECS;

use crate::base64_encode;
use crate::events::{
    CapturePreviewFrame, CaptureSourceLost, CaptureTickError, CoreEvent,
};
use crate::host::{timeout, AbortOnDrop, CaptureSpec, GrabJpeg, TaskHandle};
use crate::recorder::Recorder;
use crate::uploader::{check_session, uploader_task, SessionCheck, UploadJob, UploadOutcome};

/// Handle for cancelling a capture loop.
pub struct CaptureLoopHandle {
    cancel_tx: watch::Sender<bool>,
    task: TaskHandle,
    /// Resolves when the loop's task has run to its end.
    done_rx: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl CaptureLoopHandle {
    /// Cancel the loop and abort its task without waiting for the flush.
    /// [`Recorder::stop_capture_loop`] is the orderly path (it awaits the
    /// pause/stop flush); this is for a shell tearing the loop down from a
    /// path of its own (e.g. the main window closing).
    pub fn cancel(self) {
        let _ = self.cancel_tx.send(true);
        self.task.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// Everything a capture loop needs to start.
#[derive(Clone, Debug)]
pub struct CaptureLoopSpec {
    pub source: RecordingSource,
    pub max_width: u32,
    pub max_height: u32,
    /// 0–100.
    pub jpeg_quality: u8,
    /// The session's clip capability, when the caller already fetched the
    /// session. `None` makes the loop fetch it itself.
    pub clip_capabilities: Option<SessionClipCapabilities>,
    /// Corrected unix ms the recording (re)started at — the instant the
    /// user pressed record or resume. The seed capture is stamped with it
    /// so the credit grid is anchored there rather than a source warm-up
    /// later. `None` stamps the seed when its frame is taken.
    pub seed_captured_at_ms: Option<i64>,
}

/// Budget for re-opening the camera after a tick found it gone.
const CAMERA_REOPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// If the wall-clock gap between ticks exceeds this, the machine
/// probably slept (or the host was throttled hard).
const SLEEP_THRESHOLD_SECS: u64 = CAPTURE_INTERVAL_SECS * 2 + 30; // 150s

/// Fallback frame cadence when the server doesn't advertise one (pre-clips
/// servers): every 10s = 6 frames/min. Mirrors CLIP_FRAME_INTERVAL_MS in
/// @lookout/shared. When the server sends `frameIntervalMs` on the session
/// GET, that value wins — the cadence is server-authoritative.
const DEFAULT_FRAME_INTERVAL_MS: u64 = 10_000;

/// Consecutive clip-encoder failures tolerated before this capture run gives
/// up on clips and records plain JPEGs for the rest of the session.
///
/// A broken encoder is already survivable one interval at a time (each
/// failure falls back to a JPEG), but "survivable" was not the same as
/// "quiet": on a machine where the encoder can never initialize, the loop
/// retried it on every single frame — for hours. Latching off after a few
/// consecutive failures keeps the recording intact and stops the thrash.
const MAX_CLIP_ENCODER_FAILURES: u32 = 3;

/// Consecutive failed capture ticks that mean a source that can die
/// silently (a PipeWire stream) is gone for good rather than briefly
/// unhappy. Only armed when the host says so; a monitor capture failing a
/// few times in a row is ordinary (locked screen, sleeping display) and
/// must not end the recording.
const MAX_CONSECUTIVE_SOURCE_FAILURES: u32 = 3;

/// Record one clip-encoder failure, and latch clips off for the rest of the
/// run once they stop looking transient. Any clip that finalizes
/// successfully resets the counter, so a one-off hiccup never disables
/// clips.
fn note_clip_failure(failures: &mut u32, clips_mode: &mut bool) {
    *failures += 1;
    if *failures >= MAX_CLIP_ENCODER_FAILURES && *clips_mode {
        *clips_mode = false;
        lookout_session::log_line!(
            "[capture-loop] {} consecutive clip-encoder failures — disabling clips \
             for this session, continuing with one JPEG per minute",
            *failures
        );
    }
}

async fn fetch_clip_capabilities(recorder: &Recorder) -> SessionClipCapabilities {
    let Some(config) = recorder.config() else {
        return SessionClipCapabilities::default();
    };
    match api::session_clip_capabilities(
        recorder.host.transport(),
        &config,
        Duration::from_secs(15),
    )
    .await
    {
        Ok(caps) => caps,
        Err(e) => {
            lookout_session::log_line!("[capture-loop] capability fetch failed ({e}) — clips off");
            SessionClipCapabilities::default()
        }
    }
}

/// Where the next tick should fire, in local wall-clock ms, from the
/// server's `nextExpectedAt` — or `None` when the target is stale (see
/// [`lookout_session::schedule::next_capture_delay_ms`]).
fn next_fire_from_server_target(
    recorder: &Recorder,
    target: &str,
    interval_ms: u64,
    last_slot_ms: i64,
) -> Option<i64> {
    let corrected_now = recorder.corrected_now_ms();
    let target_ms = match parse_iso_to_unix_ms(target) {
        Some(ms) => ms,
        None => corrected_now + interval_ms as i64,
    };
    let next = next_capture_delay_ms(target_ms, corrected_now, interval_ms, last_slot_ms)?;
    if next.clamped {
        // With the offset applied this should never bind for clock skew —
        // if it fires, something else is feeding us bad targets, and
        // silence here is how the skew bug ran unnoticed for three months.
        lookout_session::log_line!(
            "[capture-loop] next-capture target {target} exceeds the 2x-interval \
             clamp — capping to {}ms",
            next.delay_ms
        );
    }
    Some(recorder.now_ms() + next.delay_ms as i64)
}

/// The capture loop task.
pub(crate) async fn capture_loop_task(
    recorder: Arc<Recorder>,
    generation: u64,
    spec: CaptureLoopSpec,
    mut cancel_rx: watch::Receiver<bool>,
) {
    let CaptureLoopSpec {
        source,
        max_width,
        max_height,
        jpeg_quality,
        clip_capabilities,
        seed_captured_at_ms,
    } = spec;
    let rt = recorder.host.runtime();

    // Self-scheduling chain: each confirm's `nextExpectedAt` refines when
    // the next tick fires. Targets are local wall-clock ms, so a machine
    // that slept fires immediately on wake (catch-up) rather than waiting
    // out the remainder on a clock that stood still.
    let interval_ms = recorder.timing().capture_interval_ms;
    let mut last_tick_ms = rt.now_unix_ms();

    // The uploader: captures are handed to it in order and it delivers
    // them in order, retrying across outages (see `uploader_task`). The
    // loop itself never waits on the network — a slow or dead link must
    // not shift WHEN frames are captured, only when they arrive.
    let (job_tx, job_rx) = tokio::sync::mpsc::unbounded_channel::<UploadJob>();
    let (outcome_tx, mut outcome_rx) = tokio::sync::mpsc::unbounded_channel::<UploadOutcome>();
    let uploader = TaskHandle::spawn(
        rt,
        uploader_task(Arc::clone(&recorder), generation, job_rx, outcome_tx),
    );
    // Guard: whatever way this task ends, the uploader goes with it, so no
    // detached upload can land after a pause POST or outlive the loop.
    let _uploader_guard = AbortOnDrop(uploader);

    // The seed capture's timestamp, handed in by the recorder: the moment
    // the recording (re)started, not the moment the first frame came out
    // of the camera or the screencast. Consumed by the first tick.
    let mut seed_captured_at_ms = seed_captured_at_ms;

    // Clip capability comes from the server, once per loop run — handed in
    // by a caller that already loaded the session, or fetched here. Any
    // fetch failure (or clips off) means legacy JPEG mode, bit-for-bit.
    let caps = match clip_capabilities {
        Some(caps) => caps,
        None => fetch_clip_capabilities(&recorder).await,
    };
    // Mutable: latches off after MAX_CLIP_ENCODER_FAILURES consecutive
    // encoder failures, so a machine with a broken encoder settles into
    // plain JPEG mode instead of retrying forever.
    let mut clips_mode = caps.clips_enabled && recorder.host.supports_clips();
    let mut clip_encoder_failures: u32 = 0;
    // Server-authoritative cadence, clamped defensively against a
    // misbehaving server so the loop can't spin or stall.
    let frame_interval_ms = caps
        .frame_interval_ms
        .unwrap_or(DEFAULT_FRAME_INTERVAL_MS)
        .clamp(500, 30_000);
    if clips_mode {
        lookout_session::log_line!("[capture-loop] clips enabled (frame every {frame_interval_ms}ms)");
    }

    // The host's frame and clip pipeline for this run. Dropped at the end,
    // which discards any half-recorded clip.
    let session = recorder.host.begin_capture(
        &source,
        &CaptureSpec {
            max_width,
            max_height,
            jpeg_quality,
            frame_interval_ms,
        },
    );
    let watch_for_lost_source = session.watch_for_lost_source();
    let mut consecutive_capture_failures: u32 = 0;

    // The first tick fires immediately in BOTH modes. The first capture is
    // the streak seed: it credits 0 seconds, and its capturedAt becomes the
    // session's startedAt and the anchor every later 60s credit mark is
    // measured from. The seed uploads as a plain JPEG at t=0 (fast
    // activation — smaller than any clip) and the clip keeps rolling
    // through it, so the first FINISHED clip covers the full first minute.
    let mut next_fire_ms: i64 = rt.now_unix_ms();
    let mut first_upload_done = false;
    // The slot of the newest capture taken, so a confirm arriving late
    // (a backlogged interval, delivered after the link came back) can't
    // schedule a slot that has already been captured.
    let mut last_slot_ms: i64 = 0;

    // Set only by the deliberate cancel path (pause/stop). Gates the final
    // partial-capture flush below — other exits (source lost, terminated
    // server-side, not configured) must not attempt it.
    let mut cancelled = false;
    // Whether anything was ever grabbed, so the flush knows there is a
    // frame to encode.
    let mut have_frame = false;

    'outer: loop {
        // ── Wait until next_fire, collecting frames along the way ──
        // Frames run at the clip cadence (server-set, 6/min) through the
        // SAME capture path as uploads. In clips mode every frame is
        // recorded into the current clip; the JPEG preview side is
        // focus-gated either way (nobody can see it unfocused).
        loop {
            let now = rt.now_unix_ms();
            if now >= next_fire_ms {
                break;
            }
            let wake_ms = (now + frame_interval_ms as i64).min(next_fire_ms);
            let sleep = rt.sleep(Duration::from_millis((wake_ms - now).max(0) as u64));
            tokio::pin!(sleep);
            // Third arm: the in-flight upload finishing mid-wait. Its body
            // only records the outcome — applying it happens after the
            // select.
            let mut upload_outcome: Option<UploadOutcome> = None;
            tokio::select! {
                _ = &mut sleep => {}
                _ = cancel_rx.changed() => {
                    lookout_session::log_line!("[capture-loop] cancelled");
                    cancelled = true;
                    break 'outer;
                }
                outcome = outcome_rx.recv() => {
                    upload_outcome = outcome;
                }
            }
            if let Some(outcome) = upload_outcome {
                if outcome.terminated {
                    // The uploader found the session over server-side (it
                    // emitted SessionTerminated). Nothing to flush.
                    break 'outer;
                }
                if let Some(target) = outcome.next_expected_at.as_deref() {
                    if let Some(refined) =
                        next_fire_from_server_target(&recorder, target, interval_ms, last_slot_ms)
                    {
                        next_fire_ms = refined;
                    }
                }
                // next_fire was just refined by the confirm — recompute the
                // wake target instead of falling through with a stale one.
                continue;
            }
            // Woke for the upload tick, not a frame.
            if rt.now_unix_ms() >= next_fire_ms {
                break;
            }

            let focused = recorder.host.wants_preview_frames();
            if !clips_mode && !focused {
                continue;
            }
            let jpeg_mode = if focused {
                GrabJpeg::Preview
            } else {
                GrabJpeg::None
            };
            match session.grab(jpeg_mode, clips_mode).await {
                Ok(grab) => {
                    have_frame = true;
                    if let Some(e) = grab.clip_error {
                        lookout_session::log_line!("[capture-loop] clip frame failed: {e} — JPEG fallback this interval");
                        note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                    }
                    if focused {
                        if let Some(jpeg) = grab.jpeg {
                            recorder.emit_tagged(
                                generation,
                                CoreEvent::PreviewFrame(CapturePreviewFrame {
                                    preview_base64: base64_encode(&jpeg.jpeg),
                                    preview_width: jpeg.width,
                                    preview_height: jpeg.height,
                                }),
                            );
                        }
                    }
                }
                Err(e) => {
                    // Frame-level failure: log and keep going — the upload
                    // tick has its own error handling and retry cadence.
                    lookout_session::log_line!("[capture-loop] frame capture failed: {e}");
                }
            }
        }

        // ── Upload tick ──
        let now_ms = rt.now_unix_ms();
        let elapsed_secs = ((now_ms - last_tick_ms).max(0) / 1000) as u64;
        last_tick_ms = now_ms;

        let Some(config) = recorder.config() else {
            recorder.emit_tagged(
                generation,
                CoreEvent::TickError(CaptureTickError {
                    message: "Not configured".into(),
                }),
            );
            break;
        };

        // Sleep detection
        if elapsed_secs > SLEEP_THRESHOLD_SECS {
            lookout_session::log_line!(
                "[capture-loop] detected sleep (gap: {}s), checking session status...",
                elapsed_secs
            );
            // A clip spanning a sleep gap would carry an hours-long hole —
            // drop it and start fresh after recovery.
            session.clip_discard();
            if let SessionCheck::Terminated = check_session(&recorder, generation, &config).await {
                break;
            }
        }

        // Grab the tick frame — the clip's final frame, the UI preview,
        // and the JPEG fallback, all from one capture. Full-size JPEG:
        // this one may be uploaded.
        let grab_result = session.grab(GrabJpeg::Full, clips_mode).await;

        // The slot this capture belongs to, in corrected (server) time.
        // The seed tick is stamped with the recording's start — the
        // instant the user pressed record/resume — so the credit grid is
        // anchored there and not a camera-warm-up later; every other tick
        // is stamped NOW, when the frame was taken, never when the upload
        // eventually reaches the server.
        let slot_ms = match seed_captured_at_ms.take() {
            Some(ms) => ms,
            None => recorder.corrected_now_ms(),
        };
        last_slot_ms = slot_ms;
        let captured_at = if ENABLE_CREDIT_MODE {
            Some(unix_ms_to_iso(slot_ms))
        } else {
            None
        };
        // Next tick one interval after THIS slot — on the grid even when
        // the upload fails or runs long; refined to the server's
        // nextExpectedAt when the confirm lands (see the wait loop).
        next_fire_ms = rt.now_unix_ms() + (slot_ms + interval_ms as i64 - recorder.corrected_now_ms()).max(0);

        match grab_result {
            Ok(grab) => {
                consecutive_capture_failures = 0;
                have_frame = true;
                let Some(frame) = grab.jpeg else {
                    lookout_session::log_line!("[capture-loop] tick grab returned no JPEG");
                    first_upload_done = true;
                    continue;
                };
                if let Some(e) = grab.clip_error {
                    lookout_session::log_line!("[capture-loop] clip frame failed: {e}");
                    note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                }
                let jpeg_base64 = base64_encode(&frame.jpeg);

                // Clips: the final frame was appended by the grab; cut this
                // interval's clip. On the SEED tick (the immediate first
                // upload) the clip is NOT cut: the seed uploads as a plain
                // JPEG — its content is dropped by the compiler anyway, and
                // what matters is planting the streak anchor at
                // record-press — while the clip keeps rolling so the first
                // finished clip covers the whole first minute.
                let clip = if clips_mode && first_upload_done {
                    match session.clip_cut().await {
                        Ok(Some(c)) => {
                            // A clip made it out whole — the encoder works,
                            // so earlier failures were transient.
                            clip_encoder_failures = 0;
                            Some(c)
                        }
                        Ok(None) => None,
                        Err(e) => {
                            lookout_session::log_line!("[capture-loop] clip finalize failed: {e} — uploading JPEG instead");
                            note_clip_failure(&mut clip_encoder_failures, &mut clips_mode);
                            None
                        }
                    }
                } else {
                    None
                };

                // Hand the interval to the uploader. Clip first; ANY
                // clip-upload failure retries it as the JPEG so the credit
                // streak never skips a beat — and a JPEG that can't be
                // delivered right now waits in the uploader's backlog for
                // the link to come back.
                let job = UploadJob {
                    token: config.token.clone(),
                    clip: clip.map(|c| {
                        UploadPayload::clip(
                            c.bytes,
                            c.content_type,
                            c.format,
                            c.frame_count,
                            c.width,
                            c.height,
                            jpeg_base64.clone(),
                        )
                    }),
                    jpeg: UploadPayload::jpeg(frame.jpeg, jpeg_base64, frame.width, frame.height),
                    captured_at,
                    captured_ms: slot_ms,
                    done: None,
                };
                if job_tx.send(job).is_err() {
                    lookout_session::log_line!("[capture-loop] uploader is gone — stopping");
                    break;
                }
            }
            Err(e) => {
                lookout_session::log_line!("[capture-loop] screenshot failed: {e}");
                consecutive_capture_failures += 1;
                if watch_for_lost_source
                    && consecutive_capture_failures >= MAX_CONSECUTIVE_SOURCE_FAILURES
                {
                    lookout_session::log_line!(
                        "[capture-loop] {consecutive_capture_failures} capture ticks in a row \
                         failed on a source that can die silently — it is gone, stopping"
                    );
                    recorder.emit_tagged(
                        generation,
                        CoreEvent::SourceLost(CaptureSourceLost { message: e }),
                    );
                    break;
                }
                recorder.emit_tagged(
                    generation,
                    CoreEvent::TickError(CaptureTickError {
                        message: e.clone(),
                    }),
                );
                // A camera that stopped answering (unplugged and replugged,
                // the stream ended across a sleep) is reopened in place, so
                // the next tick has a chance instead of the recording
                // silently never uploading again. Best effort and bounded.
                if source.is_camera() {
                    match timeout(rt, CAMERA_REOPEN_TIMEOUT, recorder.host.source_open(&source)).await {
                        Ok(Ok(())) => lookout_session::log_line!("[capture-loop] camera reopened"),
                        Ok(Err(e)) => lookout_session::log_line!("[capture-loop] camera reopen failed: {e}"),
                        Err(()) => lookout_session::log_line!("[capture-loop] camera reopen timed out"),
                    }
                }
                // Local capture failure — the next tick stays on the grid.
            }
        }

        // Whatever happened, the seed tick is spent — every later tick cuts
        // its clip. (Set even on failure: a failed seed retries on the normal
        // cadence, and that retry should behave like any other tick.)
        first_upload_done = true;
    }

    // Pause/stop: flush one last capture flagged `final` so the partial
    // minute since the last credited mark still credits (the server's
    // creditFinalCapture) — pausing at 03:15 resumes at 03:15, not 03:00.
    // Only on deliberate cancellation: the session is still active and
    // `stop_capture_loop` awaits this task before the pause POST (a paused
    // session rejects confirms).
    //
    // THE FLUSH IS A SINGLE JPEG, DELIBERATELY. Earlier versions cut and
    // uploaded the in-progress clip here, and every step of that was a
    // place for the pause button to die (the encoder draining can block
    // for seconds). The partial minute's CREDIT needs one verifiable
    // capture, not a video: encode the last frame already in memory.
    //
    // Queued BEHIND anything the uploader still owes (a still-unconfirmed
    // minute confirmed after the final would be refused as non-monotonic,
    // and the final's credit is measured from the last credited mark — so
    // the order is backlog first, then the flush). Bounded as a whole: the
    // pause button must never hang on the network. Past the budget, what
    // hasn't landed is lost — exactly what happened before the flush
    // existed, and the uploader dies with this task so nothing can land
    // after the pause POST.
    if cancelled && have_frame {
        if let Some(config) = recorder.config() {
            let flush_started = rt.now_unix_ms();
            match session.flush_frame().await {
                Ok(Some(frame)) => {
                    let slot_ms = recorder.corrected_now_ms();
                    let captured_at = if ENABLE_CREDIT_MODE {
                        Some(unix_ms_to_iso(slot_ms))
                    } else {
                        None
                    };
                    lookout_session::log_line!(
                        "[capture-loop] flush: encoded {}KB JPEG in {}ms",
                        frame.jpeg.len() / 1024,
                        rt.now_unix_ms() - flush_started
                    );
                    let jpeg_base64 = base64_encode(&frame.jpeg);
                    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
                    let job = UploadJob {
                        token: config.token.clone(),
                        clip: None,
                        jpeg: UploadPayload::jpeg(frame.jpeg, jpeg_base64, frame.width, frame.height)
                            .final_capture(),
                        captured_at,
                        captured_ms: slot_ms,
                        done: Some(done_tx),
                    };
                    let budget = recorder.timing().flush_budget();
                    if job_tx.send(job).is_ok() {
                        match timeout(rt, budget, done_rx).await {
                            Ok(Ok(Ok(()))) => lookout_session::log_line!(
                                "[capture-loop] flush: final partial capture confirmed in {}ms",
                                rt.now_unix_ms() - flush_started
                            ),
                            Ok(Ok(Err(e))) => lookout_session::log_line!(
                                "[capture-loop] flush: FAILED after {}ms: {e}",
                                rt.now_unix_ms() - flush_started
                            ),
                            Ok(Err(_)) => lookout_session::log_line!("[capture-loop] flush: uploader went away"),
                            Err(()) => lookout_session::log_line!(
                                "[capture-loop] flush: budget of {}ms exhausted — the partial minute is lost",
                                budget.as_millis()
                            ),
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => lookout_session::log_line!("[capture-loop] flush: encode failed: {e}"),
            }
        }
    }

    // Never leave a half-recorded clip behind on pause/stop/cancel. AFTER
    // the flush on purpose: discarding can wait on the encoder pipeline,
    // and that wait must never sit between the user and their credit.
    session.clip_discard();
    drop(session);

    lookout_session::log_line!("[capture-loop] stopped");
}

impl Recorder {
    /// Start the capture loop. Replaces any existing loop. Returns the
    /// loop's generation, which tags every event it emits.
    pub fn start_capture_loop(self: &Arc<Self>, spec: CaptureLoopSpec) -> Result<u64, String> {
        // Stop any existing loop first
        {
            let mut guard = self.capture_loop.lock().map_err(|e| e.to_string())?;
            if let Some(handle) = guard.take() {
                let _ = handle.cancel_tx.send(true);
                handle.task.abort();
            }
        }

        // A recording is capturing from here until the loop stops — the
        // right scope for the keep-awake assertion.
        self.host.on_capturing(true);

        let generation = self
            .loop_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let recorder = Arc::clone(self);

        lookout_session::log_line!("[capture-loop] starting with {:?}, {}x{} q{}", spec.source, spec.max_width, spec.max_height, spec.jpeg_quality);

        let task = TaskHandle::spawn(self.host.runtime(), async move {
            capture_loop_task(recorder, generation, spec, cancel_rx).await;
            let _ = done_tx.send(());
        });

        {
            let mut guard = self.capture_loop.lock().map_err(|e| e.to_string())?;
            *guard = Some(CaptureLoopHandle {
                cancel_tx,
                task,
                done_rx: Some(done_rx),
            });
        }

        Ok(generation)
    }

    /// Stop the capture loop (if running).
    ///
    /// Async, and AWAITED before the pause/stop POST: on cancellation the
    /// loop flushes the in-progress partial minute as a `final` capture,
    /// and that confirm must land while the session is still active — a
    /// paused session rejects it. Bounded so a dead network can't wedge
    /// the pause button; on timeout the flush is abandoned (the partial
    /// minute is lost, exactly as it was before the flush existed).
    pub async fn stop_capture_loop(&self) -> Result<(), String> {
        let handle = {
            let mut guard = self.capture_loop.lock().map_err(|e| e.to_string())?;
            guard.take()
        };
        if let Some(mut handle) = handle {
            lookout_session::log_line!("[capture-loop] stopping");
            let _ = handle.cancel_tx.send(true);
            // The flush itself is budgeted (Timing::flush_budget); this only
            // backstops a loop wedged outside the flush.
            let backstop = self.timing().flush_budget() + Duration::from_secs(3);
            if let Some(done_rx) = handle.done_rx.take() {
                if timeout(self.host.runtime(), backstop, done_rx).await.is_err() {
                    lookout_session::log_line!("[capture-loop] final flush timed out — aborting the loop");
                    handle.task.abort();
                }
            }
        }
        // Not capturing any more — let the machine nap/sleep normally again.
        self.host.on_capturing(false);
        Ok(())
    }

    /// Whether a capture loop is currently registered (running or not yet
    /// observed as finished). `false` if the lock is poisoned.
    pub fn has_capture_loop(&self) -> bool {
        self.capture_loop
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false)
    }

    /// Kill the loop's task without unregistering it — a simulated crash,
    /// for the recorder's liveness check. Tests only.
    pub fn abort_capture_loop_for_test(&self) {
        if let Some(handle) = self.capture_loop.lock().unwrap().as_ref() {
            handle.task.abort();
        }
    }

    /// Whether a registered capture loop's task has already ended on its
    /// own — the server ended the session, the source was lost, or it
    /// died. `false` when there is no loop at all.
    pub fn capture_loop_finished(&self) -> bool {
        self.capture_loop
            .lock()
            .map(|g| g.as_ref().is_some_and(|h| h.is_finished()))
            .unwrap_or(false)
    }

    /// Unregister the loop without stopping it — for a caller that will
    /// cancel it on its own.
    pub fn take_capture_loop(&self) -> Option<CaptureLoopHandle> {
        self.capture_loop
            .lock()
            .ok()
            .and_then(|mut g| g.take())
    }
}
