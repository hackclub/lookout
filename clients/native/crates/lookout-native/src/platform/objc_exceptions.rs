//! Names the Objective-C exception behind an AppKit crash.
//!
//! When an `NSException` escapes into AppKit's run loop (e.g. from a
//! Core Animation commit), AppKit calls `-[NSApplication _crashOnException:]`
//! and the process dies. The minidump then shows only that frame
//! (LOOKOUT-NATIVE-1A): the exception's name, reason and the stack it was
//! thrown from are gone. This wraps that method to hand them to the crash
//! reporter process first (synchronously), then lets AppKit crash as before.
//!
//! `_crashOnException:` is private; if a macOS release drops it, `install`
//! logs and does nothing.

use std::sync::OnceLock;

use objc2::ffi::method_setImplementation;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSArray, NSString};

type CrashFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

static ORIGINAL: OnceLock<CrashFn> = OnceLock::new();

/// How many frames of the throw-site stack to keep.
const FRAMES: usize = 40;

fn describe(exception: &AnyObject) -> (String, String, String) {
    // SAFETY: `name`, `reason` and `callStackSymbols` are NSException
    // getters returning (nullable) NSString / NSArray<NSString>.
    let name: Option<Retained<NSString>> = unsafe { msg_send![exception, name] };
    let reason: Option<Retained<NSString>> = unsafe { msg_send![exception, reason] };
    let stack: Option<Retained<NSArray<NSString>>> = unsafe { msg_send![exception, callStackSymbols] };
    let stack = stack
        .map(|s| (0..s.count().min(FRAMES)).map(|i| s.objectAtIndex(i).to_string()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    (
        name.map(|n| n.to_string()).unwrap_or_default(),
        reason.map(|r| r.to_string()).unwrap_or_default(),
        stack,
    )
}

unsafe extern "C-unwind" fn crash_on_exception(this: *mut AnyObject, sel: Sel, exception: *mut AnyObject) {
    // SAFETY: AppKit passes the NSException it caught (or nil).
    if let Some(exception) = unsafe { exception.as_ref() } {
        let (name, reason, stack) = describe(exception);
        log::error!("[objc] uncaught {name}: {reason}");
        // The app is about to die: give the out-of-process reporter the
        // details now, so they land on the minidump's event.
        sentry::with_integration(|m: &sentry::integrations::minidump::MinidumpIntegration, _| {
            m.set_tag("objc_exception".into(), Some(name.clone()));
            m.set_extra("objc_exception_reason".into(), Some(reason.clone().into()));
            m.set_extra("objc_exception_stack".into(), Some(stack.clone().into()));
        });
    }
    if let Some(original) = ORIGINAL.get() {
        // SAFETY: AppKit's own implementation of this selector.
        unsafe { original(this, sel, exception) };
    }
}

/// Install the wrapper. Idempotent; call before `app.run`.
pub fn install() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let sel = sel!(_crashOnException:);
        let Some(method) = AnyClass::get(c"NSApplication").and_then(|c| c.instance_method(sel)) else {
            log::warn!("objc exception note: NSApplication has no _crashOnException:");
            return;
        };
        // SAFETY: the method takes one object argument and returns void.
        let original = unsafe { std::mem::transmute::<Imp, CrashFn>(method.implementation()) };
        let _ = ORIGINAL.set(original);
        // SAFETY: replaces the IMP with one of the same signature, which
        // calls through to the original.
        unsafe { method_setImplementation(method, std::mem::transmute::<CrashFn, Imp>(crash_on_exception)) };
    });
}
