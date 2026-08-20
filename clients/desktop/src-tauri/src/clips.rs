//! Per-minute clip recording: hardware H.264 encoding of capture-loop
//! frames into MP4 clips — the `format=mp4` upload payload for sessions
//! with clips enabled.
//!
//! One `ClipRecorder` lives per upload interval: the capture loop pushes a
//! frame every `frameIntervalMs` (server-authoritative, 10s = 6/min), and
//! at the upload tick `finish()` produces the MP4 bytes. Encoding is done
//! by the OS hardware encoder on every platform — no bundled codecs:
//!
//! - macOS:   AVAssetWriter (VideoToolbox underneath), muxes MP4 itself
//! - Windows: Media Foundation sink writer (hardware MFT when available)
//! - Linux:   GStreamer (already a dependency for PipeWire capture)
//!
//! Every error is recoverable by design: the capture loop falls back to
//! the legacy one-JPEG-per-minute upload for that interval, so a broken
//! encoder degrades smoothness, never the recording.

use image::DynamicImage;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-frame byte budget — the quality dial. Mirrors the shared
/// CLIP_FRAME_BYTE_BUDGET; keep the two in step.
///
/// ~400 KB buys a JPEG-q85-class keyframe at 1080p, the bar the legacy
/// single-screenshot pipeline set. 133k/400k-per-second equivalents were
/// tried first and produced visibly soft H.264.
pub const CLIP_FRAME_BYTE_BUDGET: u64 = 400_000;

/// Bitrate (bits/second of MEDIA time) that lands CLIP_FRAME_BYTE_BUDGET per
/// frame at the given cadence. Mirrors `nativeClipBitsPerSecond` in
/// @lookout/shared.
///
/// This must scale with the cadence. These encoders get each frame's real
/// presentation timestamp, so their bitrate is denominated per second of
/// media time — the same number buys 2.5x the bytes per frame when frames
/// sit 10s apart instead of 4s. Left fixed, a slower cadence would silently
/// inflate every clip toward the server's 8 MB limit while a faster one
/// would starve it. (Browsers work differently and need a much larger
/// figure for the same quality — see CLIP_WEB_VIDEO_BITS_PER_SECOND. The
/// two are not comparable.) VBR ceiling, not a floor: static screens
/// undershoot heavily.
pub fn clip_bits_per_second(frame_interval_ms: u64) -> u32 {
    let interval_ms = frame_interval_ms.max(1);
    ((CLIP_FRAME_BYTE_BUDGET * 8 * 1000) / interval_ms).min(u32::MAX as u64) as u32
}

/// A finished clip ready for upload.
pub struct FinishedClip {
    pub mp4: Vec<u8>,
    pub frame_count: u32,
    pub width: u32,
    pub height: u32,
}

/// Unique-enough temp path for an in-progress clip container.
fn clip_temp_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "lookout-clip-{}-{}.mp4",
        std::process::id(),
        n
    ))
}

/// Records one clip: accepts RGBA frames, hands back MP4 bytes.
pub struct ClipRecorder {
    encoder: platform::Encoder,
    path: PathBuf,
    width: u32,
    height: u32,
    frame_count: u32,
    frame_interval_ms: u64,
}

impl ClipRecorder {
    /// Start a new clip sized to the given frame dimensions (rounded down
    /// to even — H.264 4:2:0 needs even dims). Later frames that arrive at
    /// a different size (display change mid-clip) are scaled to fit.
    pub fn new(width: u32, height: u32, frame_interval_ms: u64) -> Result<Self, String> {
        let width = (width & !1).max(2);
        let height = (height & !1).max(2);
        let path = clip_temp_path();
        // A failed init can still have created the container — the GStreamer
        // path opens `filesink` as soon as the pipeline goes Playing, and there
        // is no ClipRecorder yet whose finish()/discard() would remove it. That
        // matters more now the loop retries init: a machine with a broken
        // encoder would drip an orphan into the temp dir per attempt.
        let encoder = match platform::Encoder::new(
            &path,
            width,
            height,
            clip_bits_per_second(frame_interval_ms),
            frame_interval_ms,
        ) {
            Ok(e) => e,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        Ok(Self {
            encoder,
            path,
            width,
            height,
            frame_count: 0,
            frame_interval_ms,
        })
    }

    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// The in-progress container's path, so tests can assert it was cleaned up
    /// without scanning the shared OS temp directory.
    #[cfg(test)]
    fn temp_path(&self) -> std::path::PathBuf {
        self.path.clone()
    }

    /// Append one captured frame. Presentation time advances by the clip
    /// frame interval per frame, so the clip plays back in real time.
    pub fn push_frame(&mut self, frame: &DynamicImage) -> Result<(), String> {
        // Normalize to the encoder's fixed dimensions. Cheap no-op clone
        // path when dimensions already match (the common case).
        let bgra = frame_to_bgra(frame, self.width, self.height);
        let pts_ms = self.frame_count as u64 * self.frame_interval_ms;
        self.encoder
            .append_bgra_frame(&bgra, self.width, self.height, pts_ms)?;
        self.frame_count += 1;
        Ok(())
    }

    /// Finalize the container and return its bytes. Consumes the recorder;
    /// the temp file is always removed.
    pub fn finish(self) -> Result<FinishedClip, String> {
        // Hold the final duration so the last frame isn't zero-length.
        let duration_ms = self.frame_count as u64 * self.frame_interval_ms;
        let result = self.encoder.finish(duration_ms);
        let bytes = result.and_then(|()| {
            std::fs::read(&self.path).map_err(|e| format!("failed to read clip file: {e}"))
        });
        let _ = std::fs::remove_file(&self.path);
        let mp4 = bytes?;
        if self.frame_count == 0 || mp4.is_empty() {
            return Err("clip has no frames".into());
        }
        Ok(FinishedClip {
            mp4,
            frame_count: self.frame_count,
            width: self.width,
            height: self.height,
        })
    }

    /// Abort and clean up without producing a clip (pause/stop mid-minute).
    pub fn discard(self) {
        let _ = self.encoder.finish(0);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Convert a frame to tightly-packed BGRA at exactly (width, height),
/// scaling (aspect-preserving pillarbox on black) when dimensions differ.
fn frame_to_bgra(frame: &DynamicImage, width: u32, height: u32) -> Vec<u8> {
    // Common case: dimensions match and the frame is already RGBA8
    // (captures always are) — swizzle straight from the borrowed buffer
    // into the output, one copy total.
    if frame.width() == width && frame.height() == height {
        if let Some(rgba) = frame.as_rgba8() {
            let src = rgba.as_raw();
            let mut bgra = Vec::with_capacity(src.len());
            for px in src.chunks_exact(4) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            return bgra;
        }
    }

    // Rare path: mid-clip display change — normalize to the clip's fixed
    // dimensions with a pillarboxed canvas.
    let rgba = if frame.width() == width && frame.height() == height {
        frame.to_rgba8()
    } else {
        let scaled = frame.resize(width, height, image::imageops::FilterType::Triangle);
        let mut canvas = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 255]));
        let x = (width - scaled.width()) / 2;
        let y = (height - scaled.height()) / 2;
        image::imageops::overlay(&mut canvas, &scaled.to_rgba8(), x as i64, y as i64);
        canvas
    };
    let mut bgra = rgba.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    bgra
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VA rate-control setup must be a graceful no-op on elements that
    /// lack the properties it probes for — every set is introspection-
    /// guarded, and a wrong guess panics inside GObject. Exercised against
    /// non-VA elements (no VA hardware on CI): identity has none of the
    /// properties; x264enc has enum props but no "rate-control".
    #[cfg(target_os = "linux")]
    #[test]
    fn va_rate_control_config_is_safe_on_foreign_elements() {
        use gstreamer as gst;
        gst::init().expect("gst init");
        for name in ["identity", "x264enc", "openh264enc"] {
            let Ok(elem) = gst::ElementFactory::make(name).build() else {
                continue; // element not installed here — nothing to probe
            };
            platform::configure_va_rate_control(&elem, 320, 10_000);
        }
    }

