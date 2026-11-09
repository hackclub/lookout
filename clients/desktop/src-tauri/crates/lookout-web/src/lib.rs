//! The browser as a [`Host`] for `lookout-recorder`, exposed to JavaScript
//! through wasm-bindgen.
//!
//! Everything about the recording — the state machine, the capture loop on
//! the credit grid, the ordered uploader with its backlog, the pause flush,
//! the 409 handling — is the shared Rust. What JavaScript provides is
//! exactly what only the browser can do: `fetch`, `getDisplayMedia` and
//! `getUserMedia` frames drawn into a canvas, the WebCodecs clip encoder,
//! and the UI the events go to. The contract is the `host` object passed
//! to [`LookoutRecorder::new`]:
//!
//! ```text
//! request({ method, url, timeoutMs, body?: Uint8Array, contentType?, json? })
//!     -> Promise<{ status, reason, body: Uint8Array }>
//!     rejects with { timeout?: boolean, message }
//! emit(name, payload)
//! wantsPreviewFrames() -> boolean
//! setTrayTitle(text, paused)
//! sourceOpen(source) -> Promise<void>         // camera warm-up
//! sourceClose(source)
//! beginCapture(source, spec) -> capture session:
//!     grab(jpegMode: "none" | "preview" | "full", record: boolean)
//!         -> Promise<{ jpeg?: { bytes: Uint8Array, width, height }, clipError?: string }>
//!     clipCut() -> Promise<{ bytes, contentType, format, frameCount, width, height } | null>
//!     clipDiscard()
//!     flushFrame() -> Promise<{ bytes, width, height } | null>
//!     watchForLostSource() -> boolean
//! onCapturing(capturing)                      // optional
//! log(line)                                   // optional
//! clientInfo: string
//! ```

// Browser-only: the host's futures hold `JsValue`s, which aren't `Send`, so
// this only compiles where `MaybeSend` is relaxed. Empty on native so
// `cargo test --workspace` still builds.
#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use js_sys::{Promise, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use lookout_protocol::transport::{
    Body, BoxFuture, Request, Response, Transport, TransportError,
};
use lookout_recorder::host::{
    CaptureSession, CaptureSpec, ClipOutput, Frame, Grab, GrabJpeg, Host, Runtime,
};
use lookout_recorder::{CoreEvent, Recorder};
use lookout_session::source::RecordingSource;
use lookout_session::timing::Timing;

