//! Media Foundation decoder: an `IMFSourceReader` with video processing on,
//! asked for RGB32 (BGRX in memory), read synchronously. Scaling for the
//! filmstrip happens on the CPU — the reader's simple video processor only
//! converts colour, it doesn't resize.

use std::cell::Cell;
use std::os::windows::ffi::OsStrExt;
use std::sync::OnceLock;

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAttributes, IMFMediaType, IMFSample, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFMediaType_Video, MFStartup, MFVideoArea, MFVideoFormat_RGB32, MFSTARTUP_FULL,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE,
    MF_PD_DURATION, MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,  MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VT_I8, VT_UI8};

use super::{DecodedFrame, FrameDecoder};

const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
/// Seconds per Media Foundation time unit (100 ns).
const HNS: f64 = 1e-7;
/// Slack when comparing a frame's pts to a seek target, so a target taken
/// from a frame's own (rounded) pts still lands on that frame.
const SEEK_EPSILON: f64 = 0.0005;

fn mf_err(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// MFStartup once per process; never balanced with MFShutdown (MF's
/// startup is refcounted, so other users' Startup/Shutdown pairs are fine).
fn mf_startup() -> Result<(), String> {
    static START: OnceLock<Result<(), String>> = OnceLock::new();
    // SAFETY: plain FFI call.
    START.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(mf_err("MFStartup"))).clone()
}

/// Join the MTA on the calling thread (once per thread). If the thread is
/// already an STA, CoInitializeEx fails with RPC_E_CHANGED_MODE, which is
/// fine: the synchronous source reader works from either.
fn com_init_thread() {
    thread_local!(static INIT: Cell<bool> = const { Cell::new(false) });
    INIT.with(|i| {
        if !i.get() {
            // SAFETY: plain FFI call; deliberately never uninitialized (the
            // thread keeps COM for its lifetime).
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            i.set(true);
        }
    });
}

/// Layout of the decoder's RGB32 output.
#[derive(Clone, Copy)]
struct Format {
    /// Coded size (what's in the buffer).
    width: u32,
    height: u32,
    /// Bytes from one row to the next; negative for bottom-up images.
    stride: i32,
    /// Visible region inside the coded frame (e.g. 1920x1080 of 1920x1088).
    crop_x: u32,
    crop_y: u32,
    crop_w: u32,
    crop_h: u32,
}

struct MfDecoder {
    reader: IMFSourceReader,
    format: Format,
    duration: f64,
    max_height: Option<u32>,
    /// Drop frames before this pts (set by `seek`).
    seek_target: Option<f64>,
    eos: bool,
}

// SAFETY: the reader is created in the MTA and the decoder is used by one
// thread at a time (owned by the player/filmstrip thread); MTA objects may be
// called from any MTA thread, and each method joins the MTA first.
unsafe impl Send for MfDecoder {}

pub fn open(path: &std::path::Path, max_height: Option<u32>) -> Result<Box<dyn FrameDecoder>, String> {
    com_init_thread();
    mf_startup()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: COM calls on interfaces we own; `wide` outlives the call.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(mf_err("MFCreateAttributes"))?;
        let attrs = attrs.ok_or("MFCreateAttributes returned null")?;
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(mf_err("enable video processing"))?;
        let reader = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attrs)
            .map_err(mf_err("MFCreateSourceReaderFromURL"))?;

        let ty: IMFMediaType = MFCreateMediaType().map_err(mf_err("MFCreateMediaType"))?;
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(mf_err("SetGUID major"))?;
        ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(mf_err("SetGUID subtype"))?;
        reader
            .SetCurrentMediaType(VIDEO_STREAM, None, &ty)
            .map_err(mf_err("SetCurrentMediaType RGB32"))?;
        // Only decode video.
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true);

        let format = read_format(&reader)?;
        let duration = match reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION) {
            Ok(pv) => {
                let inner = &pv.Anonymous.Anonymous;
                if inner.vt == VT_UI8 {
                    inner.Anonymous.uhVal as f64 * HNS
                } else {
                    0.0
                }
            }
            Err(_) => 0.0,
        };
        Ok(Box::new(MfDecoder { reader, format, duration, max_height, seek_target: None, eos: false }))
    }
}

/// Frame size, stride and visible aperture of the reader's current output type.
unsafe fn read_format(reader: &IMFSourceReader) -> Result<Format, String> {
    unsafe {
        let ty = reader.GetCurrentMediaType(VIDEO_STREAM).map_err(mf_err("GetCurrentMediaType"))?;
        let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).map_err(mf_err("MF_MT_FRAME_SIZE"))?;
        let (width, height) = ((size >> 32) as u32, size as u32);
        if width == 0 || height == 0 {
            return Err("video has no frame size".into());
        }
        // Stored as UINT32 but meaningfully signed (negative = bottom-up).
        let stride = ty.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(width as i32 * 4);
        let (mut crop_x, mut crop_y, mut crop_w, mut crop_h) = (0, 0, width, height);
        let mut area = MFVideoArea::default();
        let buf = std::slice::from_raw_parts_mut(
            &mut area as *mut MFVideoArea as *mut u8,
            std::mem::size_of::<MFVideoArea>(),
        );
        if ty.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, buf, None).is_ok() {
            let (x, y) = (area.OffsetX.value.max(0) as u32, area.OffsetY.value.max(0) as u32);
            let (w, h) = (area.Area.cx.max(0) as u32, area.Area.cy.max(0) as u32);
            if w > 0 && h > 0 && x + w <= width && y + h <= height {
                (crop_x, crop_y, crop_w, crop_h) = (x, y, w, h);
            }
        }
        Ok(Format { width, height, stride, crop_x, crop_y, crop_w, crop_h })
    }
}

