//! The desktop's frame pipeline: stitched, redacted screen grabs on the
//! blocking pool, camera frames asked of the shell, preview-sized JPEGs.
//! The loop that drives it is `lookout_recorder::capture_loop`, re-exported
//! here along with everything a shell or a test ever imported from this
//! module.

use crate::{capture, CaptureSource, Core};

pub use lookout_recorder::capture_loop::{CaptureLoopHandle, CaptureLoopSpec};
pub use lookout_recorder::events::{
    CapturePreviewFrame, CaptureSessionTerminated, CaptureSourceLost, CaptureTickError,
    CaptureTickResult,
};
pub use lookout_recorder::uploader::UploadJob;
pub use lookout_session::config::SessionClipCapabilities;
pub use lookout_session::time::{current_unix_ms, parse_iso_to_unix_ms};
pub use lookout_session::timing::CAPTURE_INTERVAL_SECS;

/// How long a camera grab may take before the tick gives up on it. The
/// shell's own budget (canvas encode, IPC) is well inside this; the point is
/// that a webview that never answers can't stall the loop.
pub(crate) const CAMERA_GRAB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Where the capture loop's frames come from.
///
/// Both kinds run the identical loop — cadence, clips, sleep recovery, the
/// pause flush, the upload pipeline — and differ only in how a frame is
/// grabbed: screens are captured here, camera frames are asked of the shell
/// (see [`crate::Frontend::camera_grab_frame`]).
#[derive(Clone, Debug)]
pub enum FrameSource {
    /// Monitors, windows and PipeWire streams, stitched into one frame.
    Screen(Vec<CaptureSource>),
    /// A camera the shell opens; `device_id` is whatever the shell's
    /// enumeration calls it.
    Camera { device_id: String },
}

impl FrameSource {
    /// Whether a PipeWire stream is in play — the only source that can die
    /// silently under a live loop. See `MAX_CONSECUTIVE_PIPEWIRE_FAILURES`.
    pub fn has_pipewire(&self) -> bool {
        match self {
            FrameSource::Screen(sources) => sources
                .iter()
                .any(|s| matches!(s, CaptureSource::PipeWire { .. })),
            FrameSource::Camera { .. } => false,
        }
    }
}

/// What JPEG (if any) a frame grab should produce alongside the raw image.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GrabJpeg {
    /// No JPEG — clip frame while the window is unfocused.
    None,
    /// Preview-sized (≤854x480, q65) — matches the resolution the live
    /// preview always used, and keeps the per-frame IPC payload ~5x
    /// smaller than a full-res frame would be.
    Preview,
    /// Full capture resolution at upload quality — the tick frame, which
    /// doubles as the JPEG upload/fallback payload.
    Full,
}

/// Downscale bounds + quality for preview JPEGs (mirrors the values the
/// dedicated preview protocol always served).
const PREVIEW_MAX_W: u32 = 854;
const PREVIEW_MAX_H: u32 = 480;
const PREVIEW_JPEG_QUALITY: u8 = 65;

/// One frame off the capture pipeline: the raw (redacted, scaled) image
/// plus, when requested, its JPEG encoding.
pub(crate) struct FrameGrab {
    pub(crate) image: image::DynamicImage,
    pub(crate) jpeg: Option<capture::RawCaptureResult>,
}

/// The preview-sized JPEG of a frame (≤854x480, q65). Borrowing resize: the
/// full-res frame stays untouched for the clip encoder.
fn encode_preview_jpeg(image: &image::DynamicImage) -> Result<Option<capture::RawCaptureResult>, String> {
    let (w, h) = (image.width(), image.height());
    if w > PREVIEW_MAX_W || h > PREVIEW_MAX_H {
        let scale = f64::min(PREVIEW_MAX_W as f64 / w as f64, PREVIEW_MAX_H as f64 / h as f64);
        let pw = ((w as f64 * scale).round() as u32).max(2);
        let ph = ((h as f64 * scale).round() as u32).max(2);
        image
            .as_rgba8()
            .and_then(|rgba| {
                capture::fast_resize_buffer(rgba, pw, ph, capture::ResizeQuality::Preview)
            })
            .map(|small| {
                capture::encode_frame_jpeg(
                    &image::DynamicImage::ImageRgba8(small),
                    PREVIEW_JPEG_QUALITY,
                )
            })
            .transpose()
    } else {
        Ok(Some(capture::encode_frame_jpeg(image, PREVIEW_JPEG_QUALITY)?))
    }
}