    /// Full round-trip through the real OS encoder: synthetic frames in,
    /// container bytes out, then ffprobe (when installed) verifies the
    /// frame count and that the stream decodes.
    #[test]
    fn encodes_frames_into_playable_mp4() {
        let mut recorder = ClipRecorder::new(640, 360, 3000).expect("encoder init");
        for i in 0u32..5 {
            let mut img =
                image::RgbaImage::from_pixel(640, 360, image::Rgba([20, 20, 40, 255]));
            // Moving block so inter frames aren't empty.
            for x in 0..80 {
                for y in 0..80 {
                    img.put_pixel(x + i * 60, y + 40, image::Rgba([220, 90, 40, 255]));
                }
            }
            recorder
                .push_frame(&DynamicImage::ImageRgba8(img))
                .expect("push frame");
        }
        let clip = recorder.finish().expect("finish clip");

        assert_eq!(clip.frame_count, 5);
        assert_eq!(clip.width, 640);
        assert_eq!(clip.height, 360);
        assert!(clip.mp4.len() > 500, "suspiciously small mp4: {}B", clip.mp4.len());
        assert_eq!(&clip.mp4[4..8], b"ftyp", "not an MP4 container");

        // Deep verification when ffprobe is on the machine (dev boxes, CI).
        let probe = std::process::Command::new("ffprobe").arg("-version").output();
        if probe.is_ok() {
            let path = clip_temp_path();
            std::fs::write(&path, &clip.mp4).unwrap();
            let out = std::process::Command::new("ffprobe")
                .args([
                    "-v", "error",
                    "-count_packets",
                    "-select_streams", "v:0",
                    "-show_entries", "stream=nb_read_packets,codec_name",
                    "-of", "csv=p=0",
                ])
                .arg(&path)
                .output()
                .expect("ffprobe run");
            let _ = std::fs::remove_file(&path);
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("h264"),
                "expected h264 stream, got: {stdout}"
            );
            assert!(
                stdout.trim().ends_with(",5"),
                "expected 5 packets, got: {stdout}"
            );

            // GOP shape: exactly ONE keyframe. Frames are seconds apart in
            // media time, so a default max-keyframe-interval-duration makes
            // the encoder emit ALL-keyframe clips — which rations the
            // bitrate budget across 20 I-frames and produces uniformly soft
            // output (~20KB/frame). One IDR + cheap P-frames is the shape
            // that lets the keyframe stay crisp.
            let path2 = clip_temp_path();
            std::fs::write(&path2, &clip.mp4).unwrap();
            let frames_out = std::process::Command::new("ffprobe")
                .args([
                    "-v", "error",
                    "-select_streams", "v:0",
                    "-show_entries", "frame=key_frame",
                    "-of", "csv=p=0",
                ])
                .arg(&path2)
                .output()
                .expect("ffprobe frames run");
            let _ = std::fs::remove_file(&path2);
            let keyframes = String::from_utf8_lossy(&frames_out.stdout)
                .lines()
                .filter(|l| l.trim_end_matches(',') == "1")
                .count();
            assert_eq!(
                keyframes, 1,
                "expected exactly 1 keyframe in the clip, got {keyframes}"
            );
        } else {
            eprintln!("ffprobe not found — container-level checks only");
        }
    }

    /// The bitrate must buy the same bytes per FRAME at any cadence — that
    /// invariant is the whole reason it's derived instead of hardcoded.
    #[test]
    fn bitrate_holds_per_frame_quality_across_cadences() {
        for interval_ms in [2_000u64, 4_000, 12_000, 30_000] {
            let bytes_per_frame =
                (clip_bits_per_second(interval_ms) as u64 * interval_ms) / (8 * 1000);
            let drift = bytes_per_frame.abs_diff(CLIP_FRAME_BYTE_BUDGET);
            assert!(
                drift <= CLIP_FRAME_BYTE_BUDGET / 100,
                "at {interval_ms}ms a frame gets {bytes_per_frame}B, want ~{CLIP_FRAME_BYTE_BUDGET}B"
            );
        }

        // The 4s cadence is the one that was measured and tuned by hand at
        // 800 kbps. Reproducing it exactly is what makes the formula
        // trustworthy at every other cadence.
        assert_eq!(clip_bits_per_second(4_000), 800_000);

        // And a whole clip has to stay clear of the server's 8 MB limit at
        // the cadence actually shipping.
        let frames_per_clip = 60_000 / 10_000;
        assert!(
            frames_per_clip * CLIP_FRAME_BYTE_BUDGET < 8 * 1024 * 1024,
            "nominal clip exceeds MAX_CLIP_BYTES"
        );
    }

    use crate::test_support::rss_kb;

    /// Leak check for the encode cycle: many recorders, many frames each, all
    /// finished properly. The capture loop runs one of these per minute for as
    /// long as a session lasts (up to 12 hours = 720 cycles), so a per-cycle
    /// leak in the CVPixelBuffer / AVAssetWriter handling would accumulate into
    /// something a user notices.
    ///
    /// Ignored by default: it's a few seconds of real encoding. Run with
    /// `cargo test --release -- --ignored leak`.
    #[test]
    #[ignore = "stress test — run explicitly"]
    fn encode_cycle_does_not_leak() {
        let frame = |i: u32| {
            let mut img = image::RgbaImage::from_pixel(1280, 720, image::Rgba([30, 30, 40, 255]));
            for x in 0..120u32 {
                for y in 0..120u32 {
                    img.put_pixel((x + i * 37) % 1280, (y + i * 11) % 720,
                                  image::Rgba([200, 80, 40, 255]));
                }
            }
            DynamicImage::ImageRgba8(img)
        };

        // Warm up so one-time allocations (framework init, codec tables) don't
        // read as growth.
        for _ in 0..3 {
            let mut r = ClipRecorder::new(1280, 720, 10_000).expect("init");
            for i in 0..7 { r.push_frame(&frame(i)).expect("push"); }
            r.finish().expect("finish");
        }

        let before = rss_kb();
        let cycles: u32 = std::env::var("LEAK_CYCLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);
        for c in 0..cycles {
            let mut r = ClipRecorder::new(1280, 720, 10_000).expect("init");
            for i in 0..7 { r.push_frame(&frame(c * 7 + i)).expect("push"); }
            let clip = r.finish().expect("finish");
            assert!(!clip.mp4.is_empty());
        }
        let after = rss_kb();

        let growth = after.saturating_sub(before);
        eprintln!(
            "RSS {before} -> {after} KB over {cycles} encode cycles ({} KB/cycle)",
            growth / u64::from(cycles)
        );
        // A genuine per-cycle leak of a 1280x720 BGRA buffer would be ~3.6MB
        // each, i.e. ~144MB over this run. Allow generous headroom for
        // allocator behaviour and VideoToolbox's own caches while still
        // catching anything of that order.
        // Scale the budget with the run so a deeper LEAK_CYCLES run stays a
        // real assertion rather than a formality.
        let budget = 20_000 + 500 * u64::from(cycles);
        assert!(
            growth < budget,
            "RSS grew {growth} KB over {cycles} cycles (budget {budget}) — suspected leak"
        );
    }

    /// Discarding a recorder mid-clip must release just as cleanly as
    /// finishing one. This is the pause/stop path, and on Windows it is also
    /// the path that has to balance MFStartup.
    #[test]
    #[ignore = "stress test — run explicitly"]
    fn discard_path_does_not_leak() {
        let img = || {
            DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                1280, 720, image::Rgba([10, 20, 30, 255]),
            ))
        };
        for _ in 0..3 {
            let mut r = ClipRecorder::new(1280, 720, 10_000).expect("init");
            r.push_frame(&img()).expect("push");
            r.discard();
        }
        let before = rss_kb();
        let cycles: u32 = std::env::var("LEAK_CYCLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(40);
        for _ in 0..cycles {
            let mut r = ClipRecorder::new(1280, 720, 10_000).expect("init");
            for _ in 0..4 { r.push_frame(&img()).expect("push"); }
            r.discard();
        }
        let growth = rss_kb().saturating_sub(before);
        eprintln!("RSS growth over {cycles} discard cycles: {growth} KB");
        let budget = 20_000 + 500 * u64::from(cycles);
        assert!(
            growth < budget,
            "RSS grew {growth} KB over {cycles} cycles (budget {budget}) — suspected leak on discard"
        );
    }

    /// Encode cost at real capture resolution. The capture loop does this on a
    /// tokio worker while the user works, so the number that matters is CPU per
    /// captured frame — at 6 frames/min a millisecond here is nothing, but a
    /// regression into hundreds would be felt on an old laptop.
    ///
    /// Run with `cargo test --release -- --ignored perf --nocapture`.
    #[test]
    #[ignore = "benchmark — run explicitly"]
    fn encode_cost_at_1080p() {
        let frame = |i: u32| {
            // Dense detail so the encoder can't cheat: this is the worst case,
            // a screen full of text and edges.
            let mut img = image::RgbaImage::new(1920, 1080);
            for (x, y, px) in img.enumerate_pixels_mut() {
                let v = ((x * 7 + y * 13 + i * 29) % 256) as u8;
                *px = image::Rgba([v, v.wrapping_mul(3), v.wrapping_add(90), 255]);
            }
            DynamicImage::ImageRgba8(img)
        };
        let frames: Vec<_> = (0..7).map(frame).collect();

        // Warm up the codec.
        {
            let mut r = ClipRecorder::new(1920, 1080, 10_000).expect("init");
            for f in &frames { r.push_frame(f).expect("push"); }
            r.finish().expect("finish");
        }

        const CLIPS: u32 = 10;
        let t0 = std::time::Instant::now();
        let mut bytes = 0usize;
        for _ in 0..CLIPS {
            let mut r = ClipRecorder::new(1920, 1080, 10_000).expect("init");
            for f in &frames { r.push_frame(f).expect("push"); }
            bytes += r.finish().expect("finish").mp4.len();
        }
        let per_clip = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(CLIPS);
        let per_frame = per_clip / frames.len() as f64;
        eprintln!(
            "1080p worst-case: {per_clip:.1} ms/clip, {per_frame:.1} ms/frame, \
{} KB/clip avg",
            bytes / CLIPS as usize / 1024
        );

        // One clip a minute: even 2s/clip would be 3% of a core. This ceiling
        // is loose on purpose — it exists to catch an order-of-magnitude
        // regression, not to police jitter on a shared CI box.
        assert!(per_clip < 2_000.0, "encode cost regressed: {per_clip:.0} ms/clip");
    }

    /// Temp files must not accumulate. Each clip writes a container to the OS
    /// temp dir and must remove it on every exit path; a session leaking one a
    /// minute would fill a small disk.
    ///
    /// Asserts on each recorder's OWN path rather than scanning the temp
    /// directory: that directory is shared by every test in the process, so a
    /// count-based check reports another test's in-flight file as a leak. (It
    /// did exactly that in CI.)
    #[test]
    fn clip_temp_files_are_always_removed() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            320, 240, image::Rgba([1, 2, 3, 255]),
        ));

        // finish(): the happy path.
        let mut r = ClipRecorder::new(320, 240, 10_000).expect("init");
        let finished = r.temp_path();
        r.push_frame(&img).expect("push");
        r.finish().expect("finish");
        assert!(!finished.exists(), "finish() left {finished:?}");

        // discard(): pause/stop mid-clip.
        let mut r = ClipRecorder::new(320, 240, 10_000).expect("init");
        let discarded = r.temp_path();
        r.push_frame(&img).expect("push");
        r.discard();
        assert!(!discarded.exists(), "discard() left {discarded:?}");

        // finish() on a clip with no frames: returns Err, and must STILL clean
        // up. This is the path a paused-immediately session takes.
        let r = ClipRecorder::new(320, 240, 10_000).expect("init");
        let errored = r.temp_path();
        assert!(r.finish().is_err());
        assert!(!errored.exists(), "failed finish() left {errored:?}");
    }

    /// A recorder with zero frames must fail, not produce an empty clip.
    #[test]
    fn empty_clip_errors() {
        let recorder = ClipRecorder::new(640, 360, 3000).expect("encoder init");
        assert!(recorder.finish().is_err());
    }

    /// Mid-clip resolution changes are normalized to the clip's dimensions.
    #[test]
    fn resized_frames_are_normalized() {
        let mut recorder = ClipRecorder::new(640, 360, 3000).expect("encoder init");
        let small = image::RgbaImage::from_pixel(320, 200, image::Rgba([255, 0, 0, 255]));
        recorder
            .push_frame(&DynamicImage::ImageRgba8(small))
            .expect("push mismatched frame");
        let big = image::RgbaImage::from_pixel(1920, 1080, image::Rgba([0, 255, 0, 255]));
        recorder
            .push_frame(&DynamicImage::ImageRgba8(big))
            .expect("push mismatched frame");
        let clip = recorder.finish().expect("finish clip");
        assert_eq!(clip.frame_count, 2);
        assert_eq!((clip.width, clip.height), (640, 360));
    }
}

