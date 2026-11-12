//! The desktop as a [`Host`] for `lookout-recorder`: tokio for time and
//! tasks, reqwest for HTTP, native screen capture and hardware clip
//! encoders for frames, and the shell's [`Frontend`] for the UI, the
//! camera and the tray.

use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use lookout_protocol::transport::Transport;
use lookout_recorder::host::{
    ActivitySample, BoxFuture, CaptureSession, CaptureSpec, ClipOutput, Frame, Grab, GrabJpeg, Host, Runtime,
};
use lookout_recorder::CoreEvent;
use lookout_session::source::RecordingSource;

use crate::capture_loop::{self, FrameSource};
use crate::transport::ReqwestTransport;
use crate::{capture, clips, Core, Frontend};

/// tokio as the recorder's runtime.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioRuntime;

impl Runtime for TokioRuntime {
    fn spawn(&self, future: BoxFuture<'static, ()>) {
        tokio::spawn(future);
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn now_unix_ms(&self) -> i64 {
        lookout_session::time::current_unix_ms()
    }
}

/// The desktop host.
pub struct DesktopHost {
    pub(crate) frontend: Arc<dyn Frontend>,
    /// The core, for what a screen grab needs from it (the Filtered Apps
    /// list, the PipeWire fds). Weak: the core owns the host.
    core: Weak<Core>,
    runtime: TokioRuntime,
    transport: ReqwestTransport,
    client_name: String,
    client_version: String,
    client_info: OnceLock<String>,
}

impl DesktopHost {
    pub(crate) fn new(
        frontend: Arc<dyn Frontend>,
        core: Weak<Core>,
        client_name: String,
        client_version: String,
    ) -> Self {
        Self {
            frontend,
            core,
            runtime: TokioRuntime,
            transport: ReqwestTransport,
            client_name,
            client_version,
            client_info: OnceLock::new(),
        }
    }
}

impl Host for DesktopHost {
    fn runtime(&self) -> &dyn Runtime {
        &self.runtime
    }

    fn transport(&self) -> &dyn Transport {
        &self.transport
    }

    /// Free-form client telemetry string sent on every upload-url request, e.g.
    /// "Lookout Desktop/0.2.6 (macOS 14.3)". Computed once, on first use. NOT
    /// the HTTP User-Agent — explicit info for server-side telemetry/debugging.
    /// The product name and version are whatever the shell passed to
    /// [`Core::new`].
    fn client_info(&self) -> &str {
        self.client_info.get_or_init(|| {
            let info = os_info::get();
            let os_type = match info.os_type() {
                os_info::Type::Macos => "macOS".to_string(),
                other => other.to_string(),
            };
            let version = info.version().to_string();
            let os = if version.is_empty() || version == "Unknown" {
                os_type
            } else {
                format!("{os_type} {version}")
            };
            format!("{}/{} ({os})", self.client_name, self.client_version)
        })
    }

    fn emit(&self, event: CoreEvent) {
        self.frontend.emit(event);
    }

    fn wants_preview_frames(&self) -> bool {
        self.frontend.wants_preview_frames()
    }

    fn set_tray_title(&self, time_text: &str, paused: bool) {
        self.frontend.set_tray_title(time_text, paused);
    }

