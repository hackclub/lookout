//! The one thing a host has to provide: a way to send an HTTP request and
//! a way to wait. Everything above this is identical on every platform.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A boxed future, so the trait stays object-safe without an async-trait
/// dependency. `Send` everywhere a host runs on a multi-threaded runtime;
/// not on wasm, where there is one thread and JavaScript values can't be.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// `Send` on every platform with threads; nothing on wasm, where there is
/// one thread and JavaScript values can't be. Lets the same trait objects
/// cross `tokio::spawn` on the desktop and `spawn_local` in the browser.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync> MaybeSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSync for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
        }
    }
}

/// A request body. `Json` must be sent as `application/json`; `Bytes`
/// with exactly the content type given (a presigned URL is signed with
/// it); `Empty` sends no body and no content type.
#[derive(Clone, Debug)]
pub enum Body {
    Empty,
    Json(serde_json::Value),
    Bytes {
        bytes: bytes::Bytes,
        content_type: String,
    },
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: Method,
    /// Absolute, already encoded.
    pub url: String,
    pub body: Body,
    /// Deadline for the whole exchange, body included.
    pub timeout: Duration,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>, timeout: Duration) -> Self {
        Self {
            method,
            url: url.into(),
            body: Body::Empty,
            timeout,
        }
    }

    pub fn json(mut self, value: serde_json::Value) -> Self {
        self.body = Body::Json(value);
        self
    }

    pub fn bytes(mut self, bytes: bytes::Bytes, content_type: impl Into<String>) -> Self {
        self.body = Body::Bytes {
            bytes,
            content_type: content_type.into(),
        };
        self
    }
}

/// What came back. Any status, including errors: only a request that never
/// produced a response is a [`TransportError`].
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// The status line's reason phrase (`Conflict`), empty if unknown.
    pub reason: String,
    pub body: bytes::Bytes,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Why no response came. Both carry the host's best description of the
/// cause (DNS, refused, reset, TLS…) — a bare "request failed" is useless
/// in a log.
#[derive(Clone, Debug)]
pub enum TransportError {
    /// The request's deadline passed.
    Timeout(String),
    /// Anything else: connection, TLS, protocol, body read.
    Failed(String),
}

impl TransportError {
    pub fn describe(&self) -> &str {
        match self {
            TransportError::Timeout(s) | TransportError::Failed(s) => s,
        }
    }
}

/// The host's HTTP stack and clock.
pub trait Transport: MaybeSend + MaybeSync {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>>;
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
    /// The wall clock, in milliseconds since the Unix epoch — what request
    /// round trips are bracketed with for the clock-offset estimate. The
    /// default reads the system clock; a host without one (the browser)
    /// overrides it.
    fn now_unix_ms(&self) -> i64 {
        lookout_session::time::current_unix_ms()
    }
}