// ── macOS: AVAssetWriter (VideoToolbox) ─────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2_av_foundation::{
        AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
        AVAssetWriterStatus, AVFileTypeMPEG4, AVMediaTypeVideo,
        AVVideoAllowFrameReorderingKey, AVVideoAverageBitRateKey, AVVideoCodecKey,
        AVVideoCodecTypeH264, AVVideoCompressionPropertiesKey,
        AVVideoExpectedSourceFrameRateKey, AVVideoHeightKey,
        AVVideoMaxKeyFrameIntervalKey, AVVideoWidthKey,
    };
    use objc2_core_media::CMTime;
    use objc2_core_video::{
        kCVPixelFormatType_32BGRA, CVPixelBuffer, CVPixelBufferCreate,
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use objc2_foundation::{NSMutableDictionary, NSNumber, NSString, NSURL};

    fn ms_time(ms: u64) -> CMTime {
        unsafe { CMTime::new(ms as i64, 1000) }
    }

    pub struct Encoder {
        writer: Retained<AVAssetWriter>,
        input: Retained<AVAssetWriterInput>,
        adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
        started: bool,
    }

    // AVAssetWriter and friends are documented thread-safe for this usage
    // pattern (single writer thread); the recorder is driven from one loop.
    unsafe impl Send for Encoder {}

    impl Encoder {
        pub fn new(
            path: &Path,
            width: u32,
            height: u32,
            bitrate: u32,
            frame_interval_ms: u64,
        ) -> Result<Self, String> {
            // Every Cocoa object built below is autoreleased, and the capture
            // loop calls this from a tokio worker thread — which, unlike the
            // main run loop, never drains a pool. Without an explicit one the
            // settings dictionaries and the writer itself accumulate for the
            // life of the process: measured ~4.9 MB per clip, i.e. GBs over a
            // long session.
            autoreleasepool(|_| unsafe {
                let url = NSURL::fileURLWithPath(&NSString::from_str(
                    path.to_str().ok_or("non-utf8 temp path")?,
                ));

                // Weak-linked framework constants come through as Options.
                let file_type = AVFileTypeMPEG4.ok_or("AVFileTypeMPEG4 unavailable")?;
                let media_video = AVMediaTypeVideo.ok_or("AVMediaTypeVideo unavailable")?;
                let codec_h264 = AVVideoCodecTypeH264.ok_or("AVVideoCodecTypeH264 unavailable")?;
                let key_codec = AVVideoCodecKey.ok_or("AVVideoCodecKey unavailable")?;
                let key_width = AVVideoWidthKey.ok_or("AVVideoWidthKey unavailable")?;
                let key_height = AVVideoHeightKey.ok_or("AVVideoHeightKey unavailable")?;
                let key_compression =
                    AVVideoCompressionPropertiesKey.ok_or("AVVideoCompressionPropertiesKey unavailable")?;
                let key_bitrate =
                    AVVideoAverageBitRateKey.ok_or("AVVideoAverageBitRateKey unavailable")?;
                let key_max_kf =
                    AVVideoMaxKeyFrameIntervalKey.ok_or("AVVideoMaxKeyFrameIntervalKey unavailable")?;
                let key_reorder = AVVideoAllowFrameReorderingKey
                    .ok_or("AVVideoAllowFrameReorderingKey unavailable")?;
                let key_expected_fps = AVVideoExpectedSourceFrameRateKey
                    .ok_or("AVVideoExpectedSourceFrameRateKey unavailable")?;

                let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
                    .map_err(|e| format!("AVAssetWriter init failed: {e}"))?;

                // {AVVideoCodecKey: h264, AVVideoWidthKey, AVVideoHeightKey,
                //  AVVideoCompressionPropertiesKey: {AVVideoAverageBitRateKey}}
                let compression: Retained<NSMutableDictionary<NSString, AnyObject>> =
                    NSMutableDictionary::new();
                compression.setObject_forKey(
                    NSNumber::new_u32(bitrate).as_ref(),
                    ProtocolObject::from_ref(key_bitrate),
                );
                // One IDR per clip: frames sit seconds apart in media time,
                // so any keyframe-interval default expressed in seconds
                // would turn the whole clip into rationed I-frames —
                // uniformly soft. One crisp keyframe + cheap P-frames is
                // the intended shape.
                compression.setObject_forKey(
                    NSNumber::new_u32(1200).as_ref(),
                    ProtocolObject::from_ref(key_max_kf),
                );
                // No B-frames: pointless at this cadence and they add
                // reorder latency/complexity.
                compression.setObject_forKey(
                    NSNumber::new_bool(false).as_ref(),
                    ProtocolObject::from_ref(key_reorder),
                );
                // Rate-control hint: the source is ~1 frame/interval, not
                // 30fps — lets the encoder budget bits per frame correctly.
                // The key is integer fps, so any interval at or above one
                // second floors to 1; that's the honest answer and matches
                // the measured behaviour (VideoToolbox budgets against the
                // real presentation timestamps we hand it, which is why
                // `bitrate` is derived from the cadence rather than fixed).
                let expected_fps = (1000 / frame_interval_ms.max(1)).max(1) as u32;
                compression.setObject_forKey(
                    NSNumber::new_u32(expected_fps).as_ref(),
                    ProtocolObject::from_ref(key_expected_fps),
                );

                let settings: Retained<NSMutableDictionary<NSString, AnyObject>> =
                    NSMutableDictionary::new();
                settings.setObject_forKey(
                    codec_h264.as_ref(),
                    ProtocolObject::from_ref(key_codec),
                );
                settings.setObject_forKey(
                    NSNumber::new_u32(width).as_ref(),
                    ProtocolObject::from_ref(key_width),
                );
                settings.setObject_forKey(
                    NSNumber::new_u32(height).as_ref(),
                    ProtocolObject::from_ref(key_height),
                );
                settings.setObject_forKey(
                    compression.as_ref(),
                    ProtocolObject::from_ref(key_compression),
                );

                let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                    media_video,
                    Some(&*settings),
                );
                // Live source: encode as frames arrive instead of buffering.
                input.setExpectsMediaDataInRealTime(true);

                if !writer.canAddInput(&input) {
                    return Err("AVAssetWriter rejected video input".into());
                }
                writer.addInput(&input);

                let adaptor = AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(&input, None);

                Ok(Self {
                    writer,
                    input,
                    adaptor,
                    started: false,
                })
            })
        }

        pub fn append_bgra_frame(
            &mut self,
            bgra: &[u8],
            width: u32,
            height: u32,
            pts_ms: u64,
        ) -> Result<(), String> {
            // Per-frame pool: this is the hottest of the three entry points,
            // and CVPixelBufferCreate's buffer is only one of several objects
            // the frameworks autorelease on the way through.
            autoreleasepool(|_| unsafe {
                if !self.started {
                    if !self.writer.startWriting() {
                        return Err(format!(
                            "AVAssetWriter startWriting failed: {:?}",
                            self.writer.error()
                        ));
                    }
                    self.writer.startSessionAtSourceTime(ms_time(0));
                    self.started = true;
                }

                // Wait (bounded) for the encoder to drain. With realtime
                // input and 3s between frames this is virtually always
                // immediate.
                let mut waited_ms = 0u64;
                while !self.input.isReadyForMoreMediaData() {
                    if waited_ms > 2_000 {
                        return Err("encoder not ready after 2s".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    waited_ms += 10;
                }

                // BGRA CVPixelBuffer, row-by-row copy (CV row stride may
                // exceed width*4).
                let mut pb_out: *mut CVPixelBuffer = std::ptr::null_mut();
                let ret = CVPixelBufferCreate(
                    None,
                    width as usize,
                    height as usize,
                    kCVPixelFormatType_32BGRA,
                    None,
                    NonNull::from(&mut pb_out),
                );
                if ret != 0 || pb_out.is_null() {
                    return Err(format!("CVPixelBufferCreate failed: {ret}"));
                }
                // Take ownership so the buffer is released on all paths.
                let pb = Retained::from_raw(pb_out).ok_or("null pixel buffer")?;

                CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::empty());
                let base = CVPixelBufferGetBaseAddress(&pb) as *mut u8;
                let dst_stride = CVPixelBufferGetBytesPerRow(&pb);
                let src_stride = (width * 4) as usize;
                for row in 0..height as usize {
                    std::ptr::copy_nonoverlapping(
                        bgra.as_ptr().add(row * src_stride),
                        base.add(row * dst_stride),
                        src_stride,
                    );
                }
                CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::empty());

                if !self
                    .adaptor
                    .appendPixelBuffer_withPresentationTime(&pb, ms_time(pts_ms))
                {
                    return Err(format!(
                        "appendPixelBuffer failed: {:?}",
                        self.writer.error()
                    ));
                }
                Ok(())
            })
        }

        pub fn finish(self, duration_ms: u64) -> Result<(), String> {
            autoreleasepool(|_| unsafe {
                if !self.started {
                    // Nothing was written; cancel to avoid a zero-byte file
                    // error from finishWriting.
                    self.writer.cancelWriting();
                    return Err("no frames written".into());
                }
                self.input.markAsFinished();
                self.writer.endSessionAtSourceTime(ms_time(duration_ms));

                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let block = RcBlock::new(move || {
                    let _ = tx.send(());
                });
                self.writer.finishWritingWithCompletionHandler(&block);
                rx.recv_timeout(std::time::Duration::from_secs(15))
                    .map_err(|_| "finishWriting timed out".to_string())?;

                if self.writer.status() != AVAssetWriterStatus::Completed {
                    return Err(format!(
                        "AVAssetWriter finished with status {:?}: {:?}",
                        self.writer.status(),
                        self.writer.error()
                    ));
                }
                Ok(())
            })
        }
    }
}

