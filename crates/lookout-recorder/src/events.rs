//! Everything the recorder reports to the UI. [`CoreEvent::name`] is the
//! stable wire name each event has always had on the Tauri event bus, for
//! hosts that forward these over some other bus as JSON.

use serde::Serialize;

use lookout_session::state::{RecorderNotice, RecorderState};

/// Event payload delivered after each successful capture.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureTickResult {
    pub confirmed: bool,
    pub tracked_seconds: i64,
    pub next_expected_at: String,
    /// The server's id for the confirmed capture.
    pub screenshot_id: String,
    pub preview_base64: String,
    pub preview_width: u32,
    pub preview_height: u32,
}

impl From<lookout_protocol::upload::CaptureUploadResult> for CaptureTickResult {
    fn from(r: lookout_protocol::upload::CaptureUploadResult) -> Self {
        Self {
            confirmed: r.confirmed,
            tracked_seconds: r.tracked_seconds,
            next_expected_at: r.next_expected_at,
            screenshot_id: r.screenshot_id,
            preview_base64: r.preview_base64,
            preview_width: r.preview_width,
            preview_height: r.preview_height,
        }
    }
}

/// Event payload emitted when a capture tick fails.
#[derive(Clone, Serialize)]
pub struct CaptureTickError {
    pub message: String,
}

/// Event payload emitted when the capture source is gone for good and the
/// loop has given up — as opposed to `CaptureTickError`, which is a bad
/// minute the loop expects to recover from.
#[derive(Clone, Serialize)]
pub struct CaptureSourceLost {
    pub message: String,
}

/// Event payload for an in-between live-preview frame from the capture
/// loop (one per frame interval while the preview is wanted).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePreviewFrame {
    pub preview_base64: String,
    pub preview_width: u32,
    pub preview_height: u32,
}

/// Event payload emitted when the capture loop detects a terminal session state.
#[derive(Clone, Serialize)]
pub struct CaptureSessionTerminated {
    pub status: String,
}

/// The compositor tore a screencast session down (the Linux portal's
/// `Closed`): these PipeWire nodes are gone.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreencastRevoked {
    pub node_ids: Vec<u32>,
}

/// Everything the recorder reports.
#[derive(Clone)]
pub enum CoreEvent {
    /// Human-readable upload pipeline progress (`"uploading 84KB to R2..."`).
    Progress(String),
    /// Authoritative credited seconds learned during sleep recovery.
    TrackedSeconds(i64),
    /// The server reports the session in a terminal state; the loop stopped.
    SessionTerminated(CaptureSessionTerminated),
    /// An upload tick confirmed.
    TickResult(CaptureTickResult),
    /// An upload tick failed (after retries); the loop carries on.
    TickError(CaptureTickError),
    /// The capture source is gone for good and the loop gave up.
    SourceLost(CaptureSourceLost),
    /// A live-preview frame, between upload ticks, while previews are wanted.
    PreviewFrame(CapturePreviewFrame),
    /// The compositor tore a screencast session down (Linux portal).
    ScreencastRevoked(ScreencastRevoked),
    /// The recorder's state changed — the whole thing, every time. `None`
    /// once the recording is closed. A shell draws this and nothing else.
    RecorderState(Option<RecorderState>),
    /// Something the user should be told about even when the window is
    /// behind whatever they're working in: a pause, a capture failure, a
    /// session the server ended. The shell turns it into an OS notification.
    RecorderNotice(RecorderNotice),
}

impl CoreEvent {
    /// The event's wire name (the Tauri event channel it is emitted on).
    pub fn name(&self) -> &'static str {
        match self {
            CoreEvent::Progress(_) => "capture-progress",
            CoreEvent::TrackedSeconds(_) => "capture-tracked-seconds",
            CoreEvent::SessionTerminated(_) => "capture-session-terminated",
            CoreEvent::TickResult(_) => "capture-tick-result",
            CoreEvent::TickError(_) => "capture-tick-error",
            CoreEvent::SourceLost(_) => "capture-source-lost",
            CoreEvent::PreviewFrame(_) => "capture-preview-frame",
            CoreEvent::ScreencastRevoked(_) => "screencast-revoked",
            CoreEvent::RecorderState(_) => "recorder-state",
            CoreEvent::RecorderNotice(_) => "recorder-notice",
        }
    }
}