/// Grab one frame from wherever this loop's frames come from. Shared by
/// the upload tick, the clip frames, and the live preview, so everything
/// goes through the exact same path for a given source.
pub(crate) async fn grab_frame(
    core: &Core,
    source: &FrameSource,
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    jpeg: GrabJpeg,
) -> Result<FrameGrab, String> {
    match source {
        FrameSource::Screen(sources) => {
            grab_screen_frame(core, sources, max_width, max_height, jpeg_quality, jpeg).await
        }
        FrameSource::Camera { device_id } => grab_camera_frame(core, device_id, jpeg).await,
    }
}

/// Read the current blacklist (+ Linux PipeWire fds) and capture one
/// redaction-aware stitched frame on the blocking pool — Filtered Apps
/// redaction included.
async fn grab_screen_frame(
    core: &Core,
    sources: &[CaptureSource],
    max_width: u32,
    max_height: u32,
    jpeg_quality: u8,
    jpeg: GrabJpeg,
) -> Result<FrameGrab, String> {
    let blacklisted = {
        core
            .blacklisted_apps
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    };

    #[allow(unused_mut, unused_assignments)]
    let mut pipewire_fds = std::collections::HashMap::new();
    #[cfg(target_os = "linux")]
    {
        if let Ok(guard) = core.pipewire_fds.lock() {
            pipewire_fds = guard.clone();
        };
    }

    let sources_clone = sources.to_vec();
    tokio::task::spawn_blocking(move || {
        let image = capture::take_stitched_screenshots_image_with_blacklist(
            &sources_clone,
            max_width,
            max_height,
            &pipewire_fds,
            &blacklisted,
            // This frame is what gets encoded and uploaded — the one image
            // a viewer ever sees of this minute.
            capture::ResizeQuality::Recorded,
        )?;
        let encoded = match jpeg {
            GrabJpeg::None => None,
            GrabJpeg::Full => Some(capture::encode_frame_jpeg(&image, jpeg_quality)?),
            GrabJpeg::Preview => encode_preview_jpeg(&image)?,
        };
        Ok(FrameGrab {
            image,
            jpeg: encoded,
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking panicked: {e}"))
    .and_then(|r| r)
}

/// Ask the shell for a camera frame. The shell hands over a JPEG already
/// scaled to the capture bounds; it is uploaded as-is for the tick and
/// decoded (on the blocking pool) when pixels are needed for the clip
/// encoder or the live preview.
async fn grab_camera_frame(core: &Core, device_id: &str, jpeg: GrabJpeg) -> Result<FrameGrab, String> {
    let frame = tokio::time::timeout(
        CAMERA_GRAB_TIMEOUT,
        core.frontend().camera_grab_frame(device_id),
    )
    .await
    .map_err(|_| {
        format!(
            "camera frame timed out after {}s",
            CAMERA_GRAB_TIMEOUT.as_secs()
        )
    })??;
    tokio::task::spawn_blocking(move || {
        let decoded = image::load_from_memory_with_format(&frame.jpeg, image::ImageFormat::Jpeg)
            .map_err(|e| format!("camera frame is not a decodable JPEG: {e}"))?;
        // RGBA8 like every screen capture, so the clip encoder's fast path
        // and the preview resize both apply.
        let image = image::DynamicImage::ImageRgba8(decoded.into_rgba8());
        let encoded = match jpeg {
            GrabJpeg::None => None,
            // The bytes the shell encoded ARE the upload: no second
            // generation of JPEG loss on the way to the server.
            GrabJpeg::Full => Some(capture::RawCaptureResult {
                data: frame.jpeg,
                width: image.width(),
                height: image.height(),
            }),
            GrabJpeg::Preview => encode_preview_jpeg(&image)?,
        };
        Ok(FrameGrab {
            image,
            jpeg: encoded,
        })
    })
    .await
    .map_err(|e| format!("spawn_blocking panicked: {e}"))
    .and_then(|r| r)
}

