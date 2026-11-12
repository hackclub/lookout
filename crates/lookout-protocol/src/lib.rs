//! What a Lookout client says to the server, independent of how it says
//! it: the session API (status, pause, resume, stop, rename, cuts, the
//! program registry), the capture upload pipeline (presigned URL → PUT →
//! confirm) with its retry policy, error classification and clock-offset
//! bookkeeping, and the wire types with their compatibility guarantees.
//!
//! Everything goes through a [`Transport`] the host supplies — reqwest on
//! the desktop, `fetch` in a browser, whatever a device has — so every
//! client sends the same URLs, bodies, timeouts and error shapes. The
//! shapes mirror the shared TypeScript client (`clients/react/src/api`).

#![forbid(unsafe_code)]

pub mod api;
pub mod error;
pub mod transport;
pub mod upload;

pub use api::{API_TIMEOUT, SessionStatusResponse};
pub use error::{ApiError, ApiResult};
pub use transport::{Body, BoxFuture, MaybeSend, MaybeSync, Method, Request, Response, Transport, TransportError};
pub use upload::{
    CaptureUploadResult, ConfirmResponse, UploadError, UploadErrorKind, UploadHost, UploadPayload,
    UploadUrlResponse, ENABLE_CREDIT_MODE,
};
