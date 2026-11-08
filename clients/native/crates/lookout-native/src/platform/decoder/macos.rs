//! AVFoundation decoder: AVURLAsset → AVAssetReader + AVAssetReaderTrackOutput
//! vending 32BGRA pixel buffers (optionally scaled by the decoder itself).
//! Seeking recreates the reader with a time range starting at the target
//! and drops any samples that still come out before it.

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVMediaTypeVideo, AVURLAsset,
};
use objc2_core_media::{kCMTimePositiveInfinity, CMSampleBuffer, CMTime, CMTimeRange};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
    kCVReturnSuccess, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferIsPlanar, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSError, NSNumber, NSString, NSURL};

use super::{DecodedFrame, FrameDecoder};

pub fn open(path: &std::path::Path, max_height: Option<u32>) -> Result<Box<dyn FrameDecoder>, String> {
    autoreleasepool(|_| AvDecoder::new(path, max_height)).map(|d| Box::new(d) as Box<dyn FrameDecoder>)
}

struct AvDecoder {
    asset: Retained<AVURLAsset>,
    track: Retained<AVAssetTrack>,
    settings: Retained<NSDictionary<NSString, AnyObject>>,
    reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderTrackOutput>,
    duration: f64,
    /// Frames with pts below this are dropped (accurate seek).
    seek_target: f64,
}

// SAFETY: the AVFoundation objects are only ever touched by whichever single
// thread currently owns the decoder (the player thread or a filmstrip
// worker); they are never shared, so moving the whole bundle across threads
// is fine (AVAssetReader is documented as usable off the main thread).
unsafe impl Send for AvDecoder {}

fn ns_err(e: &NSError) -> String {
    e.localizedDescription().to_string()
}

/// CoreVideo dictionary keys are `CFString`s, toll-free bridged to `NSString`.
fn cf_key<T>(k: &T) -> &NSString {
    // SAFETY: `T` is CFString, which has the same object layout as NSString.
    unsafe { &*(k as *const T as *const NSString) }
}

impl AvDecoder {
    fn new(path: &std::path::Path, max_height: Option<u32>) -> Result<Self, String> {
        let url = NSURL::from_file_path(path).ok_or_else(|| format!("bad video path: {}", path.display()))?;
        unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let video = AVMediaTypeVideo.ok_or("AVMediaTypeVideo unavailable")?;
            // Synchronous track access is deprecated in favour of async
            // loading, but blocks fine on a background thread for local files.
            #[allow(deprecated)]
            let track = asset.tracksWithMediaType(video).firstObject().ok_or("video has no video track")?;

            // Output settings: BGRA, plus a target size when downscaling.
            let mut keys: Vec<&NSString> = vec![cf_key(kCVPixelBufferPixelFormatTypeKey)];
            let mut vals: Vec<Retained<NSNumber>> = vec![NSNumber::new_u32(kCVPixelFormatType_32BGRA)];
            if let Some(mh) = max_height {
                let size = track.naturalSize();
                let (nw, nh) = (size.width.abs(), size.height.abs());
                if nh > mh as f64 && nw > 0.0 {
                    let h = mh.max(2) & !1;
                    // Keep aspect; even dimensions keep the scaler happy.
                    let w = (((nw * h as f64 / nh).round() as u32).max(2) + 1) & !1;
                    keys.push(cf_key(kCVPixelBufferWidthKey));
                    vals.push(NSNumber::new_u32(w));
                    keys.push(cf_key(kCVPixelBufferHeightKey));
                    vals.push(NSNumber::new_u32(h));
                }
            }
            let vals: Vec<&AnyObject> = vals.iter().map(|v| &***v as &AnyObject).collect();
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &vals);

            let duration = {
                let d = asset.duration().seconds();
                if d.is_finite() && d > 0.0 { d } else { 0.0 }
            };
            let (reader, output) = make_reader(&asset, &track, &settings, 0.0)?;
            Ok(Self { asset, track, settings, reader, output, duration, seek_target: 0.0 })
        }
    }
}