#[wasm_bindgen]
extern "C" {
    /// The JavaScript host object (duck-typed; see the module docs).
    #[wasm_bindgen(typescript_type = "LookoutHost")]
    pub type JsHost;

    #[wasm_bindgen(method, structural)]
    fn request(this: &JsHost, request: JsValue) -> Promise;
    #[wasm_bindgen(method, structural)]
    fn emit(this: &JsHost, name: &str, payload: JsValue);
    #[wasm_bindgen(method, structural, js_name = wantsPreviewFrames)]
    fn wants_preview_frames(this: &JsHost) -> bool;
    #[wasm_bindgen(method, structural, js_name = setTrayTitle)]
    fn set_tray_title(this: &JsHost, text: &str, paused: bool);
    #[wasm_bindgen(method, structural, js_name = sourceOpen)]
    fn source_open(this: &JsHost, source: JsValue) -> Promise;
    #[wasm_bindgen(method, structural, js_name = sourceClose)]
    fn source_close(this: &JsHost, source: JsValue);
    #[wasm_bindgen(method, structural, js_name = beginCapture)]
    fn begin_capture(this: &JsHost, source: JsValue, spec: JsValue) -> JsCaptureSession;
    #[wasm_bindgen(method, structural, js_name = onCapturing)]
    fn on_capturing(this: &JsHost, capturing: bool);
    #[wasm_bindgen(method, structural, getter, js_name = clientInfo)]
    fn client_info(this: &JsHost) -> String;
    #[wasm_bindgen(method, structural, getter)]
    fn log(this: &JsHost) -> JsValue;

    /// One capture run on the JavaScript side.
    pub type JsCaptureSession;

    #[wasm_bindgen(method, structural)]
    fn grab(this: &JsCaptureSession, jpeg_mode: &str, record: bool) -> Promise;
    #[wasm_bindgen(method, structural, js_name = clipCut)]
    fn clip_cut(this: &JsCaptureSession) -> Promise;
    #[wasm_bindgen(method, structural, js_name = clipDiscard)]
    fn clip_discard(this: &JsCaptureSession);
    #[wasm_bindgen(method, structural, js_name = flushFrame)]
    fn flush_frame(this: &JsCaptureSession) -> Promise;
    #[wasm_bindgen(method, structural, js_name = watchForLostSource)]
    fn watch_for_lost_source(this: &JsCaptureSession) -> bool;
}

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &'static str = r#"
export interface LookoutTransportRequest {
  method: "GET" | "POST" | "PUT" | "PATCH";
  url: string;
  timeoutMs: number;
  /** Raw bytes to send with `contentType`, or absent. */
  body?: Uint8Array;
  contentType?: string;
  /** A JSON document to send as application/json, or absent. */
  json?: string;
}
export interface LookoutTransportResponse {
  status: number;
  reason: string;
  body: Uint8Array;
}
export interface LookoutFrame { bytes: Uint8Array; width: number; height: number }
export interface LookoutClip {
  bytes: Uint8Array;
  contentType: string;
  format: "mp4" | "webm";
  frameCount: number;
  width: number;
  height: number;
}
export interface LookoutCaptureSpec {
  maxWidth: number;
  maxHeight: number;
  jpegQuality: number;
  frameIntervalMs: number;
}
export interface LookoutCaptureSession {
  grab(jpegMode: "none" | "preview" | "full", record: boolean): Promise<{ jpeg?: LookoutFrame; clipError?: string }>;
  clipCut(): Promise<LookoutClip | null>;
  clipDiscard(): void;
  flushFrame(): Promise<LookoutFrame | null>;
  watchForLostSource(): boolean;
}
export type LookoutSource =
  | { type: "screen"; sources: unknown[] }
  | { type: "camera"; deviceId: string };
export interface LookoutHost {
  request(request: LookoutTransportRequest): Promise<LookoutTransportResponse>;
  emit(name: string, payload: unknown): void;
  wantsPreviewFrames(): boolean;
  setTrayTitle(text: string, paused: boolean): void;
  sourceOpen(source: LookoutSource): Promise<void>;
  sourceClose(source: LookoutSource): void;
  beginCapture(source: LookoutSource, spec: LookoutCaptureSpec): LookoutCaptureSession;
  onCapturing?(capturing: boolean): void;
  log?(line: string): void;
  readonly clientInfo: string;
}
"#;

/// The browser's executor and clock.
struct WebRuntime;

impl Runtime for WebRuntime {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        spawn_local(future);
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        let ms = duration.as_millis().min(u32::MAX as u128) as u32;
        Box::pin(gloo_timers::future::TimeoutFuture::new(ms))
    }

    fn now_unix_ms(&self) -> i64 {
        js_sys::Date::now() as i64
    }
}

