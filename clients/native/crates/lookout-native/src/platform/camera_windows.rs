//! Windows camera capture via Media Foundation: the device's media source
//! feeds a source reader (video processing on, RGB32 out) that a background
//! thread drains, keeping the newest frame; `grab` JPEG-encodes it.
//! COM objects never leave that thread.

use super::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{Interface, PCWSTR, PWSTR};

/// COM + Media Foundation for the current thread, torn down on drop.
struct MfThread {
    com: bool,
    mf: bool,
}

impl MfThread {
    fn init() -> Result<Self, String> {
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
            .map_err(|e| format!("Media Foundation didn't start: {e}"))?;
        Ok(Self { com, mf: true })
    }
}

impl Drop for MfThread {
    fn drop(&mut self) {
        unsafe {
            if self.mf {
                let _ = MFShutdown();
            }
            if self.com {
                CoUninitialize();
            }
        }
    }
}

fn attributes(n: u32) -> windows::core::Result<IMFAttributes> {
    let mut attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut attrs, n)? };
    attrs.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn string_attr(a: &IMFActivate, key: &windows::core::GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        a.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// All video capture devices' activation objects (caller must have MF started).
fn enum_video_devices() -> windows::core::Result<Vec<IMFActivate>> {
    let attrs = attributes(1)?;
    unsafe {
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut pp: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut pp, &mut count)?;
        let mut out = Vec::new();
        if !pp.is_null() {
            for a in std::slice::from_raw_parts_mut(pp, count as usize) {
                if let Some(a) = a.take() {
                    out.push(a);
                }
            }
            CoTaskMemFree(Some(pp as *const _));
        }
        Ok(out)
    }
}

/// Media source for a symbolic link, or the first camera when the id is empty
/// or no longer resolves.
fn open_source(device_id: &str) -> Result<IMFMediaSource, String> {
    if !device_id.is_empty() {
        let by_link = (|| -> windows::core::Result<IMFMediaSource> {
            let attrs = attributes(2)?;
            let link = wide(device_id);
            unsafe {
                attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
                attrs.SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, PCWSTR(link.as_ptr()))?;
                MFCreateDeviceSource(&attrs)
            }
        })();
        if let Ok(src) = by_link {
            return Ok(src);
        }
    }
    let devices = enum_video_devices().map_err(|e| e.to_string())?;
    let first = devices.first().ok_or("no camera was found")?;
    unsafe { first.ActivateObject::<IMFMediaSource>() }.map_err(|e| format!("couldn't open the camera: {e}"))
}

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// Pick the largest native format that fits 1920×1080, then ask for RGB32.
fn configure_reader(reader: &IMFSourceReader) -> Result<(), String> {
    unsafe {
        let mut best: Option<(u64, IMFMediaType)> = None;
        let mut i = 0;
        while let Ok(t) = reader.GetNativeMediaType(STREAM, i) {
            i += 1;
            if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                let (w, h) = ((size >> 32) as u64, (size & 0xffff_ffff) as u64);
                if w <= 1920 && h <= 1080 && best.as_ref().is_none_or(|(a, _)| w * h > *a) {
                    best = Some((w * h, t));
                }
            }
        }
        if let Some((_, t)) = best {
            let _ = reader.SetCurrentMediaType(STREAM, None, &t);
        }
        let out = MFCreateMediaType().map_err(|e| e.to_string())?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(|e| e.to_string())?;
        reader
            .SetCurrentMediaType(STREAM, None, &out)
            .map_err(|e| format!("the camera can't deliver RGB frames: {e}"))?;
        let _ = reader.SetStreamSelection(STREAM, true);
    }
    Ok(())
}

/// Width, height and signed stride of the reader's current output.
fn current_layout(reader: &IMFSourceReader) -> Option<(u32, u32, i32)> {
    unsafe {
        let t = reader.GetCurrentMediaType(STREAM).ok()?;
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let (w, h) = ((size >> 32) as u32, size as u32);
        let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(w as i32 * 4);
        Some((w, h, stride))
    }
}

/// Copy a sample's RGB32 pixels into top-down, tightly packed rows.
fn copy_sample(sample: &IMFSample, (w, h, default_stride): (u32, u32, i32)) -> Option<RawFrame> {
    let row = w as usize * 4;
    let rows = h as usize;
    if row == 0 || rows == 0 {
        return None;
    }
    let mut bgra = Vec::with_capacity(row * rows);
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer().ok()?;
        if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
            let mut scan0: *mut u8 = std::ptr::null_mut();
            let mut pitch = 0i32;
            b2.Lock2D(&mut scan0, &mut pitch).ok()?;
            if !scan0.is_null() && pitch.unsigned_abs() as usize >= row {
                for y in 0..rows {
                    let p = scan0.offset(y as isize * pitch as isize);
                    bgra.extend_from_slice(std::slice::from_raw_parts(p, row));
                }
            }
            let _ = b2.Unlock2D();
        } else {
            let mut base: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut base, None, Some(&mut len)).ok()?;
            let abs = default_stride.unsigned_abs() as usize;
            if !base.is_null() && abs >= row && (len as usize) >= abs * (rows - 1) + row {
                for y in 0..rows {
                    // Negative stride = bottom-up rows.
                    let src_row = if default_stride < 0 { rows - 1 - y } else { y };
                    bgra.extend_from_slice(std::slice::from_raw_parts(base.add(src_row * abs), row));
                }
            }
            let _ = buffer.Unlock();
        }
    }
    (bgra.len() == row * rows).then(|| RawFrame { width: w, height: h, bgra, at: Instant::now() })
}

