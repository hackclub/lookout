//! The desktop's HTTP transport for `lookout-protocol`: reqwest over
//! rustls, one shared client, tokio for the retry sleeps.

use std::time::Duration;

use lookout_protocol::transport::{Body, BoxFuture, Method, Request, Response, Transport, TransportError};

use crate::upload::{describe_reqwest_error, http_client};

/// reqwest-backed [`Transport`]. Stateless: the client is process-wide.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestTransport;

impl Transport for ReqwestTransport {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async move {
            let method = match request.method {
                Method::Get => reqwest::Method::GET,
                Method::Post => reqwest::Method::POST,
                Method::Put => reqwest::Method::PUT,
                Method::Patch => reqwest::Method::PATCH,
            };
            let mut req = http_client().request(method, &request.url).timeout(request.timeout);
            req = match request.body {
                Body::Empty => req,
                Body::Json(v) => req.json(&v),
                Body::Bytes { bytes, content_type } => {
                    req.header("Content-Type", content_type).body(bytes)
                }
            };
            let res = req.send().await.map_err(map_err)?;
            let status = res.status();
            let reason = status.canonical_reason().unwrap_or("").to_string();
            let body = res.bytes().await.map_err(map_err)?;
            Ok(Response {
                status: status.as_u16(),
                reason,
                body,
            })
        })
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

fn map_err(e: reqwest::Error) -> TransportError {
    let described = describe_reqwest_error(&e);
    if e.is_timeout() {
        TransportError::Timeout(described)
    } else {
        TransportError::Failed(described)
    }
}
