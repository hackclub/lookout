#[cfg(target_os = "linux")]
pub fn capture_pipewire_node(
    node_id: u32,
    fd: std::os::fd::RawFd,
) -> Result<image::DynamicImage, String> {
    // The CPU capture path does not need alpha. Letting videoconvert choose
    // the source format prefers BGRA on some compositors, where allocation
    // can fail even though the same stream supplies BGRx in shared memory.
    // Keep the unrestricted path for portals that only offer other formats.
    capture_pipewire_node_with_caps(node_id, fd, "video/x-raw,format=BGRx")
        .or_else(|preferred_error| {
            capture_pipewire_node_with_caps(node_id, fd, "video/x-raw").map_err(|fallback_error| {
                format!("{preferred_error}; fallback capture: {fallback_error}")
            })
        })
}

/// Everything after the source caps: shared with the tests, which feed it
/// from an appsrc instead of a portal stream.
#[cfg(target_os = "linux")]
const CONVERT_TO_SINK: &str =
    "videoconvert ! video/x-raw,format=RGBA ! appsink name=sink max-buffers=1 drop=true";

#[cfg(target_os = "linux")]
pub(crate) fn capture_pipewire_node_with_caps(
    node_id: u32,
    fd: std::os::fd::RawFd,
    source_caps: &str,
) -> Result<image::DynamicImage, String> {
    use gstreamer::prelude::*;

    gstreamer::init().map_err(|e| format!("Failed to init gstreamer: {}", e))?;

    let pipeline_str = format!(
        "pipewiresrc name=src fd={} path={} ! {source_caps} ! {CONVERT_TO_SINK}",
        fd, node_id
    );

    let pipeline = gstreamer::parse::launch(&pipeline_str)
        .map_err(|e| format!("Failed to create pipeline: {}", e))?
        .downcast::<gstreamer::Pipeline>()
        .map_err(|_| "Expected a pipeline".to_string())?;

    let src_pad = pipeline
        .by_name("src")
        .and_then(|src| src.static_pad("src"))
        .ok_or_else(|| "Failed to get pipewiresrc pad".to_string())?;
    drop_frames_without_a_picture(&src_pad);

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| "Failed to get appsink".to_string())?
        .downcast::<gstreamer_app::AppSink>()
        .map_err(|_| "Not an appsink".to_string())?;

    if let Err(e) = pipeline.set_state(gstreamer::State::Playing) {
        // Read the bus before NULL flushes it, and tear down even a partially
        // started pipeline. The state-change error alone hides the cause.
        let error = pipeline_error(&pipeline, &format!("Failed to set playing: {e}"), node_id);
        let _ = pipeline.set_state(gstreamer::State::Null);
        return Err(error);
    }

    // Wait up to 2 seconds for a frame. Buffers without a picture never get
    // this far (see drop_frames_without_a_picture), so the first sample is a
    // real one.
    let sample = appsink.try_pull_sample(gstreamer::ClockTime::from_seconds(2));

    // Tear down BEFORE bailing on a timeout: an early return here used to
    // drop the pipeline still PLAYING, stranding its streaming threads and
    // PipeWire stream — a leak per failed capture.
    let capture_error = sample.is_none().then(|| {
        pipeline_error(&pipeline, "Failed to pull sample within timeout", node_id)
    });
    let stopped = pipeline.set_state(gstreamer::State::Null);

    let sample = sample.ok_or_else(|| capture_error.unwrap())?;
    stopped.map_err(|e| format!("Failed to stop pipeline: {}", e))?;

    let buffer = sample
        .buffer()
        .ok_or_else(|| "Sample has no buffer".to_string())?;
    let caps = sample
        .caps()
        .ok_or_else(|| "Sample has no caps".to_string())?;
    let structure = caps
        .structure(0)
        .ok_or_else(|| "Caps have no structure".to_string())?;

    let width = structure
        .get::<i32>("width")
        .map_err(|_| "No width".to_string())?;
    let height = structure
        .get::<i32>("height")
        .map_err(|_| "No height".to_string())?;

    let map = buffer
        .map_readable()
        .map_err(|_| "Failed to map buffer".to_string())?;

    let img = image::RgbaImage::from_raw(width as u32, height as u32, map.as_slice().to_vec())
        .ok_or_else(|| "Failed to create image from raw bytes".to_string())?;

    Ok(image::DynamicImage::ImageRgba8(img))
}

