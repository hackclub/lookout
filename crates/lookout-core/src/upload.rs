//! The desktop's side of the upload pipeline: the shared reqwest client and
//! the one-shot capture paths a shell can call.
//! The pipeline itself — presign → PUT → confirm, retries, error shaping —
//! is `lookout_protocol::upload`, re-exported here.

pub use lookout_protocol::upload::{
    captured_at_now, upload_and_confirm, CaptureUploadResult, ConfirmResponse, UploadError,
    UploadErrorKind, UploadHost, UploadPayload, UploadUrlResponse, ENABLE_CREDIT_MODE,
};
pub use lookout_session::time::unix_ms_to_iso;

use crate::{base64_decode, base64_encode, capture, CaptureSource, Core, CoreEvent, SessionConfig};

/// Shared HTTP client for all server/R2 traffic. Building a `reqwest::Client`
/// allocates a fresh connection pool + TLS config, so constructing one per
/// request (as each capture tick used to) both wastes CPU and forces a new
/// TCP/TLS handshake every 60 seconds. One shared client keeps connections
/// alive between ticks. Timeouts differ per call site, so they're applied
/// per-request via `RequestBuilder::timeout` instead of on the client.
pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
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
pub(crate) fn describe_reqwest_error(err: &reqwest::Error) -> String {
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

impl Core {
    /// Full capture-upload-confirm pipeline in Rust (no browser CORS issues).
    /// Returns the confirm data AND the screenshot preview (base64) so the
    /// frontend can display the captured frame without a separate IPC call.
    pub async fn capture_and_upload(
        &self,
        sources: Vec<CaptureSource>,
        max_width: u32,
        max_height: u32,
        jpeg_quality: u8,
    ) -> Result<CaptureUploadResult, String> {
        let config = self
            .config()
            .ok_or("Not configured — call configure() first")?;

        // Read blacklisted apps
        let blacklisted = {
            let guard = self.blacklisted_apps.lock().map_err(|e| e.to_string())?;
            guard.clone()
        };

        // Native screenshot
        self.emit(CoreEvent::Progress("capturing screen...".to_string()));
        #[allow(unused_mut, unused_assignments)]
        let mut pipewire_fds = std::collections::HashMap::new();
        #[cfg(target_os = "linux")]
        if let Ok(guard) = self.pipewire_fds.lock() {
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
                // Uploaded, so it is a recorded frame even though JS drove it.
                capture::ResizeQuality::Recorded,
            )
        })
        .await
        .map_err(|e| format!("spawn_blocking panicked: {e}"))??;
        self.emit(CoreEvent::Progress(format!(
            "captured {}x{} ({}KB jpeg)",
            screenshot.width,
            screenshot.height,
            screenshot.data.len() / 1024
        )));

        let captured_at = if ENABLE_CREDIT_MODE {
            Some(captured_at_now(&**self.recorder()))
        } else {
            None
        };
        let jpeg_base64 = base64_encode(&screenshot.data);
        let payload = UploadPayload::jpeg(
            bytes::Bytes::from(screenshot.data),
            jpeg_base64,
            screenshot.width,
            screenshot.height,
        );
        upload_and_confirm(&**self.recorder(), &payload, captured_at.as_deref(), &config)
            .await
            .map_err(|e| e.message)
    }

    /// Upload a pre-captured frame (e.g. from browser camera capture).
    /// Accepts base64-encoded JPEG from the frontend, runs the upload pipeline.
    pub async fn upload_frame(
        &self,
        base64: String,
        width: u32,
        height: u32,
    ) -> Result<CaptureUploadResult, String> {
        let config = self
            .config()
            .ok_or("Not configured — call configure() first")?;

        self.emit(CoreEvent::Progress(format!("uploading camera frame {}x{}", width, height)));

        let captured_at = if ENABLE_CREDIT_MODE {
            Some(captured_at_now(&**self.recorder()))
        } else {
            None
        };
        let jpeg_bytes = bytes::Bytes::from(base64_decode(&base64)?);
        let payload = UploadPayload::jpeg(jpeg_bytes, base64, width, height);
        upload_and_confirm(&**self.recorder(), &payload, captured_at.as_deref(), &config)
            .await
            .map_err(|e| e.message)
    }
}

/// Best-effort "pause the session" for a shell that is about to exit with a
/// capture loop still running, so the session doesn't sit "active" until
/// the server auto-pauses it. Short timeout, errors only logged: the
/// process is leaving either way.
pub async fn pause_session_before_exit(config: &SessionConfig) {
    let transport = crate::transport::ReqwestTransport;
    match lookout_protocol::api::session_pause(
        &transport,
        config,
        std::time::Duration::from_secs(5),
    )
    .await
    {
        Ok(_) => lookout_session::log_line!("[exit] pause response: ok"),
        Err(e) => lookout_session::log_line!("[exit] pause failed (best-effort): {e}"),
    }
}
