//! Linux backend: GStreamer `playbin` into an NV12 `appsink`.
//!
//! - Decoders are autoplugged by rank, so VA-API (`vah264dec`) is used when
//!   the driver supports it; frames stay NV12 and the GPU converts colour.
//! - Remote URLs stream (souphttpsrc) with progressive download, so seeking
//!   inside what's been fetched is instant. If streaming fails before the
//!   first frame (no soup plugin, odd server), the file is downloaded to the
//!   cache and played from disk instead.
//! - Seeks are accurate and coalesced: while one is in flight, newer targets
//!   replace each other and the latest runs when it lands, so scrubbing
//!   tracks the pointer as fast as the decoder allows.
//!
//! Locking rule: never call into GStreamer while holding `Shared` — a
//! flushing seek waits for the streaming thread, which may be waiting for
//! the lock in an appsink callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::gpui::{SurfaceFormat, SurfaceFrame, SurfaceSourceId, YuvColor};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use parking_lot::Mutex;

use super::{Frame, Status, Waker};

#[derive(Default)]
struct Shared {
    frame: Option<Frame>,
    generation: u64,
    duration: f64,
    want_playing: bool,
    buffering: bool,
    ended: bool,
    error: Option<String>,
    looping: bool,
    rate: f64,
    /// Reached PAUSED once; seeks before that are queued.
    prerolled: bool,
    in_flight: Option<(f64, Instant)>,
    pending: Option<f64>,
    /// Streaming failed and we're fetching the whole file instead.
    downloading: bool,
    fallback_tried: bool,
    buffered: f64,
    buffered_at: Option<Instant>,
}

pub struct Backend {
    playbin: gst::Element,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    bus_thread: Option<std::thread::JoinHandle<()>>,
    remote: bool,
}

/// A mapped GStreamer buffer handed to the renderer without copying.
struct Mapped(gst::MappedBuffer<gst::buffer::Readable>);

impl AsRef<[u8]> for Mapped {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

fn to_frame(sample: &gst::Sample, source: SurfaceSourceId, generation: u64) -> Option<Frame> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    if info.format() != gst_video::VideoFormat::Nv12 {
        return None;
    }
    let buffer = sample.buffer_owned()?;
    // Decoders that pad planes describe the real layout in a VideoMeta.
    let (offsets, strides): (Vec<usize>, Vec<i32>) = match buffer.meta::<gst_video::VideoMeta>() {
        Some(m) => (m.offset().to_vec(), m.stride().to_vec()),
        None => (info.offset().to_vec(), info.stride().to_vec()),
    };
    if offsets.len() < 2 || strides.len() < 2 || strides[0] <= 0 || strides[1] <= 0 {
        return None;
    }
    let colorimetry = info.colorimetry();
    let color = YuvColor {
        bt709: !matches!(colorimetry.matrix(), gst_video::VideoColorMatrix::Bt601),
        full_range: matches!(colorimetry.range(), gst_video::VideoColorRange::Range0255),
    };
    let (width, height) = (info.width(), info.height());
    let map = buffer.into_mapped_buffer_readable().ok()?;
    Some(Frame {
        width,
        height,
        surface: Arc::new(SurfaceFrame {
            source,
            generation,
            width,
            height,
            format: SurfaceFormat::Nv12 {
                y_offset: offsets[0],
                y_stride: strides[0] as usize,
                uv_offset: offsets[1],
                uv_stride: strides[1] as usize,
                color,
            },
            data: Box::new(Mapped(map)),
        }),
    })
}

fn secs(t: f64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds((t.max(0.0) * 1e9) as u64)
}

/// Issue an accurate flushing seek at `rate`. Must not hold `Shared`.
fn raw_seek(playbin: &gst::Element, t: f64, rate: f64) -> bool {
    playbin
        .seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            secs(t),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        )
        .is_ok()
}

/// Start the queued seek if none is running. Must not hold `Shared`.
fn pump_seek(playbin: &gst::Element, shared: &Mutex<Shared>) {
    let (t, rate) = {
        let mut s = shared.lock();
        if s.in_flight.is_some() || !s.prerolled {
            return;
        }
        let Some(t) = s.pending.take() else { return };
        s.in_flight = Some((t, Instant::now()));
        (t, s.rate)
    };
    if !raw_seek(playbin, t, rate) {
        shared.lock().in_flight = None;
    }
}

