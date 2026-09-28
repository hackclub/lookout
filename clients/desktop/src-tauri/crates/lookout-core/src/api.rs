//! The Lookout server API, as the core sees it.
//!
//! Everything a native client says to the server that is not a capture
//! upload goes through here: reading and driving a session (status, pause,
//! resume, stop, rename, video, cut editing), the program registry, the
//! announcement banner, and the gallery's batch lookup. Shells call these
//! instead of speaking HTTP themselves, so every native frontend gets the
//! same URLs, bodies, timeouts and error shaping.
//!
//! Responses are returned as the server's JSON, verbatim (`serde_json::Value`).
//! The shapes are documented in `packages/server/API.md` and typed in
//! `packages/shared/src/types.ts`; passing them through untouched means a
//! field the server adds tomorrow reaches the UI without a core release.
//!
//! Error shaping mirrors the shared TypeScript client
//! (`clients/react/src/api/client.ts`, `fetchJson`) so UI code that
//! branches on an HTTP status — the 409 "already paused/stopped" cases —
//! behaves the same whichever client produced the error.

use serde::Serialize;
use serde_json::Value;

use crate::upload::{describe_reqwest_error, http_client};
use crate::{Core, SessionConfig};

/// Per-request deadline. Mirrors `UPLOAD_STEP_TIMEOUT_MS` in @lookout/shared,
/// which the TypeScript client applies to every API call.
pub const API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Why an API call failed.
///
/// `status` is set when the server answered with a non-2xx code, and is
/// `None` for anything that never produced a response (DNS, connection,
/// timeout, unparseable body). Serializes as `{ status, message }` so a
/// shell can rebuild its own error type on the other side of an IPC bridge.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = Result<T, ApiError>;

fn session_url(s: &SessionConfig, path: &str) -> String {
    format!("{}/api/sessions/{}{}", s.api_base_url, s.token, path)
}

