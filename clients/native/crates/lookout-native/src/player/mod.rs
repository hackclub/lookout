//! Native video playback: one small interface over each OS's own media
//! stack, so decoding is hardware-accelerated, remote files stream instead of
//! downloading first, and frames reach the GPU without going through GPUI's
//! image atlas.
//!
//! - **macOS**: `AVPlayer` + `AVPlayerItemVideoOutput`. Frames are
//!   `CVPixelBuffer`s painted zero-copy with `Window::paint_surface`.
//! - **Windows**: Media Foundation's `IMFMediaEngine` in frame-server mode,
//!   on a D3D11 device so decoding uses the GPU. Frames are BGRA, painted
//!   with `Window::paint_surface_frame` (our GPUI patch).
//! - **Linux**: GStreamer `playbin` into an NV12 `appsink`; the mapped
//!   buffer goes straight to the renderer (colour conversion on the GPU).
//!
//! The UI (`ui::video::VideoPlayer`) owns a [`Backend`], reads [`Status`]
//! every frame it renders, and calls [`Backend::tick`] on each animation
//! frame while [`Status::active`] says something is changing. Backends call
//! the [`Waker`] when something changes on their own (new frame while
//! paused, duration known, error, end of stream).

use std::sync::Arc;

#[cfg(target_os = "linux")]
mod gst;
#[cfg(target_os = "linux")]
pub use gst::Backend;

#[cfg(target_os = "macos")]
mod avf;
#[cfg(target_os = "macos")]
pub use avf::Backend;

#[cfg(target_os = "windows")]
mod mf;
#[cfg(target_os = "windows")]
pub use mf::Backend;

/// Called from any thread when the UI should re-render.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// One displayable frame.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    #[cfg(target_os = "macos")]
    pub buffer: objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>,
    #[cfg(not(target_os = "macos"))]
    pub surface: Arc<gpui_kit::gpui::SurfaceFrame>,
}

impl Frame {
    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height.max(1) as f32
    }
}

/// A snapshot of the player, cheap to take every frame.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Seconds. While a seek is pending this is the seek target, so the
    /// playhead never jumps back.
    pub position: f64,
    /// Seconds; 0 until known.
    pub duration: f64,
    /// Playing (or trying to: see `buffering`).
    pub playing: bool,
    /// Waiting for data before it can play or show the requested frame.
    pub buffering: bool,
    /// Reached the end (and not looping).
    pub ended: bool,
    /// How far the media is available, in seconds from the start.
    pub buffered: f64,
    pub error: Option<String>,
    /// Keep ticking: playing, seeking or loading. When false the UI can go
    /// idle until the waker fires.
    pub active: bool,
    /// The first frame has been shown (the poster is up).
    pub has_frame: bool,
}

/// Playback speeds offered in the UI.
pub const RATES: [f64; 6] = [0.25, 0.5, 1.0, 1.5, 2.0, 4.0];

/// Download a remote video into the cache (used when a platform can't stream
/// it directly). Shared with the editor's filmstrip.
pub fn cached_copy(url: &str) -> Result<std::path::PathBuf, String> {
    crate::platform::decoder::local_path(url)
}

pub fn is_remote(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}
