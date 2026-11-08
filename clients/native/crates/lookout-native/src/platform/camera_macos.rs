//! macOS camera capture via AVFoundation: an `AVCaptureSession` with a
//! `AVCaptureVideoDataOutput` (32BGRA) whose sample-buffer delegate copies
//! each frame into a shared slot; `grab` JPEG-encodes the newest one.
//! Camera permission prompting lives in `platform/permissions.rs`.

use super::*;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Instant;

define_class!(
    // SAFETY: NSObject has no subclassing requirements and we don't implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "LookoutCameraSampleDelegate"]
    #[ivars = FrameSlot]
    struct SampleDelegate;

    unsafe impl NSObjectProtocol for SampleDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for SampleDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            if let Some(frame) = copy_frame(sample_buffer) {
                *self.ivars().lock() = Some(Arc::new(frame));
            }
        }
    }
);

impl SampleDelegate {
    fn new(slot: FrameSlot) -> Retained<Self> {
        let this = Self::alloc().set_ivars(slot);
        unsafe { msg_send![super(this), init] }
    }
}

/// Copy a 32BGRA pixel buffer out of the sample into tightly packed rows.
fn copy_frame(sample: &CMSampleBuffer) -> Option<RawFrame> {
    let pb = unsafe { sample.image_buffer() }?;
    if CVPixelBufferGetPixelFormatType(&pb) != kCVPixelFormatType_32BGRA {
        return None;
    }
    if unsafe { CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let width = CVPixelBufferGetWidth(&pb);
    let height = CVPixelBufferGetHeight(&pb);
    let stride = CVPixelBufferGetBytesPerRow(&pb);
    let base = CVPixelBufferGetBaseAddress(&pb) as *const u8;
    let row = width * 4;
    let out = if base.is_null() || width == 0 || height == 0 || stride < row {
        None
    } else {
        let mut bgra = Vec::with_capacity(row * height);
        for y in 0..height {
            // SAFETY: the buffer is locked and holds `height` rows of `stride` bytes.
            let src = unsafe { std::slice::from_raw_parts(base.add(y * stride), row) };
            bgra.extend_from_slice(src);
        }
        Some(RawFrame { width: width as u32, height: height as u32, bgra, at: Instant::now() })
    };
    unsafe { CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::ReadOnly) };
    out
}

struct Session {
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    _delegate: Retained<SampleDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    slot: FrameSlot,
}

// SAFETY: AVCaptureSession/AVCaptureVideoDataOutput may be configured, started
// and stopped from any thread; the delegate only touches its Arc<Mutex> slot.
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Session {
    fn open(device_id: &str) -> Result<Self, String> {
        let media = unsafe { AVMediaTypeVideo }.ok_or("AVFoundation has no video media type")?;
        let device = unsafe {
            (!device_id.is_empty())
                .then(|| AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(device_id)))
                .flatten()
                .or_else(|| AVCaptureDevice::defaultDeviceWithMediaType(media))
        }
        .ok_or("no camera was found")?;

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| format!("couldn't open the camera: {}", e.localizedDescription()))?;

        let slot: FrameSlot = Arc::new(Mutex::new(None));
        let delegate = SampleDelegate::new(slot.clone());
        let queue = DispatchQueue::new("sh.hackclub.lookout.camera", None);

        unsafe {
            let session = AVCaptureSession::new();
            let output = AVCaptureVideoDataOutput::new();
            output.setAlwaysDiscardsLateVideoFrames(true);
            // SAFETY: CFString is toll-free bridged to NSString.
            let key: &NSString =
                &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString);
            let fmt = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
            let value: &AnyObject = &fmt;
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[value]);
            output.setVideoSettings(Some(&settings));
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );

            session.beginConfiguration();
            if !session.canAddInput(&input) {
                session.commitConfiguration();
                return Err("the camera can't be added to a capture session".into());
            }
            session.addInput(&input);
            if !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err("the camera's video output can't be added".into());
            }
            session.addOutput(&output);
            session.commitConfiguration();
            session.startRunning();

            Ok(Self { session, output, _delegate: delegate, _queue: queue, slot })
        }
    }

    fn close(&self) {
        unsafe {
            self.session.stopRunning();
            self.output.setSampleBufferDelegate_queue(None, None);
        }
        *self.slot.lock() = None;
    }
}

pub struct Camera {
    session: Mutex<Option<Arc<Session>>>,
}

impl Camera {
    pub fn new() -> Self {
        Self { session: Mutex::new(None) }
    }

    pub async fn start(&self, device_id: &str) -> Result<(), String> {
        self.stop();
        let id = device_id.to_string();
        // startRunning blocks until the device is up.
        let session = tokio::task::spawn_blocking(move || Session::open(&id).map(Arc::new))
            .await
            .map_err(|e| e.to_string())??;
        let slot = session.slot.clone();
        *self.session.lock() = Some(session);
        match wait_first_frame(&slot).await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.stop();
                Err(e)
            }
        }
    }

    pub async fn grab(&self, _device_id: &str) -> Result<CameraFrame, String> {
        let slot = self.session.lock().as_ref().map(|s| s.slot.clone()).ok_or(NO_FRAME)?;
        encode_latest(&slot).await
    }

    /// Stops the session, which releases the device and turns the light off.
    pub fn stop(&self) {
        if let Some(s) = self.session.lock().take() {
            s.close();
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Video capture devices (built-in, external/USB, Continuity Camera).
/// id = AVCaptureDevice.uniqueID, label = localizedName.
#[allow(deprecated)] // devicesWithMediaType: covers every macOS version we ship on.
pub fn list_devices() -> Vec<CameraDevice> {
    let Some(media) = (unsafe { AVMediaTypeVideo }) else {
        return Vec::new();
    };
    let devices = unsafe { AVCaptureDevice::devicesWithMediaType(media) };
    devices
        .iter()
        .map(|d| unsafe {
            CameraDevice { id: d.uniqueID().to_string(), label: d.localizedName().to_string() }
        })
        .collect()
}
