//! Editor filmstrip thumbnails (`filmstripFrames.ts`): decode one frame per
//! tile at the tile's centre time, at display height, on a background
//! thread. Linux decodes with GStreamer; elsewhere the track stays plain
//! (the web app's "all sources failed" fallback).

use std::sync::Arc;

use gpui_kit::gpui::RenderImage;

/// Frames for each `times` entry (seconds), `height` px tall. Calls
/// `publish` as frames land (the web app published every third tile).
pub fn extract(url: &str, times: &[f64], height: u32, publish: &dyn Fn(Vec<Option<Arc<RenderImage>>>)) -> Result<(), String> {
    imp::extract(url, times, height, publish)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    pub fn extract(url: &str, times: &[f64], height: u32, publish: &dyn Fn(Vec<Option<Arc<RenderImage>>>)) -> Result<(), String> {
        gst::init().map_err(|e| e.to_string())?;
        let desc = format!(
            "uridecodebin uri=\"{url}\" ! videoconvert ! videoscale ! video/x-raw,format=BGRA,height={height},pixel-aspect-ratio=1/1 ! appsink name=sink sync=false max-buffers=1"
        );
        let pipeline = gst::parse::launch(&desc).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>().map_err(|_| "not a pipeline")?;
        let sink = pipeline.by_name("sink").ok_or("no sink")?.downcast::<gst_app::AppSink>().map_err(|_| "not an appsink")?;
        pipeline.set_state(gst::State::Paused).map_err(|e| e.to_string())?;
        let (res, _, _) = pipeline.state(gst::ClockTime::from_seconds(10));
        res.map_err(|e| format!("preroll: {e}"))?;
        let mut out: Vec<Option<Arc<RenderImage>>> = vec![None; times.len()];
        let mut faults = 0;
        for (i, t) in times.iter().enumerate() {
            let ok = pipeline.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds((t.max(0.0) * 1e9) as u64),
            );
            if ok.is_err() {
                faults += 1;
            } else {
                let _ = pipeline.state(gst::ClockTime::from_seconds(4));
                match sink.try_pull_preroll(gst::ClockTime::from_seconds(4)) {
                    Some(sample) => out[i] = sample_to_image(&sample),
                    None => faults += 1,
                }
            }
            // MAX_TILE_FAULTS: give up on a source that keeps failing.
            if faults > 2 {
                break;
            }
            if i % 3 == 2 {
                publish(out.clone());
            }
        }
        publish(out);
        let _ = pipeline.set_state(gst::State::Null);
        Ok(())
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

    /// Sample one frame per tile with the native decoder.
    pub fn extract(url: &str, times: &[f64], height: u32, publish: &dyn Fn(Vec<Option<Arc<RenderImage>>>)) -> Result<(), String> {
        let path = crate::platform::decoder::local_path(url)?;
        let mut dec = crate::platform::decoder::open(&path, Some(height))?;
        let mut out: Vec<Option<Arc<RenderImage>>> = vec![None; times.len()];
        let mut faults = 0;
        for (i, t) in times.iter().enumerate() {
            let frame = dec.seek(*t).and_then(|_| dec.next_frame());
            match frame {
                Ok(Some(f)) => {
                    if let Some(img) = image::RgbaImage::from_raw(f.width, f.height, f.bgra) {
                        out[i] = Some(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(img)])));
                    }
                }
                _ => faults += 1,
            }
            if faults > 2 {
                break;
            }
            if i % 3 == 2 {
                publish(out.clone());
            }
        }
        publish(out);
        Ok(())
    }
}
