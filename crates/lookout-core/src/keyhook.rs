//! Windows: how many keys were pressed, and nothing else.
//!
//! A low-level keyboard hook (`WH_KEYBOARD_LL`) on its own thread bumps one
//! counter for every key-down. It never reads, stores or logs which key it
//! was (no `vkCode`, no `scanCode`, no timing): the activity sampler swaps
//! the counter back to 0 and reports how many there were since the last
//! sample (`ActivitySample::keys`). Keys another program injected
//! (`SendInput`, `LLKHF_INJECTED`) aren't counted.
//!
//! The hook only sees keys while its thread pumps messages, and Windows
//! drops a hook whose callback stalls past `LowLevelHooksTimeout`, so the
//! thread does nothing but run the message loop.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Once;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

/// Key-downs since the sampler last took the count.
static KEYS: AtomicU32 = AtomicU32::new(0);
/// The hook is in place (so a 0 means "no keys", not "can't tell").
static ACTIVE: AtomicBool = AtomicBool::new(false);
static START: Once = Once::new();

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let msg = wparam.0 as u32;
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            // Only `flags` is read, to skip injected keys; which key it was
            // is never looked at.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if info.flags.0 & LLKHF_INJECTED.0 == 0 {
                KEYS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Start the hook thread, once. Later calls do nothing.
pub fn install() {
    START.call_once(|| {
        let spawned = std::thread::Builder::new().name("lookout-keyhook".into()).spawn(|| unsafe {
            let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) {
                Ok(h) => h,
                Err(e) => {
                    lookout_session::log_line!("[keyhook] SetWindowsHookExW failed: {e}");
                    return;
                }
            };
            KEYS.store(0, Ordering::Relaxed);
            ACTIVE.store(true, Ordering::Relaxed);
            lookout_session::log_line!("[keyhook] counting key presses");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            ACTIVE.store(false, Ordering::Relaxed);
            let _ = UnhookWindowsHookEx(hook);
        });
        if let Err(e) = spawned {
            lookout_session::log_line!("[keyhook] couldn't start the hook thread: {e}");
        }
    });
}

/// Key-downs since the last call, resetting the count. `None` while the
/// hook isn't in place.
pub(crate) fn take() -> Option<u32> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    Some(KEYS.swap(0, Ordering::Relaxed))
}
