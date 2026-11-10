//! Windows backend: Media Foundation's `IMFMediaEngine` in frame-server
//! mode.
//!
//! The engine is the same one behind `<video>` in Edge/WebView2: it handles
//! HTTP streaming, buffering, hardware decode (through the DXGI device
//! manager we give it) and the presentation clock. Each display frame the UI
//! sends a tick; if the engine has a new frame we `TransferVideoFrame` it
//! into a BGRA texture on our own D3D11 device, read it back and hand the
//! pixels to GPUI's renderer (`paint_surface_frame`), which keeps one
//! texture per player and updates it in place.
//!
//! All engine calls happen on one MTA worker thread; the UI talks to it
//! through a command channel and reads a shared snapshot.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::gpui::{SurfaceFormat, SurfaceFrame, SurfaceSourceId};
use parking_lot::Mutex;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{BSTR, Interface, implement};

use super::{Frame, Status, Waker};

/// Frames larger than this are scaled down by the engine on transfer
/// (readback cost grows with area; the player is rarely shown larger).
const MAX_WIDTH: u32 = 3840;

enum Cmd {
    Play,
    Pause,
    Seek(f64),
    Rate(f64),
    Loop(bool),
    Tick,
    Stop,
}

#[derive(Default)]
struct Shared {
    frame: Option<Frame>,
    position: f64,
    duration: f64,
    want_playing: bool,
    ended: bool,
    waiting: bool,
    seeking: bool,
    buffered: f64,
    error: Option<String>,
    /// Latest requested seek not yet confirmed by the engine.
    seek_target: Option<f64>,
}

pub struct Backend {
    tx: flume::Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    tick_queued: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Backend {
    pub fn open(url: &str, wake: Waker) -> Result<Self, String> {
        let (tx, rx) = flume::unbounded::<Cmd>();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let tick_queued = Arc::new(AtomicBool::new(false));
        let thread = {
            let url = url.to_string();
            let shared = shared.clone();
            let tx = tx.clone();
            let tick_queued = tick_queued.clone();
            std::thread::Builder::new()
                .name("lookout-video".into())
                .spawn(move || {
                    if let Err(e) = run(&url, &rx, tx, &shared, &wake, &tick_queued) {
                        log::warn!("[video] {e}");
                        shared.lock().error = Some(e);
                        wake();
                    }
                })
                .map_err(|e| e.to_string())?
        };
        Ok(Self { tx, shared, tick_queued, thread: Some(thread) })
    }

    pub fn play(&self) {
        {
            let mut s = self.shared.lock();
            s.want_playing = true;
            s.ended = false;
        }
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        self.shared.lock().want_playing = false;
        let _ = self.tx.send(Cmd::Pause);
    }

    pub fn seek(&self, t: f64) {
        {
            let mut s = self.shared.lock();
            s.seek_target = Some(t.max(0.0));
            s.ended = false;
        }
        let _ = self.tx.send(Cmd::Seek(t.max(0.0)));
    }

    pub fn set_rate(&self, rate: f64) {
        let _ = self.tx.send(Cmd::Rate(rate));
    }

    pub fn set_looping(&self, looping: bool) {
        let _ = self.tx.send(Cmd::Loop(looping));
    }

    /// Ask the worker to fetch the frame for "now" (one queued at a time).
    pub fn tick(&self) {
        if !self.tick_queued.swap(true, Ordering::AcqRel) {
            let _ = self.tx.send(Cmd::Tick);
        }
    }

    pub fn frame(&self) -> Option<Frame> {
        self.shared.lock().frame.clone()
    }

    pub fn status(&self) -> Status {
        let s = self.shared.lock();
        let has_frame = s.frame.is_some();
        let seeking = s.seeking || s.seek_target.is_some();
        Status {
            position: if s.ended { s.duration } else { s.seek_target.unwrap_or(s.position) },
            duration: s.duration,
            playing: s.want_playing,
            buffering: s.error.is_none() && ((s.want_playing && s.waiting) || !has_frame),
            ended: s.ended,
            buffered: s.buffered.min(s.duration),
            error: s.error.clone(),
            active: s.error.is_none() && (s.want_playing || seeking || !has_frame || s.waiting),
            has_frame,
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Receives engine events on Media Foundation's threads.
#[implement(IMFMediaEngineNotify)]
struct Notify {
    shared: Arc<Mutex<Shared>>,
    tx: flume::Sender<Cmd>,
    wake: Waker,
    tick_queued: Arc<AtomicBool>,
}

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, param2: u32) -> windows::core::Result<()> {
        let event = MF_MEDIA_ENGINE_EVENT(event as i32);
        {
            let mut s = self.shared.lock();
            match event {
                MF_MEDIA_ENGINE_EVENT_ERROR => {
                    let code = match MF_MEDIA_ENGINE_ERR(param1 as i32) {
                        MF_MEDIA_ENGINE_ERR_ABORTED => "aborted",
                        MF_MEDIA_ENGINE_ERR_NETWORK => "network error",
                        MF_MEDIA_ENGINE_ERR_DECODE => "decode error",
                        MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED => "format not supported",
                        MF_MEDIA_ENGINE_ERR_ENCRYPTED => "encrypted",
                        _ => "error",
                    };
                    s.error = Some(format!("Media Foundation: {code} (0x{param2:08x})"));
                }
                MF_MEDIA_ENGINE_EVENT_WAITING | MF_MEDIA_ENGINE_EVENT_STALLED | MF_MEDIA_ENGINE_EVENT_BUFFERINGSTARTED => {
                    s.waiting = true
                }
                MF_MEDIA_ENGINE_EVENT_PLAYING
                | MF_MEDIA_ENGINE_EVENT_CANPLAY
                | MF_MEDIA_ENGINE_EVENT_BUFFERINGENDED
                | MF_MEDIA_ENGINE_EVENT_SEEKED => s.waiting = false,
                _ => {}
            }
        }
        // Let the worker refresh its snapshot (and pull a frame) promptly.
        if !self.tick_queued.swap(true, Ordering::AcqRel) {
            let _ = self.tx.send(Cmd::Tick);
        }
        (self.wake)();
        Ok(())
    }
}

fn hr(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{what}: {e}")
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let attempt = |driver: D3D_DRIVER_TYPE| -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
        let mut device = None;
        let mut context = None;
        // SAFETY: out-pointers are valid for the call.
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(hr("D3D11CreateDevice"))?;
        }
        Ok((device.ok_or("no D3D11 device")?, context.ok_or("no D3D11 context")?))
    };
    attempt(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| attempt(D3D_DRIVER_TYPE_WARP))
}

