#[cfg(target_os = "linux")]
pub fn capture_pipewire_node(
    node_id: u32,
    fd: std::os::fd::RawFd,
) -> Result<image::DynamicImage, String> {
    use gstreamer::prelude::*;

    gstreamer::init().map_err(|e| format!("Failed to init gstreamer: {}", e))?;

    let pipeline_str = format!(
        "pipewiresrc fd={} path={} ! videoconvert ! video/x-raw,format=RGBA ! appsink name=sink max-buffers=1 drop=true",
        fd, node_id
    );

    let pipeline = gstreamer::parse::launch(&pipeline_str)
        .map_err(|e| format!("Failed to create pipeline: {}", e))?
        .downcast::<gstreamer::Pipeline>()
        .map_err(|_| "Expected a pipeline".to_string())?;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| "Failed to get appsink".to_string())?
        .downcast::<gstreamer_app::AppSink>()
        .map_err(|_| "Not an appsink".to_string())?;

    pipeline
        .set_state(gstreamer::State::Playing)
        .map_err(|e| format!("Failed to set playing: {}", e))?;

    // Wait up to 2 seconds for a frame.
    let sample = appsink.try_pull_sample(gstreamer::ClockTime::from_seconds(2));

    // Tear down BEFORE bailing on a timeout: an early return here used to
    // drop the pipeline still PLAYING, stranding its streaming threads and
    // PipeWire stream — a leak per failed capture.
    let stopped = pipeline.set_state(gstreamer::State::Null);

    let sample = sample.ok_or_else(|| "Failed to pull sample within timeout".to_string())?;
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