/// Build and start a reader whose time range begins at `start` seconds.
fn make_reader(
    asset: &AVURLAsset,
    track: &AVAssetTrack,
    settings: &NSDictionary<NSString, AnyObject>,
    start: f64,
) -> Result<(Retained<AVAssetReader>, Retained<AVAssetReaderTrackOutput>), String> {
    unsafe {
        let reader = AVAssetReader::assetReaderWithAsset_error(asset).map_err(|e| ns_err(&e))?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(track, Some(settings));
        // We copy the pixels out anyway, so let AVFoundation skip its own copy.
        output.setAlwaysCopiesSampleData(false);
        if !reader.canAddOutput(&output) {
            return Err("AVAssetReader cannot add video output".into());
        }
        reader.addOutput(&output);
        if start > 0.0 {
            reader.setTimeRange(CMTimeRange::new(CMTime::with_seconds(start, 600), kCMTimePositiveInfinity));
        }
        if !reader.startReading() {
            return Err(reader.error().map(|e| ns_err(&e)).unwrap_or_else(|| "AVAssetReader failed to start".into()));
        }
        Ok((reader, output))
    }
}

/// Copy the sample's BGRA image into a tightly packed buffer.
/// Returns `Ok(None)` for samples that carry no image.
fn copy_frame(sample: &CMSampleBuffer) -> Result<Option<DecodedFrame>, String> {
    unsafe {
        let pts = sample.presentation_time_stamp().seconds();
        let Some(pb) = sample.image_buffer() else { return Ok(None) };
        if CVPixelBufferGetPixelFormatType(&pb) != kCVPixelFormatType_32BGRA || CVPixelBufferIsPlanar(&pb) {
            return Err("unexpected pixel format from AVAssetReader".into());
        }
        if CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) != kCVReturnSuccess {
            return Err("CVPixelBufferLockBaseAddress failed".into());
        }
        // Everything between lock and unlock is infallible except the null
        // check, which still falls through to the unlock below.
        let width = CVPixelBufferGetWidth(&pb);
        let height = CVPixelBufferGetHeight(&pb);
        let stride = CVPixelBufferGetBytesPerRow(&pb);
        let base = CVPixelBufferGetBaseAddress(&pb) as *const u8;
        let row = width * 4;
        let result = if base.is_null() || stride < row {
            Err("pixel buffer has no accessible base address".to_string())
        } else {
            let mut bgra = Vec::with_capacity(row * height);
            for y in 0..height {
                bgra.extend_from_slice(std::slice::from_raw_parts(base.add(y * stride), row));
            }
            Ok(Some(DecodedFrame { pts, width: width as u32, height: height as u32, bgra }))
        };
        CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly);
        result
    }
}

impl FrameDecoder for AvDecoder {
    fn duration(&self) -> f64 {
        self.duration
    }

    fn seek(&mut self, t: f64) -> Result<(), String> {
        let t = t.max(0.0);
        autoreleasepool(|_| {
            unsafe { self.reader.cancelReading() };
            let (reader, output) = make_reader(&self.asset, &self.track, &self.settings, t)?;
            self.reader = reader;
            self.output = output;
            self.seek_target = t;
            Ok(())
        })
    }

    fn next_frame(&mut self) -> Result<Option<DecodedFrame>, String> {
        loop {
            // One pool per sample so decoded buffers are released promptly
            // on threads that have no run loop draining a pool for us.
            let step = autoreleasepool(|_| -> Result<Option<Option<DecodedFrame>>, String> {
                let Some(sample) = (unsafe { self.output.copyNextSampleBuffer() }) else {
                    // nil means end of range or failure; tell them apart.
                    return match unsafe { self.reader.status() } {
                        AVAssetReaderStatus::Failed => Err(unsafe { self.reader.error() }
                            .map(|e| ns_err(&e))
                            .unwrap_or_else(|| "AVAssetReader failed".into())),
                        _ => Ok(None),
                    };
                };
                match copy_frame(&sample)? {
                    // Readers may start at the keyframe before the range;
                    // drop frames before the seek target (with a µs tolerance).
                    Some(f) if f.pts + 1e-6 >= self.seek_target => Ok(Some(Some(f))),
                    _ => Ok(Some(None)),
                }
            })?;
            match step {
                None => return Ok(None),
                Some(Some(f)) => return Ok(Some(f)),
                Some(None) => continue,
            }
        }
    }
}