/// The BGRA render target the engine draws into, and its CPU-readable twin.
struct Targets {
    width: u32,
    height: u32,
    gpu: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

fn make_targets(device: &ID3D11Device, width: u32, height: u32) -> Result<Targets, String> {
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut gpu = None;
    let mut staging = None;
    // SAFETY: descriptors and out-pointers are valid for the calls.
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut gpu)).map_err(hr("CreateTexture2D"))?;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        device.CreateTexture2D(&desc, None, Some(&mut staging)).map_err(hr("CreateTexture2D staging"))?;
    }
    Ok(Targets { width, height, gpu: gpu.ok_or("no texture")?, staging: staging.ok_or("no staging texture")? })
}

/// Copy the staging texture into a tightly packed BGRA buffer.
fn read_back(context: &ID3D11DeviceContext, t: &Targets) -> Result<Vec<u8>, String> {
    // SAFETY: the staging texture is CPU-readable; we only read `height`
    // rows of `RowPitch` bytes from the mapped pointer and unmap after.
    unsafe {
        context.CopyResource(&t.staging, &t.gpu);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&t.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(hr("Map"))?;
        let row = t.width as usize * 4;
        let pitch = mapped.RowPitch as usize;
        let mut out = Vec::with_capacity(row * t.height as usize);
        let base = mapped.pData as *const u8;
        for y in 0..t.height as usize {
            out.extend_from_slice(std::slice::from_raw_parts(base.add(y * pitch), row));
        }
        context.Unmap(&t.staging, 0);
        Ok(out)
    }
}