// ── Windows: Media Foundation sink writer ───────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use std::path::Path;

    use windows::core::HSTRING;
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaType, IMFSample, IMFSinkWriter, MFCreateMediaType, MFCreateMemoryBuffer,
        MFCreateSample, MFCreateSinkWriterFromURL, MFShutdown, MFStartup, MFSTARTUP_FULL,
        MFVideoFormat_H264, MFVideoFormat_RGB32, MFVideoInterlace_Progressive,
        MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
        MF_MT_MAJOR_TYPE, MF_MT_MAX_KEYFRAME_SPACING, MF_MT_SUBTYPE, MF_VERSION,
        MFMediaType_Video,
    };
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    /// Media Foundation needs COM initialized on the calling thread, and the
    /// capture loop's calls land on tokio worker threads that never did so.
    /// Refcounted and idempotent per thread; S_FALSE (already initialized)
    /// and RPC_E_CHANGED_MODE (thread is STA) are both fine for the sink
    /// writer, so the result is deliberately ignored. Called at the top of
    /// every encoder entry point because consecutive async calls may run on
    /// different pool threads.
    fn ensure_com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    /// Pack two u32s into the u64 layout MF uses for SIZE/RATIO attributes.
    fn pack_u64(hi: u32, lo: u32) -> u64 {
        ((hi as u64) << 32) | lo as u64
    }

    /// Balances one `MFStartup` on drop.
    ///
    /// MFStartup/MFShutdown are refcounted, and encoder construction has a
    /// dozen fallible steps after the startup call. Every one of those early
    /// returns used to leak a refcount — invisible on a healthy machine
    /// (init succeeds, finish() balances it), unbounded on one whose encoder
    /// always fails, because the capture loop retries init on every frame
    /// for the length of the session. `std::mem::forget` on the success path
    /// hands the refcount to the encoder instead.
    struct MfStartupGuard;

    impl Drop for MfStartupGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
            }
        }
    }

    pub struct Encoder {
        writer: IMFSinkWriter,
        stream_index: u32,
        width: u32,
        height: u32,
        frame_interval_ms: u64,
    }

    // Single-threaded use from the capture loop.
    unsafe impl Send for Encoder {}

    impl Encoder {
        /// Two attempts, in order of correctness:
        ///
        ///  1. Declare the REAL sub-1fps cadence as a ratio (1000 :
        ///     interval_ms) and hand over the per-media-second bitrate. Both
        ///     readings of `MF_MT_AVG_BITRATE` — bits per second of media
        ///     time, or bits per declared frame — then agree on the same
        ///     ~CLIP_FRAME_BYTE_BUDGET per frame.
        ///  2. If the MFT refuses that media type (some hardware encoders
        ///     reject fractional frame rates outright), fall back to the
        ///     1 fps hint this code shipped with, and scale the bitrate to
        ///     match so the per-frame budget is preserved rather than
        ///     silently divided by the interval.
        ///
        /// Only if BOTH fail does the caller fall back to a JPEG for the
        /// interval. Windows per-frame output has not been measured on real
        /// hardware the way the macOS path has; if clips come back soft or
        /// oversize, this pair of attempts is where to look first.
        pub fn new(
            path: &Path,
            width: u32,
            height: u32,
            bitrate: u32,
            frame_interval_ms: u64,
        ) -> Result<Self, String> {
            let interval_ms = frame_interval_ms.max(1);
            match Self::try_new(path, width, height, bitrate, interval_ms, 1000, interval_ms as u32)
            {
                Ok(enc) => Ok(enc),
                Err(real_cadence_err) => {
                    let per_frame_bitrate = ((bitrate as u64 * interval_ms) / 1000)
                        .min(u32::MAX as u64) as u32;
                    eprintln!(
                        "[clips] Media Foundation rejected the {interval_ms}ms cadence \
                         ({real_cadence_err}) — retrying at a 1fps hint"
                    );
                    Self::try_new(path, width, height, per_frame_bitrate, interval_ms, 1, 1)
                }
            }
        }

        fn try_new(
            path: &Path,
            width: u32,
            height: u32,
            bitrate: u32,
            frame_interval_ms: u64,
            frame_rate_num: u32,
            frame_rate_den: u32,
        ) -> Result<Self, String> {
            ensure_com();
            unsafe {
                // Idempotent per-process init (returns S_OK on repeat calls).
                MFStartup(MF_VERSION, MFSTARTUP_FULL)
                    .map_err(|e| format!("MFStartup failed: {e}"))?;

                // From here on every early return must balance that startup.
                // Without this the refcount leaked once per failed init —
                // and a machine whose encoder always fails attempts one per
                // frame, for the length of the session.
                let guard = MfStartupGuard;

                let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(
                    &HSTRING::from(path.to_string_lossy().as_ref()),
                    None,
                    None,
                )
                .map_err(|e| format!("MFCreateSinkWriterFromURL failed: {e}"))?;

                // Output: H.264 at the clip bitrate. Frame timing is also
                // carried per-sample; the rate attribute seeds the encoder's
                // rate control, so it and `bitrate` have to agree about what
                // a "second" means (see `new`).
                let out_type: IMFMediaType =
                    MFCreateMediaType().map_err(|e| format!("MFCreateMediaType: {e}"))?;
                out_type
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .map_err(|e| e.to_string())?;
                out_type
                    .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                    .map_err(|e| e.to_string())?;
                out_type
                    .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
                    .map_err(|e| e.to_string())?;
                out_type
                    .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                    .map_err(|e| e.to_string())?;
                out_type
                    .SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
                    .map_err(|e| e.to_string())?;
                out_type
                    .SetUINT64(
                        &MF_MT_FRAME_RATE,
                        pack_u64(frame_rate_num, frame_rate_den),
                    )
                    .map_err(|e| e.to_string())?;
                // One IDR per clip (see the macOS encoder for rationale).
                out_type
                    .SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, 10_000)
                    .map_err(|e| e.to_string())?;
                let stream_index = writer
                    .AddStream(&out_type)
                    .map_err(|e| format!("AddStream failed: {e}"))?;

                // Input: BGRA (MF calls it RGB32); the sink writer inserts
                // the color converter to the encoder's NV12 automatically.
                let in_type: IMFMediaType =
                    MFCreateMediaType().map_err(|e| format!("MFCreateMediaType: {e}"))?;
                in_type
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .map_err(|e| e.to_string())?;
                in_type
                    .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                    .map_err(|e| e.to_string())?;
                in_type
                    .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                    .map_err(|e| e.to_string())?;
                in_type
                    .SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
                    .map_err(|e| e.to_string())?;
                in_type
                    .SetUINT64(
                        &MF_MT_FRAME_RATE,
                        pack_u64(frame_rate_num, frame_rate_den),
                    )
                    .map_err(|e| e.to_string())?;
                writer
                    .SetInputMediaType(stream_index, &in_type, None)
                    .map_err(|e| format!("SetInputMediaType failed: {e}"))?;

                writer
                    .BeginWriting()
                    .map_err(|e| format!("BeginWriting failed: {e}"))?;

                // Success: ownership of the MFStartup refcount passes to the
                // encoder, which balances it in finish().
                std::mem::forget(guard);
                Ok(Self {
                    writer,
                    stream_index,
                    width,
                    height,
                    frame_interval_ms,
                })
            }
        }

        pub fn append_bgra_frame(
            &mut self,
            bgra: &[u8],
            width: u32,
            height: u32,
            pts_ms: u64,
        ) -> Result<(), String> {
            debug_assert_eq!((width, height), (self.width, self.height));
            ensure_com();
            unsafe {
                let byte_len = (width * height * 4) as u32;
                let buffer = MFCreateMemoryBuffer(byte_len)
                    .map_err(|e| format!("MFCreateMemoryBuffer: {e}"))?;

                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                buffer
                    .Lock(&mut data_ptr, None, None)
                    .map_err(|e| e.to_string())?;
                // MF RGB32 with positive stride is bottom-up; flip rows so
                // the frame is right side up.
                let stride = (width * 4) as usize;
                for row in 0..height as usize {
                    let src = bgra.as_ptr().add(row * stride);
                    let dst = data_ptr.add((height as usize - 1 - row) * stride);
                    std::ptr::copy_nonoverlapping(src, dst, stride);
                }
                buffer.Unlock().map_err(|e| e.to_string())?;
                buffer
                    .SetCurrentLength(byte_len)
                    .map_err(|e| e.to_string())?;

                let sample: IMFSample =
                    MFCreateSample().map_err(|e| format!("MFCreateSample: {e}"))?;
                sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
                // MF time units: 100ns.
                sample
                    .SetSampleTime((pts_ms * 10_000) as i64)
                    .map_err(|e| e.to_string())?;
                // Duration must be the REAL frame interval. This was pinned
                // at 3000ms — correct only for a cadence the app no longer
                // uses — which left every sample claiming a span that
                // disagreed with its own presentation timestamps.
                sample
                    .SetSampleDuration((self.frame_interval_ms * 10_000) as i64)
                    .map_err(|e| e.to_string())?;

                self.writer
                    .WriteSample(self.stream_index, &sample)
                    .map_err(|e| format!("WriteSample failed: {e}"))
            }
        }

        pub fn finish(self, _duration_ms: u64) -> Result<(), String> {
            ensure_com();
            // Destructured so the sink writer can be RELEASED before the
            // platform is shut down. Held as `self.writer`, it would outlive
            // the MFShutdown below and only drop at the end of this function.
            //
            // That ordering is the one Media Foundation forbids: every MF
            // object must be released while the platform is still up. A sink
            // writer released afterwards never runs its real teardown, so the
            // H.264 MFT it owns — and the RGB32 input samples and NV12
            // conversion buffers its allocators hold, tens of MB at capture
            // resolution — are orphaned rather than freed. The capture loop
            // builds one of these per upload tick, so that is a leak per
            // minute of recording, for the length of the session.
            //
            // (The init path in `try_new` gets this right by accident: locals
            // drop in reverse declaration order, so `writer` is released
            // before `MfStartupGuard`. Only this explicit call inverted it.)
            let Self { writer, .. } = self;
            unsafe {
                let result = writer
                    .Finalize()
                    .map_err(|e| format!("sink writer Finalize failed: {e}"));
                drop(writer);
                // Balance MFStartup from new() on BOTH paths — an early
                // return here would leak a startup refcount per failed clip.
                let _ = MFShutdown();
                result
            }
        }
    }
}