/// Drop every buffer on `pad` that does not hold a whole frame for the
/// pad's caps, before `videoconvert` sees it.
///
/// With the portal's cursor "metadata" mode, mutter sends cursor-only
/// buffers whose chunk is empty. pipewiresrc up to 1.4 (Ubuntu 26.04) still
/// pushes them, as a buffer with no memory. `videoconvert` cannot map that,
/// logs "invalid video buffer received", and pushes its output buffer
/// anyway, never written. That output is a fresh allocation: zero pages
/// (a pure black frame) or recycled heap still holding an older frame at
/// another address (the screen shown shifted sideways and wrapped). With
/// `max-buffers=1 drop=true` such a buffer also replaces the good frame the
/// appsink was holding. Checking after conversion cannot catch it, because
/// the converted buffer always has the full size.
///
/// Crop metadata goes too. `videoconvert` ignores it anyway, and the black
/// border it describes is removed by `crop::auto_crop_black_borders`.
#[cfg(target_os = "linux")]
fn drop_frames_without_a_picture(pad: &gstreamer::Pad) {
    use gstreamer::prelude::*;

    pad.add_probe(gstreamer::PadProbeType::BUFFER, |pad, info| {
        let Some(gstreamer::PadProbeData::Buffer(buffer)) = &mut info.data else {
            return gstreamer::PadProbeReturn::Ok;
        };
        let info = pad
            .current_caps()
            .and_then(|caps| gstreamer_video::VideoInfo::from_caps(&caps).ok());
        let whole = info.as_ref().is_some_and(|info| holds_whole_frame(buffer, info));
        if !whole {
            return gstreamer::PadProbeReturn::Drop;
        }
        if buffer.meta::<gstreamer_video::VideoCropMeta>().is_some() {
            let buffer = buffer.make_mut();
            while let Some(meta) = buffer.meta_mut::<gstreamer_video::VideoCropMeta>() {
                if meta.remove().is_err() {
                    break;
                }
            }
        }
        gstreamer::PadProbeReturn::Ok
    });
}

/// Does `buffer` carry every byte of every plane `info` describes, laid
/// out by its video meta when it has one? Also rejects buffers the producer
/// marked corrupted.
#[cfg(target_os = "linux")]
fn holds_whole_frame(buffer: &gstreamer::BufferRef, info: &gstreamer_video::VideoInfo) -> bool {
    if buffer.n_memory() == 0
        || buffer.size() == 0
        || buffer.flags().intersects(gstreamer::BufferFlags::CORRUPTED | gstreamer::BufferFlags::GAP)
    {
        return false;
    }
    let size = buffer.size();
    let Some(meta) = buffer.meta::<gstreamer_video::VideoMeta>() else {
        return size >= info.size();
    };
    if meta.width() != info.width() || meta.height() != info.height() || meta.n_planes() != info.n_planes() {
        return false;
    }
    let finfo = info.format_info();
    (0..info.n_planes() as usize).all(|plane| {
        let (Some(&offset), Some(&stride)) = (meta.offset().get(plane), meta.stride().get(plane)) else {
            return false;
        };
        let rows = finfo.scale_height(plane as u8, info.height()) as usize;
        let row_bytes = info.stride().get(plane).copied().unwrap_or(0).max(0) as usize;
        let stride = stride.unsigned_abs() as usize;
        rows == 0
            || (stride >= row_bytes
                && offset
                    .checked_add(stride * (rows - 1))
                    .and_then(|end| end.checked_add(row_bytes))
                    .is_some_and(|end| end <= size))
    })
}

