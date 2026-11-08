//! macOS backend: `AVPlayer` + `AVPlayerItemVideoOutput`.
//!
//! AVFoundation does the hard parts: hardware decode (VideoToolbox), HTTP
//! streaming with range requests, buffering and A/V clocking. We pull the
//! pixel buffer for "now" on every display frame and hand it to GPUI's
//! `paint_surface`, which samples the IOSurface directly (no copies).
//!
//! The renderer only accepts `420f` (bi-planar 4:2:0, full range), so the
//! output is configured for exactly that and anything else is dropped
//! rather than reaching the renderer's assertion.
//!
//! Everything here runs on the main thread (AVPlayer objects aren't
//! `Send`); seek completions arrive on a dispatch queue and only flip an
//! atomic and wake the UI.
//!
//! Scrubbing follows Apple's "chase" pattern (QA1820): one accurate seek in
//! flight at a time, newer targets replace the queued one, and the next is
//! issued when the previous completes.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2::{AnyThread, MainThreadMarker};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2_av_foundation::{
    AVPlayer, AVPlayerActionAtItemEnd, AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput,
    AVPlayerTimeControlStatus, NSValueAVFoundationExtensions,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::{Frame, Status, Waker};

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

/// CoreVideo dictionary keys are `CFString`s, toll-free bridged to `NSString`.
fn cf_key<T>(k: &T) -> &NSString {
    // SAFETY: `T` is CFString, which has the same object layout as NSString.
    unsafe { &*(k as *const T as *const NSString) }
}

fn seconds(t: CMTime) -> Option<f64> {
    let valid = t.flags.contains(CMTimeFlags::Valid) && !t.flags.contains(CMTimeFlags::Indefinite);
    let s = unsafe { t.seconds() };
    (valid && s.is_finite()).then_some(s)
}

fn cm(t: f64) -> CMTime {
    unsafe { CMTime::with_seconds(t.max(0.0), 90_000) }
}

fn cm_zero() -> CMTime {
    unsafe { CMTime::with_seconds(0.0, 1) }
}

pub struct Backend {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    wake: Waker,
    frame: RefCell<Option<Frame>>,
    want_playing: Cell<bool>,
    ended: Cell<bool>,
    looping: Cell<bool>,
    rate: Cell<f64>,
    /// Seek chase state.
    seek_in_flight: Arc<AtomicBool>,
    seek_pending: Cell<Option<f64>>,
    seek_target: Cell<Option<f64>>,
    /// Keep ticking until a frame for the new position arrives.
    frame_wanted_until: Cell<Option<Instant>>,
}

