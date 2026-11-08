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

    /// Run `f` with CPU access to the frame's pixels (None if they can't be
    /// read). On macOS this locks the pixel buffer for the duration.
    pub fn with_pixels<R>(&self, f: impl FnOnce(&FramePixels) -> R) -> Option<R> {
        #[cfg(target_os = "macos")]
        {
            use objc2_core_video::{
                CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
                CVPixelBufferGetHeightOfPlane, CVPixelBufferLockBaseAddress,
                CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
            };
            let b = &*self.buffer;
            let flags = CVPixelBufferLockFlags::ReadOnly;
            if unsafe { CVPixelBufferLockBaseAddress(b, flags) } != 0 {
                return None;
            }
            let plane = |i: usize| {
                let base = CVPixelBufferGetBaseAddressOfPlane(b, i) as *const u8;
                let stride = CVPixelBufferGetBytesPerRowOfPlane(b, i);
                let rows = CVPixelBufferGetHeightOfPlane(b, i);
                // SAFETY: the buffer stays locked until after `f` returns.
                (!base.is_null()).then(|| {
                    (
                        unsafe { std::slice::from_raw_parts(base, stride * rows) },
                        stride,
                    )
                })
            };
            // The AVPlayer output is always 420f (checked in `tick`).
            let out = match (plane(0), plane(1)) {
                (Some((y, y_stride)), Some((uv, uv_stride))) => Some(f(&FramePixels {
                    width: self.width,
                    height: self.height,
                    planes: Planes::Nv12 {
                        y,
                        y_stride,
                        uv,
                        uv_stride,
                        bt709: true,
                        full_range: true,
                    },
                })),
                _ => None,
            };
            unsafe { CVPixelBufferUnlockBaseAddress(b, flags) };
            out
        }
        #[cfg(not(target_os = "macos"))]
        {
            use gpui_kit::gpui::SurfaceFormat;
            let s = &self.surface;
            let bytes = s.bytes();
            let planes = match s.format {
                SurfaceFormat::Nv12 {
                    y_offset,
                    y_stride,
                    uv_offset,
                    uv_stride,
                    color,
                } => Planes::Nv12 {
                    y: bytes.get(y_offset..)?,
                    y_stride,
                    uv: bytes.get(uv_offset..)?,
                    uv_stride,
                    bt709: color.bt709,
                    full_range: color.full_range,
                },
                SurfaceFormat::Bgra { offset, stride } => Planes::Bgra {
                    data: bytes.get(offset..)?,
                    stride,
                },
            };
            Some(f(&FramePixels {
                width: s.width,
                height: s.height,
                planes,
            }))
        }
    }
}

/// A frame's pixels in CPU memory, borrowed for [`Frame::with_pixels`].
pub struct FramePixels<'a> {
    pub width: u32,
    pub height: u32,
    planes: Planes<'a>,
}

enum Planes<'a> {
    Nv12 {
        y: &'a [u8],
        y_stride: usize,
        uv: &'a [u8],
        uv_stride: usize,
        bt709: bool,
        full_range: bool,
    },
    Bgra {
        data: &'a [u8],
        stride: usize,
    },
}

impl FramePixels<'_> {
    /// The sRGB colour (0..1) that a 4:2:0 encode of this frame shows with
    /// luma from pixel `luma` and chroma from the chroma sample `chroma`
    /// (in half-resolution chroma coordinates). Out-of-range reads give
    /// black.
    pub fn yuv420_rgb(&self, luma: (u32, u32), chroma: (u32, u32)) -> [f32; 3] {
        let at = |d: &[u8], i: usize| d.get(i).copied().unwrap_or(0) as f32;
        match &self.planes {
            Planes::Nv12 {
                y,
                y_stride,
                uv,
                uv_stride,
                bt709,
                full_range,
            } => {
                let l = at(y, luma.1 as usize * y_stride + luma.0 as usize);
                let ci = chroma.1 as usize * uv_stride + chroma.0 as usize * 2;
                let (cb, cr) = (at(uv, ci), at(uv, ci + 1));
                let (l, cb, cr) = if *full_range {
                    (l / 255.0, (cb - 128.0) / 255.0, (cr - 128.0) / 255.0)
                } else {
                    (
                        (l - 16.0) / 219.0,
                        (cb - 128.0) / 224.0,
                        (cr - 128.0) / 224.0,
                    )
                };
                yuv_to_rgb(l, cb, cr, *bt709)
            }
            Planes::Bgra { data, stride } => {
                // Already RGB: split it back into luma and chroma so the
                // chroma still comes from its own (coarser) sample.
                let rgb = |x: u32, y: u32| {
                    let i = y as usize * stride + x as usize * 4;
                    [
                        at(data, i + 2) / 255.0,
                        at(data, i + 1) / 255.0,
                        at(data, i) / 255.0,
                    ]
                };
                let (l, _, _) = rgb_to_yuv(rgb(luma.0, luma.1));
                let (_, cb, cr) = rgb_to_yuv(rgb(chroma.0 * 2, chroma.1 * 2));
                yuv_to_rgb(l, cb, cr, true)
            }
        }
    }
}

fn yuv_to_rgb(y: f32, cb: f32, cr: f32, bt709: bool) -> [f32; 3] {
    let (kr, kb) = if bt709 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let kg = 1.0 - kr - kb;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / kg;
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

fn rgb_to_yuv([r, g, b]: [f32; 3]) -> (f32, f32, f32) {
    let (kr, kb) = (0.2126, 0.0722);
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    (
        y,
        (b - y) / (2.0 * (1.0 - kb)),
        (r - y) / (2.0 * (1.0 - kr)),
    )
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
