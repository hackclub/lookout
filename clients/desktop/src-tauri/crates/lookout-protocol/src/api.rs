//! The Lookout server API.
//!
//! Everything a client says to the server that is not a capture upload:
//! reading and driving a session (status, pause, resume, stop, rename,
//! video, cut editing), the program registry, the announcement banner,
//! and the gallery's batch lookup. Hosts call these instead of speaking
//! HTTP themselves, so every client gets the same URLs, bodies, timeouts
//! and error shaping.
//!
//! Responses are returned as the server's JSON, verbatim (`serde_json::Value`).
//! The shapes are documented in `packages/server/API.md` and typed in
//! `packages/shared/src/types.ts`; passing them through untouched means a
//! field the server adds tomorrow reaches the UI without a core release.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use lookout_session::config::{SessionClipCapabilities, SessionConfig};

use crate::error::{error_detail, xml_tag, ApiError, ApiResult};
use crate::transport::{Method, Request, Transport, TransportError};

/// Per-request deadline. Mirrors `UPLOAD_STEP_TIMEOUT_MS` in @lookout/shared,
/// which the TypeScript client applies to every API call.
pub const API_TIMEOUT: Duration = Duration::from_secs(30);

/// `/api/sessions/:token/status` as the capture loop reads it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusResponse {
    pub status: String,
    #[serde(default)]
    pub tracked_seconds: Option<i64>,
}

pub fn session_url(s: &SessionConfig, path: &str) -> String {
    format!("{}/api/sessions/{}{}", s.api_base_url, s.token, path)
}

/// `base` with `query` form-encoded onto it, in order.
pub fn url_with_query(base: &str, query: &[(&str, &str)]) -> Result<String, String> {
    if query.is_empty() {
        return Ok(base.to_string());
    }
    url::Url::parse_with_params(base, query)
        .map(|u| u.to_string())
        .map_err(|e| e.to_string())
}

/// One JSON round trip with the shared client's error shaping. A JSON body
/// sets `Content-Type: application/json`; no body sets nothing — the same
/// requests the TypeScript client sends, byte for byte.
pub async fn request_json(
    transport: &dyn Transport,
    method: Method,
    url: String,
    body: Option<Value>,
    timeout: Duration,
) -> ApiResult<Value> {
    let mut req = Request::new(method, url.clone(), timeout);
    if let Some(body) = body {
        req = req.json(body);
    }
    let res = match transport.send(req).await {
        Ok(res) => res,
        Err(TransportError::Timeout(_)) => {
            return Err(ApiError {
                status: None,
                message: format!("Timed out after {}s fetching {url}", timeout.as_secs()),
            })
        }
        Err(TransportError::Failed(detail)) => {
            return Err(ApiError {
                status: None,
                message: format!("Network error fetching {url}: {detail}"),
            })
        }
    };
    if !res.is_success() {
        let detail = error_detail(&res.text());
        let mut message = format!("HTTP {} {} from {url}", res.status, res.reason);
        if !detail.is_empty() {
            message.push('\n');
            message.push_str(&detail);
        }
        return Err(ApiError {
            status: Some(res.status),
            message,
        });
    }
    serde_json::from_slice::<Value>(&res.body).map_err(|e| ApiError {
        status: None,
        message: format!("Invalid JSON from {url}: {e}"),
    })
}

async fn get(t: &dyn Transport, url: String) -> ApiResult<Value> {
    request_json(t, Method::Get, url, None, API_TIMEOUT).await
}

async fn post(t: &dyn Transport, url: String, body: Option<Value>) -> ApiResult<Value> {
    request_json(t, Method::Post, url, body, API_TIMEOUT).await
}

// ── Session ──────────────────────────────────────────────────────────

/// `GET /api/sessions/:token` — the session record.
pub async fn session_get(t: &dyn Transport, s: &SessionConfig, timeout: Duration) -> ApiResult<Value> {
    request_json(t, Method::Get, session_url(s, ""), None, timeout).await
}