#[cfg(target_os = "linux")]
fn pipeline_error(pipeline: &gstreamer::Pipeline, fallback: &str, node_id: u32) -> String {
    use gstreamer::prelude::*;

    let mut details = Vec::new();
    if let Some(bus) = pipeline.bus() {
        while let Some(message) = bus.pop_filtered(&[gstreamer::MessageType::Error]) {
            if let gstreamer::MessageView::Error(error) = message.view() {
                let source = error.src().map(|s| s.path_string().to_string()).unwrap_or_default();
                let debug = error.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                details.push(format!("{source}: {}{debug}", error.error()));
            }
        }
    }
    if details.is_empty() {
        format!("{fallback} (PipeWire node {node_id})")
    } else {
        format!("{fallback} (PipeWire node {node_id}): {}", details.join("; "))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use gstreamer::prelude::*;


    const W: u32 = 64;
    const H: u32 = 8;

    /// BGRx with a distinct value per pixel, `stride` bytes per row.
    fn frame(stride: usize, seed: u8) -> Vec<u8> {
        let mut data = vec![0u8; stride * H as usize];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let px = &mut data[y * stride + x * 4..][..4];
                px.copy_from_slice(&[x as u8 ^ seed, y as u8 * 16, seed, 0xff]);
            }
        }
        data
    }

    fn with_meta(data: Vec<u8>, stride: usize) -> gstreamer::Buffer {
        gstreamer::init().unwrap();
        let mut buffer = gstreamer::Buffer::from_mut_slice(data);
        gstreamer_video::VideoMeta::add_full(
            buffer.get_mut().unwrap(),
            gstreamer_video::VideoFrameFlags::empty(),
            gstreamer_video::VideoFormat::Bgrx,
            W,
            H,
            &[0],
            &[stride as i32],
        )
        .unwrap();
        buffer
    }

    /// What mutter's cursor-only update becomes in pipewiresrc 1.4: video
    /// meta, no memory.
    fn cursor_only() -> gstreamer::Buffer {
        gstreamer::init().unwrap();
        let mut buffer = gstreamer::Buffer::new();
        gstreamer_video::VideoMeta::add_full(
            buffer.get_mut().unwrap(),
            gstreamer_video::VideoFrameFlags::empty(),
            gstreamer_video::VideoFormat::Bgrx,
            W,
            H,
            &[0],
            &[(W * 4) as i32],
        )
        .unwrap();
        buffer
    }

    /// Push `buffers` through the capture's conversion tail (guarded like
    /// the pipewiresrc pad) and return what the appsink holds at the end.
    fn capture(buffers: Vec<gstreamer::Buffer>, guarded: bool) -> Option<Vec<u8>> {
        gstreamer::init().unwrap();
        let pipeline = gstreamer::parse::launch(&format!(
            "appsrc name=src format=time caps=video/x-raw,format=BGRx,width={W},height={H},framerate=0/1 ! {CONVERT_TO_SINK}"
        ))
        .unwrap()
        .downcast::<gstreamer::Pipeline>()
        .unwrap();
        let src = pipeline.by_name("src").unwrap().downcast::<gstreamer_app::AppSrc>().unwrap();
        if guarded {
            drop_frames_without_a_picture(&src.static_pad("src").unwrap());
        }
        let sink = pipeline.by_name("sink").unwrap().downcast::<gstreamer_app::AppSink>().unwrap();
        // EOS reaches the appsink's pad after every buffer before it, so
        // the sink then holds whatever the last delivered buffer was. (The
        // bus EOS waits for the held sample to be pulled.)
        let (eos_tx, eos_rx) = std::sync::mpsc::channel();
        sink.static_pad("sink").unwrap().add_probe(gstreamer::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(gstreamer::PadProbeData::Event(event)) = &info.data {
                if event.type_() == gstreamer::EventType::Eos {
                    let _ = eos_tx.send(());
                }
            }
            gstreamer::PadProbeReturn::Ok
        });
        pipeline.set_state(gstreamer::State::Playing).unwrap();
        let n = buffers.len();
        for buffer in buffers {
            src.push_buffer(buffer).unwrap();
        }
        src.end_of_stream().unwrap();
        eos_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{n} buffers never reached the sink"));
        let sample = sink.try_pull_sample(gstreamer::ClockTime::ZERO);
        pipeline.set_state(gstreamer::State::Null).unwrap();
        sample.map(|s| s.buffer().unwrap().map_readable().unwrap().to_vec())
    }

    /// The RGBA the capture should produce for `frame(_, seed)`.
    fn expected(seed: u8) -> Vec<u8> {
        frame(W as usize * 4, seed)
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0], 0xff])
            .collect()
    }

    #[test]
    fn unguarded_cursor_only_buffer_replaces_the_frame_with_garbage() {
        // The bug, reproduced: the empty buffer comes out of videoconvert
        // as a full-size frame that was never written.
        let out = capture(vec![with_meta(frame(W as usize * 4, 7), W as usize * 4), cursor_only()], false)
            .expect("videoconvert passes the unwritten frame on");
        assert_eq!(out.len(), (W * H * 4) as usize);
        assert_ne!(out, expected(7));
    }

    #[test]
    fn cursor_only_buffers_never_replace_the_frame() {
        let out = capture(
            vec![cursor_only(), with_meta(frame(W as usize * 4, 7), W as usize * 4), cursor_only(), cursor_only()],
            true,
        );
        assert_eq!(out, Some(expected(7)));
    }

    #[test]
    fn only_cursor_updates_is_no_frame_at_all() {
        assert_eq!(capture(vec![cursor_only(), cursor_only()], true), None);
    }

    #[test]
    fn short_and_corrupted_buffers_are_dropped() {
        let stride = W as usize * 4;
        gstreamer::init().unwrap();
        let mut short = frame(stride, 9);
        short.truncate(stride * (H as usize - 1));
        let mut corrupted = with_meta(frame(stride, 9), stride);
        corrupted.get_mut().unwrap().set_flags(gstreamer::BufferFlags::CORRUPTED);
        let out = capture(
            vec![with_meta(frame(stride, 3), stride), with_meta(short.clone(), stride), gstreamer::Buffer::from_mut_slice(short), corrupted],
            true,
        );
        assert_eq!(out, Some(expected(3)));
    }

    #[test]
    fn padded_rows_and_crop_meta_still_give_the_whole_frame() {
        // Producers pad rows (mutter aligns its stride) and may attach a
        // crop region; neither may change what comes out.
        let stride = W as usize * 4 + 64;
        let mut buffer = with_meta(frame(stride, 5), stride);
        gstreamer_video::VideoCropMeta::add(buffer.get_mut().unwrap(), (16, 0, W, H));
        assert_eq!(capture(vec![buffer], true), Some(expected(5)));
    }

    #[test]
    fn capture_error_preserves_bus_details() {
        gstreamer::init().unwrap();
        let pipeline = gstreamer::Pipeline::new();
        pipeline.post_message(
            gstreamer::message::Error::builder(gstreamer::ResourceError::OpenRead, "Permission denied")
                .src(&pipeline)
                .debug("portal remote rejected the connection")
                .build(),
        ).unwrap();
        let error = pipeline_error(&pipeline, "Failed to set playing", 42);
        assert!(error.contains("PipeWire node 42"), "{error}");
        assert!(error.contains("Permission denied"), "{error}");
        assert!(error.contains("portal remote rejected the connection"), "{error}");
    }
}