fn run(
    url: &str,
    rx: &flume::Receiver<Cmd>,
    tx: flume::Sender<Cmd>,
    shared: &Arc<Mutex<Shared>>,
    wake: &Waker,
    tick_queued: &Arc<AtomicBool>,
) -> Result<(), String> {
    // SAFETY: plain COM / MF initialisation for this thread; MFStartup is
    // reference counted, so it is fine alongside the filmstrip decoder's.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(hr("MFStartup"))?;
    }
    let (device, context) = create_device()?;
    // SAFETY: COM calls on interfaces we own.
    let engine = unsafe {
        // The engine decodes on its own threads with our device.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(hr("MFCreateDXGIDeviceManager"))?;
        let manager = manager.ok_or("no DXGI device manager")?;
        manager.ResetDevice(&device, token).map_err(hr("ResetDevice"))?;

        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 3).map_err(hr("MFCreateAttributes"))?;
        let attrs = attrs.ok_or("no attributes")?;
        let notify: IMFMediaEngineNotify = Notify {
            shared: shared.clone(),
            tx: tx.clone(),
            wake: wake.clone(),
            tick_queued: tick_queued.clone(),
        }
        .into();
        attrs.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager).map_err(hr("DXGI manager"))?;
        attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify).map_err(hr("callback"))?;
        attrs
            .SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32)
            .map_err(hr("output format"))?;

        let factory: IMFMediaEngineClassFactory =
            CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_ALL).map_err(hr("engine factory"))?;
        let engine = factory.CreateInstance(0, &attrs).map_err(hr("CreateInstance"))?;
        let _ = engine.SetMuted(true);
        let _ = engine.SetAutoPlay(false);
        let _ = engine.SetPreload(MF_MEDIA_ENGINE_PRELOAD_AUTOMATIC);
        engine.SetSource(&BSTR::from(url)).map_err(hr("SetSource"))?;
        engine
    };

    let source = SurfaceSourceId::next();
    let mut generation = 0u64;
    let mut targets: Option<Targets> = None;
    let mut last_pts = i64::MIN;
    // Seeks are chased: one at a time, latest target wins.
    let mut pending_seek: Option<f64> = None;
    let mut seek_started: Option<Instant> = None;
    // The last target handed to the engine: only that one may clear the
    // UI-side `seek_target` once it lands (a newer one may be queued).
    let mut issued: Option<f64> = None;

    loop {
        let playing = shared.lock().want_playing;
        let cmd = rx.recv_timeout(if playing { Duration::from_millis(50) } else { Duration::from_millis(250) });
        let mut cmds: Vec<Cmd> = Vec::new();
        match cmd {
            Ok(c) => cmds.push(c),
            Err(flume::RecvTimeoutError::Timeout) => {}
            Err(flume::RecvTimeoutError::Disconnected) => break,
        }
        cmds.extend(rx.try_iter());
        let mut stop = false;
        // SAFETY: engine calls on the thread that owns it.
        unsafe {
            for c in cmds {
                match c {
                    Cmd::Play => {
                        if engine.IsEnded().as_bool() {
                            pending_seek = Some(0.0);
                        }
                        let _ = engine.Play();
                    }
                    Cmd::Pause => {
                        let _ = engine.Pause();
                    }
                    Cmd::Seek(t) => pending_seek = Some(t),
                    Cmd::Rate(r) => {
                        let _ = engine.SetDefaultPlaybackRate(r);
                        let _ = engine.SetPlaybackRate(r);
                    }
                    Cmd::Loop(l) => {
                        let _ = engine.SetLoop(l);
                    }
                    Cmd::Tick => tick_queued.store(false, Ordering::Release),
                    Cmd::Stop => stop = true,
                }
            }
            if stop {
                break;
            }

            let seeking = engine.IsSeeking().as_bool();
            if seeking && seek_started.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                seek_started = None;
            }
            if !seeking {
                seek_started = None;
            }
            if seek_started.is_none() {
                if let Some(t) = pending_seek.take() {
                    issued = Some(t);
                    if engine.SetCurrentTime(t).is_ok() {
                        seek_started = Some(Instant::now());
                    }
                }
            }

            // Frame: S_OK from OnVideoStreamTick means a new one is ready.
            let mut pts = 0i64;
            let ready = (Interface::vtable(&engine).OnVideoStreamTick)(Interface::as_raw(&engine), &mut pts).0 == 0;
            if ready && pts != last_pts {
                let (mut w, mut h) = (0u32, 0u32);
                if engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).is_ok() && w > 0 && h > 0 {
                    if w > MAX_WIDTH {
                        h = (h as u64 * MAX_WIDTH as u64 / w as u64) as u32;
                        w = MAX_WIDTH;
                    }
                    if targets.as_ref().is_none_or(|t| t.width != w || t.height != h) {
                        targets = Some(make_targets(&device, w, h)?);
                    }
                    let t = targets.as_ref().unwrap();
                    let dst = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
                    let border = MFARGB { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbAlpha: 255 };
                    if engine.TransferVideoFrame(&t.gpu, None, &dst, Some(&border)).is_ok() {
                        match read_back(&context, t) {
                            Ok(pixels) => {
                                last_pts = pts;
                                generation += 1;
                                let surface = SurfaceFrame {
                                    source,
                                    generation,
                                    width: w,
                                    height: h,
                                    format: SurfaceFormat::Bgra { offset: 0, stride: w as usize * 4 },
                                    data: Box::new(pixels),
                                };
                                shared.lock().frame = Some(Frame { width: w, height: h, surface: Arc::new(surface) });
                                wake();
                            }
                            Err(e) => log::warn!("[video] readback: {e}"),
                        }
                    }
                }
            }

            // Snapshot for the UI.
            let duration = engine.GetDuration();
            let buffered = engine
                .GetBuffered()
                .ok()
                .map(|ranges| {
                    (0..ranges.GetLength()).filter_map(|i| ranges.GetEnd(i).ok()).fold(0.0f64, f64::max)
                })
                .unwrap_or(0.0);
            let ended = engine.IsEnded().as_bool();
            let position = engine.GetCurrentTime();
            // No engine calls while `shared` is held: the engine fires
            // `Notify::EventNotify` (which locks `shared`) from its work queue
            // while holding its own lock, so calling into it here deadlocks
            // the worker, MF's thread and the UI thread (LOOKOUT-NATIVE-B).
            let mut s = shared.lock();
            s.duration = if duration.is_finite() && duration > 0.0 { duration } else { 0.0 };
            s.position = position;
            s.buffered = buffered;
            s.seeking = seek_started.is_some() || pending_seek.is_some();
            if !s.seeking && (s.seek_target.is_none() || s.seek_target == issued) {
                s.seek_target = None;
            }
            if ended && s.want_playing {
                s.want_playing = false;
                s.ended = true;
                drop(s);
                wake();
            }
        }
    }
    // SAFETY: shut the engine down on its own thread before it drops.
    unsafe {
        let _ = engine.Shutdown();
    }
    Ok(())
}
