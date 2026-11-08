//! The Lookout server API, as the core sees it: thin wrappers over
//! `lookout-protocol`'s calls with this core's transport, so shells call
//! `core.session_pause(..)` and never speak HTTP themselves. The calls a
//! recording waits on (load, status, pause, resume, stop, rename) use the
//! core's `Timing::api_timeout`; the rest use [`API_TIMEOUT`].
//!
//! Responses are the server's JSON, verbatim (`serde_json::Value`) — see
//! `packages/server/API.md`. Error shaping mirrors the shared TypeScript
//! client (`clients/react/src/api/client.ts`).

use serde_json::Value;

pub use lookout_protocol::api::API_TIMEOUT;
pub use lookout_protocol::{ApiError, ApiResult};

use lookout_protocol::api;

use crate::{Core, SessionConfig};

impl Core {
    /// `GET /api/sessions/:token` — the session record.
    pub async fn session_get(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_get(self.transport(), s, self.timing().api_timeout()).await
    }

    /// `GET /api/sessions/:token/status`.
    pub async fn session_status(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_status(self.transport(), s, self.timing().api_timeout()).await
    }

    /// `POST /api/sessions/:token/pause`.
    pub async fn session_pause(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_pause(self.transport(), s, self.timing().api_timeout()).await
    }

    /// `POST /api/sessions/:token/resume`.
    pub async fn session_resume(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_resume(self.transport(), s, self.timing().api_timeout()).await
    }

    /// `POST /api/sessions/:token/stop`; with `edit`, hold for cutting.
    pub async fn session_stop(&self, s: &SessionConfig, edit: bool) -> ApiResult<Value> {
        api::session_stop(self.transport(), s, edit, self.timing().api_timeout()).await
    }

    /// `PATCH /api/sessions/:token/name`.
    pub async fn session_rename(&self, s: &SessionConfig, name: &str) -> ApiResult<Value> {
        api::session_rename(self.transport(), s, name, self.timing().api_timeout()).await
    }

    /// `GET /api/sessions/:token/video` — the published video URL.
    pub async fn session_video(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_video(self.transport(), s).await
    }

    /// `GET /api/sessions/:token/units` — editor metadata.
    pub async fn session_units(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_units(self.transport(), s).await
    }

    /// `PUT /api/sessions/:token/cuts` — replace the cut and mask lists. `cuts` is the
    /// JSON array of `CutInterval`s; `masks` is the JSON array of `MaskRegion`s.
    pub async fn session_set_cuts(&self, s: &SessionConfig, cuts: Value, masks: Option<Value>) -> ApiResult<Value> {
        api::session_set_cuts(self.transport(), s, cuts, masks).await
    }

    /// `POST /api/sessions/:token/compile` — publish with the cuts applied.
    pub async fn session_apply_cuts(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_apply_cuts(self.transport(), s).await
    }

    /// `POST /api/sessions/:token/editing` — renew the edit lease.
    pub async fn session_heartbeat_editing(&self, s: &SessionConfig) -> ApiResult<Value> {
        api::session_heartbeat_editing(self.transport(), s).await
    }

    /// `GET /api/sessions/:token/upload-url`, carrying this core's
    /// [`Core::client_info`].
    pub async fn session_upload_url(
        &self,
        s: &SessionConfig,
        captured_at: Option<&str>,
        format: Option<&str>,
    ) -> ApiResult<Value> {
        api::session_upload_url(self.transport(), s, self.client_info(), captured_at, format).await
    }

    /// `POST /api/sessions/:token/screenshots` — confirm an upload.
    pub async fn session_confirm_screenshot(&self, s: &SessionConfig, body: Value) -> ApiResult<Value> {
        api::session_confirm_screenshot(self.transport(), s, body).await
    }

    /// PUT a payload to a presigned R2 URL.
    pub async fn upload_to_r2(&self, upload_url: &str, bytes: Vec<u8>, content_type: &str) -> ApiResult<()> {
        api::upload_to_r2(self.transport(), upload_url, bytes::Bytes::from(bytes), content_type).await
    }

    /// `GET /api/programs` — the program registry. `timeout` overrides
    /// [`API_TIMEOUT`] so a settings page can probe a typed server fast.
    pub async fn programs(&self, api_base_url: &str, timeout: Option<std::time::Duration>) -> ApiResult<Value> {
        api::programs(self.transport(), api_base_url, timeout).await
    }

    /// `GET /api/announcement`.
    pub async fn announcement(
        &self,
        api_base_url: &str,
        client: Option<&str>,
        version: Option<&str>,
    ) -> ApiResult<Value> {
        api::announcement(self.transport(), api_base_url, client, version).await
    }

    /// `GET /api/tip`.
    pub async fn tip(&self, api_base_url: &str, client: Option<&str>, version: Option<&str>) -> ApiResult<Value> {
        api::tip(self.transport(), api_base_url, client, version).await
    }

    /// `POST /api/sessions/batch` — gallery summaries for up to 100 tokens.
    pub async fn sessions_batch(&self, api_base_url: &str, tokens: &[String]) -> ApiResult<Value> {
        api::sessions_batch(self.transport(), api_base_url, tokens).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;





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

}