/// What a rejected `request` promise carried.
fn transport_error(e: JsValue) -> TransportError {
    let message = js_sys::Reflect::get(&e, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_else(|| format!("{e:?}"));
    let timeout = js_sys::Reflect::get(&e, &"timeout".into())
        .ok()
        .and_then(|t| t.as_bool())
        .unwrap_or(false);
    if timeout {
        TransportError::Timeout(message)
    } else {
        TransportError::Failed(message)
    }
}

fn js_string(v: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(v, &key.into()).ok().and_then(|x| x.as_string())
}

fn js_f64(v: &JsValue, key: &str) -> Option<f64> {
    js_sys::Reflect::get(v, &key.into()).ok().and_then(|x| x.as_f64())
}

fn js_bytes(v: &JsValue, key: &str) -> Option<bytes::Bytes> {
    let arr = js_sys::Reflect::get(v, &key.into()).ok()?;
    if arr.is_undefined() || arr.is_null() {
        return None;
    }
    Some(bytes::Bytes::from(Uint8Array::new(&arr).to_vec()))
}

fn js_frame(v: &JsValue) -> Option<Frame> {
    if v.is_undefined() || v.is_null() {
        return None;
    }
    Some(Frame {
        jpeg: js_bytes(v, "bytes")?,
        width: js_f64(v, "width")? as u32,
        height: js_f64(v, "height")? as u32,
    })
}

fn to_js<T: serde::Serialize>(value: &T) -> JsValue {
    serde_wasm_bindgen::to_value(value).unwrap_or(JsValue::UNDEFINED)
}

/// The JavaScript host, wrapped.
struct WebHost {
    js: JsHost,
    runtime: WebRuntime,
    client_info: String,
}

impl Transport for WebHost {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        Box::pin(async move {
            let req = js_sys::Object::new();
            let set = |k: &str, v: JsValue| {
                let _ = js_sys::Reflect::set(&req, &k.into(), &v);
            };
            set("method", request.method.as_str().into());
            set("url", request.url.into());
            set("timeoutMs", (request.timeout.as_millis() as f64).into());
            match request.body {
                Body::Empty => {}
                Body::Json(v) => set("json", v.to_string().into()),
                Body::Bytes { bytes, content_type } => {
                    set("body", Uint8Array::from(bytes.as_ref()).into());
                    set("contentType", content_type.into());
                }
            }
            let res = JsFuture::from(self.js.request(req.into()))
                .await
                .map_err(transport_error)?;
            Ok(Response {
                status: js_f64(&res, "status").unwrap_or(0.0) as u16,
                reason: js_string(&res, "reason").unwrap_or_default(),
                body: js_bytes(&res, "body").unwrap_or_default(),
            })
        })
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        self.runtime.sleep(duration)
    }

    fn now_unix_ms(&self) -> i64 {
        self.runtime.now_unix_ms()
    }
}

impl Host for WebHost {
    fn runtime(&self) -> &dyn Runtime {
        &self.runtime
    }

    fn transport(&self) -> &dyn Transport {
        self
    }

    fn client_info(&self) -> &str {
        &self.client_info
    }

    fn emit(&self, event: CoreEvent) {
        let name = event.name();
        let payload = match &event {
            CoreEvent::Progress(m) => JsValue::from_str(m),
            CoreEvent::TrackedSeconds(s) => JsValue::from_f64(*s as f64),
            CoreEvent::SessionTerminated(p) => to_js(p),
            CoreEvent::TickResult(p) => to_js(p),
            CoreEvent::TickError(p) => to_js(p),
            CoreEvent::SourceLost(p) => to_js(p),
            CoreEvent::PreviewFrame(p) => to_js(p),
            CoreEvent::ScreencastRevoked(p) => to_js(p),
            CoreEvent::RecorderState(s) => to_js(s),
            CoreEvent::RecorderNotice(n) => to_js(n),
        };
        self.js.emit(name, payload);
    }

    fn wants_preview_frames(&self) -> bool {
        self.js.wants_preview_frames()
    }

    fn set_tray_title(&self, time_text: &str, paused: bool) {
        self.js.set_tray_title(time_text, paused);
    }

    fn source_open(&self, source: &RecordingSource) -> BoxFuture<'static, Result<(), String>> {
        let promise = self.js.source_open(to_js(source));
        Box::pin(async move {
            JsFuture::from(promise)
                .await
                .map(|_| ())
                .map_err(|e| js_string(&e, "message").unwrap_or_else(|| format!("{e:?}")))
        })
    }

    fn source_close(&self, source: &RecordingSource) {
        self.js.source_close(to_js(source));
    }

    fn begin_capture(&self, source: &RecordingSource, spec: &CaptureSpec) -> Box<dyn CaptureSession> {
        let spec_js = js_sys::Object::new();
        let set = |k: &str, v: f64| {
            let _ = js_sys::Reflect::set(&spec_js, &k.into(), &v.into());
        };
        set("maxWidth", spec.max_width as f64);
        set("maxHeight", spec.max_height as f64);
        set("jpegQuality", spec.jpeg_quality as f64);
        set("frameIntervalMs", spec.frame_interval_ms as f64);
        Box::new(WebCaptureSession {
            js: self.js.begin_capture(to_js(source), spec_js.into()),
        })
    }

    fn on_capturing(&self, capturing: bool) {
        if js_sys::Reflect::has(&self.js, &"onCapturing".into()).unwrap_or(false) {
            self.js.on_capturing(capturing);
        }
    }
}

