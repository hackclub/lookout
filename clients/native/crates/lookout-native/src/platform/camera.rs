//! Native camera capture — what the Tauri app did with the webview's
//! `getUserMedia` and a request/response dance over events. Here the core's
//! `Frontend::camera_*` hooks are answered directly.
//!
//! Linux uses GStreamer (`v4l2src`/`pipewiresrc` via `autovideosrc` with a
//! device), which the engine already links for screencasts. macOS uses
//! AVFoundation (`camera_macos.rs`) and Windows Media Foundation
//! (`camera_windows.rs`); both keep the newest raw frame and JPEG-encode it
//! on `grab`.

use lookout_core::CameraFrame;

#[derive(Clone, Debug, PartialEq)]
pub struct CameraDevice {
    pub id: String,
    pub label: String,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;
    use parking_lot::Mutex;

    pub struct Camera {
        pipeline: Mutex<Option<Pipeline>>,
    }

    // A playing GStreamer pipeline must explicitly enter Null before its
    // last reference is dropped. Own that cleanup even when a preview task
    // is canceled or a spawn_blocking result is never consumed.
    struct Pipeline {
        pipeline: gst::Pipeline,
        sink: gst_app::AppSink,
    }

    impl Drop for Pipeline {
        fn drop(&mut self) {
            let _ = self.pipeline.set_state(gst::State::Null);
        }
    }

    impl Camera {
        pub fn new() -> Self {
            Self {
                pipeline: Mutex::new(None),
            }
        }

        pub async fn start(&self, device_id: &str) -> Result<(), String> {
            self.stop();
            let id = device_id.to_string();
            let built = tokio::task::spawn_blocking(move || build_pipeline(&id))
                .await
                .map_err(|e| e.to_string())??;
            *self.pipeline.lock() = Some(built);
            // Wait until the first frame arrives so the seed capture has one.
            for _ in 0..50 {
                if self.grab_now().is_ok() {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            self.stop();
            Err("the camera didn't produce a frame".into())
        }

        fn grab_now(&self) -> Result<CameraFrame, String> {
            let guard = self.pipeline.lock();
            let Some(pipeline) = guard.as_ref() else {
                return Err("no camera frame — the stream isn't ready or has ended".into());
            };
            let sample = pipeline
                .sink
                .try_pull_sample(gst::ClockTime::from_mseconds(500))
                .ok_or_else(|| "no camera frame — the stream isn't ready or has ended".to_string())?;
            let caps = sample.caps().ok_or("no caps")?;
            let s = caps.structure(0).ok_or("no caps structure")?;
            let width = s.get::<i32>("width").map_err(|e| e.to_string())? as u32;
            let height = s.get::<i32>("height").map_err(|e| e.to_string())? as u32;
            let buffer = sample.buffer().ok_or("no buffer")?;
            let map = buffer.map_readable().map_err(|e| e.to_string())?;
            Ok(CameraFrame {
                jpeg: map.as_slice().to_vec(),
                width,
                height,
            })
        }

        pub async fn grab(&self, _device_id: &str) -> Result<CameraFrame, String> {
            self.grab_now()
        }

        pub fn stop(&self) {
            self.pipeline.lock().take();
        }
    }

    fn build_pipeline(device_id: &str) -> Result<Pipeline, String> {
        gst::init().map_err(|e| e.to_string())?;
        // Fit within the engine's 1920×1080 capture bounds; JPEG like the webview did.
        let src = if device_id.starts_with("/dev/") {
            format!("v4l2src device={device_id}")
        } else {
            "autovideosrc".to_string()
        };
        let desc = format!(
            "{src} ! videoconvert ! videoscale ! video/x-raw,width=[1,1920],height=[1,1080],pixel-aspect-ratio=1/1 ! \
             jpegenc quality=85 ! appsink name=sink max-buffers=1 drop=true sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;
        let sink = pipeline
            .by_name("sink")
            .ok_or("no sink")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "sink is not an appsink".to_string())?;
        let pipeline = Pipeline { pipeline, sink };
        pipeline.pipeline.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        Ok(pipeline)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn dropping_camera_releases_pipeline() {
            gst::init().unwrap();
            // No physical camera required: retain a reference so we can check
            // the state after the camera owner (or an unconsumed result) drops.
            let pipeline = gst::parse::launch("videotestsrc is-live=true ! appsink name=sink max-buffers=1 drop=true sync=false")
                .unwrap()
                .downcast::<gst::Pipeline>()
                .unwrap();
            let sink = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();
            let owned = Pipeline {
                pipeline: pipeline.clone(),
                sink,
            };
            pipeline.set_state(gst::State::Playing).unwrap();
            let camera = Camera {
                pipeline: Mutex::new(Some(owned)),
            };
            drop(camera);
            assert_eq!(pipeline.current_state(), gst::State::Null);
        }
    }

    /// Video sources known to the system (GStreamer device monitor).
    pub fn list_devices() -> Vec<CameraDevice> {
        if gst::init().is_err() {
            return Vec::new();
        }
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Video/Source"), None);
        if monitor.start().is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for d in monitor.devices() {
            let props = d.properties();
            let path = props
                .as_ref()
                .and_then(|p| {
                    p.get::<String>("device.path")
                        .ok()
                        .or_else(|| p.get::<String>("api.v4l2.path").ok())
                })
                .or_else(|| props.as_ref().and_then(|p| p.get::<String>("object.path").ok()))
                .unwrap_or_else(|| d.display_name().to_string());
            let label = d.display_name().to_string();
            if !out.iter().any(|c: &CameraDevice| c.id == path) {
                out.push(CameraDevice { id: path, label });
            }
        }
        monitor.stop();
        out
    }
}

#[cfg(target_os = "macos")]
#[path = "camera_macos.rs"]
mod imp;

#[cfg(target_os = "windows")]
#[path = "camera_windows.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    use super::*;