fn capture_thread(device_id: String, slot: FrameSlot, stop: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<Result<(), String>>) {
    let _mf = match MfThread::init() {
        Ok(m) => m,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let opened = (|| -> Result<(IMFMediaSource, IMFSourceReader), String> {
        let source = open_source(&device_id)?;
        let reader = (|| unsafe {
            let attrs = attributes(1)?;
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            MFCreateSourceReaderFromMediaSource(&source, &attrs)
        })();
        let reader = match reader {
            Ok(r) => r,
            Err(e) => {
                let _ = unsafe { source.Shutdown() };
                return Err(format!("couldn't read from the camera: {e}"));
            }
        };
        if let Err(e) = configure_reader(&reader) {
            let _ = unsafe { source.Shutdown() };
            return Err(e);
        }
        Ok((source, reader))
    })();
    let (source, reader) = match opened {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));

    let mut layout = current_layout(&reader);
    while !stop.load(Ordering::Relaxed) {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        let read = unsafe { reader.ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample)) };
        if read.is_err() {
            break;
        }
        let flags = flags as i32;
        if flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) != 0 {
            break;
        }
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 != 0 {
            layout = current_layout(&reader);
        }
        if let (Some(sample), Some(l)) = (sample, layout) {
            if let Some(frame) = copy_sample(&sample, l) {
                *slot.lock() = Some(Arc::new(frame));
            }
        }
    }
    *slot.lock() = None;
    drop(reader);
    // Releases the device so the camera light turns off.
    let _ = unsafe { source.Shutdown() };
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
    slot: FrameSlot,
}

pub struct Camera {
    running: Mutex<Option<Running>>,
    /// A stopped capture thread that may still be releasing the device.
    draining: Mutex<Option<JoinHandle<()>>>,
}

impl Camera {
    pub fn new() -> Self {
        Self { running: Mutex::new(None), draining: Mutex::new(None) }
    }

    pub async fn start(&self, device_id: &str) -> Result<(), String> {
        self.stop();
        // Let the previous session let go of the device before reopening it.
        let prev = self.draining.lock().take();
        if let Some(prev) = prev {
            let _ = tokio::task::spawn_blocking(move || prev.join()).await;
        }
        let slot: FrameSlot = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = {
            let (id, slot, stop) = (device_id.to_string(), slot.clone(), stop.clone());
            std::thread::Builder::new()
                .name("lookout-camera".into())
                .spawn(move || capture_thread(id, slot, stop, tx))
                .map_err(|e| e.to_string())?
        };
        *self.running.lock() = Some(Running { stop, thread, slot: slot.clone() });
        let opened = tokio::task::spawn_blocking(move || rx.recv().unwrap_or(Err("the camera thread exited".into())))
            .await
            .map_err(|e| e.to_string())?;
        let result = match opened {
            Ok(()) => wait_first_frame(&slot).await,
            Err(e) => Err(e),
        };
        if result.is_err() {
            self.stop();
        }
        result
    }

    pub async fn grab(&self, _device_id: &str) -> Result<CameraFrame, String> {
        let slot = self.running.lock().as_ref().map(|r| r.slot.clone()).ok_or(NO_FRAME)?;
        encode_latest(&slot).await
    }

    /// Signals the capture thread, which shuts the media source down (light
    /// off) after its current read — at most about one frame later.
    pub fn stop(&self) {
        if let Some(r) = self.running.lock().take() {
            r.stop.store(true, Ordering::Relaxed);
            *r.slot.lock() = None;
            *self.draining.lock() = Some(r.thread);
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Video capture devices: id = symbolic link, label = friendly name.
pub fn list_devices() -> Vec<CameraDevice> {
    // Own thread so COM apartment state of the caller doesn't matter.
    std::thread::spawn(|| {
        let Ok(_mf) = MfThread::init() else {
            return Vec::new();
        };
        let Ok(devices) = enum_video_devices() else {
            return Vec::new();
        };
        devices
            .iter()
            .filter_map(|a| {
                let id = string_attr(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)?;
                let label = string_attr(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_else(|| "Camera".into());
                Some(CameraDevice { id, label })
            })
            .collect()
    })
    .join()
    .unwrap_or_default()
}
