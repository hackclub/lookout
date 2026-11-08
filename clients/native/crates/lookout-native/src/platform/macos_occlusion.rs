//! Keeps GPUI windows animating on macOS 26+.
//!
//! GPUI starts a window's display link (its per-frame callback) only when
//! `-[NSWindow occlusionState]` contains `NSWindowOcclusionStateVisible`
//! (`0x2`, see `gpui-pre-macos` `window_did_change_occlusion_state`). macOS 26
//! reports on-screen windows with a new, undocumented bit (`0x2000`) and can
//! leave `0x2` clear, so the link never starts: the window paints its first
//! frame and then never redraws (no springs, no clock, no spinner).
//!
//! Cap hit the same thing in its GPUI port. This is our own take on the fix:
//! GPUI's window classes (`GPUIWindow`, `GPUIPanel`) get an `occlusionState`
//! override that adds `0x2` back whenever AppKit reports *any* visibility bit
//! and the window is actually ordered in. A genuinely occluded window still
//! reports 0, so GPUI keeps stopping the link for covered, minimised or
//! hidden windows exactly as before.
//!
//! GPUI registers both classes from a load-time constructor, so `install()`
//! only has to run before the first window opens.

use std::sync::OnceLock;

use objc2::ffi::class_addMethod;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{msg_send, sel};

const VISIBLE: usize = 0x2;

type OcclusionFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize;

/// NSWindow's own `occlusionState`, which the override calls through to.
/// GPUIWindow and GPUIPanel both inherit it unchanged from NSWindow.
static ORIGINAL: OnceLock<OcclusionFn> = OnceLock::new();

/// What GPUI should see for a raw AppKit state.
fn reported(raw: usize, ordered_in: impl FnOnce() -> bool) -> usize {
    if raw != 0 && raw & VISIBLE == 0 && ordered_in() { raw | VISIBLE } else { raw }
}

unsafe extern "C-unwind" fn occlusion_state(this: *mut AnyObject, sel: Sel) -> usize {
    let Some(original) = ORIGINAL.get() else { return VISIBLE };
    // SAFETY: `original` is NSWindow's implementation of this exact selector
    // and `this` is an NSWindow subclass instance.
    let raw = unsafe { original(this, sel) };
    reported(raw, || {
        // SAFETY: `isVisible` is a plain NSWindow getter.
        let visible: Bool = unsafe { msg_send![&*this, isVisible] };
        visible.as_bool()
    })
}

/// Install the override. Idempotent; logs and does nothing if the classes
/// aren't there (e.g. a GPUI update renamed them).
pub fn install() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        let sel = sel!(occlusionState);
        let Some(original) = AnyClass::get(c"NSWindow").and_then(|c| c.instance_method(sel)) else {
            log::warn!("occlusion shim: NSWindow has no occlusionState");
            return;
        };
        // SAFETY: the IMP of `-[NSWindow occlusionState]` has the
        // `(id, SEL) -> NSUInteger` signature.
        let original: OcclusionFn = unsafe { std::mem::transmute::<Imp, OcclusionFn>(original.implementation()) };
        let _ = ORIGINAL.set(original);
        for name in [c"GPUIWindow", c"GPUIPanel"] {
            let Some(class) = AnyClass::get(name) else {
                log::warn!("occlusion shim: {name:?} isn't registered");
                continue;
            };
            // SAFETY: adds an override with a matching type encoding
            // (`Q@:` = NSUInteger(id, SEL)) to a class GPUI registered; it
            // fails harmlessly if the class already defines the method.
            let added = unsafe {
                class_addMethod(
                    (class as *const AnyClass).cast_mut(),
                    sel,
                    std::mem::transmute::<OcclusionFn, Imp>(occlusion_state),
                    c"Q@:".as_ptr(),
                )
            };
            if !added.as_bool() {
                log::warn!("occlusion shim: couldn't override occlusionState on {name:?}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::reported;

    #[test]
    fn adds_the_documented_bit_only_for_ordered_in_windows() {
        assert_eq!(reported(0x2000, || true), 0x2002);
        assert_eq!(reported(0x2000, || false), 0x2000);
        assert_eq!(reported(0x2, || panic!("not asked")), 0x2);
        assert_eq!(reported(0, || panic!("not asked")), 0);
    }
}