impl MfDecoder {
    /// Copy the visible part of a sample into tightly packed BGRA, alpha 255.
    unsafe fn copy_sample(&self, sample: &IMFSample) -> Result<(u32, u32, Vec<u8>), String> {
        let f = self.format;
        let mut out = vec![0u8; f.crop_w as usize * f.crop_h as usize * 4];
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().map_err(mf_err("ConvertToContiguousBuffer"))?;
            // Prefer IMF2DBuffer: it reports the real pitch and hands back a
            // pointer to the top row even for bottom-up images.
            if let Ok(b2d) = buffer.cast::<IMF2DBuffer>() {
                let mut scan0: *mut u8 = std::ptr::null_mut();
                let mut pitch: i32 = 0;
                b2d.Lock2D(&mut scan0, &mut pitch).map_err(mf_err("Lock2D"))?;
                copy_rows(scan0, pitch, &f, &mut out);
                let _ = b2d.Unlock2D();
            } else {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut len: u32 = 0;
                buffer.Lock(&mut ptr, None, Some(&mut len)).map_err(mf_err("Lock"))?;
                let abs = f.stride.unsigned_abs() as usize;
                if abs < f.width as usize * 4 || (len as usize) < abs * f.height as usize {
                    let _ = buffer.Unlock();
                    return Err(format!("sample buffer too small ({len} bytes for {}x{})", f.width, f.height));
                }
                // Bottom-up: the top row is the last one in memory.
                let top = if f.stride < 0 { ptr.add(abs * (f.height as usize - 1)) } else { ptr };
                copy_rows(top, f.stride, &f, &mut out);
                let _ = buffer.Unlock();
            }
        }
        Ok((f.crop_w, f.crop_h, out))
    }

    fn scale(&self, w: u32, h: u32, bgra: Vec<u8>) -> (u32, u32, Vec<u8>) {
        let Some(max_h) = self.max_height.filter(|&m| m > 0 && m < h) else {
            return (w, h, bgra);
        };
        let new_w = ((w as u64 * max_h as u64 + h as u64 / 2) / h as u64).max(1) as u32;
        // Channel order doesn't matter to the filter, so BGRA rides in an RgbaImage.
        let Some(img) = image::RgbaImage::from_raw(w, h, bgra) else {
            return (w, h, Vec::new());
        };
        let small = image::imageops::resize(&img, new_w, max_h, image::imageops::FilterType::Triangle);
        (new_w, max_h, small.into_raw())
    }
}

/// Copy `f`'s crop rectangle from a top-row pointer with signed `pitch`.
unsafe fn copy_rows(top: *const u8, pitch: i32, f: &Format, out: &mut [u8]) {
    let row_bytes = f.crop_w as usize * 4;
    for y in 0..f.crop_h as usize {
        let src_row = (f.crop_y as isize + y as isize) * pitch as isize + f.crop_x as isize * 4;
        // SAFETY: the crop lies inside the coded frame, and the locked buffer
        // holds `height` rows of `|pitch|` bytes starting at the top row.
        let src = unsafe { std::slice::from_raw_parts(top.offset(src_row), row_bytes) };
        let dst = &mut out[y * row_bytes..(y + 1) * row_bytes];
        dst.copy_from_slice(src);
        // RGB32 is BGRX: the X byte is undefined, make it opaque.
        for px in dst.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
}

impl FrameDecoder for MfDecoder {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn seek(&mut self, t: f64) -> Result<(), String> {
        com_init_thread();
        let t = t.max(0.0);
        let mut pv = PROPVARIANT::default();
        // SAFETY: filling a zeroed PROPVARIANT as VT_I8 (no owned data, so
        // no PropVariantClear needed); the reader seeks to the keyframe at or
        // before the position, and next_frame drops frames until `t`.
        unsafe {
            let inner = &mut *pv.Anonymous.Anonymous;
            inner.vt = VT_I8;
            inner.Anonymous.hVal = (t / HNS).round() as i64;
            // GUID_NULL = 100-ns units.
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &pv)
                .map_err(mf_err("SetCurrentPosition"))?;
        }
        self.seek_target = Some(t);
        self.eos = false;
        Ok(())
    }

    fn next_frame(&mut self) -> Result<Option<DecodedFrame>, String> {
        com_init_thread();
        loop {
            if self.eos {
                return Ok(None);
            }
            let mut flags: u32 = 0;
            let mut ts: i64 = 0;
            let mut sample: Option<IMFSample> = None;
            // SAFETY: synchronous ReadSample with valid out-pointers.
            unsafe {
                self.reader
                    .ReadSample(VIDEO_STREAM, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))
                    .map_err(mf_err("ReadSample"))?;
            }
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err("Media Foundation stream error".into());
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                // SAFETY: COM call on our reader.
                self.format = unsafe { read_format(&self.reader)? };
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.eos = true;
                // A final sample can accompany the EOS flag; fall through to it.
            }
            // A stream tick (MF_SOURCE_READERF_STREAMTICK: a gap) carries no sample.
            let Some(sample) = sample else { continue };
            let pts = ts as f64 * HNS;
            if let Some(target) = self.seek_target {
                if pts + SEEK_EPSILON < target {
                    continue; // decoded from the preceding keyframe; not there yet
                }
                self.seek_target = None;
            }
            // SAFETY: COM calls on a sample we own.
            let (w, h, bgra) = unsafe { self.copy_sample(&sample)? };
            let (width, height, bgra) = self.scale(w, h, bgra);
            if bgra.is_empty() {
                return Err("failed to scale frame".into());
            }
            return Ok(Some(DecodedFrame { pts, width, height, bgra }));
        }
    }
}
