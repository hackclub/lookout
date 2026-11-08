//! macOS: the "+" menu is the native SwiftUI panel from the Tauri app
//! (`AddMenu.swift`): material background, a 0.85 → 1 spring "fling" from
//! the button's corner, 150ms fade in / 170ms out, arrow keys and Return.

use gpui_kit::gpui::{Bounds, Pixels, Window};
use serde::Serialize;

#[derive(Serialize)]
pub struct Entry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(rename = "iconUrl", skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub separator: bool,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use parking_lot::Mutex;
    use std::ffi::{CStr, CString, c_char, c_void};

    unsafe extern "C" {
        fn lookout_add_menu_show(items_json: *const c_char, ns_window: *mut c_void, x: f64, y: f64, w: f64, h: f64, cb: extern "C" fn(*const c_char));
        fn lookout_add_menu_prefetch_icons(urls_json: *const c_char);
    }

    static PENDING: Mutex<Option<flume::Sender<Option<String>>>> = Mutex::new(None);

    extern "C" fn on_select(id: *const c_char) {
        let v = (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned());
        if let Some(tx) = PENDING.lock().take() {
            let _ = tx.send(v);
        }
    }

    fn ns_window(window: &Window) -> Option<*mut c_void> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::AppKit(h) => {
                let view = h.ns_view.as_ptr() as *mut objc2::runtime::AnyObject;
                let win: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![view, window] };
                (!win.is_null()).then_some(win as *mut c_void)
            }
            _ => None,
        }
    }

    pub fn show(window: &Window, entries: &[Entry], anchor: Bounds<Pixels>) -> Option<flume::Receiver<Option<String>>> {
        let json = CString::new(serde_json::to_string(entries).ok()?).ok()?;
        let win = ns_window(window)?;
        let (tx, rx) = flume::bounded(1);
        // A new menu replaces the old one; its callback never fires.
        *PENDING.lock() = Some(tx);
        unsafe {
            lookout_add_menu_show(
                json.as_ptr(),
                win,
                f32::from(anchor.origin.x) as f64,
                f32::from(anchor.origin.y) as f64,
                f32::from(anchor.size.width) as f64,
                f32::from(anchor.size.height) as f64,
                on_select,
            )
        };
        Some(rx)
    }

    pub fn prefetch(urls: &[String]) {
        if let Ok(json) = CString::new(serde_json::to_string(urls).unwrap_or_default()) {
            unsafe { lookout_add_menu_prefetch_icons(json.as_ptr()) };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn show(_window: &Window, _entries: &[Entry], _anchor: Bounds<Pixels>) -> Option<flume::Receiver<Option<String>>> {
        None
    }
    pub fn prefetch(_urls: &[String]) {}
}

pub use imp::{prefetch, show};