/// Pull the human-readable detail out of an error body the way the
/// TypeScript client does: `error` or `message` from a JSON object, else
/// the raw text, capped at 500 chars.
fn error_detail(text: &str) -> String {
    let detail = match serde_json::from_str::<Value>(text) {
        Ok(json) => json
            .get("error")
            .or_else(|| json.get("message"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| text.to_string()),
        Err(_) => text.to_string(),
    };
    detail.chars().take(500).collect()
}

impl Core {
    /// One JSON round trip with the shared client's error shaping. A JSON
    /// body sets `Content-Type: application/json`; no body sets nothing —
    /// the same requests the TypeScript client sends, byte for byte.
    async fn api_json(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<Value>,
        timeout: std::time::Duration,
    ) -> ApiResult<Value> {
        let mut req = http_client().request(method, &url).timeout(timeout);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = match req.send().await {
            Ok(res) => res,
            Err(e) if e.is_timeout() => {
                return Err(ApiError {
                    status: None,
                    message: format!(
                        "Timed out after {}s fetching {url}",
                        timeout.as_secs()
                    ),
                })
            }
            Err(e) => {
                return Err(ApiError {
                    status: None,
                    message: format!(
                        "Network error fetching {url}: {}",
                        describe_reqwest_error(&e)
                    ),
                })
            }
        };
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            let detail = error_detail(&text);
            let reason = status.canonical_reason().unwrap_or("");
            let mut message = format!("HTTP {} {reason} from {url}", status.as_u16());
            if !detail.is_empty() {
                message.push('\n');
                message.push_str(&detail);
            }
            return Err(ApiError {
                status: Some(status.as_u16()),
                message,
            });
        }
        res.json::<Value>().await.map_err(|e| ApiError {
            status: None,
            message: format!("Invalid JSON from {url}: {}", describe_reqwest_error(&e)),
        })
    }

    async fn api_get(&self, url: String) -> ApiResult<Value> {
        self.api_json(reqwest::Method::GET, url, None, API_TIMEOUT).await
    }

    async fn api_post(&self, url: String, body: Option<Value>) -> ApiResult<Value> {
        self.api_json(reqwest::Method::POST, url, body, API_TIMEOUT).await
    }

    // ── Session ──────────────────────────────────────────────────────

    /// `GET /api/sessions/:token` — the session record.
    pub async fn session_get(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_get(session_url(s, "")).await
    }

    /// `GET /api/sessions/:token/status`.
    pub async fn session_status(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_get(session_url(s, "/status")).await
    }

    /// `POST /api/sessions/:token/pause`.
    pub async fn session_pause(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_post(session_url(s, "/pause"), None).await
    }

    /// `POST /api/sessions/:token/resume`.
    pub async fn session_resume(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_post(session_url(s, "/resume"), None).await
    }

    /// `POST /api/sessions/:token/stop`. With `edit`, the session is held
    /// unpublished after compiling so the user can cut it first; the body
    /// is omitted entirely otherwise, so a plain stop stays identical to
    /// what old servers expect.
    pub async fn session_stop(&self, s: &SessionConfig, edit: bool) -> ApiResult<Value> {
        let body = if edit {
            Some(serde_json::json!({ "edit": true }))
        } else {
            None
        };
        self.api_post(session_url(s, "/stop"), body).await
    }

    /// `PATCH /api/sessions/:token/name`.
    pub async fn session_rename(&self, s: &SessionConfig, name: &str) -> ApiResult<Value> {
        self.api_json(
            reqwest::Method::PATCH,
            session_url(s, "/name"),
            Some(serde_json::json!({ "name": name })),
            API_TIMEOUT,
        )
        .await
    }

    /// `GET /api/sessions/:token/video` — the published video URL.
    pub async fn session_video(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_get(session_url(s, "/video")).await
    }

    /// `GET /api/sessions/:token/units` — editor metadata: unit map, current
    /// cuts, and a presigned URL for the uncut original.
    pub async fn session_units(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_get(session_url(s, "/units")).await
    }

    /// `PUT /api/sessions/:token/cuts` — replace the cut and mask lists. `cuts` is the
    /// JSON array of `CutInterval`s; `masks` is the JSON array of `MaskRegion`s.
    pub async fn session_set_cuts(
        &self,
        s: &SessionConfig,
        cuts: Value,
        masks: Option<Value>,
    ) -> ApiResult<Value> {
        self.api_json(
            reqwest::Method::PUT,
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
    pub async fn session_apply_cuts(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_post(session_url(s, "/compile"), None).await
    }

    /// `POST /api/sessions/:token/editing` — renew the edit lease.
    pub async fn session_heartbeat_editing(&self, s: &SessionConfig) -> ApiResult<Value> {
        self.api_post(session_url(s, "/editing"), None).await
    }

    /// `GET /api/sessions/:token/upload-url`. Always carries this core's
    /// [`Core::client_info`]; `captured_at` opts the session into credit-mode
    /// tracking, `format` requests a clip grant (`"mp4"`/`"webm"`). The
    /// response's `format` is what was GRANTED — upload exactly that.
    pub async fn session_upload_url(
        &self,
        s: &SessionConfig,
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
        query.push(("clientInfo", self.client_info()));
        let url = reqwest::Url::parse_with_params(&session_url(s, "/upload-url"), &query)
            .map_err(|e| ApiError {
                status: None,
                message: format!("Invalid upload-url URL: {e}"),
            })?;
        self.api_get(url.to_string()).await
    }

    /// `POST /api/sessions/:token/screenshots` — confirm an upload. `body`
    /// is the `ConfirmScreenshotRequest` JSON.
    pub async fn session_confirm_screenshot(
        &self,
        s: &SessionConfig,
        body: Value,
    ) -> ApiResult<Value> {
        self.api_post(session_url(s, "/screenshots"), Some(body)).await
    }

    /// PUT a payload to a presigned R2 URL. `content_type` must match what
    /// the URL was signed with. Failures name the S3 error code and what it
    /// usually means, the way the TypeScript client does.
    pub async fn upload_to_r2(
        &self,
        upload_url: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> ApiResult<()> {
        if !upload_url.starts_with("https://") && !upload_url.starts_with('/') {
            return Err(ApiError {
                status: None,
                message: "Invalid upload URL: must be HTTPS or a relative path.".into(),
            });
        }
        let size = bytes.len();
        let res = match http_client()
            .put(upload_url)
            .header("Content-Type", content_type)
            .body(bytes)
            .timeout(API_TIMEOUT)
            .send()
            .await
        {
            Ok(res) => res,
            Err(e) if e.is_timeout() => {
                return Err(ApiError {
                    status: None,
                    message: format!(
                        "R2 upload timed out after {}s ({size} bytes) — the connection stalled mid-transfer.",
                        API_TIMEOUT.as_secs()
                    ),
                })
            }
            Err(e) => {
                return Err(ApiError {
                    status: None,
                    message: format!("Upload failed: {}", describe_reqwest_error(&e)),
                })
            }
        };
        let status = res.status();
        if status.is_success() {
            return Ok(());
        }
        let text = res.text().await.unwrap_or_default();
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
        } else if status.as_u16() == 403 && text.is_empty() {
            " Empty 403 body usually means CORS stripped the response: check the R2 bucket's CORS rules allow PUT from this origin."
        } else {
            ""
        };
        eprintln!(
            "[lookout] R2 upload failed: status={} code={code} detail={detail} body={}",
            status.as_u16(),
            text.chars().take(1000).collect::<String>()
        );
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
            status: Some(status.as_u16()),
            message: format!(
                "R2 upload failed: HTTP {}{code_part}{detail_part}{hint}",
                status.as_u16()
            ),
        })
    }

    // ── Not session-scoped ───────────────────────────────────────────

    /// `GET /api/programs` — the program registry the "+" menu offers.
    /// `timeout` overrides [`API_TIMEOUT`]; the Settings page probes a
    /// user-typed server with a short one so a typo fails fast.
    pub async fn programs(
        &self,
        api_base_url: &str,
        timeout: Option<std::time::Duration>,
    ) -> ApiResult<Value> {
        self.api_json(
            reqwest::Method::GET,
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
        &self,
        api_base_url: &str,
        client: Option<&str>,
        version: Option<&str>,
    ) -> ApiResult<Value> {
        self.api_get(who_is_asking_url(api_base_url, "/api/announcement", client, version)?)
            .await
    }

    /// `GET /api/tip` — the active tip sheet, if any. Same `client`/`version`
    /// targeting as [`Core::announcement`].
    pub async fn tip(
        &self,
        api_base_url: &str,
        client: Option<&str>,
        version: Option<&str>,
    ) -> ApiResult<Value> {
        self.api_get(who_is_asking_url(api_base_url, "/api/tip", client, version)?)
            .await
    }

    /// `POST /api/sessions/batch` — gallery summaries for up to 100 tokens.
    pub async fn sessions_batch(&self, api_base_url: &str, tokens: &[String]) -> ApiResult<Value> {
        self.api_post(
            format!("{api_base_url}/api/sessions/batch"),
            Some(serde_json::json!({ "tokens": tokens })),
        )
        .await
    }
}

/// `{base}{path}` plus the optional `client`/`version` query the targeted
/// public endpoints (announcement, tip) accept. No query at all when neither
/// is given, so the request stays identical to what old builds sent.
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
    let base = format!("{api_base_url}{path}");
    if query.is_empty() {
        return Ok(base);
    }
    reqwest::Url::parse_with_params(&base, &query)
        .map(|u| u.to_string())
        .map_err(|e| ApiError {
            status: None,
            message: format!("Invalid URL for {path}: {e}"),
        })
}