struct WebCaptureSession {
    js: JsCaptureSession,
}

fn js_error(e: JsValue) -> String {
    js_string(&e, "message").unwrap_or_else(|| format!("{e:?}"))
}

impl CaptureSession for WebCaptureSession {
    fn grab(&self, jpeg: GrabJpeg, record: bool) -> BoxFuture<'_, Result<Grab, String>> {
        let mode = match jpeg {
            GrabJpeg::None => "none",
            GrabJpeg::Preview => "preview",
            GrabJpeg::Full => "full",
        };
        let promise = self.js.grab(mode, record);
        Box::pin(async move {
            let v = JsFuture::from(promise).await.map_err(js_error)?;
            let jpeg = js_sys::Reflect::get(&v, &"jpeg".into())
                .ok()
                .and_then(|j| js_frame(&j));
            Ok(Grab {
                jpeg,
                clip_error: js_string(&v, "clipError"),
            })
        })
    }

    fn clip_cut(&self) -> BoxFuture<'_, Result<Option<ClipOutput>, String>> {
        let promise = self.js.clip_cut();
        Box::pin(async move {
            let v = JsFuture::from(promise).await.map_err(js_error)?;
            if v.is_undefined() || v.is_null() {
                return Ok(None);
            }
            let format = js_string(&v, "format").unwrap_or_default();
            let (format, content_type): (&'static str, &'static str) = match format.as_str() {
                "mp4" => ("mp4", "video/mp4"),
                "webm" => ("webm", "video/webm"),
                other => return Err(format!("unknown clip format {other:?}")),
            };
            Ok(Some(ClipOutput {
                bytes: js_bytes(&v, "bytes").ok_or("clip without bytes")?,
                content_type,
                format,
                frame_count: js_f64(&v, "frameCount").unwrap_or(0.0) as u32,
                width: js_f64(&v, "width").unwrap_or(0.0) as u32,
                height: js_f64(&v, "height").unwrap_or(0.0) as u32,
            }))
        })
    }

    fn clip_discard(&self) {
        self.js.clip_discard();
    }

    fn flush_frame(&self) -> BoxFuture<'_, Result<Option<Frame>, String>> {
        let promise = self.js.flush_frame();
        Box::pin(async move {
            let v = JsFuture::from(promise).await.map_err(js_error)?;
            Ok(js_frame(&v))
        })
    }

    fn watch_for_lost_source(&self) -> bool {
        self.js.watch_for_lost_source()
    }
}

fn recorder_error(e: lookout_session::state::RecorderError) -> JsValue {
    to_js(&e)
}

fn api_error(e: lookout_protocol::ApiError) -> JsValue {
    to_js(&e)
}

/// The recorder, for JavaScript. One per page; `open` a session into it,
/// drive it with the actions, draw the `recorder-state` events.
#[wasm_bindgen]
pub struct LookoutRecorder {
    recorder: Arc<Recorder>,
}

static LOG_SINK: Mutex<Option<js_sys::Function>> = Mutex::new(None);

#[wasm_bindgen]
impl LookoutRecorder {
    /// `host` is the JavaScript host object (see the module docs).
    #[wasm_bindgen(constructor)]
    pub fn new(host: JsHost) -> Result<LookoutRecorder, JsValue> {
        console_error_panic_hook::set_once();
        let log = host.log();
        if let Ok(f) = log.dyn_into::<js_sys::Function>() {
            *LOG_SINK.lock().unwrap() = Some(f);
            lookout_session::log::set_sink(|line| {
                if let Some(f) = LOG_SINK.lock().unwrap().as_ref() {
                    let _ = f.call1(&JsValue::NULL, &JsValue::from_str(line));
                }
            });
        }
        let client_info = host.client_info();
        let web_host = Arc::new(WebHost {
            js: host,
            runtime: WebRuntime,
            client_info,
        });
        Ok(LookoutRecorder {
            recorder: Recorder::new(web_host),
        })
    }