// ── Linux: GStreamer (already a dependency for PipeWire capture) ────

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;

    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app::AppSrc;

    /// Encoders in preference order: x264enc first, VA-API hardware only as
    /// a fallback, openh264enc last. Availability differs per distro/GPU;
    /// first that exists wins.
    ///
    /// Software-first is deliberate, not a leftover. This job is 6 frames a
    /// minute — hardware buys nothing — and hardware rate control actively
    /// hurts at sub-1fps: the VA encoders default to CBR with an
    /// auto-calculated CPB of 2 seconds of bitrate, which caps every frame
    /// at ~80 KB (2s x 320kbps at the 10s cadence) no matter how far apart
    /// frames sit. So the better the machine (working VA-API drivers), the
    /// blockier the clips — while x264's ABR, measured at this exact
    /// cadence, spends the full ~400 KB per-frame budget. When a VA encoder
    /// IS the only one installed, `configure_va_rate_control` steers it off
    /// those defaults.
    ///
    /// PACKAGING: none of these are guaranteed present, and they live in
    /// different packages from the ones the pipeline's other elements need.
    /// The full Linux runtime set, declared in tauri.conf.json's
    /// `bundle.linux`:
    ///
    ///   pipewiresrc    gstreamer1.0-pipewire      (capture — see pipewire.rs)
    ///   videoconvert   gstreamer1.0-plugins-base  (capture + clips)
    ///   mp4mux         gstreamer1.0-plugins-good
    ///   h264parse      gstreamer1.0-plugins-bad
    ///   x264enc        gstreamer1.0-plugins-ugly  (Recommends, not Depends)
    ///
    /// The encoder is deliberately a soft dependency: -ugly carries GPL x264,
    /// and gstreamer1-plugins-ugly isn't in Fedora proper at all, so a hard
    /// dependency would either impose that licence or make the package
    /// uninstallable. Missing it is survivable — `ClipRecorder::new` fails,
    /// the interval falls back to a JPEG, and after MAX_CLIP_ENCODER_FAILURES
    /// the loop stops trying. A user with no encoder gets the legacy
    /// one-frame-per-minute recording rather than a broken app.
    const ENCODER_CANDIDATES: &[&str] = &["x264enc", "vah264enc", "vaapih264enc", "openh264enc"];

    /// Quantizer for the VA encoders' constant-QP mode. H.264 QP 23 at
    /// 1080p screen content lands in the same visual class as the JPEG-q85
    /// bar the CLIP_FRAME_BYTE_BUDGET was calibrated against, at frame
    /// sizes comfortably inside that budget.
    const VA_CQP_QP: u32 = 23;

    /// Set a uint property clamped into its declared range. Returns false
    /// (and sets nothing) if the property doesn't exist or isn't a uint —
    /// the property sets below are all keyed on element type by name, so
    /// "not there" is an expected outcome, not an error.
    fn set_uint_prop_clamped(enc: &gst::Element, name: &str, value: u64) -> bool {
        let Some(pspec) = enc.find_property(name) else {
            return false;
        };
        let Some(spec) = pspec.downcast_ref::<gst::glib::ParamSpecUInt>() else {
            return false;
        };
        let clamped = (value.min(spec.maximum() as u64) as u32).max(spec.minimum());
        enc.set_property(name, clamped);
        true
    }

    /// Steer a VA-API encoder off its default rate control, which starves
    /// frames at our cadence (see ENCODER_CANDIDATES).
    ///
    /// Preferred: constant-QP, which sidesteps the driver's HRD/frame-rate
    /// math entirely — deterministic quality at any cadence. Everything is
    /// introspection-guarded because these properties are conditional:
    /// `rate-control` is only installed when the driver exposes more than
    /// one mode, and its enum only lists modes the driver supports (which
    /// is also why none of this can go in the parse_launch string — an
    /// unknown property there fails pipeline creation outright).
    ///
    /// Fallback when the driver has no CQP: stay on its default mode but
    /// widen the CPB window from the 2s auto default to one frame interval,
    /// so a frame is at least allowed to spend its whole byte budget.
    pub(super) fn configure_va_rate_control(
        enc: &gst::Element,
        bitrate_kbps: u32,
        interval_ms: u64,
    ) {
        let cqp = enc.find_property("rate-control").and_then(|pspec| {
            pspec
                .downcast_ref::<gst::glib::ParamSpecEnum>()?
                .enum_class()
                .to_value_by_nick("cqp")
        });

        if let Some(cqp) = cqp {
            enc.set_property_from_value("rate-control", &cqp);
            // vah264enc spells its quantizers qpi/qpp; the legacy
            // vaapih264enc has init-qp. Each element has one spelling.
            set_uint_prop_clamped(enc, "qpi", VA_CQP_QP as u64);
            set_uint_prop_clamped(enc, "qpp", VA_CQP_QP as u64);
            set_uint_prop_clamped(enc, "init-qp", VA_CQP_QP as u64);
            return;
        }

        // vah264enc counts CPB in kbit; vaapih264enc in ms of bitrate.
        let cpb_kbits = (bitrate_kbps as u64).saturating_mul(interval_ms) / 1000;
        if !set_uint_prop_clamped(enc, "cpb-size", cpb_kbits) {
            set_uint_prop_clamped(enc, "cpb-length", interval_ms);
        }
    }

    pub struct Encoder {
        pipeline: gst::Pipeline,
        appsrc: AppSrc,
        width: u32,
        height: u32,
        frame_interval_ms: u64,
    }

    unsafe impl Send for Encoder {}

    impl Encoder {
        /// The cadence must be declared, not just carried per-buffer.
        ///
        /// `bitrate` is bits per second of MEDIA time, scaled by the caller
        /// so that bitrate x interval lands CLIP_FRAME_BYTE_BUDGET in every
        /// frame. An encoder can only honour that if it knows how long a
        /// frame lasts: told `framerate=0/1`, x264enc assumes a normal video
        /// cadence and divides the budget across ~30 frames a second, so a
        /// clip at one frame per 4s got about 3 KB a frame instead of 400 KB
        /// — which is why Linux clips came out blocky and smeared while the
        /// same code looked right on macOS and Windows. Both of those pass
        /// the real frame rate to their encoders; this now does too.
        pub fn new(
            path: &Path,
            width: u32,
            height: u32,
            bitrate: u32,
            frame_interval_ms: u64,
        ) -> Result<Self, String> {
            let interval_ms = frame_interval_ms.max(1);
            gst::init().map_err(|e| format!("gst init failed: {e}"))?;

            let encoder_name = ENCODER_CANDIDATES
                .iter()
                .find(|name| gst::ElementFactory::find(name).is_some())
                .ok_or("no H.264 encoder element available")?;

            // x264enc wants kbit/s; the VA encoders take kbps too;
            // openh264enc uses bps. x264enc's default `medium` preset burns
            // 5-10x the CPU this job needs — superfast + zerolatency keeps
            // the software fallback cheap at screen-recording quality.
            let encoder_props = match *encoder_name {
                "openh264enc" => format!("bitrate={bitrate}"),
                "x264enc" => format!(
                    "bitrate={} speed-preset=superfast tune=zerolatency",
                    (bitrate / 1000).max(1)
                ),
                _ => format!("bitrate={}", (bitrate / 1000).max(1)),
            };

            let desc = format!(
                "appsrc name=src is-live=false format=time \
                 caps=video/x-raw,format=BGRA,width={width},height={height},framerate=1000/{interval_ms} \
                 ! videoconvert ! {encoder_name} name=enc {encoder_props} \
                 ! h264parse ! mp4mux ! filesink location=\"{}\"",
                path.to_string_lossy()
            );
            let pipeline = gst::parse::launch(&desc)
                .map_err(|e| format!("gst pipeline parse failed: {e}"))?
                .downcast::<gst::Pipeline>()
                .map_err(|_| "not a pipeline".to_string())?;

            if encoder_name.starts_with("va") {
                if let Some(enc) = pipeline.by_name("enc") {
                    configure_va_rate_control(&enc, (bitrate / 1000).max(1), interval_ms);
                }
            }

            let appsrc = pipeline
                .by_name("src")
                .ok_or("appsrc missing")?
                .downcast::<AppSrc>()
                .map_err(|_| "appsrc cast failed".to_string())?;

            pipeline
                .set_state(gst::State::Playing)
                .map_err(|e| format!("gst set_state failed: {e}"))?;

            Ok(Self {
                pipeline,
                appsrc,
                width,
                height,
                frame_interval_ms: interval_ms,
            })
        }

        pub fn append_bgra_frame(
            &mut self,
            bgra: &[u8],
            width: u32,
            height: u32,
            pts_ms: u64,
        ) -> Result<(), String> {
            debug_assert_eq!((width, height), (self.width, self.height));
            let mut buffer = gst::Buffer::with_size(bgra.len())
                .map_err(|e| format!("gst buffer alloc failed: {e}"))?;
            {
                let buffer_mut = buffer.get_mut().ok_or("buffer not writable")?;
                buffer_mut.set_pts(gst::ClockTime::from_mseconds(pts_ms));
                // Without a duration mp4mux has to infer each sample's
                // length from the next frame's timestamp, which leaves the
                // final frame zero-length.
                buffer_mut.set_duration(gst::ClockTime::from_mseconds(self.frame_interval_ms));
                let mut map = buffer_mut
                    .map_writable()
                    .map_err(|e| format!("gst buffer map failed: {e}"))?;
                map.copy_from_slice(bgra);
            }
            self.appsrc
                .push_buffer(buffer)
                .map_err(|e| format!("gst push_buffer failed: {e}"))?;
            Ok(())
        }

        pub fn finish(self, _duration_ms: u64) -> Result<(), String> {
            self.appsrc
                .end_of_stream()
                .map_err(|e| format!("gst EOS failed: {e}"))?;
            // Wait for the muxer to flush the moov atom.
            let bus = self.pipeline.bus().ok_or("no gst bus")?;
            let result = (|| {
                for msg in bus.iter_timed(gst::ClockTime::from_seconds(15)) {
                    use gst::MessageView;
                    match msg.view() {
                        MessageView::Eos(_) => return Ok(()),
                        MessageView::Error(e) => {
                            return Err(format!("gst error: {}", e.error()));
                        }
                        _ => {}
                    }
                }
                Err("gst EOS timed out".to_string())
            })();
            let _ = self.pipeline.set_state(gst::State::Null);
            result
        }
    }
}