/// `GET /api/sessions/:token/status`.
pub async fn session_status(t: &dyn Transport, s: &SessionConfig, timeout: Duration) -> ApiResult<Value> {
    request_json(t, Method::Get, session_url(s, "/status"), None, timeout).await
}

/// `GET /api/sessions/:token/status`, typed — the capture loop's
/// sleep/pause recovery check.
pub async fn session_status_typed(
    t: &dyn Transport,
    s: &SessionConfig,
    timeout: Duration,
) -> ApiResult<SessionStatusResponse> {
    let v = session_status(t, s, timeout).await?;
    serde_json::from_value(v).map_err(|e| ApiError {
        status: None,
        message: format!("Invalid status JSON: {e}"),
    })
}

/// `POST /api/sessions/:token/pause`.
pub async fn session_pause(t: &dyn Transport, s: &SessionConfig, timeout: Duration) -> ApiResult<Value> {
    request_json(t, Method::Post, session_url(s, "/pause"), None, timeout).await
}

/// `POST /api/sessions/:token/resume`.
pub async fn session_resume(t: &dyn Transport, s: &SessionConfig, timeout: Duration) -> ApiResult<Value> {
    request_json(t, Method::Post, session_url(s, "/resume"), None, timeout).await
}

/// `POST /api/sessions/:token/stop`. With `edit`, the session is held
/// unpublished after compiling so the user can cut it first; the body is
/// omitted entirely otherwise, so a plain stop stays identical to what old
/// servers expect.
pub async fn session_stop(
    t: &dyn Transport,
    s: &SessionConfig,
    edit: bool,
    timeout: Duration,
) -> ApiResult<Value> {
    let body = if edit {
        Some(serde_json::json!({ "edit": true }))
    } else {
        None
    };
    request_json(t, Method::Post, session_url(s, "/stop"), body, timeout).await
}

/// `PATCH /api/sessions/:token/name`.
pub async fn session_rename(
    t: &dyn Transport,
    s: &SessionConfig,
    name: &str,
    timeout: Duration,
) -> ApiResult<Value> {
    request_json(
        t,
        Method::Patch,
        session_url(s, "/name"),
        Some(serde_json::json!({ "name": name })),
        timeout,
    )
    .await
}

/// `GET /api/sessions/:token/video` — the published video URL.
pub async fn session_video(t: &dyn Transport, s: &SessionConfig) -> ApiResult<Value> {
    get(t, session_url(s, "/video")).await
}

/// `GET /api/sessions/:token/units` — editor metadata: unit map, current
/// cuts, and a presigned URL for the uncut original.
pub async fn session_units(t: &dyn Transport, s: &SessionConfig) -> ApiResult<Value> {
    get(t, session_url(s, "/units")).await
}

/// `PUT /api/sessions/:token/cuts` — replace the cut and mask lists. `cuts`
/// is the JSON array of `CutInterval`s, `masks` of `MaskRegion`s (`None`
/// sends `[]`); `[]` for both clears all edits.
pub async fn session_set_cuts(t: &dyn Transport, s: &SessionConfig, cuts: Value, masks: Option<Value>) -> ApiResult<Value> {
    request_json(
        t,
        Method::Put,
        session_url(s, "/cuts"),
        Some(serde_json::json!({
            "cuts": cuts,
            "masks": masks.unwrap_or_else(|| serde_json::json!([])),
        })),
        API_TIMEOUT,
    )
    .await
}

/// `POST /api/sessions/:token/compile` — publish with the current cuts
/// applied (a cut-compile; also how an edit hold is released).
pub async fn session_apply_cuts(t: &dyn Transport, s: &SessionConfig) -> ApiResult<Value> {
    post(t, session_url(s, "/compile"), None).await
}

/// `POST /api/sessions/:token/editing` — renew the edit lease.
pub async fn session_heartbeat_editing(t: &dyn Transport, s: &SessionConfig) -> ApiResult<Value> {
    post(t, session_url(s, "/editing"), None).await
}