    fn source_open(&self, source: &RecordingSource) -> BoxFuture<'static, Result<(), String>> {
        match source.camera_device() {
            Some(device_id) => self.frontend.camera_start(device_id),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn source_close(&self, source: &RecordingSource) {
        if source.is_camera() {
            self.frontend.camera_stop();
        }
    }

    fn begin_capture(&self, source: &RecordingSource, spec: &CaptureSpec) -> Box<dyn CaptureSession> {
        Box::new(NativeCaptureSession {
            core: self.core.clone(),
            source: match source {
                RecordingSource::Screen { sources } => FrameSource::Screen(sources.clone()),
                RecordingSource::Camera { device_id } => FrameSource::Camera {
                    device_id: device_id.clone(),
                },
            },
            spec: *spec,
            last_frame: Mutex::new(None),
            recorder: Mutex::new(None),
        })
    }

    fn on_capturing(&self, capturing: bool) {
        #[cfg(target_os = "macos")]
        {
            if capturing {
                crate::power::begin_recording_assertion();
            } else {
                crate::power::end_recording_assertion();
            }
        }
        let _ = capturing;
    }
}

/// One capture run on the desktop: native grabs, the last frame kept for
/// the flush, and the in-progress clip in a hardware encoder.
struct NativeCaptureSession {
    core: Weak<Core>,
    source: FrameSource,
    spec: CaptureSpec,
    /// Newest successfully captured frame, retained so the pause/stop flush
    /// never has to grab a fresh one — at most one cadence interval stale,
    /// and already redacted/scaled by the capture path.
    last_frame: Mutex<Option<image::DynamicImage>>,
    recorder: Mutex<Option<clips::ClipRecorder>>,
}

impl NativeCaptureSession {
    /// Append `image` to the clip, starting one if none is in progress.
    /// On failure the clip is dropped and the error returned — the loop
    /// counts it toward the clips latch.
    fn record(&self, image: &image::DynamicImage) -> Result<(), String> {
        let mut guard = self.recorder.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            match clips::ClipRecorder::new(image.width(), image.height(), self.spec.frame_interval_ms) {
                Ok(r) => *guard = Some(r),
                Err(e) => return Err(format!("clip encoder init failed: {e}")),
            }
        }
        if let Some(r) = guard.as_mut() {
            if let Err(e) = r.push_frame(image) {
                if let Some(r) = guard.take() {
                    r.discard();
                }
                return Err(format!("clip frame append failed: {e}"));
            }
        }
        Ok(())
    }
}

impl CaptureSession for NativeCaptureSession {
    fn grab(&self, jpeg: GrabJpeg, record: bool) -> BoxFuture<'_, Result<Grab, String>> {
        Box::pin(async move {
            let Some(core) = self.core.upgrade() else {
                return Err("the core is gone".into());
            };
            let mode = match jpeg {
                GrabJpeg::None => capture_loop::GrabJpeg::None,
                GrabJpeg::Preview => capture_loop::GrabJpeg::Preview,
                GrabJpeg::Full => capture_loop::GrabJpeg::Full,
            };
            let grab = capture_loop::grab_frame(
                &core,
                &self.source,
                self.spec.max_width,
                self.spec.max_height,
                self.spec.jpeg_quality,
                mode,
            )
            .await?;
            let clip_error = if record {
                self.record(&grab.image).err()
            } else {
                None
            };
            *self.last_frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(grab.image);
            Ok(Grab {
                jpeg: grab.jpeg.map(|j| Frame {
                    jpeg: bytes::Bytes::from(j.data),
                    width: j.width,
                    height: j.height,
                }),
                clip_error,
            })
        })
    }

    fn clip_cut(&self) -> BoxFuture<'_, Result<Option<ClipOutput>, String>> {
        Box::pin(async move {
            let recorder = self.recorder.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(recorder) = recorder else {
                return Ok(None);
            };
            // `finish()` blocks on the OS encoder draining — keep it off
            // the async workers.
            let clip = tokio::task::spawn_blocking(move || recorder.finish())
                .await
                .map_err(|e| format!("spawn_blocking panicked: {e}"))??;
            Ok(Some(ClipOutput {
                bytes: bytes::Bytes::from(clip.bytes),
                content_type: clip.format.content_type(),
                format: clip.format.upload_format(),
                frame_count: clip.frame_count,
                width: clip.width,
                height: clip.height,
            }))
        })
    }

    fn clip_discard(&self) {
        if let Some(r) = self.recorder.lock().unwrap_or_else(|e| e.into_inner()).take() {
            r.discard();
        }
    }

    fn flush_frame(&self) -> BoxFuture<'_, Result<Option<Frame>, String>> {
        Box::pin(async move {
            let frame = self.last_frame.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let Some(frame) = frame else {
                return Ok(None);
            };
            let quality = self.spec.jpeg_quality;
            let jpeg = tokio::task::spawn_blocking(move || capture::encode_frame_jpeg(&frame, quality))
                .await
                .map_err(|e| format!("spawn_blocking panicked: {e}"))??;
            Ok(Some(Frame {
                jpeg: bytes::Bytes::from(jpeg.data),
                width: jpeg.width,
                height: jpeg.height,
            }))
        })
    }

    fn watch_for_lost_source(&self) -> bool {
        self.source.has_pipewire()
    }

    fn activity(&self) -> BoxFuture<'_, Option<ActivitySample>> {
        Box::pin(async move {
            let FrameSource::Screen(sources) = &self.source else {
                return None;
            };
            let sources = sources.clone();
            #[cfg(target_os = "linux")]
            let streams = self.core.upgrade().map(|c| c.screencast_streams()).unwrap_or_default();
            #[cfg(not(target_os = "linux"))]
            let streams: Vec<crate::activity::PortalStream> = Vec::new();
            // Window-server (or compositor) round trips; bounded so a wedged
            // one can't hold up the frame cadence.
            let task = tokio::task::spawn_blocking(move || crate::activity::sample(&sources, &streams));
            match tokio::time::timeout(Duration::from_secs(2), task).await {
                Ok(Ok(sample)) => sample,
                _ => None,
            }
        })
    }
}

impl Drop for NativeCaptureSession {
    fn drop(&mut self) {
        self.clip_discard();
    }
}
