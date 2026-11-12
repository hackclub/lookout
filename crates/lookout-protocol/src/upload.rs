//! The upload pipeline: presigned URL → PUT to R2 → confirm, with the retry
//! policy and error shaping every capture path shares, and the clock-offset
//! bookkeeping those round trips feed.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use lookout_session::clock_offset::ClockOffset;
use lookout_session::config::SessionConfig;
use lookout_session::time::{parse_iso_to_unix_ms, unix_ms_to_iso};
use lookout_session::timing::Timing;

use crate::api::{session_url, url_with_query};
use crate::transport::{MaybeSend, MaybeSync, Method, Request, Transport};

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
    /// The server's id for the confirmed capture.
    #[serde(default)]
    pub screenshot_id: String,
    /// Base64-encoded JPEG of the captured frame (same image that was uploaded)
    pub preview_base64: String,
    pub preview_width: u32,
    pub preview_height: u32,
}

/// What the pipeline needs from whoever runs it.
pub trait UploadHost: MaybeSend + MaybeSync {
    fn transport(&self) -> &dyn Transport;
    fn timing(&self) -> Timing;
    /// Free-form client telemetry sent on every upload-url request, e.g.
    /// `Lookout Desktop/0.2.6 (macOS 14.3)`. NOT the HTTP User-Agent.
    fn client_info(&self) -> &str;
    /// The running estimate of `serverNow - clientNow`, fed by every
    /// response's `serverTime` and read wherever a capture is stamped.
    fn clock_offset(&self) -> &Mutex<ClockOffset>;
    /// Human-readable pipeline progress, for a UI that shows it.
    fn progress(&self, message: String);
}

/// Credit-mode opt-in toggle. New clients always send `capturedAt`; the
/// server flips the session into credit mode on the first request and
/// stays there. Old clients never send it and stay in bucket mode.
pub const ENABLE_CREDIT_MODE: bool = true;

/// Build an ISO-8601 timestamp in UTC for the current instant, corrected
/// into server time by the running clock-offset estimate. Used as the
/// client-attested `capturedAt` on upload requests when credit mode is on.
/// A no-op for a healthy clock; for a skewed one it's the difference between
/// every capture landing in the ±30s credit window and none of them doing so.
pub fn captured_at_now(host: &dyn UploadHost) -> String {
    let corrected_ms = {
        let offset = host.clock_offset().lock().unwrap();
        offset.correct(host.transport().now_unix_ms())
    };
    unix_ms_to_iso(corrected_ms)
}

/// Outcome of a single upload-pipeline attempt, used by [`retry_step`] to
/// decide whether a failure is worth retrying.
enum StepError {
    /// Transient failure (timeout, connection error, 5xx, …) — retry with
    /// backoff.
    Retryable(String),
    /// Permanent failure — fail fast, no retry: the session's state or the
    /// request itself is the problem, and more attempts can't change that.
    Terminal(UploadErrorKind, String),
}

/// Why an upload failed, as far as the uploader's decisions go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadErrorKind {
    /// Network trouble, a timeout, a 5xx, a rate limit: the capture is
    /// fine and worth delivering later.
    Transient,
    /// The server refused this capture for good (a 4xx other than a rate
    /// limit — a non-monotonic timestamp, a bad payload). Retrying it is
    /// pointless; drop it.
    Rejected(u16),
    /// HTTP 409: the session is paused or over server-side. The capture
    /// may still be deliverable once the session's state is sorted out.
    SessionState,
}

/// An upload-pipeline failure: the diagnostic plus what kind it was.
#[derive(Debug, Clone)]
pub struct UploadError {
    pub message: String,
    pub kind: UploadErrorKind,
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl UploadError {
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: UploadErrorKind::Transient,
        }
    }
}

/// Compact, consistent message for an HTTP error response: the status code
/// and reason, plus the body when there is one.
fn http_error(status: u16, reason: &str, body: &str) -> String {
    let status = if reason.is_empty() {
        status.to_string()
    } else {
        format!("{status} {reason}")
    };
    let body = body.trim();
    if body.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {body}")
    }
}