/// `GET /api/sessions/:token/upload-url`. Always carries `client_info`;
/// `captured_at` opts the session into credit-mode tracking, `format`
/// requests a clip grant (`"mp4"`/`"webm"`). The response's `format` is
/// what was GRANTED — upload exactly that.
pub async fn session_upload_url(
    t: &dyn Transport,
    s: &SessionConfig,
    client_info: &str,
    captured_at: Option<&str>,
    format: Option<&str>,
) -> ApiResult<Value> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(c) = captured_at {
        query.push(("capturedAt", c));
    }
    if let Some(f) = format {
        query.push(("format", f));
    }
    query.push(("clientInfo", client_info));
    let url = url_with_query(&session_url(s, "/upload-url"), &query).map_err(|e| ApiError {
        status: None,
        message: format!("Invalid upload-url URL: {e}"),
    })?;
    get(t, url).await
}

/// `POST /api/sessions/:token/screenshots` — confirm an upload. `body` is
/// the `ConfirmScreenshotRequest` JSON.
pub async fn session_confirm_screenshot(
    t: &dyn Transport,
    s: &SessionConfig,
    body: Value,
) -> ApiResult<Value> {
    post(t, session_url(s, "/screenshots"), Some(body)).await
}

/// The clip capability the session GET advertises. Any failure means
/// clips off — legacy one-JPEG-per-minute behaviour — and says why.
pub async fn session_clip_capabilities(
    t: &dyn Transport,
    s: &SessionConfig,
    timeout: Duration,
) -> Result<SessionClipCapabilities, String> {
    let v = request_json(t, Method::Get, session_url(s, ""), None, timeout)
        .await
        .map_err(|e| e.message)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// PUT a payload to a presigned R2 URL. `content_type` must match what
/// the URL was signed with. Failures name the S3 error code and what it
/// usually means, the way the TypeScript client does.
pub async fn upload_to_r2(
    t: &dyn Transport,
    upload_url: &str,
    bytes: bytes::Bytes,
    content_type: &str,
) -> ApiResult<()> {
    if !upload_url.starts_with("https://") && !upload_url.starts_with('/') {
        return Err(ApiError {
            status: None,
            message: "Invalid upload URL: must be HTTPS or a relative path.".into(),
        });
    }
    let size = bytes.len();
    let req = Request::new(Method::Put, upload_url, API_TIMEOUT).bytes(bytes, content_type);
    let res = match t.send(req).await {
        Ok(res) => res,
        Err(TransportError::Timeout(_)) => {
            return Err(ApiError {
                status: None,
                message: format!(
                    "R2 upload timed out after {}s ({size} bytes) — the connection stalled mid-transfer.",
                    API_TIMEOUT.as_secs()
                ),
            })
        }
        Err(TransportError::Failed(detail)) => {
            return Err(ApiError {
                status: None,
                message: format!("Upload failed: {detail}"),
            })
        }
    };
    if res.is_success() {
        return Ok(());
    }
    let text = res.text();
    // R2 answers with S3-style XML. A bare "403" is unactionable and
    // every cause has a different fix, so name the cause.
    let code = xml_tag(&text, "Code").unwrap_or_default();
    let detail = xml_tag(&text, "Message").unwrap_or_default();
    let hint = if code == "SignatureDoesNotMatch" {
        " The request didn't match the presigned URL — something between the client and R2 is altering the request (a proxy, or a rewritten method/headers)."
    } else if code == "RequestTimeTooSkewed" {
        " The signing server's clock is off relative to R2; fix NTP on the API server."
    } else if code == "AccessDenied" || detail.to_ascii_lowercase().contains("expire") {
        " The presigned URL had expired (they last ~2 minutes) or the credentials can't write this key. A slow upload of a large clip can outrun the expiry."
    } else if res.status == 403 && text.is_empty() {
        " Empty 403 body usually means CORS stripped the response: check the R2 bucket's CORS rules allow PUT from this origin."
    } else {
        ""
    };
    let code_part = if code.is_empty() {
        String::new()
    } else {
        format!(" ({code})")
    };
    let detail_part = if !detail.is_empty() {
        format!(" — {detail}")
    } else if !text.is_empty() {
        format!(" — {}", text.chars().take(200).collect::<String>())
    } else {
        String::new()
    };
    Err(ApiError {
        status: Some(res.status),
        message: format!(
            "R2 upload failed: HTTP {}{code_part}{detail_part}{hint}",
            res.status
        ),
    })
}

// ── Not session-scoped ───────────────────────────────────────────────

/// `GET /api/programs` — the program registry the "+" menu offers.
/// `timeout` overrides [`API_TIMEOUT`]; a settings page probes a
/// user-typed server with a short one so a typo fails fast.
pub async fn programs(
    t: &dyn Transport,
    api_base_url: &str,
    timeout: Option<Duration>,
) -> ApiResult<Value> {
    request_json(
        t,
        Method::Get,
        format!("{api_base_url}/api/programs"),
        None,
        timeout.unwrap_or(API_TIMEOUT),
    )
    .await
}

/// `GET /api/announcement` — the admin-authored banner, if any. `client`
/// and `version` say who is asking so the server can target announcements
/// (e.g. "please update" only to versions ≤ X); both are optional and a
/// server treats a missing version as version 0, like the old builds.
pub async fn announcement(
    t: &dyn Transport,
    api_base_url: &str,
    client: Option<&str>,
    version: Option<&str>,
) -> ApiResult<Value> {
    get(t, who_is_asking_url(api_base_url, "/api/announcement", client, version)?).await
}

/// `GET /api/tip` — the active tip sheet, if any. Same `client`/`version`
/// targeting as [`announcement`].
pub async fn tip(
    t: &dyn Transport,
    api_base_url: &str,
    client: Option<&str>,
    version: Option<&str>,
) -> ApiResult<Value> {
    get(t, who_is_asking_url(api_base_url, "/api/tip", client, version)?).await
}

/// `POST /api/sessions/batch` — gallery summaries for up to 100 tokens.
pub async fn sessions_batch(t: &dyn Transport, api_base_url: &str, tokens: &[String]) -> ApiResult<Value> {
    post(
        t,
        format!("{api_base_url}/api/sessions/batch"),
        Some(serde_json::json!({ "tokens": tokens })),
    )
    .await
}

/// `{base}{path}` plus the optional `client`/`version` query the targeted
/// public endpoints (announcement, tip) accept. No query at all when
/// neither is given, so the request stays identical to what old builds
/// sent.
fn who_is_asking_url(
    api_base_url: &str,
    path: &str,
    client: Option<&str>,
    version: Option<&str>,
) -> ApiResult<String> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(c) = client {
        query.push(("client", c));
    }
    if let Some(v) = version {
        query.push(("version", v));
    }
    url_with_query(&format!("{api_base_url}{path}"), &query).map_err(|e| ApiError {
        status: None,
        message: format!("Invalid URL for {path}: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_url_shape() {
        let s = SessionConfig {
            token: "abc".into(),
            api_base_url: "https://x.test".into(),
        };
        assert_eq!(session_url(&s, ""), "https://x.test/api/sessions/abc");
        assert_eq!(session_url(&s, "/pause"), "https://x.test/api/sessions/abc/pause");
    }

    #[test]
    fn who_is_asking_url_omits_an_empty_query() {
        assert_eq!(
            who_is_asking_url("https://x", "/api/tip", None, None).unwrap(),
            "https://x/api/tip"
        );
        assert_eq!(
            who_is_asking_url("https://x", "/api/tip", Some("d"), Some("1.0")).unwrap(),
            "https://x/api/tip?client=d&version=1.0"
        );
    }
}
