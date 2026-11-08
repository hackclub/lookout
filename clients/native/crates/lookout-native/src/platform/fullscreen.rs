//! Letting the fixed-size main window go full screen.
//!
//! The main window is created non-resizable (the Tauri app's fixed
//! 480×640), and AppKit ignores `toggleFullScreen:` on windows without
//! `NSWindowStyleMaskResizable`, so the video player's full screen button
//! did nothing on macOS. [`prepare`] makes the window resizable and a
//! full-screen primary window just before entering, and puts both back once
//! AppKit reports the exit (`NSWindowDidExitFullScreenNotification`),
//! however it happened: our button, Esc, or the green traffic light. This
//! is what winit does for the same case.
//!
//! Windows and Linux full screen non-resizable windows already; there it is
//! a no-op.

use gpui_kit::gpui::Window;

/// Call right before `window.toggle_fullscreen()` when entering.
pub fn prepare(window: &Window) {
    imp::prepare(window)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_app_kit::{NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask};
    use objc2_foundation::NSString;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Window;

    thread_local! {
        /// The pending exit observer, so a second `prepare` before the exit
        /// doesn't stack another.
        static OBSERVER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
        // SAFETY: GPUI's AppKit handle points at its live NSView (GPUIView).
        let view: &NSView = unsafe { &*(h.ns_view.as_ptr() as *const NSView) };
        view.window()
    }

    pub fn prepare(window: &Window) {
        let Some(win) = ns_window(window) else { return };
        let mask = win.styleMask();
        if mask.contains(NSWindowStyleMask::Resizable) || OBSERVER.with(|o| o.borrow().is_some()) {
            return;
        }
        let behavior = win.collectionBehavior();
        win.setStyleMask(mask | NSWindowStyleMask::Resizable);
        win.setCollectionBehavior(behavior | NSWindowCollectionBehavior::FullScreenPrimary);

        let target = win.clone();
        let block = RcBlock::new(move |_note: NonNull<AnyObject>| {
            target.setStyleMask(target.styleMask() & !NSWindowStyleMask::Resizable);
            target.setCollectionBehavior(behavior);
            if let Some(obs) = OBSERVER.with(|o| o.borrow_mut().take()) {
                // SAFETY: removing an observer token from the default center.
                unsafe {
                    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
                    let _: () = msg_send![center, removeObserver: &*obs];
                }
            }
        });
        let name = NSString::from_str("NSWindowDidExitFullScreenNotification");
        // SAFETY: standard block-based observer registration on the main
        // queue; the returned token is kept until the block removes it.
        let obs: Option<Retained<AnyObject>> = unsafe {
            let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
            let queue: *mut AnyObject = msg_send![class!(NSOperationQueue), mainQueue];
            msg_send![center, addObserverForName: &*name, object: &*win, queue: queue, usingBlock: &*block]
        };
        OBSERVER.with(|o| *o.borrow_mut() = obs);
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Window;

    pub fn prepare(_window: &Window) {}
}