/// Classify an HTTP error response into a [`StepError`]. 409 is the
/// session's state (terminal for the attempt, the uploader decides what
/// next); a rate limit, a request timeout or any 5xx is worth retrying;
/// every other 4xx is a refusal that no retry can fix.
fn classify_http(status: u16, msg: String) -> StepError {
    match status {
        409 => StepError::Terminal(UploadErrorKind::SessionState, msg),
        408 | 429 => StepError::Retryable(msg),
        400..=499 => StepError::Terminal(UploadErrorKind::Rejected(status), msg),
        _ => StepError::Retryable(msg),
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
///   (1.2s); #3 …`.
///
/// Each entry is `(attempt_number, cause, elapsed_seconds)`. Per-attempt
/// elapsed times matter because they distinguish a hard timeout from an
/// instant refusal, which point at different failure modes.
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

/// Attempts per step before giving up on it.
/// Retry an upload step with exponential backoff, mirroring the SDK's
/// original `retry()` (clients/react/src/hooks/useUploader.ts): up to
/// `timing.max_attempts` attempts, sleeping `timing.retry_delay(i)` between them. A
/// `StepError::Terminal` short-circuits without retrying. On exhaustion the
/// error is the full per-attempt history with timing (via
/// [`summarize_attempts`]) rather than just the last failure — that's what
/// tells a steady outage apart from flapping connectivity.
async fn retry_step<T, F, Fut>(
    transport: &dyn Transport,
    timing: &Timing,
    label: &str,
    mut attempt: F,
) -> Result<T, UploadError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, StepError>>,
{
    let mut history: Vec<(usize, String, f64)> = Vec::new();
    let mut n = 0usize;
    loop {
        let start = transport.now_unix_ms();
        match attempt().await {
            Ok(v) => return Ok(v),
            Err(StepError::Terminal(kind, msg)) => {
                return Err(UploadError {
                    message: format!("{label}: {msg}"),
                    kind,
                })
            }
            Err(StepError::Retryable(msg)) => {
                let elapsed = (transport.now_unix_ms() - start).max(0) as f64 / 1000.0;
                history.push((n + 1, msg, elapsed));
                if n + 1 >= timing.max_attempts.max(1) as usize {
                    return Err(UploadError::transient(summarize_attempts(label, &history)));
                }
                transport.sleep(timing.retry_delay(n)).await;
                n += 1;
            }
        }
    }
}

/// One capture unit ready for upload: a single JPEG or a per-interval clip.
/// The content type must match the granted format — the presigned URL is
/// signed with it.
#[derive(Clone)]
pub struct UploadPayload {
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
    /// Pause/stop flush: the confirm carries `final: true`, and the server
    /// credits the partial minute since the last credited mark (its
    /// creditFinalCapture) instead of the all-or-nothing streak rule.
    is_final: bool,
    /// Interaction metadata for the interval (pointer and focus samples),
    /// sent as the confirm's `activity`. Program-only on the server.
    activity: Option<serde_json::Value>,
}

impl UploadPayload {
    pub fn jpeg(bytes: bytes::Bytes, base64: String, width: u32, height: u32) -> Self {
        Self {
            bytes,
            content_type: "image/jpeg",
            format: None,
            frame_count: None,
            width,
            height,
            preview_base64: base64,
            is_final: false,
            activity: None,
        }
    }

    /// A finished clip: its bytes, the content type and `format` query
    /// value of its container, how many frames it holds, and a JPEG
    /// preview of its last frame.
    pub fn clip(
        bytes: bytes::Bytes,
        content_type: &'static str,
        format: &'static str,
        frame_count: u32,
        width: u32,
        height: u32,
        preview_base64: String,
    ) -> Self {
        Self {
            bytes,
            content_type,
            format: Some(format),
            frame_count: Some(frame_count),
            width,
            height,
            preview_base64,
            is_final: false,
            activity: None,
        }
    }

    /// Mark this as the pause/stop flush.
    pub fn final_capture(mut self) -> Self {
        self.is_final = true;
        self
    }

    pub fn is_final(&self) -> bool {
        self.is_final
    }

    /// Attach the interval's interaction metadata (see `activity` on the
    /// confirm). `None` sends nothing, exactly as before.
    pub fn with_activity(mut self, activity: Option<serde_json::Value>) -> Self {
        self.activity = activity;
        self
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// The pipeline: get a presigned URL, PUT to R2, POST the confirmation.
/// Each network step is retried with backoff (see [`retry_step`]).
///
/// `captured_at` is the ISO-8601 timestamp (in corrected client clock) of
/// when the capture was taken. Optional — when `None`, the request matches
/// the legacy bucket-mode payload byte-for-byte. When `Some`, it opts the
/// session into credit-mode tracking on the first request.
///
/// Takes the payload by reference: its bytes are refcounted, so retries
/// and a backlog that holds on to it cost no copies.
pub async fn upload_and_confirm(
    host: &dyn UploadHost,
    payload: &UploadPayload,
    captured_at: Option<&str>,
    config: &SessionConfig,
) -> Result<CaptureUploadResult, UploadError> {
    let transport = host.transport();
    let timing = host.timing();
    let step_timeout = timing.step_timeout();
    let size_bytes = payload.bytes.len();

    // Step 1: a presigned URL. clientInfo is always sent.
    host.progress("getting upload url from server...".to_string());
    let mut query: Vec<(&str, &str)> = vec![("clientInfo", host.client_info())];
    if let Some(c) = captured_at {
        query.push(("capturedAt", c));
    }
    if let Some(f) = payload.format {
        query.push(("format", f));
    }
    let upload_url_url = url_with_query(&session_url(config, "/upload-url"), &query)
        .map_err(|e| UploadError {
            message: format!("upload-url: invalid URL: {e}"),
            kind: UploadErrorKind::Rejected(0),
        })?;
    // Each attempt re-requests a FRESH presigned URL (it has a 120s expiry).
    // Bracket the request on the local clock for the offset estimate below.
    // The bracket spans the whole retry block (matching the web SDK) — a
    // retried attempt inflates the window and the midpoint with it, but the
    // estimator smooths samples and only ever needs ±30s accuracy.
    let url_sent_at_ms = transport.now_unix_ms();
    let upload_url_resp: UploadUrlResponse = retry_step(transport, &timing, "upload-url", || {
        let req = Request::new(Method::Get, upload_url_url.clone(), step_timeout);
        async move {
            let res = transport
                .send(req)
                .await
                .map_err(|e| StepError::Retryable(e.describe().to_string()))?;
            if !res.is_success() {
                Err(classify_http(res.status, http_error(res.status, &res.reason, &res.text())))
            } else {
                serde_json::from_slice::<UploadUrlResponse>(&res.body)
                    .map_err(|e| StepError::Retryable(format!("malformed response: {e}")))
            }
        }
    })
    .await?;
    host.progress(format!(
        "got upload url, screenshot id: {}",
        upload_url_resp.screenshot_id
    ));

    if let Some(server_ms) = upload_url_resp
        .server_time
        .as_deref()
        .and_then(parse_iso_to_unix_ms)
    {
        let mut offset = host.clock_offset().lock().unwrap();
        offset.observe(server_ms, url_sent_at_ms, transport.now_unix_ms());
        if upload_url_resp.captured_at_adopted {
            let skew_s = offset.offset_ms() / 1000;
            host.progress(format!("system clock is ~{skew_s}s off — corrected automatically"));
        }
    }

    // The server grants a format; a downgrade on a clip request is a
    // refusal of this payload (the caller retries the interval as a JPEG).
    if let Some(requested) = payload.format {
        let granted = upload_url_resp.format.as_deref().unwrap_or("jpeg");
        if granted != requested {
            return Err(UploadError {
                message: format!("server granted \"{granted}\" for a \"{requested}\" clip upload"),
                kind: UploadErrorKind::Rejected(0),
            });
        }
    }

    // Step 2: PUT to R2.
    host.progress(format!("uploading {}KB to R2...", size_bytes / 1024));
    let r2_host = url::Url::parse(&upload_url_resp.upload_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "r2".to_string());
    let r2_label = format!("r2-upload {}KB → {}", size_bytes / 1024, r2_host);
    retry_step(transport, &timing, &r2_label, || {
        let req = Request::new(Method::Put, upload_url_resp.upload_url.clone(), step_timeout)
            .bytes(payload.bytes.clone(), payload.content_type);
        async move {
            let res = transport
                .send(req)
                .await
                .map_err(|e| StepError::Retryable(e.describe().to_string()))?;
            if !res.is_success() {
                return Err(StepError::Retryable(http_error(res.status, &res.reason, "")));
            }
            Ok(())
        }
    })
    .await?;
    host.progress("uploaded to R2 successfully".to_string());

    // Step 3: confirm.
    host.progress("confirming upload with server...".to_string());
    let mut confirm_body = serde_json::json!({
        "screenshotId": upload_url_resp.screenshot_id,
        "width": payload.width,
        "height": payload.height,
        "fileSize": size_bytes,
    });
    if let Some(fc) = payload.frame_count {
        confirm_body["frameCount"] = fc.into();
    }
    if let Some(activity) = &payload.activity {
        // Servers that predate it drop the field (Fastify removeAdditional).
        confirm_body["activity"] = activity.clone();
    }
    if payload.is_final {
        confirm_body["final"] = true.into();
    }
    let confirm_url = session_url(config, "/screenshots");
    let confirm_sent_at_ms = transport.now_unix_ms();
    let confirm_resp: ConfirmResponse = retry_step(transport, &timing, "confirm", || {
        let req = Request::new(Method::Post, confirm_url.clone(), step_timeout)
            .json(confirm_body.clone());
        async move {
            let res = transport
                .send(req)
                .await
                .map_err(|e| StepError::Retryable(e.describe().to_string()))?;
            if !res.is_success() {
                Err(classify_http(res.status, http_error(res.status, &res.reason, &res.text())))
            } else {
                serde_json::from_slice::<ConfirmResponse>(&res.body)
                    .map_err(|e| StepError::Retryable(format!("malformed response: {e}")))
            }
        }
    })
    .await?;
    if let Some(server_ms) = confirm_resp
        .server_time
        .as_deref()
        .and_then(parse_iso_to_unix_ms)
    {
        host.clock_offset()
            .lock()
            .unwrap()
            .observe(server_ms, confirm_sent_at_ms, transport.now_unix_ms());
    }

    host.progress(format!(
        "confirmed! tracked {}s, next expected at {}",
        confirm_resp.tracked_seconds, confirm_resp.next_expected_at
    ));

    Ok(CaptureUploadResult {
        confirmed: confirm_resp.confirmed,
        tracked_seconds: confirm_resp.tracked_seconds,
        next_expected_at: confirm_resp.next_expected_at,
        screenshot_id: upload_url_resp.screenshot_id,
        preview_base64: payload.preview_base64.clone(),
        preview_width: payload.width,
        preview_height: payload.height,
    })
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::transport::{BoxFuture, Response, TransportError};
    use std::cell::Cell;
    use std::time::Duration;

    /// A transport that never sends and never waits.
    struct Inert;
    impl Transport for Inert {
        fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            Box::pin(async { Err(TransportError::Failed("inert".into())) })
        }
        fn sleep(&self, _: Duration) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    fn timing() -> Timing {
        Timing::default()
    }

    #[test]
    fn summarize_single_attempt() {
        let history = vec![(1, "timed out".to_string(), 30.0)];
        assert_eq!(summarize_attempts("upload-url", &history), "upload-url: timed out (30.0s)");
    }

    #[test]
    fn summarize_same_cause_collapses() {
        let history = vec![
            (1, "timed out".to_string(), 30.0),
            (2, "timed out".to_string(), 30.1),
            (3, "timed out".to_string(), 29.9),
        ];
        assert_eq!(
            summarize_attempts("confirm", &history),
            "confirm failed after 3 attempts: timed out (30.0s, 30.1s, 29.9s)"
        );
    }

    #[test]
    fn summarize_distinct_causes_lists_each() {
        let history = vec![
            (1, "timed out".to_string(), 30.0),
            (2, "connection refused".to_string(), 1.2),
        ];
        assert_eq!(
            summarize_attempts("r2-upload", &history),
            "r2-upload failed after 2 attempts: #1 timed out (30.0s); #2 connection refused (1.2s)"
        );
    }

    #[test]
    fn classify_409_conflict_is_session_state() {
        assert!(matches!(
            classify_http(409, "paused".into()),
            StepError::Terminal(UploadErrorKind::SessionState, _)
        ));
    }

    #[test]
    fn classify_5xx_is_retryable() {
        assert!(matches!(classify_http(500, "boom".into()), StepError::Retryable(_)));
    }

    #[test]
    fn classify_other_4xx_is_rejected() {
        assert!(matches!(
            classify_http(404, "gone".into()),
            StepError::Terminal(UploadErrorKind::Rejected(404), _)
        ));
    }

    #[test]
    fn classify_rate_limit_is_retryable() {
        assert!(matches!(classify_http(429, "slow down".into()), StepError::Retryable(_)));
    }

    #[test]
    fn http_error_names_status_reason_and_body() {
        assert_eq!(http_error(409, "Conflict", "{\"error\":\"x\"}"), "HTTP 409 Conflict: {\"error\":\"x\"}");
        assert_eq!(http_error(500, "", ""), "HTTP 500");
    }

    async fn run<T>(
        label: &str,
        attempt: impl FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, StepError>>>>,
    ) -> Result<T, UploadError> {
        retry_step(&Inert, &timing(), label, attempt).await
    }

    #[tokio::test]
    async fn first_attempt_success_returns_immediately() {
        let attempts = Cell::new(0usize);
        let result = run("test", || {
            attempts.set(attempts.get() + 1);
            Box::pin(async { Ok::<&str, StepError>("ok") })
        })
        .await;
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(attempts.get(), 1);
    }

    #[tokio::test]
    async fn retryable_failure_recovers_within_max_attempts() {
        let attempts = Cell::new(0usize);
        let result = run("test", || {
            let n = attempts.get() + 1;
            attempts.set(n);
            Box::pin(async move {
                if n < 3 {
                    Err(StepError::Retryable("flaky".into()))
                } else {
                    Ok::<u32, StepError>(42)
                }
            })
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.get(), 3);
    }

    #[tokio::test]
    async fn retryable_failure_exhausts_after_three_attempts() {
        let attempts = Cell::new(0usize);
        let result = run("upload-url", || {
            attempts.set(attempts.get() + 1);
            Box::pin(async { Err::<(), StepError>(StepError::Retryable("timed out".into())) })
        })
        .await;
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            result.unwrap_err().message,
            "upload-url failed after 3 attempts: timed out (0.0s, 0.0s, 0.0s)"
        );
    }

    /// The SDK's `retry.maxRetries`, carried in `Timing::max_attempts`.
    #[tokio::test]
    async fn max_attempts_is_honoured_both_ways() {
        for (max, expected) in [(5u32, 5usize), (1, 1), (0, 1)] {
            let attempts = Cell::new(0usize);
            let timing = Timing { max_attempts: max, ..Timing::default() };
            let _ = retry_step(&Inert, &timing, "confirm", || {
                attempts.set(attempts.get() + 1);
                Box::pin(async { Err::<(), StepError>(StepError::Retryable("down".into())) })
            })
            .await;
            assert_eq!(attempts.get(), expected, "max_attempts = {max}");
        }
    }

    #[tokio::test]
    async fn distinct_failures_are_all_listed() {
        let attempts = Cell::new(0usize);
        let result = run("r2-upload", || {
            let n = attempts.get() + 1;
            attempts.set(n);
            Box::pin(async move { Err::<(), StepError>(StepError::Retryable(format!("cause {n}"))) })
        })
        .await;
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            result.unwrap_err().message,
            "r2-upload failed after 3 attempts: #1 cause 1 (0.0s); #2 cause 2 (0.0s); #3 cause 3 (0.0s)"
        );
    }

    #[tokio::test]
    async fn terminal_failure_short_circuits_without_retry() {
        let attempts = Cell::new(0usize);
        let result = run("confirm", || {
            attempts.set(attempts.get() + 1);
            Box::pin(async {
                Err::<(), StepError>(StepError::Terminal(UploadErrorKind::SessionState, "paused".into()))
            })
        })
        .await;
        assert_eq!(attempts.get(), 1);
        let e = result.unwrap_err();
        assert_eq!(e.message, "confirm: paused");
        assert_eq!(e.kind, UploadErrorKind::SessionState);
    }
}

#[cfg(test)]
mod compat_tests {
    use super::{ConfirmResponse, UploadUrlResponse};
    use lookout_session::time::current_unix_ms;
    use lookout_session::time::{parse_iso_to_unix_ms, unix_ms_to_iso};

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
    // `tokio::spawn` via `start_tray_timer` (today: `Core::start_tray_timer`,
    // called from the shell's async command). Sync Tauri commands run on a
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

#[cfg(test)]
mod activity_tests {
    use super::*;
    use crate::transport::{Body, BoxFuture, Response, TransportError};
    use std::time::Duration;

    /// Answers the three pipeline steps and keeps every confirm body.
    #[derive(Default)]
    struct Recording {
        confirms: Mutex<Vec<serde_json::Value>>,
    }

    impl Transport for Recording {
        fn send(&self, req: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
            let body = match req.method {
                Method::Get => serde_json::json!({
                    "uploadUrl": "https://r2.test/put",
                    "r2Key": "k",
                    "screenshotId": "s",
                    "minuteBucket": 0,
                    "nextExpectedAt": "2026-01-01T00:01:00.000Z",
                })
                .to_string(),
                Method::Put => String::new(),
                _ => {
                    if let Body::Json(v) = &req.body {
                        self.confirms.lock().unwrap().push(v.clone());
                    }
                    serde_json::json!({
                        "confirmed": true,
                        "trackedSeconds": 0,
                        "nextExpectedAt": "2026-01-01T00:01:00.000Z",
                    })
                    .to_string()
                }
            };
            Box::pin(async move { Ok(Response { status: 200, reason: String::new(), body: body.into() }) })
        }
        fn sleep(&self, _: Duration) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    struct Host {
        transport: Recording,
        offset: Mutex<lookout_session::clock_offset::ClockOffset>,
    }

    impl UploadHost for Host {
        fn transport(&self) -> &dyn Transport {
            &self.transport
        }
        fn timing(&self) -> Timing {
            Timing::default()
        }
        fn client_info(&self) -> &str {
            "test"
        }
        fn clock_offset(&self) -> &Mutex<lookout_session::clock_offset::ClockOffset> {
            &self.offset
        }
        fn progress(&self, _: String) {}
    }

    fn confirm_for(payload: UploadPayload) -> serde_json::Value {
        let host = Host { transport: Recording::default(), offset: Mutex::new(lookout_session::clock_offset::ClockOffset::new()) };
        let config = SessionConfig { token: "t".into(), api_base_url: "https://api.test".into() };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(upload_and_confirm(&host, &payload, None, &config))
            .map_err(|e| e.message)
            .unwrap();
        let confirms = host.transport.confirms.lock().unwrap();
        confirms.last().cloned().unwrap()
    }

    fn jpeg() -> UploadPayload {
        UploadPayload::jpeg(bytes::Bytes::from_static(b"jpeg"), String::new(), 10, 10)
    }

    #[test]
    fn the_confirm_carries_the_intervals_activity() {
        let activity = serde_json::json!({ "v": 1, "samples": [{ "at": 1, "method": "x11" }] });
        let body = confirm_for(jpeg().with_activity(Some(activity.clone())));
        assert_eq!(body["activity"], activity);
    }

    #[test]
    fn no_activity_leaves_the_confirm_as_it_was() {
        let body = confirm_for(jpeg());
        assert!(body.get("activity").is_none());
        assert_eq!(body.as_object().unwrap().len(), 4, "{body}");
    }
}
