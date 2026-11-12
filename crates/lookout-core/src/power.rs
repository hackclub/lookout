//! Scoped App Nap / idle-system-sleep suppression (macOS).
//!
//! The assertion must be held while a session is recording (or paused
//! mid-session) so macOS never throttles the capture cadence or lets the
//! machine idle-sleep out from under an active recording. It must NOT be
//! held for the whole process lifetime — that kept the user's Mac from ever
//! idle-sleeping just because Lookout sat open on the gallery.

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
use std::sync::Mutex;

struct ActivityToken(Retained<ProtocolObject<dyn NSObjectProtocol>>);
// SAFETY: the token is an opaque handle whose only use is being handed
// back to `NSProcessInfo::endActivity`, which is documented thread-safe.
unsafe impl Send for ActivityToken {}

static ACTIVITY: Mutex<Option<ActivityToken>> = Mutex::new(None);

/// Begin the recording assertion. Idempotent — a second call while one
/// is already held is a no-op.
pub fn begin_recording_assertion() {
    let mut guard = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_some() {
        return;
    }
    let info = NSProcessInfo::processInfo();
    let reason = NSString::from_str("Periodic screenshot capture must not be throttled");
    let opts =
        NSActivityOptions::LatencyCritical | NSActivityOptions::IdleSystemSleepDisabled;
    *guard = Some(ActivityToken(
        info.beginActivityWithOptions_reason(opts, &reason),
    ));
    lookout_session::log_line!("[power] recording sleep/App Nap suppression ON");
}

/// End the recording assertion (no-op if none is held).
pub fn end_recording_assertion() {
    let mut guard = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(token) = guard.take() {
        // SAFETY: `token.0` came from `beginActivityWithOptions_reason`,
        // so it is the correct activity type.
        unsafe { NSProcessInfo::processInfo().endActivity(&token.0) };
        lookout_session::log_line!("[power] recording sleep/App Nap suppression OFF");
    }
}
