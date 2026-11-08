//! The recording, as every Lookout client runs it.
//!
//! The recorder state machine and its actions (open, pause, resume, stop,
//! close), the capture loop that ticks on the server's credit grid, the
//! ordered uploader with its backlog, and the once-a-second tray clock —
//! written once, over a [`Host`]: whatever captures frames, encodes clips,
//! sends HTTP, sleeps and spawns on a given platform. The desktop host is
//! `lookout-core` (native capture, tokio, reqwest); the browser host is
//! `lookout-web` (canvas and WebCodecs through JavaScript, `fetch`,
//! `setTimeout`).
//!
//! Nothing here knows which platform it is on. Time is the host's wall
//! clock in unix milliseconds; waiting is the host's `sleep`; running
//! something in the background is the host's `spawn`. Every behaviour the
//! desktop tests pin — the flush before the pause POST, the 409 retries,
//! the backlog across outages, the stale-target rule — lives here and
//! nowhere else.

#![forbid(unsafe_code)]

pub mod capture_loop;
pub mod events;
pub mod host;
pub mod recorder;
pub mod ticker;
pub mod uploader;

pub use capture_loop::{CaptureLoopHandle, CaptureLoopSpec};
pub use events::{
    CapturePreviewFrame, CaptureSessionTerminated, CaptureSourceLost, CaptureTickError,
    CaptureTickResult, CoreEvent, ScreencastRevoked,
};
pub use host::{
    BoxFuture, CaptureSession, ClipOutput, Frame, Grab, GrabJpeg, Host, MaybeSend, MaybeSync,
    Runtime, TaskHandle,
};
pub use recorder::Recorder;
pub use uploader::UploadJob;

/// Base64 (standard alphabet, padded) — what the preview events carry.
pub fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}
