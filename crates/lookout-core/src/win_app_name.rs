//! The display name of the app that owns a window, on Windows.
//!
//! Same answer as xcap's `Window::app_name` (the exe's `FileDescription`,
//! else `ProductName`, …, from its version resource), which is what the
//! Filtered Apps list shows and redaction matches against. We don't call
//! xcap's version: its fallbacks log at error level on every call, for
//! failures that are normal here, and redaction calls it for every window
//! on every capture tick. Sentry got thousands of them:
//! - `GetModuleBaseNameW` always fails with ACCESS_DENIED (5) on the
//!   `PROCESS_QUERY_LIMITED_INFORMATION` handle xcap opens (LOOKOUT-NATIVE-H);
//! - `GetFileVersionInfoSizeW` fails with ERROR_RESOURCE_TYPE_NOT_FOUND
//!   (1813) for any exe without a version resource (LOOKOUT-NATIVE-M, -X).
//!
//! Here the path comes from `QueryFullProcessImageNameW`, which a limited
//! handle is enough for, an exe without a usable version resource is named
//! by its file stem (xcap ended up with the full path), and the result is
//! cached per exe so the version resource is read once, not every tick.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

/// Exe path -> name. Bounded: cleared if it ever grows past this.
const CACHE_LIMIT: usize = 512;
static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// The app name for the window with xcap id `window_id` (its HWND), or
/// `""` when the owning process can't be opened at all (protected and
/// system processes) — callers skip nameless windows, as with xcap.
pub(crate) fn app_name_for_window(window_id: u32) -> String {
    let hwnd = HWND(window_id as usize as *mut _);
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }
    let Some(path) = process_image_path(pid) else { return String::new() };

    if let Some(name) = CACHE.lock().ok().and_then(|c| c.as_ref()?.get(&path).cloned()) {
        return name;
    }
    let name = version_name(&path).unwrap_or_else(|| crate::sources::file_stem(&path).to_string());
    if let Ok(mut cache) = CACHE.lock() {
        let cache = cache.get_or_insert_with(HashMap::new);
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(path, name.clone());
    }
    name
}

/// Full Win32 path of `pid`'s exe. A limited-information handle is all this
/// needs, so it works for elevated processes too.
fn process_image_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        (!path.is_empty()).then_some(path)
    }
}

#[repr(C)]
struct LangCodePage {
    language: u16,
    code_page: u16,
}

/// The first non-empty string, in xcap's order, from the exe's version
/// resource. `None` when it has none (error 1813, common for small and
/// self-built tools) or none of the keys are set.
fn version_name(path: &str) -> Option<String> {
    let wide = HSTRING::from(path);
    let path = PCWSTR(wide.as_ptr());
    unsafe {
        let size = GetFileVersionInfoSizeW(path, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(path, None, size, data.as_mut_ptr().cast()).ok()?;

        let mut pages_ptr = std::ptr::null_mut();
        let mut pages_len = 0u32;
        if !VerQueryValueW(
            data.as_ptr().cast(),
            &HSTRING::from("\\VarFileInfo\\Translation"),
            &mut pages_ptr,
            &mut pages_len,
        )
        .as_bool()
            || pages_ptr.is_null()
        {
            return None;
        }
        // `pages_len` is in bytes here.
        let count = pages_len as usize / std::mem::size_of::<LangCodePage>();
        let pages = std::slice::from_raw_parts(pages_ptr as *const LangCodePage, count);

        for key in ["FileDescription", "ProductName", "ProductShortName", "InternalName", "OriginalFilename"] {
            for page in pages {
                let query = HSTRING::from(format!(
                    "\\StringFileInfo\\{:04x}{:04x}\\{key}",
                    page.language, page.code_page
                ));
                let mut value_ptr = std::ptr::null_mut();
                let mut value_len = 0u32;
                if !VerQueryValueW(data.as_ptr().cast(), &query, &mut value_ptr, &mut value_len).as_bool()
                    || value_ptr.is_null()
                {
                    continue;
                }
                // `value_len` is in characters, including the terminator.
                let value = std::slice::from_raw_parts(value_ptr as *const u16, value_len as usize);
                let end = value.iter().position(|&c| c == 0).unwrap_or(value.len());
                let value = String::from_utf16_lossy(&value[..end]);
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        None
    }
}