    /// Open a recording for a session. `source` is a `LookoutSource`.
    /// Resolves with the `RecorderState` (a load failure is a state, not a
    /// rejection).
    pub fn open(&self, token: String, api_base_url: String, source: JsValue) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            let source: RecordingSource =
                serde_wasm_bindgen::from_value(source).map_err(|e| JsValue::from_str(&e.to_string()))?;
            let cfg = lookout_session::config::SessionConfig { token, api_base_url };
            recorder
                .recorder_open(cfg, source)
                .await
                .map(|s| to_js(&s))
                .map_err(recorder_error)
        })
    }

    /// `open`, without capturing: for a page that has no stream until the
    /// user shares one. A pending session waits for `resume`; an active
    /// one is paused so it doesn't accrue time.
    #[wasm_bindgen(js_name = openParked)]
    pub fn open_parked(&self, token: String, api_base_url: String, source: JsValue) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            let source: RecordingSource =
                serde_wasm_bindgen::from_value(source).map_err(|e| JsValue::from_str(&e.to_string()))?;
            let cfg = lookout_session::config::SessionConfig { token, api_base_url };
            recorder
                .recorder_open_parked(cfg, source)
                .await
                .map(|s| to_js(&s))
                .map_err(recorder_error)
        })
    }

    pub fn pause(&self) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            recorder.recorder_pause().await.map(|s| to_js(&s)).map_err(recorder_error)
        })
    }

    pub fn resume(&self) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            recorder.recorder_resume().await.map(|s| to_js(&s)).map_err(recorder_error)
        })
    }

    #[wasm_bindgen(js_name = beginStop)]
    pub fn begin_stop(&self) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            recorder.recorder_begin_stop().await.map(|s| to_js(&s)).map_err(recorder_error)
        })
    }

    #[wasm_bindgen(js_name = cancelStop)]
    pub fn cancel_stop(&self) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            recorder.recorder_cancel_stop().await.map(|s| to_js(&s)).map_err(recorder_error)
        })
    }

    /// Finish the stop: name (optional), stop with retries, finish the
    /// recording. Resolves with the `StopOutcome`.
    #[wasm_bindgen(js_name = finishStop)]
    pub fn finish_stop(&self, name: Option<String>, edit: bool) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            recorder
                .recorder_finish_stop(name, edit)
                .await
                .map(|o| to_js(&o))
                .map_err(recorder_error)
        })
    }

    /// Close the recording: flush, release the source, forget it.
    pub fn close(&self) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            recorder.recorder_close().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Tear down without flushing — the page is going away.
    pub fn abort(&self) {
        self.recorder.recorder_abort();
    }

    /// The current `RecorderState`, or `undefined` with nothing open.
    pub fn state(&self) -> JsValue {
        match self.recorder.recorder_state() {
            Some(s) => to_js(&s),
            None => JsValue::UNDEFINED,
        }
    }

    /// Rename (optional) and stop a session that is not open here, with the
    /// recorder's retries. Resolves with the `FinishOutcome`.
    #[wasm_bindgen(js_name = sessionFinish)]
    pub fn session_finish(
        &self,
        token: String,
        api_base_url: String,
        name: Option<String>,
        edit: bool,
    ) -> Promise {
        let recorder = Arc::clone(&self.recorder);
        wasm_bindgen_futures::future_to_promise(async move {
            let cfg = lookout_session::config::SessionConfig { token, api_base_url };
            recorder
                .session_finish(&cfg, name.as_deref(), edit)
                .await
                .map(|o| to_js(&o))
                .map_err(api_error)
        })
    }

    /// Replace the pipeline's timing (`Timing`, camelCase JSON; missing
    /// fields take their defaults). The SDK sets `maxAttempts` and
    /// `retryDelaysMs` from its `retry` option; simulations scale the rest.
    #[wasm_bindgen(js_name = setTiming)]
    pub fn set_timing(&self, timing: JsValue) -> Result<(), JsValue> {
        let timing: Timing =
            serde_wasm_bindgen::from_value(timing).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.recorder.set_timing(timing);
        Ok(())
    }

    /// The pipeline's timing, as camelCase JSON.
    pub fn timing(&self) -> JsValue {
        to_js(&self.recorder.timing())
    }
}
