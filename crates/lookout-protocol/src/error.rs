//! The error every API call produces, shaped like the shared TypeScript
//! client's (`fetchJson`) so UI code that branches on an HTTP status — the
//! 409 "already paused/stopped" cases — behaves the same whichever client
//! produced it.

use serde::Serialize;
use serde_json::Value;

use lookout_session::state::RecorderError;

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

impl From<ApiError> for RecorderError {
    fn from(e: ApiError) -> Self {
        RecorderError::Api {
            status: e.status,
            message: e.message,
        }
    }
}

/// Pull the human-readable detail out of an error body the way the
/// TypeScript client does: `error` or `message` from a JSON object, else
/// the raw text, capped at 500 chars.
pub fn error_detail(text: &str) -> String {
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

/// First `<tag>…</tag>` text in a small XML body, without an XML parser.
pub fn xml_tag(xml: &str, tag: &str) -> Option<String> {
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