impl Backend {
    pub fn open(url: &str, wake: Waker) -> Result<Self, String> {
        gst::init().map_err(|e| e.to_string())?;
        let playbin = gst::ElementFactory::make("playbin").build().map_err(|e| format!("playbin: {e}"))?;
        playbin.set_property("uri", url);
        // Video only; progressive download for remote files so seeking
        // within what's fetched doesn't re-request.
        playbin.set_property_from_str("flags", "video+download+buffering");
        let sink = gst_app::AppSink::builder()
            .caps(&gst::Caps::builder("video/x-raw").field("format", "NV12").build())
            .max_buffers(2)
            .drop(true)
            .qos(true)
            .build();
        playbin.set_property("video-sink", &sink);

        let shared = Arc::new(Mutex::new(Shared { rate: 1.0, ..Default::default() }));
        let source = SurfaceSourceId::next();
        let on_sample = {
            let shared = shared.clone();
            let wake = wake.clone();
            move |sample: gst::Sample| {
                let generation = {
                    let mut s = shared.lock();
                    s.generation += 1;
                    s.generation
                };
                if let Some(frame) = to_frame(&sample, source, generation) {
                    shared.lock().frame = Some(frame);
                    wake();
                }
            }
        };
        let on_preroll = on_sample.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |s| {
                    if let Ok(sample) = s.pull_sample() {
                        on_sample(sample);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .new_preroll(move |s| {
                    if let Ok(sample) = s.pull_preroll() {
                        on_preroll(sample);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        playbin.set_state(gst::State::Paused).map_err(|e| format!("can't start playback: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let bus = playbin.bus().ok_or("no bus")?;
        let bus_thread = {
            let playbin = playbin.downgrade();
            let shared = shared.clone();
            let stop = stop.clone();
            let url = url.to_string();
            std::thread::Builder::new()
                .name("lookout-video-bus".into())
                .spawn(move || bus_loop(bus, playbin, shared, stop, wake, url))
                .map_err(|e| e.to_string())?
        };
        Ok(Self { playbin, shared, stop, bus_thread: Some(bus_thread), remote: super::is_remote(url) })
    }

    pub fn play(&self) {
        let (restart, buffering) = {
            let mut s = self.shared.lock();
            s.want_playing = true;
            let restart = std::mem::take(&mut s.ended);
            if restart {
                s.pending = Some(0.0);
            }
            (restart, s.buffering || s.downloading)
        };
        if restart {
            pump_seek(&self.playbin, &self.shared);
        }
        if !buffering {
            let _ = self.playbin.set_state(gst::State::Playing);
        }
    }

    pub fn pause(&self) {
        self.shared.lock().want_playing = false;
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    pub fn seek(&self, t: f64) {
        {
            let mut s = self.shared.lock();
            s.ended = false;
            s.pending = Some(t.max(0.0));
        }
        pump_seek(&self.playbin, &self.shared);
    }

    pub fn set_rate(&self, rate: f64) {
        let (prerolled, position) = {
            let mut s = self.shared.lock();
            s.rate = rate;
            (s.prerolled, s.in_flight.map(|(t, _)| t).or(s.pending))
        };
        if !prerolled {
            return;
        }
        // Instant rate change (GStreamer >= 1.18) keeps playing without a
        // flush; otherwise re-seek to where we are at the new rate.
        let instant = self
            .playbin
            .seek(
                rate,
                gst::SeekFlags::INSTANT_RATE_CHANGE,
                gst::SeekType::None,
                gst::ClockTime::NONE,
                gst::SeekType::None,
                gst::ClockTime::NONE,
            )
            .is_ok();
        if !instant {
            let t = position
                .or_else(|| self.playbin.query_position::<gst::ClockTime>().map(|p| p.nseconds() as f64 / 1e9))
                .unwrap_or(0.0);
            self.seek(t);
        }
    }

    pub fn set_looping(&self, looping: bool) {
        self.shared.lock().looping = looping;
    }

    /// Frames are pushed by GStreamer; nothing to pull.
    pub fn tick(&self) {}

    pub fn frame(&self) -> Option<Frame> {
        self.shared.lock().frame.clone()
    }

    pub fn status(&self) -> Status {
        let (seek_target, ended, need_duration, need_buffered) = {
            let s = self.shared.lock();
            (
                s.pending.or(s.in_flight.map(|(t, _)| t)),
                s.ended,
                s.duration <= 0.0,
                s.buffered_at.is_none_or(|at| at.elapsed() > Duration::from_millis(250)),
            )
        };
        if need_duration {
            if let Some(d) = self.playbin.query_duration::<gst::ClockTime>() {
                self.shared.lock().duration = d.nseconds() as f64 / 1e9;
            }
        }
        if need_buffered {
            let buffered = self.query_buffered();
            let mut s = self.shared.lock();
            s.buffered = buffered.unwrap_or(s.duration);
            s.buffered_at = Some(Instant::now());
        }
        let position = match seek_target {
            Some(t) => Some(t),
            None if ended => None,
            None => self.playbin.query_position::<gst::ClockTime>().map(|p| p.nseconds() as f64 / 1e9),
        };
        let s = self.shared.lock();
        let seeking = s.pending.is_some() || s.in_flight.is_some();
        Status {
            position: if ended { s.duration } else { position.unwrap_or(0.0).min(s.duration.max(0.0)) },
            duration: s.duration,
            playing: s.want_playing,
            buffering: s.buffering || s.downloading || (s.frame.is_none() && s.error.is_none()),
            ended: s.ended,
            buffered: s.buffered.min(s.duration),
            error: s.error.clone(),
            active: s.error.is_none() && (s.want_playing || seeking || s.frame.is_none() || s.buffering),
            has_frame: s.frame.is_some(),
        }
    }

    /// Seconds downloaded from the start, for remote media.
    fn query_buffered(&self) -> Option<f64> {
        if !self.remote {
            return None;
        }
        let mut q = gst::query::Buffering::new(gst::Format::Percent);
        if !self.playbin.query(&mut q) {
            return None;
        }
        let (_, _, stop, _) = q.range();
        let gst::GenericFormattedValue::Percent(Some(p)) = stop else { return None };
        let duration = self.shared.lock().duration;
        Some(duration * (*p as f64 / gst::format::Percent::MAX.into_inner() as f64))
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.playbin.set_state(gst::State::Null);
        if let Some(t) = self.bus_thread.take() {
            let _ = t.join();
        }
    }
}

fn bus_loop(
    bus: gst::Bus,
    playbin: gst::glib::WeakRef<gst::Element>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    wake: Waker,
    url: String,
) {
    while !stop.load(Ordering::Relaxed) {
        let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
            // A seek that never completes (e.g. issued mid state change)
            // mustn't wedge scrubbing.
            let stuck = shared.lock().in_flight.is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2));
            if stuck {
                shared.lock().in_flight = None;
                if let Some(pb) = playbin.upgrade() {
                    pump_seek(&pb, &shared);
                }
            }
            continue;
        };
        let Some(pb) = playbin.upgrade() else { break };
        use gst::MessageView;
        match msg.view() {
            MessageView::Error(e) => {
                let detail = format!("{} ({})", e.error(), e.debug().map(|d| d.to_string()).unwrap_or_default());
                let fallback = {
                    let mut s = shared.lock();
                    let fallback = s.frame.is_none() && super::is_remote(&url) && !s.fallback_tried;
                    if fallback {
                        s.fallback_tried = true;
                        s.downloading = true;
                    } else {
                        s.error = Some(detail.clone());
                    }
                    fallback
                };
                log::warn!("[video] {detail}{}", if fallback { " — downloading instead" } else { "" });
                if fallback {
                    let _ = pb.set_state(gst::State::Null);
                    let shared = shared.clone();
                    let wake = wake.clone();
                    let url = url.clone();
                    let pb = pb.downgrade();
                    std::thread::spawn(move || {
                        let result = super::cached_copy(&url);
                        let Some(pb) = pb.upgrade() else { return };
                        match result {
                            Ok(path) => {
                                let file = url::Url::from_file_path(&path).map(|u| u.to_string()).unwrap_or_default();
                                pb.set_property("uri", file);
                                let playing = {
                                    let mut s = shared.lock();
                                    s.downloading = false;
                                    s.want_playing
                                };
                                let _ = pb.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
                            }
                            Err(e) => {
                                let mut s = shared.lock();
                                s.downloading = false;
                                s.error = Some(e);
                            }
                        }
                        wake();
                    });
                }
                wake();
            }
            MessageView::Eos(_) => {
                let looping = {
                    let mut s = shared.lock();
                    if !s.looping {
                        s.ended = true;
                        s.want_playing = false;
                    }
                    s.looping
                };
                if looping {
                    shared.lock().pending = Some(0.0);
                    pump_seek(&pb, &shared);
                } else {
                    let _ = pb.set_state(gst::State::Paused);
                }
                wake();
            }
            MessageView::Buffering(b) => {
                let percent = b.percent();
                let want = {
                    let mut s = shared.lock();
                    s.buffering = percent < 100;
                    s.want_playing
                };
                if want {
                    let _ = pb.set_state(if percent < 100 { gst::State::Paused } else { gst::State::Playing });
                }
                wake();
            }
            MessageView::AsyncDone(_) => {
                {
                    let mut s = shared.lock();
                    s.prerolled = true;
                    s.in_flight = None;
                }
                pump_seek(&pb, &shared);
                wake();
            }
            MessageView::DurationChanged(_) => {
                shared.lock().duration = 0.0;
                wake();
            }
            _ => {}
        }
    }
}