    pub struct Camera;

    impl Camera {
        pub fn new() -> Self {
            Self
        }
        pub async fn start(&self, _device_id: &str) -> Result<(), String> {
            Err("Camera capture isn't available in this build of Lookout yet.".into())
        }
        pub async fn grab(&self, _device_id: &str) -> Result<CameraFrame, String> {
            Err(NO_FRAME.into())
        }
        pub fn stop(&self) {}
    }

    pub fn list_devices() -> Vec<CameraDevice> {
        Vec::new()
    }
}

#[allow(dead_code)]
const NO_FRAME: &str = "no camera frame — the stream isn't ready or has ended";

/// The newest raw frame a native backend has seen: tightly packed BGRA/BGRX
/// rows, plus when it arrived so a stalled or unplugged camera reads as ended.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub at: std::time::Instant,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) type FrameSlot = std::sync::Arc<parking_lot::Mutex<Option<std::sync::Arc<RawFrame>>>>;

/// Frames older than this mean the stream has stopped delivering.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Shared by the native backends: wait for the first frame after starting.
#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn wait_first_frame(slot: &FrameSlot) -> Result<(), String> {
    for _ in 0..80 {
        if slot.lock().is_some() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err("the camera didn't produce a frame".into())
}

/// Shared by the native backends: JPEG-encode the latest frame (quality 85,
/// downscaled to fit 1920×1080), like the webview capture did.
#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn encode_latest(slot: &FrameSlot) -> Result<CameraFrame, String> {
    let frame = slot.lock().clone().ok_or(NO_FRAME)?;
    if frame.at.elapsed() > STALE_AFTER {
        return Err(NO_FRAME.into());
    }
    tokio::task::spawn_blocking(move || encode_bgra(&frame))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn encode_bgra(frame: &RawFrame) -> Result<CameraFrame, String> {
    use image::{RgbImage, imageops};
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 || frame.bgra.len() < (w as usize) * (h as usize) * 4 {
        return Err("the camera produced an empty frame".into());
    }
    let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
    for px in frame.bgra.chunks_exact(4).take((w as usize) * (h as usize)) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let mut img = RgbImage::from_raw(w, h, rgb).ok_or("bad camera frame size")?;
    let scale = (1920.0 / w as f64).min(1080.0 / h as f64);
    if scale < 1.0 {
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        img = imageops::resize(&img, nw, nh, imageops::FilterType::Triangle);
    }
    let (width, height) = img.dimensions();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&img)
        .map_err(|e| e.to_string())?;
    Ok(CameraFrame { jpeg, width, height })
}

pub use imp::{Camera, list_devices};