impl Backend {
    pub fn open(url: &str, wake: Waker) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("video player must be created on the main thread")?;
        unsafe {
            let ns_url = NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(|| format!("bad video URL: {url}"))?;
            let item = AVPlayerItem::playerItemWithURL(&ns_url, mtm);

            let keys = [cf_key(kCVPixelBufferPixelFormatTypeKey), cf_key(kCVPixelBufferMetalCompatibilityKey)];
            let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
            let metal = NSNumber::new_bool(true);
            let vals: [&AnyObject; 2] = [&format, &metal];
            let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &vals);
            let output =
                AVPlayerItemVideoOutput::initWithPixelBufferAttributes(AVPlayerItemVideoOutput::alloc(), Some(&attrs));
            item.addOutput(&output);

            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setMuted(true);
            player.setActionAtItemEnd(AVPlayerActionAtItemEnd::Pause);
            Ok(Self {
                player,
                item,
                output,
                wake,
                frame: RefCell::new(None),
                want_playing: Cell::new(false),
                ended: Cell::new(false),
                looping: Cell::new(false),
                rate: Cell::new(1.0),
                seek_in_flight: Arc::new(AtomicBool::new(false)),
                seek_pending: Cell::new(None),
                seek_target: Cell::new(None),
                frame_wanted_until: Cell::new(Some(Instant::now() + Duration::from_secs(30))),
            })
        }
    }

    pub fn play(&self) {
        if self.ended.replace(false) {
            self.seek(0.0);
        }
        self.want_playing.set(true);
        unsafe { self.player.setRate(self.rate.get() as f32) };
    }

    pub fn pause(&self) {
        self.want_playing.set(false);
        unsafe { self.player.pause() };
    }

    pub fn seek(&self, t: f64) {
        self.ended.set(false);
        self.seek_pending.set(Some(t.max(0.0)));
        self.pump_seek();
    }

    fn pump_seek(&self) {
        if self.seek_in_flight.load(Ordering::Acquire) {
            return;
        }
        let Some(t) = self.seek_pending.take() else {
            if self.seek_target.take().is_some() {
                // The last seek landed: keep ticking until its frame shows.
                self.frame_wanted_until.set(Some(Instant::now() + Duration::from_millis(750)));
            }
            return;
        };
        self.seek_target.set(Some(t));
        self.seek_in_flight.store(true, Ordering::Release);
        let flag = self.seek_in_flight.clone();
        let wake = self.wake.clone();
        let done = block2::RcBlock::new(move |_finished: Bool| {
            flag.store(false, Ordering::Release);
            wake();
        });
        unsafe {
            self.player.seekToTime_toleranceBefore_toleranceAfter_completionHandler(cm(t), cm_zero(), cm_zero(), &done);
        }
    }

    pub fn set_rate(&self, rate: f64) {
        self.rate.set(rate);
        if self.want_playing.get() {
            unsafe { self.player.setRate(rate as f32) };
        }
    }

    pub fn set_looping(&self, looping: bool) {
        self.looping.set(looping);
    }

    /// Pull the frame for the current display time.
    pub fn tick(&self) {
        unsafe {
            let now = self.output.itemTimeForMachAbsoluteTime(mach_absolute_time() as i64);
            if !self.output.hasNewPixelBufferForItemTime(now) {
                return;
            }
            let Some(buffer) = self.output.copyPixelBufferForItemTime_itemTimeForDisplay(now, std::ptr::null_mut())
            else {
                return;
            };
            // Same +1 reference, re-wrapped as the CF smart pointer GPUI takes.
            let raw = Retained::into_raw(buffer) as *mut CVPixelBuffer;
            let Some(buffer) = NonNull::new(raw).map(|p| CFRetained::from_raw(p)) else { return };
            if CVPixelBufferGetPixelFormatType(&buffer) != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange {
                return;
            }
            let (width, height) = (CVPixelBufferGetWidth(&buffer) as u32, CVPixelBufferGetHeight(&buffer) as u32);
            *self.frame.borrow_mut() = Some(Frame { width, height, buffer });
            self.frame_wanted_until.set(None);
        }
    }

    pub fn frame(&self) -> Option<Frame> {
        self.frame.borrow().clone()
    }

    pub fn status(&self) -> Status {
        self.pump_seek();
        unsafe {
            let error = (self.item.status() == AVPlayerItemStatus::Failed).then(|| {
                self.item
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "AVFoundation couldn't play this video".into())
            });
            let duration = seconds(self.item.duration()).unwrap_or(0.0).max(0.0);
            let current = seconds(self.player.currentTime()).unwrap_or(0.0);
            let rate = self.player.rate();

            // End of item: AVPlayer pauses itself (actionAtItemEnd = Pause).
            if self.want_playing.get() && duration > 0.0 && rate == 0.0 && current >= duration - 0.01 {
                if self.looping.get() {
                    self.seek(0.0);
                    self.player.setRate(self.rate.get() as f32);
                } else {
                    self.want_playing.set(false);
                    self.ended.set(true);
                    (self.wake)();
                }
            }

            let waiting = self.player.timeControlStatus() == AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate;
            let mut buffered = 0.0f64;
            for value in self.item.loadedTimeRanges().iter() {
                let range = value.CMTimeRangeValue();
                if let (Some(start), Some(len)) = (seconds(range.start), seconds(range.duration)) {
                    buffered = buffered.max(start + len);
                }
            }

            let seeking = self.seek_target.get().is_some() || self.seek_pending.get().is_some();
            let has_frame = self.frame.borrow().is_some();
            let wants_frame = self.frame_wanted_until.get().is_some_and(|t| Instant::now() < t);
            let ended = self.ended.get();
            let position = if ended {
                duration
            } else {
                self.seek_pending.get().or(self.seek_target.get()).unwrap_or(current)
            };
            Status {
                position: position.clamp(0.0, duration.max(position.max(0.0))),
                duration,
                playing: self.want_playing.get(),
                buffering: error.is_none() && ((self.want_playing.get() && waiting) || !has_frame),
                ended,
                buffered: buffered.min(duration),
                active: error.is_none() && (self.want_playing.get() || seeking || wants_frame || !has_frame),
                error,
                has_frame,
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            self.player.pause();
            self.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}
