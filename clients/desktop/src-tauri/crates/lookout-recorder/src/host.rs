//! What a platform provides to run a recording: a runtime, a transport,
//! frames, clips, and a place to report to.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lookout_protocol::transport::Transport;
use lookout_session::source::RecordingSource;

pub use lookout_protocol::transport::BoxFuture;

use crate::events::CoreEvent;

pub use lookout_protocol::transport::{MaybeSend, MaybeSync};

/// The host's executor and clock.
pub trait Runtime: MaybeSend + MaybeSync + 'static {
    /// Run a future to completion in the background. The recorder wraps
    /// everything it spawns so it can abort it; the host only has to run
    /// it.
    fn spawn(&self, future: BoxFuture<'static, ()>);
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    /// The wall clock, in milliseconds since the Unix epoch. Every stamp
    /// and every schedule is in these units.
    fn now_unix_ms(&self) -> i64;
}

/// A spawned task the recorder can abort and ask about.
#[derive(Clone)]
pub struct TaskHandle {
    abort: futures_util::future::AbortHandle,
    finished: Arc<AtomicBool>,
}

impl TaskHandle {
    /// Spawn `future` on `runtime`, abortably.
    pub fn spawn(runtime: &dyn Runtime, future: impl Future<Output = ()> + MaybeSend + 'static) -> Self {
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        let wrapped = futures_util::future::Abortable::new(future, registration);
        runtime.spawn(Box::pin(async move {
            let _ = wrapped.await;
            done.store(true, Ordering::Release);
        }));
        Self { abort, finished }
    }

    pub fn abort(&self) {
        self.abort.abort();
    }

    /// Whether the task has run to its end (or been aborted).
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire) || self.abort.is_aborted()
    }
}

/// Abort a task when dropped.
pub struct AbortOnDrop(pub TaskHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `future`, or `Err(())` once `duration` has passed on the host's clock.
pub async fn timeout<T>(
    runtime: &dyn Runtime,
    duration: Duration,
    future: impl Future<Output = T>,
) -> Result<T, ()> {
    let sleep = runtime.sleep(duration);
    tokio::pin!(future);
    tokio::pin!(sleep);
    tokio::select! {
        v = &mut future => Ok(v),
        _ = &mut sleep => Err(()),
    }
}

/// A JPEG the host produced: the upload payload, the flush, or a preview.
#[derive(Clone, Debug)]
pub struct Frame {
    pub jpeg: bytes::Bytes,
    pub width: u32,
    pub height: u32,
}

/// A finished clip from the host's encoder.
#[derive(Clone, Debug)]
pub struct ClipOutput {
    pub bytes: bytes::Bytes,
    /// `video/mp4` or `video/webm` — what the presigned URL will be signed
    /// with.
    pub content_type: &'static str,
    /// `mp4` or `webm` — the `format` the presign is asked for.
    pub format: &'static str,
    pub frame_count: u32,
    pub width: u32,
    pub height: u32,
}

/// What JPEG (if any) a frame grab should produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrabJpeg {
    /// No JPEG — a clip frame while nobody is looking at the preview.
    None,
    /// Preview-sized — the live preview between ticks.
    Preview,
    /// Full capture resolution at upload quality — the tick frame, which
    /// doubles as the JPEG upload / fallback payload.
    Full,
}

/// One grabbed frame.
#[derive(Clone, Debug, Default)]
pub struct Grab {
    /// The JPEG asked for, if any.
    pub jpeg: Option<Frame>,
    /// The frame was grabbed but could not be recorded into the clip
    /// (encoder init or append failed). The host has already discarded
    /// the clip; the loop counts the failure toward the clips latch.
    pub clip_error: Option<String>,
}

/// One capture run's frame and clip pipeline on the host: a loop opens one
/// when it starts and drops it when it ends. The host keeps the last
/// grabbed frame (for the flush) and the in-progress clip.
pub trait CaptureSession: MaybeSend + MaybeSync {
    /// Grab a frame from the source. With `record`, append it to the
    /// current clip, starting one if none is in progress.
    fn grab(&self, jpeg: GrabJpeg, record: bool) -> BoxFuture<'_, Result<Grab, String>>;
    /// Finish the in-progress clip and start the next. `Ok(None)` when
    /// there is nothing to cut (no clip was started, or it has no frames).
    fn clip_cut(&self) -> BoxFuture<'_, Result<Option<ClipOutput>, String>>;
    /// Drop the in-progress clip (a sleep gap, the end of the run).
    fn clip_discard(&self);
    /// The last grabbed frame as a full-quality JPEG, for the pause/stop
    /// flush. `Ok(None)` when nothing was ever grabbed.
    fn flush_frame(&self) -> BoxFuture<'_, Result<Option<Frame>, String>>;
    /// Whether this source can die without saying so, so that a run of
    /// failed grabs means it is gone (the PipeWire watchdog).
    fn watch_for_lost_source(&self) -> bool;
}

/// Everything the recorder asks of the platform.
pub trait Host: MaybeSend + MaybeSync + 'static {
    fn runtime(&self) -> &dyn Runtime;
    fn transport(&self) -> &dyn Transport;
    /// Free-form client telemetry sent on every upload-url request, e.g.
    /// `Lookout Desktop/0.2.6 (macOS 14.3)`.
    fn client_info(&self) -> &str;

    /// Deliver an event to the UI.
    fn emit(&self, event: CoreEvent);
    /// Whether anyone is looking at the live preview right now. When false
    /// the loop skips the per-frame preview JPEG (and, outside clips mode,
    /// the in-between frame grab entirely).
    fn wants_preview_frames(&self) -> bool;
    /// Show the recording clock text (`mm:ss` or `h:mm:ss`) in the menu
    /// bar / tray, and whether the recording is paused. Called once a
    /// second while a recording is open, only when something changed.
    fn set_tray_title(&self, time_text: &str, paused: bool);

    /// Open the source and wait until it produces frames — a camera's
    /// warm-up. Called before the loop starts (and again on resume), so
    /// the seed tick has something to upload. Screens need nothing.
    fn source_open(&self, source: &RecordingSource) -> BoxFuture<'static, Result<(), String>>;
    /// Release the source — the camera light must go off when the
    /// recording isn't recording.
    fn source_close(&self, source: &RecordingSource);
    /// Start a capture run for `source` at these bounds.
    fn begin_capture(&self, source: &RecordingSource, spec: &CaptureSpec) -> Box<dyn CaptureSession>;

    /// A capture loop started (`true`) or the last one ended (`false`):
    /// the scope for a keep-awake assertion.
    fn on_capturing(&self, capturing: bool) {
        let _ = capturing;
    }

    /// Whether this host can encode clips. A host without a video encoder
    /// (a microcontroller camera) returns `false` to stay in legacy JPEG
    /// mode, one JPEG per interval, even when the server enables clips.
    fn supports_clips(&self) -> bool {
        true
    }
}

/// Capture bounds for a run.
#[derive(Clone, Copy, Debug)]
pub struct CaptureSpec {
    pub max_width: u32,
    pub max_height: u32,
    /// 0–100.
    pub jpeg_quality: u8,
    /// The clip cadence the server dictates.
    pub frame_interval_ms: u64,
}