/// First `<tag>…</tag>` text in a small XML body, without an XML parser.
fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_detail_prefers_error_then_message_then_text() {
        assert_eq!(error_detail(r#"{"error":"paused","message":"x"}"#), "paused");
        assert_eq!(error_detail(r#"{"message":"nope"}"#), "nope");
        assert_eq!(error_detail("plain text"), "plain text");
        assert_eq!(error_detail(r#"{"other":1}"#), r#"{"other":1}"#);
    }

    #[test]
    fn error_detail_caps_at_500_chars() {
        let long = "x".repeat(800);
        assert_eq!(error_detail(&long).len(), 500);
    }

    #[test]
    fn xml_tag_extracts_s3_fields() {
        let body = "<Error><Code>AccessDenied</Code><Message>Request has expired</Message></Error>";
        assert_eq!(xml_tag(body, "Code").as_deref(), Some("AccessDenied"));
        assert_eq!(xml_tag(body, "Message").as_deref(), Some("Request has expired"));
        assert_eq!(xml_tag(body, "Nope"), None);
    }

    #[test]
    fn session_url_shape() {
        let s = SessionConfig {
            token: "abc".into(),
            api_base_url: "https://lookout.example".into(),
        };
        assert_eq!(session_url(&s, ""), "https://lookout.example/api/sessions/abc");
        assert_eq!(
            session_url(&s, "/status"),
            "https://lookout.example/api/sessions/abc/status"
        );
    }

    // ── Against a local mock server ─────────────────────────────────
    // Minimal HTTP/1.1 responder: enough to prove the request shapes
    // (method, path, query, body) and the error mapping, with no network.

    struct Noop;
    impl crate::Frontend for Noop {
        fn emit(&self, _: crate::CoreEvent) {}
        fn wants_preview_frames(&self) -> bool {
            false
        }
        fn set_tray_title(&self, _: &str, _: bool) {}
    }

    fn test_core() -> std::sync::Arc<Core> {
        Core::new(std::sync::Arc::new(Noop), "Test", "0.0.0")
    }

    struct Req {
        method: String,
        target: String,
        content_type: Option<String>,
        body: String,
    }

    async fn mock_server(
        respond: impl Fn(&Req) -> (u16, &'static str, String) + Send + Sync + 'static,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let (head_end, mut n) = loop {
                    let k = sock.read(&mut tmp).await.unwrap();
                    if k == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..k]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break (i + 4, buf.len());
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let target = parts.next().unwrap_or_default().to_string();
                let mut content_length = 0usize;
                let mut content_type = None;
                for l in lines {
                    let lower = l.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                    if let Some(v) = l
                        .get(..13)
                        .filter(|p| p.eq_ignore_ascii_case("content-type:"))
                        .map(|_| l[13..].trim())
                    {
                        content_type = Some(v.to_string());
                    }
                }
                while n - head_end < content_length {
                    let k = sock.read(&mut tmp).await.unwrap();
                    if k == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..k]);
                    n = buf.len();
                }
                let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                let req = Req {
                    method,
                    target,
                    content_type,
                    body,
                };
                let (status, reason, resp_body) = respond(&req);
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    fn cfg(base: &str) -> SessionConfig {
        SessionConfig {
            token: "tok".into(),
            api_base_url: base.to_string(),
        }
    }

    #[tokio::test]
    async fn get_returns_server_json_verbatim() {
        let base = mock_server(|r| {
            assert_eq!(r.method, "GET");
            assert_eq!(r.target, "/api/sessions/tok/status");
            assert!(r.content_type.is_none(), "GET must not send Content-Type");
            (200, "OK", r#"{"status":"active","extra":{"nested":[1,2]}}"#.into())
        })
        .await;
        let v = test_core().session_status(&cfg(&base)).await.unwrap();
        assert_eq!(v["status"], "active");
        assert_eq!(v["extra"]["nested"][1], 2);
    }

    #[tokio::test]
    async fn non_2xx_maps_to_status_and_shared_client_message_shape() {
        let base = mock_server(|_| (409, "Conflict", r#"{"error":"Session is already paused"}"#.into())).await;
        let err = test_core().session_pause(&cfg(&base)).await.unwrap_err();
        assert_eq!(err.status, Some(409));
        assert_eq!(
            err.message,
            format!("HTTP 409 Conflict from {base}/api/sessions/tok/pause\nSession is already paused")
        );
    }

    #[tokio::test]
    async fn stop_sends_body_only_when_editing() {
        let base = mock_server(|r| {
            assert_eq!(r.method, "POST");
            assert_eq!(r.target, "/api/sessions/tok/stop");
            let echo = serde_json::json!({
                "hadBody": !r.body.is_empty(),
                "body": r.body,
                "contentType": r.content_type,
            });
            (200, "OK", echo.to_string())
        })
        .await;
        let core = test_core();
        let plain = core.session_stop(&cfg(&base), false).await.unwrap();
        assert_eq!(plain["hadBody"], false);
        assert_eq!(plain["contentType"], Value::Null);
        let edit = core.session_stop(&cfg(&base), true).await.unwrap();
        assert_eq!(edit["body"], r#"{"edit":true}"#);
        assert_eq!(edit["contentType"], "application/json");
    }

    #[tokio::test]
    async fn rename_and_cuts_use_patch_and_put() {
        let base = mock_server(|r| {
            let echo = serde_json::json!({ "method": r.method, "target": r.target, "body": r.body });
            (200, "OK", echo.to_string())
        })
        .await;
        let core = test_core();
        let r = core.session_rename(&cfg(&base), "My cut").await.unwrap();
        assert_eq!(r["method"], "PATCH");
        assert_eq!(r["target"], "/api/sessions/tok/name");
        assert_eq!(r["body"], r#"{"name":"My cut"}"#);
        let c = core
            .session_set_cuts(
                &cfg(&base),
                serde_json::json!([{ "start": 1, "end": 2 }]),
                None,
            )
            .await
            .unwrap();
        assert_eq!(c["method"], "PUT");
        assert_eq!(c["target"], "/api/sessions/tok/cuts");
        assert_eq!(c["body"], r#"{"cuts":[{"end":2,"start":1}],"masks":[]}"#);

        let c_masks = core
            .session_set_cuts(
                &cfg(&base),
                serde_json::json!([{ "start": 1, "end": 2 }]),
                Some(serde_json::json!([{ "x": 0.1, "y": 0.2 }])),
            )
            .await
            .unwrap();
        assert_eq!(c_masks["method"], "PUT");
        assert_eq!(c_masks["target"], "/api/sessions/tok/cuts");
        assert_eq!(
            c_masks["body"],
            r#"{"cuts":[{"end":2,"start":1}],"masks":[{"x":0.1,"y":0.2}]}"#
        );
    }

    #[tokio::test]
    async fn upload_url_carries_query_in_client_order() {
        let base = mock_server(|r| (200, "OK", serde_json::json!({ "target": r.target }).to_string())).await;
        let v = test_core()
            .session_upload_url(&cfg(&base), Some("2025-06-01T12:00:00.000Z"), Some("mp4"))
            .await
            .unwrap();
        let target = v["target"].as_str().unwrap();
        assert!(target.starts_with("/api/sessions/tok/upload-url?"), "{target}");
        assert!(target.contains("capturedAt=2025-06-01T12%3A00%3A00.000Z"), "{target}");
        assert!(target.contains("&format=mp4&clientInfo=Test%2F0.0.0"), "{target}");
    }

    #[tokio::test]
    async fn batch_and_programs_paths() {
        let base = mock_server(|r| {
            let echo = serde_json::json!({ "method": r.method, "target": r.target, "body": r.body });
            (200, "OK", echo.to_string())
        })
        .await;
        let core = test_core();
        let b = core.sessions_batch(&base, &["a".into(), "b".into()]).await.unwrap();
        assert_eq!(b["method"], "POST");
        assert_eq!(b["target"], "/api/sessions/batch");
        assert_eq!(b["body"], r#"{"tokens":["a","b"]}"#);
        let p = core.programs(&base, None).await.unwrap();
        assert_eq!(p["target"], "/api/programs");
        let a = core.announcement(&base, None, None).await.unwrap();
        assert_eq!(a["target"], "/api/announcement");
        let a = core
            .announcement(&base, Some("lookout-desktop"), Some("0.4.0"))
            .await
            .unwrap();
        assert_eq!(a["target"], "/api/announcement?client=lookout-desktop&version=0.4.0");
        let t = core.tip(&base, Some("lookout-desktop"), None).await.unwrap();
        assert_eq!(t["target"], "/api/tip?client=lookout-desktop");
    }

    #[tokio::test]
    async fn connection_failure_has_no_status() {
        // Bind then drop: the port is free again, so connecting is refused.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        let err = test_core()
            .session_status(&cfg(&format!("http://{addr}")))
            .await
            .unwrap_err();
        assert_eq!(err.status, None);
        assert!(
            err.message.starts_with(&format!("Network error fetching http://{addr}/api/sessions/tok/status: ")),
            "{}",
            err.message
        );
    }

    #[test]
    fn api_error_serializes_as_status_and_message() {
        let e = ApiError {
            status: Some(409),
            message: "HTTP 409 Conflict from x".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["status"], 409);
        assert_eq!(v["message"], "HTTP 409 Conflict from x");
        let e = ApiError {
            status: None,
            message: "net".into(),
        };
        assert_eq!(serde_json::to_value(&e).unwrap()["status"], Value::Null);
    }
}
