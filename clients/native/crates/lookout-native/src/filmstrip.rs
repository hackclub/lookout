//! Editor filmstrip thumbnails, decoded on demand: the editor asks for the
//! frames its visible tiles need (they change as the timeline zooms and
//! pans) and each lands as it's decoded. One long-lived decoder per editor,
//! on its own thread; a newer request replaces whatever is still queued.
//! Linux decodes with GStreamer, elsewhere with the platform decoder.

use std::sync::Arc;

use gpui_kit::gpui::RenderImage;

/// Frames are keyed by their index at this rate, so requests for times
/// inside the same frame share one decode.
pub const KEY_FPS: f64 = 30.0;

pub fn key(t: f64) -> u64 {
    (t.max(0.0) * KEY_FPS).round() as u64
}

pub fn key_time(k: u64) -> f64 {
    k as f64 / KEY_FPS
}

/// A decoded tile, or `None` when the decoder gave up on the source.
pub type Tile = (u64, Option<Arc<RenderImage>>);

/// Handle to the decoder thread. Dropping it stops the thread.
pub struct Filmstrip {
    tx: flume::Sender<Vec<u64>>,
}

impl Filmstrip {
    /// Start a decoder for `url` producing frames `height` px tall; tiles
    /// arrive on the returned receiver.
    pub fn spawn(url: String, height: u32) -> (Self, flume::Receiver<Tile>) {
        let (tx, rx) = flume::unbounded::<Vec<u64>>();
        let (out_tx, out_rx) = flume::unbounded::<Tile>();
        std::thread::Builder::new()
            .name("filmstrip".into())
            .spawn(move || {
                if let Err(e) = imp::run(&url, height, &rx, &out_tx) {
                    log::warn!("[filmstrip] {e}");
                    let _ = out_tx.send((u64::MAX, None));
                }
            })
            .ok();
        (Self { tx }, out_rx)
    }

    /// Decode these frames (in this order), dropping any older request.
    pub fn request(&self, keys: Vec<u64>) {
        let _ = self.tx.send(keys);
    }
}

/// Pull the newest request, waiting for one if `block`.
fn latest(rx: &flume::Receiver<Vec<u64>>, block: bool) -> Option<Result<Vec<u64>, ()>> {
    let mut got = if block {
        match rx.recv() {
            Ok(v) => Some(v),
            Err(_) => return Some(Err(())),
        }
    } else {
        None
    };
    loop {
        match rx.try_recv() {
            Ok(v) => got = Some(v),
            Err(flume::TryRecvError::Empty) => return got.map(Ok),
            Err(flume::TryRecvError::Disconnected) => return Some(Err(())),
        }
    }
}

/// The shared request loop: `decode` turns a time into an image.
fn serve(
    rx: &flume::Receiver<Vec<u64>>,
    out: &flume::Sender<Tile>,
    mut decode: impl FnMut(f64) -> Option<Arc<RenderImage>>,
) -> Result<(), String> {
    let mut faults = 0;
    let mut queue: Vec<u64> = Vec::new();
    loop {
        match latest(rx, queue.is_empty()) {
            Some(Ok(next)) => queue = next,
            Some(Err(())) => return Ok(()),
            None => {}
        }
        if queue.is_empty() {
            continue;
        }
        let k = queue.remove(0);
        let img = decode(key_time(k));
        if img.is_none() {
            faults += 1;
            // MAX_TILE_FAULTS: give up on a source that keeps failing.
            if faults > 3 {
                return Err("too many decode failures".into());
            }
        } else {
            faults = 0;
        }
        if out.send((k, img)).is_err() {
            return Ok(());
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    pub fn run(url: &str, height: u32, rx: &flume::Receiver<Vec<u64>>, out: &flume::Sender<Tile>) -> Result<(), String> {
        gst::init().map_err(|e| e.to_string())?;
        let desc = format!(
            "uridecodebin uri=\"{url}\" ! videoconvert ! videoscale ! video/x-raw,format=BGRA,height={height},pixel-aspect-ratio=1/1 ! appsink name=sink sync=false max-buffers=1"
        );
        let pipeline = gst::parse::launch(&desc).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>().map_err(|_| "not a pipeline")?;
        let sink = pipeline.by_name("sink").ok_or("no sink")?.downcast::<gst_app::AppSink>().map_err(|_| "not an appsink")?;
        pipeline.set_state(gst::State::Paused).map_err(|e| e.to_string())?;
        let (res, _, _) = pipeline.state(gst::ClockTime::from_seconds(10));
        res.map_err(|e| format!("preroll: {e}"))?;
        let result = serve(rx, out, |t| {
            pipeline
                .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, gst::ClockTime::from_nseconds((t * 1e9) as u64))
                .ok()?;
            let _ = pipeline.state(gst::ClockTime::from_seconds(4));
            sink.try_pull_preroll(gst::ClockTime::from_seconds(4)).and_then(|s| sample_to_image(&s))
        });
        let _ = pipeline.set_state(gst::State::Null);
        result
    }

    fn sample_to_image(sample: &gst::Sample) -> Option<Arc<RenderImage>> {
        let info = gstreamer_video::VideoInfo::from_caps(sample.caps()?).ok()?;
        let buffer = sample.buffer()?;
        let map = buffer.map_readable().ok()?;
        let (w, h) = (info.width(), info.height());
        let stride = info.stride()[0] as usize;
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            data.extend_from_slice(&map.as_slice()[row * stride..row * stride + w as usize * 4]);
        }
        let img = image::RgbaImage::from_raw(w, h, data)?;
        Some(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(img)])))
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub fn run(url: &str, height: u32, rx: &flume::Receiver<Vec<u64>>, out: &flume::Sender<Tile>) -> Result<(), String> {
        let path = crate::platform::decoder::local_path(url)?;
        let mut dec = crate::platform::decoder::open(&path, Some(height))?;
        serve(rx, out, |t| {
            let f = dec.seek(t).and_then(|_| dec.next_frame()).ok()??;
            let img = image::RgbaImage::from_raw(f.width, f.height, f.bgra)?;
            Some(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(img)])))
        })
    }
}
