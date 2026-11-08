//! Interaction metadata: where the pointer is, which app is in front, and
//! whether the windows being recorded have the focus. Sampled by the
//! capture loop at every frame wake and sent with each confirm as
//! `activity`, which the server keeps for programs only (like the JA4).
//!
//! Each platform takes a [`Snapshot`] in its own coordinate space and
//! [`build`] turns it into the sample, so the rules (which display the
//! pointer is on, what counts as "captured") are the same everywhere and
//! tested here. A platform that can't tell something leaves it out rather
//! than guessing: Wayland (no global pointer or focus for clients) and
//! cameras give no sample at all.
//!
//! Coordinate spaces, all consistent within one snapshot:
//! - macOS: global display points, origin top-left of the main display
//!   (`CGDisplayBounds`, `CGEventGetLocation`, `kCGWindowBounds`).
//! - Windows: physical pixels — the sampling thread switches itself to
//!   per-monitor DPI awareness so `GetCursorPos`, `GetMonitorInfoW` and
//!   `GetWindowRect` all report unscaled values.
//! - X11: root-window pixels (`QueryPointer`, RandR monitors).

use lookout_recorder::host::{ActivitySample, CursorSample, FrontmostSample, WindowFocus};
use lookout_session::source::CaptureSource;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    fn centre(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Squared distance from a point to the rectangle (0 inside).
    fn distance_sq(&self, px: f64, py: f64) -> f64 {
        let dx = (self.x - px).max(px - (self.x + self.w)).max(0.0);
        let dy = (self.y - py).max(py - (self.y + self.h)).max(0.0);
        dx * dx + dy * dy
    }
}

/// One display. Several ids when one display is fed by several outputs
/// (X11 mirroring): the capture-source id may be any of them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Display {
    pub ids: Vec<u32>,
    pub rect: Rect,
}

/// What a platform could see at one instant.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub method: &'static str,
    pub displays: Vec<Display>,
    pub cursor: Option<(f64, f64)>,
    /// The topmost window under the pointer, plus the windows it belongs
    /// to (owner, top-level ancestor), as capture-source window ids.
    pub under_cursor: Vec<u32>,
    /// The focused window, plus the windows it belongs to, as
    /// capture-source window ids. Empty when nothing has the focus.
    pub focused: Vec<u32>,
    pub focused_rect: Option<Rect>,
    /// The frontmost app's id (bundle id / exe name / WM_CLASS).
    pub app: Option<String>,
    /// Captured windows whose focus couldn't be told (Wayland, where the
    /// portal doesn't say which window a stream is). Left out of
    /// `windows` rather than guessed.
    pub unknown_windows: Vec<u32>,
    /// The capture sources as this snapshot names them, when that differs
    /// from what was recorded (Wayland's PipeWire nodes, resolved to the
    /// screens and windows they are).
    pub sources: Option<Vec<CaptureSource>>,
}

/// A screencast stream the portal opened (Wayland), and the PipeWire
/// socket that reaches it.
/// `None` when the session has no pointer remote (no metadata cursor mode).
pub(crate) type PortalStream = (crate::screencast::StreamInfo, Option<i32>);

/// Fractions are rounded to 1/10000 (a hundredth of a percent): finer than
/// any pointer, and it keeps the payload short.
fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// The display a point is on, or the nearest one when it's in a gap
/// between displays (a pointer is never really off every screen).
fn display_at(displays: &[Display], px: f64, py: f64) -> Option<&Display> {
    displays.iter().find(|d| d.rect.contains(px, py)).or_else(|| {
        displays
            .iter()
            .filter(|d| d.rect.w > 0.0 && d.rect.h > 0.0)
            .min_by(|a, b| a.rect.distance_sq(px, py).total_cmp(&b.rect.distance_sq(px, py)))
    })
}

/// Turn a platform snapshot into the sample for these capture sources.
pub(crate) fn build(snap: &Snapshot, sources: &[CaptureSource]) -> ActivitySample {
    let sources = snap.sources.as_deref().unwrap_or(sources);
    let monitors: Vec<u32> = sources
        .iter()
        .filter_map(|s| match s {
            CaptureSource::Monitor { id } => Some(*id),
            _ => None,
        })
        .collect();
    let windows: Vec<u32> = sources
        .iter()
        .filter_map(|s| match s {
            CaptureSource::Window { id } => Some(*id),
            _ => None,
        })
        .collect();
    let display_captured = |d: &Display| d.ids.iter().any(|id| monitors.contains(id));

    let cursor = snap.cursor.and_then(|(px, py)| {
        let d = display_at(&snap.displays, px, py)?;
        if d.rect.w <= 0.0 || d.rect.h <= 0.0 {
            return None;
        }
        // Name the display by the id it was captured under, if it was.
        let id = d
            .ids
            .iter()
            .find(|id| monitors.contains(id))
            .or_else(|| d.ids.first())
            .copied()?;
        let on_display = d.rect.contains(px, py);
        Some(CursorSample {
            display: id,
            x: round4(((px - d.rect.x) / d.rect.w).clamp(0.0, 1.0)),
            y: round4(((py - d.rect.y) / d.rect.h).clamp(0.0, 1.0)),
            in_capture: (on_display && display_captured(d))
                || snap.under_cursor.iter().any(|w| windows.contains(w)),
        })
    });

    let focused_captured = snap.focused.iter().any(|w| windows.contains(w));
    let centre_captured = snap.focused_rect.is_some_and(|rect| {
        let (cx, cy) = rect.centre();
        snap.displays
            .iter()
            .find(|d| d.rect.contains(cx, cy))
            .is_some_and(display_captured)
    });
    let captured = if focused_captured || centre_captured {
        Some(true)
    } else if !snap.unknown_windows.is_empty() {
        // It may be one of the windows we can't place.
        None
    } else if snap.focused_rect.is_some() {
        Some(false)
    } else if !snap.focused.is_empty() && monitors.is_empty() {
        // Not a captured window, and no display is captured.
        Some(false)
    } else {
        None
    };
    let frontmost = snap.app.as_ref().map(|app| FrontmostSample {
        app: app.clone(),
        captured,
    });

    ActivitySample {
        at: 0,
        method: snap.method,
        cursor,
        frontmost,
        windows: windows
            .iter()
            .filter(|id| !snap.unknown_windows.contains(id))
            .map(|&id| WindowFocus {
                id,
                active: snap.focused.contains(&id),
            })
            .collect(),
    }
}

/// Sample now for these capture sources. Blocking (window-server round
/// trips; on macOS an Accessibility query bounded at 250ms) — run it off
/// the async workers. `None` where the platform can't see the pointer.
/// `streams` are the live portal streams (Linux; empty elsewhere).
pub(crate) fn sample(sources: &[CaptureSource], streams: &[PortalStream]) -> Option<ActivitySample> {
    let snap = platform::snapshot(sources, streams)?;
    Some(build(&snap, sources))
}

// ── macOS ───────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    use objc2_app_kit::NSWorkspace;
    use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFString, CGRect};
    use objc2_core_graphics::{
        CGDisplayBounds, CGEvent, CGGetActiveDisplayList, CGWindowListCopyWindowInfo, CGWindowListOption,
    };
    use lookout_session::source::CaptureSource;

    use super::{Display, Rect, Snapshot};
    use crate::sources::{dict_i32, get_cf_dictionary_get_value, window_bounds};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
        fn AXUIElementSetMessagingTimeout(element: *const c_void, timeout_seconds: f32) -> i32;
        fn AXUIElementCopyAttributeValue(
            element: *const c_void,
            attribute: *const c_void,
            value: *mut *const c_void,
        ) -> i32;
        /// The CGWindowID behind an AX window. Private, but stable since
        /// 10.5 and what every window manager on macOS relies on.
        fn _AXUIElementGetWindow(element: *const c_void, window_id: *mut u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    fn rect(r: CGRect) -> Rect {
        Rect { x: r.origin.x, y: r.origin.y, w: r.size.width, h: r.size.height }
    }

    struct Entry {
        id: u32,
        pid: i32,
        layer: i32,
        alpha: f64,
        rect: Rect,
    }

    fn dict_f64(dict: &CFDictionary, key: &str) -> Option<f64> {
        let n = get_cf_dictionary_get_value(dict, key)? as *const CFNumber;
        let mut v: f64 = 0.0;
        let ok = unsafe { (*n).value(CFNumberType::Float64Type, &mut v as *mut f64 as *mut c_void) };
        ok.then_some(v)
    }

    /// On-screen windows, front to back.
    fn window_list() -> Vec<Entry> {
        let Some(entries) = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
            0,
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..entries.count() {
            let dict_ref = unsafe { entries.value_at_index(i) } as *const CFDictionary;
            if dict_ref.is_null() {
                continue;
            }
            let dict = unsafe { &*dict_ref };
            let (Some(id), Some(pid), Some(bounds)) = (
                dict_i32(dict, "kCGWindowNumber"),
                dict_i32(dict, "kCGWindowOwnerPID"),
                window_bounds(dict),
            ) else {
                continue;
            };
            out.push(Entry {
                id: id as u32,
                pid,
                layer: dict_i32(dict, "kCGWindowLayer").unwrap_or(0),
                alpha: dict_f64(dict, "kCGWindowAlpha").unwrap_or(1.0),
                rect: rect(bounds),
            });
        }
        out
    }

    fn displays() -> Vec<Display> {
        let mut ids = [0u32; 32];
        let mut count = 0u32;
        let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
        if err.0 != 0 {
            return Vec::new();
        }
        ids[..(count as usize).min(ids.len())]
            .iter()
            .map(|&id| Display { ids: vec![id], rect: rect(CGDisplayBounds(id)) })
            .collect()
    }

    /// The focused window of `pid`, by Accessibility.
    fn ax_focused_window(pid: i32) -> Option<u32> {
        unsafe {
            let app = AXUIElementCreateApplication(pid);
            if app.is_null() {
                return None;
            }
            // A hung app must not hang the sampler.
            AXUIElementSetMessagingTimeout(app, 0.25);
            let attr = CFString::from_str("AXFocusedWindow");
            let mut window: *const c_void = std::ptr::null();
            let err = AXUIElementCopyAttributeValue(app, (&*attr as *const CFString).cast(), &mut window);
            CFRelease(app);
            if err != 0 || window.is_null() {
                return None;
            }
            let mut id = 0u32;
            let err = _AXUIElementGetWindow(window, &mut id);
            CFRelease(window);
            (err == 0 && id != 0).then_some(id)
        }
    }

    pub(super) fn snapshot(_sources: &[CaptureSource], _streams: &[super::PortalStream]) -> Option<Snapshot> {
        let cursor = CGEvent::new(None).map(|e| {
            let p = CGEvent::location(Some(&e));
            (p.x, p.y)
        });
        let list = window_list();

        #[allow(unused_unsafe)]
        let front = unsafe { NSWorkspace::sharedWorkspace().frontmostApplication() };
        #[allow(unused_unsafe)]
        let (pid, app) = match &front {
            Some(a) => unsafe {
                let app = a
                    .bundleIdentifier()
                    .or_else(|| a.localizedName())
                    .map(|s| s.to_string());
                (Some(a.processIdentifier()), app)
            },
            None => (None, None),
        };

        // Accessibility names the focused window exactly. Without it (or
        // when the app won't answer), the frontmost app's topmost normal
        // window — the same window except with utility panels or several
        // windows mid-switch.
        let ax = pid.filter(|_| unsafe { AXIsProcessTrusted() }).and_then(ax_focused_window);
        let (method, focused_id) = match ax {
            Some(id) => ("ax", Some(id)),
            None => (
                "window-order",
                pid.and_then(|pid| {
                    list.iter().find(|e| e.pid == pid && e.layer == 0 && e.alpha > 0.0).map(|e| e.id)
                }),
            ),
        };
        let focused_rect = focused_id.and_then(|id| list.iter().find(|e| e.id == id)).map(|e| e.rect);

        // The topmost visible window at the pointer: the menu bar, a
        // popover or another app's window in front all count as covering.
        let under_cursor = cursor
            .and_then(|(x, y)| list.iter().find(|e| e.alpha > 0.01 && e.rect.contains(x, y)))
            .map(|e| vec![e.id])
            .unwrap_or_default();

        Some(Snapshot {
            method,
            displays: displays(),
            cursor,
            under_cursor,
            focused: focused_id.into_iter().collect(),
            focused_rect,
            app,
            ..Default::default()
        })
    }
}

// ── Windows ─────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
        WindowFromPoint, GA_ROOT, GA_ROOTOWNER,
    };
    use lookout_session::source::CaptureSource;

    use super::{Display, Rect, Snapshot};

    fn rect(r: RECT) -> Rect {
        Rect {
            x: r.left as f64,
            y: r.top as f64,
            w: (r.right - r.left) as f64,
            h: (r.bottom - r.top) as f64,
        }
    }

    /// xcap's window id for an HWND.
    fn id(hwnd: HWND) -> u32 {
        hwnd.0 as usize as u32
    }

    /// `hwnd`, its top-level window and that window's owner, as ids.
    fn lineage(hwnd: HWND) -> Vec<u32> {
        let mut ids = vec![id(hwnd)];
        for flag in [GA_ROOT, GA_ROOTOWNER] {
            let h = unsafe { GetAncestor(hwnd, flag) };
            if !h.is_invalid() && !ids.contains(&id(h)) {
                ids.push(id(h));
            }
        }
        ids
    }

    unsafe extern "system" fn monitor_proc(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<Display>) };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
            out.push(Display { ids: vec![m.0 as usize as u32], rect: rect(info.rcMonitor) });
        }
        true.into()
    }

    fn displays() -> Vec<Display> {
        let mut out: Vec<Display> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(monitor_proc), LPARAM(&mut out as *mut _ as isize));
        }
        out
    }

    fn pid_of(hwnd: HWND) -> u32 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        pid
    }

    fn exe_name(pid: u32) -> Option<String> {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
            let _ = CloseHandle(process);
            ok.ok()?;
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit(['\\', '/']).next().map(str::to_string)
        }
    }

    struct HostedSearch {
        host_pid: u32,
        found: u32,
    }

    unsafe extern "system" fn child_proc(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = unsafe { &mut *(data.0 as *mut HostedSearch) };
        let pid = pid_of(hwnd);
        if pid != 0 && pid != search.host_pid {
            search.found = pid;
            return false.into();
        }
        true.into()
    }

    /// The process that owns what's in `hwnd`: for a UWP app that is not
    /// the frame's ApplicationFrameHost.exe but the app inside it.
    fn app_of(hwnd: HWND) -> Option<String> {
        let pid = pid_of(hwnd);
        let name = exe_name(pid)?;
        if name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
            let mut search = HostedSearch { host_pid: pid, found: 0 };
            unsafe {
                let _ = EnumChildWindows(Some(hwnd), Some(child_proc), LPARAM(&mut search as *mut _ as isize));
            }
            if search.found != 0 {
                if let Some(hosted) = exe_name(search.found) {
                    return Some(hosted);
                }
            }
        }
        Some(name)
    }

    /// Restores the thread's DPI awareness when dropped.
    struct DpiScope(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);

    impl Drop for DpiScope {
        fn drop(&mut self) {
            if !self.0 .0.is_null() {
                unsafe { SetThreadDpiAwarenessContext(self.0) };
            }
        }
    }

    pub(super) fn snapshot(_sources: &[CaptureSource], _streams: &[super::PortalStream]) -> Option<Snapshot> {
        // Physical pixels for every call below, whatever the process is.
        let _dpi = DpiScope(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) });

        let mut pt = POINT::default();
        let cursor = unsafe { GetCursorPos(&mut pt) }.ok().map(|_| (pt.x as f64, pt.y as f64));
        let under_cursor = cursor
            .map(|_| unsafe { WindowFromPoint(pt) })
            .filter(|h| !h.is_invalid())
            .map(lineage)
            .unwrap_or_default();

        // No foreground window on the lock screen, UAC, or mid-switch.
        let fg = unsafe { GetForegroundWindow() };
        let (focused, focused_rect, app) = if fg.is_invalid() {
            (Vec::new(), None, None)
        } else {
            let mut r = RECT::default();
            let focused_rect = unsafe { GetWindowRect(fg, &mut r) }.ok().map(|_| rect(r));
            (lineage(fg), focused_rect, app_of(fg))
        };

        Some(Snapshot {
            method: "win32",
            displays: displays(),
            cursor,
            under_cursor,
            focused,
            focused_rect,
            app,
            ..Default::default()
        })
    }
}

// ── Linux (X11) ─────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Mutex;

    use lookout_session::source::CaptureSource;
    use xcb::{randr, x, Xid, XidNew};

    use super::{Display, Rect, Snapshot};

    /// One X connection, reused across samples; dropped on any error so the
    /// next sample reconnects (an X server restart, a lost link).
    static CONN: Mutex<Option<(xcb::Connection, i32)>> = Mutex::new(None);

    /// Wayland gives clients no global pointer or focus, and XWayland only
    /// knows about X windows — anything read there would be wrong.
    /// The login session's own type wins; without one (WSLg, a bare
    /// compositor), a Wayland socket means Wayland.
    fn is_wayland() -> bool {
        match std::env::var("XDG_SESSION_TYPE") {
            Ok(t) if !t.is_empty() => t.eq_ignore_ascii_case("wayland"),
            _ => std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
        }
    }

    pub(super) fn snapshot(sources: &[CaptureSource], streams: &[super::PortalStream]) -> Option<Snapshot> {
        if is_wayland() {
            return super::wayland::snapshot(sources, streams);
        }
        let mut guard = CONN.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = xcb::Connection::connect_with_extensions(None, &[], &[xcb::Extension::RandR]).ok();
        }
        let (conn, screen) = guard.as_ref()?;
        match snapshot_with(conn, *screen, sources) {
            Ok(s) => Some(s),
            Err(_) => {
                *guard = None;
                None
            }
        }
    }

    fn atom(conn: &xcb::Connection, name: &[u8]) -> xcb::Result<x::Atom> {
        Ok(conn
            .wait_for_reply(conn.send_request(&x::InternAtom { only_if_exists: true, name }))?
            .atom())
    }

    /// The top-level ancestor of `w` (the window manager's frame when it
    /// reparents): what `QueryPointer` names as the root's child.
    fn toplevel(conn: &xcb::Connection, mut w: x::Window) -> xcb::Result<x::Window> {
        for _ in 0..32 {
            let tree = conn.wait_for_reply(conn.send_request(&x::QueryTree { window: w }))?;
            if tree.parent() == tree.root() || tree.parent().is_none() {
                return Ok(w);
            }
            w = tree.parent();
        }
        Ok(w)
    }

    fn snapshot_with(conn: &xcb::Connection, screen: i32, sources: &[CaptureSource]) -> xcb::Result<Snapshot> {
        let Some(root) = conn.get_setup().roots().nth(screen.max(0) as usize).map(|s| s.root()) else {
            return Ok(Snapshot::default());
        };

        let displays = conn
            .wait_for_reply(conn.send_request(&randr::GetMonitors { window: root, get_active: true }))
            .map(|reply| {
                reply
                    .monitors()
                    .map(|m| Display {
                        ids: m.outputs().iter().map(|o| o.resource_id()).collect(),
                        rect: Rect {
                            x: m.x() as f64,
                            y: m.y() as f64,
                            w: m.width() as f64,
                            h: m.height() as f64,
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();

        let pointer = conn.wait_for_reply(conn.send_request(&x::QueryPointer { window: root }))?;
        let cursor = pointer
            .same_screen()
            .then(|| (pointer.root_x() as f64, pointer.root_y() as f64));
        let mut under_cursor = Vec::new();
        if cursor.is_some() && !pointer.child().is_none() {
            let child = pointer.child();
            under_cursor.push(child.resource_id());
            // Captured windows are clients; the pointer names their frame.
            for source in sources {
                if let CaptureSource::Window { id } = source {
                    let w = x::Window::new(*id);
                    if toplevel(conn, w).ok() == Some(child) {
                        under_cursor.push(*id);
                    }
                }
            }
        }

        let active_atom = atom(conn, b"_NET_ACTIVE_WINDOW")?;
        let active = if active_atom == x::ATOM_NONE {
            None
        } else {
            conn.wait_for_reply(conn.send_request(&x::GetProperty {
                delete: false,
                window: root,
                property: active_atom,
                r#type: x::ATOM_WINDOW,
                long_offset: 0,
                long_length: 1,
            }))
            .ok()
            .and_then(|r| r.value::<x::Window>().first().copied())
            .filter(|w| !w.is_none())
        };

        let (focused, focused_rect, app) = match active {
            None => (Vec::new(), None, None),
            Some(w) => {
                let origin = conn
                    .wait_for_reply(conn.send_request(&x::TranslateCoordinates {
                        src_window: w,
                        dst_window: root,
                        src_x: 0,
                        src_y: 0,
                    }))
                    .ok();
                let geometry = conn
                    .wait_for_reply(conn.send_request(&x::GetGeometry { drawable: x::Drawable::Window(w) }))
                    .ok();
                let rect = origin.zip(geometry).map(|(o, g)| Rect {
                    x: o.dst_x() as f64,
                    y: o.dst_y() as f64,
                    w: g.width() as f64,
                    h: g.height() as f64,
                });
                // WM_CLASS is "instance\0class\0"; the class names the app.
                let app = conn
                    .wait_for_reply(conn.send_request(&x::GetProperty {
                        delete: false,
                        window: w,
                        property: x::ATOM_WM_CLASS,
                        r#type: x::ATOM_STRING,
                        long_offset: 0,
                        long_length: 256,
                    }))
                    .ok()
                    .and_then(|r| {
                        let parts: Vec<String> = r
                            .value::<u8>()
                            .split(|b| *b == 0)
                            .filter(|p| !p.is_empty())
                            .map(|p| String::from_utf8_lossy(p).into_owned())
                            .collect();
                        parts.get(1).or_else(|| parts.first()).cloned()
                    });
                (vec![w.resource_id()], rect, app)
            }
        };

        Ok(Snapshot {
            method: "x11",
            displays,
            cursor,
            under_cursor,
            focused,
            focused_rect,
            app,
            ..Default::default()
        })
    }
}

// ── Linux (Wayland) ─────────────────────────────────────────────────────

/// Wayland gives clients neither the pointer nor the focus. Two sources
/// make up for it: the screencast streams themselves, which report the
/// pointer over them when the portal grants the "metadata" cursor mode
/// (pw_cursor.rs), and the compositor, asked its own way (compositor.rs).
/// The portal never says which window a window stream is, so whether a
/// captured window has the focus is worked out from geometry, and left
/// unknown when the geometry can't tell.
#[cfg(target_os = "linux")]
pub(crate) mod wayland {
    use lookout_session::source::CaptureSource;

    use super::{Display, PortalStream, Rect, Snapshot};
    use crate::compositor::CompositorState;
    use crate::pw_cursor::StreamCursor;
    use crate::screencast::StreamInfo;

    /// A monitor stream matches a compositor monitor this closely (logical
    /// pixels; fractional scaling rounds).
    const MONITOR_SLOP: f64 = 2.0;
    /// The pointer's place in the focused window and in a window stream
    /// agree this closely (fractions) when they are the same window —
    /// loose enough for client-side shadows a window stream may include.
    const POINTER_SLOP: f64 = 0.08;
    /// Aspect ratios further apart than this can't be the same window.
    const ASPECT_SLOP: f64 = 0.10;

    pub(crate) fn snapshot(sources: &[CaptureSource], streams: &[PortalStream]) -> Option<Snapshot> {
        let keep: Vec<u32> = streams.iter().filter(|(_, fd)| fd.is_some()).map(|(i, _)| i.node_id).collect();
        let observed: Vec<(StreamInfo, Option<StreamCursor>)> = streams
            .iter()
            .filter(|(i, _)| sources.iter().any(|s| matches!(s, CaptureSource::PipeWire { id } if *id == i.node_id)))
            .map(|(info, fd)| (info.clone(), fd.and_then(|fd| crate::pw_cursor::cursor(info.node_id, fd, &keep))))
            .collect();
        let compositor = crate::compositor::query();
        assemble(sources, compositor.as_ref(), &observed)
    }

    fn is_window(info: &StreamInfo) -> bool {
        match info.source_type.as_deref() {
            Some(t) => t == "window",
            // Older portals don't say; only monitor streams have a place.
            None => info.position.is_none(),
        }
    }

    fn logical_rect(info: &StreamInfo) -> Option<Rect> {
        let (x, y) = info.position?;
        let (w, h) = info.size?;
        Some(Rect { x: x as f64, y: y as f64, w: w as f64, h: h as f64 })
    }

    /// The pointer in stream pixels, if it is over the stream.
    fn pointer_in(c: &StreamCursor) -> Option<(f64, f64, f64, f64)> {
        let (x, y) = c.position?;
        let (w, h) = c.size?;
        let (x, y, w, h) = (x as f64, y as f64, w as f64, h as f64);
        (w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 && x < w && y < h).then_some((x, y, w, h))
    }

    fn close(a: &Rect, b: &Rect) -> bool {
        (a.x - b.x).abs() <= MONITOR_SLOP
            && (a.y - b.y).abs() <= MONITOR_SLOP
            && (a.w - b.w).abs() <= MONITOR_SLOP
            && (a.h - b.h).abs() <= MONITOR_SLOP
    }

    /// Whether a captured window stream is the focused window: `Some` when
    /// the geometry says so either way, `None` when it can't tell.
    fn window_has_focus(compositor: &CompositorState, cursor: Option<&StreamCursor>) -> Option<bool> {
        let Some(focus) = &compositor.focus else {
            // Nothing has the focus, so neither does this window.
            return Some(false);
        };
        let fr = focus.rect.filter(|r| r.w > 0.0 && r.h > 0.0)?;
        let cursor = cursor?;
        if let (Some((gx, gy)), Some((sx, sy, sw, sh))) = (compositor.pointer, pointer_in(cursor)) {
            // The pointer is over the stream: it is the focused window only
            // if the pointer sits at the same place in both.
            let same = ((gx - fr.x) / fr.w - sx / sw).abs() <= POINTER_SLOP
                && ((gy - fr.y) / fr.h - sy / sh).abs() <= POINTER_SLOP;
            return Some(same);
        }
        if let Some((gx, gy)) = compositor.pointer {
            // The stream reports the pointer, and it isn't over it — yet the
            // pointer is well inside the focused window: a different window.
            let inset = Rect { x: fr.x + fr.w * POINTER_SLOP, y: fr.y + fr.h * POINTER_SLOP, w: fr.w * (1.0 - 2.0 * POINTER_SLOP), h: fr.h * (1.0 - 2.0 * POINTER_SLOP) };
            if cursor.has_metadata && cursor.position.is_none() && inset.contains(gx, gy) {
                return Some(false);
            }
        }
        let (sw, sh) = cursor.size?;
        if sw == 0 || sh == 0 {
            return None;
        }
        let ratio = (fr.w / fr.h) / (sw as f64 / sh as f64);
        // A different shape is a different window; the same shape could be
        // any same-sized window, so that stays unknown.
        if (ratio - 1.0).abs() > ASPECT_SLOP {
            Some(false)
        } else {
            None
        }
    }

    pub(crate) fn assemble(
        sources: &[CaptureSource],
        compositor: Option<&CompositorState>,
        observed: &[(StreamInfo, Option<StreamCursor>)],
    ) -> Option<Snapshot> {
        let stream = |id: u32| observed.iter().find(|(i, _)| i.node_id == id);
        let translated: Vec<CaptureSource> = sources
            .iter()
            .map(|s| match s {
                CaptureSource::PipeWire { id } => match stream(*id) {
                    Some((info, _)) if is_window(info) => CaptureSource::Window { id: *id },
                    _ => CaptureSource::Monitor { id: *id },
                },
                other => other.clone(),
            })
            .collect();
        let monitor_streams: Vec<&(StreamInfo, Option<StreamCursor>)> =
            observed.iter().filter(|(i, _)| !is_window(i)).collect();
        let window_streams: Vec<&(StreamInfo, Option<StreamCursor>)> =
            observed.iter().filter(|(i, _)| is_window(i)).collect();

        let mut snap = Snapshot {
            method: compositor.map_or("portal", |c| c.method),
            sources: Some(translated),
            ..Default::default()
        };

        // The layout, in logical coordinates: the compositor's monitors
        // (named by the stream that records them, if one does), else the
        // monitor streams' own places.
        let layout: Vec<Display> = match compositor.filter(|c| !c.monitors.is_empty()) {
            Some(c) => c
                .monitors
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let ids: Vec<u32> = monitor_streams
                        .iter()
                        .filter(|(info, _)| logical_rect(info).is_some_and(|m| close(&m, r)))
                        .map(|(info, _)| info.node_id)
                        .collect();
                    Display { ids: if ids.is_empty() { vec![i as u32] } else { ids }, rect: *r }
                })
                .collect(),
            None => monitor_streams
                .iter()
                .filter_map(|(info, _)| Some(Display { ids: vec![info.node_id], rect: logical_rect(info)? }))
                .collect(),
        };

        // A monitor stream's pointer, carried into the logical layout.
        let from_monitor_stream = monitor_streams.iter().find_map(|(info, c)| {
            let (x, y, w, h) = pointer_in(c.as_ref()?)?;
            let r = logical_rect(info)?;
            Some((r.x + x / w * r.w, r.y + y / h * r.h))
        });

        let mut logical = true;
        if let Some(p) = compositor.and_then(|c| c.pointer).or(from_monitor_stream) {
            snap.cursor = Some(p);
            snap.displays = layout;
        } else if let Some((info, (x, y, w, h))) = observed
            .iter()
            .find_map(|(info, c)| Some((info, pointer_in(c.as_ref()?)?)))
        {
            // Only a stream knows where the pointer is, and only in its own
            // pixels: the stream is the "display" it is a fraction of.
            snap.cursor = Some((x, y));
            snap.displays = vec![Display { ids: vec![info.node_id], rect: Rect { x: 0.0, y: 0.0, w, h } }];
            logical = false;
        } else {
            snap.displays = layout;
        }

        for (info, c) in &window_streams {
            if c.as_ref().and_then(pointer_in).is_some() {
                snap.under_cursor.push(info.node_id);
            }
            let focus = compositor.and_then(|comp| window_has_focus(comp, c.as_ref()));
            match focus {
                Some(true) => snap.focused.push(info.node_id),
                Some(false) => {}
                None => snap.unknown_windows.push(info.node_id),
            }
        }

        if let Some(c) = compositor {
            if let Some(f) = &c.focus {
                snap.app = f.app.clone();
                if logical {
                    snap.focused_rect = f.rect;
                }
            }
        }

        let knows_something = snap.cursor.is_some() || compositor.is_some();
        knows_something.then_some(snap)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub(super) fn snapshot(
        _sources: &[lookout_session::source::CaptureSource],
        _streams: &[super::PortalStream],
    ) -> Option<super::Snapshot> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_displays() -> Vec<Display> {
        vec![
            Display { ids: vec![1], rect: Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 } },
            // Right of the first, taller and offset upward.
            Display { ids: vec![2], rect: Rect { x: 1920.0, y: -200.0, w: 2560.0, h: 1440.0 } },
        ]
    }

    #[test]
    fn the_pointer_is_a_fraction_of_its_own_display() {
        let snap = Snapshot {
            method: "test",
            displays: two_displays(),
            cursor: Some((1920.0 + 640.0, -200.0 + 1080.0)),
            ..Default::default()
        };
        let s = build(&snap, &[CaptureSource::Monitor { id: 1 }]);
        let c = s.cursor.unwrap();
        assert_eq!(c.display, 2);
        assert_eq!(c.x, 0.25);
        assert_eq!(c.y, 0.75);
        assert!(!c.in_capture, "display 2 isn't recorded");

        let s = build(&snap, &[CaptureSource::Monitor { id: 2 }]);
        assert!(s.cursor.unwrap().in_capture);
    }

    #[test]
    fn a_pointer_in_a_gap_snaps_to_the_nearest_display_and_is_not_in_capture() {
        // Below display 1's bottom edge but beside display 2's lower part:
        // between them, nearest is display 1.
        let snap = Snapshot {
            method: "test",
            displays: two_displays(),
            cursor: Some((1900.0, 1100.0)),
            ..Default::default()
        };
        let c = build(&snap, &[CaptureSource::Monitor { id: 1 }]).cursor.unwrap();
        assert_eq!(c.display, 1);
        assert_eq!((c.x, c.y), (round4(1900.0 / 1920.0), 1.0));
        assert!(!c.in_capture);
    }

    #[test]
    fn a_window_counts_as_under_the_pointer_only_when_it_is_on_top_there() {
        let snap = Snapshot {
            method: "test",
            displays: two_displays(),
            cursor: Some((100.0, 100.0)),
            under_cursor: vec![77],
            focused: vec![77],
            app: Some("com.microsoft.VSCode".into()),
            ..Default::default()
        };
        let s = build(&snap, &[CaptureSource::Window { id: 77 }]);
        assert!(s.cursor.as_ref().unwrap().in_capture);
        assert_eq!(s.windows, vec![WindowFocus { id: 77, active: true }]);
        assert_eq!(s.frontmost.as_ref().unwrap().captured, Some(true));

        let covered = Snapshot { under_cursor: vec![12], focused: vec![12], ..snap };
        let s = build(&covered, &[CaptureSource::Window { id: 77 }]);
        assert!(!s.cursor.unwrap().in_capture);
        assert_eq!(s.windows, vec![WindowFocus { id: 77, active: false }]);
        assert_eq!(s.frontmost.unwrap().captured, Some(false));
    }

    #[test]
    fn with_screens_recorded_the_focused_window_counts_by_where_its_centre_is() {
        let snap = Snapshot {
            method: "test",
            displays: two_displays(),
            focused: vec![5],
            // Mostly on display 2.
            focused_rect: Some(Rect { x: 1800.0, y: 0.0, w: 1000.0, h: 600.0 }),
            app: Some("firefox".into()),
            ..Default::default()
        };
        assert_eq!(build(&snap, &[CaptureSource::Monitor { id: 2 }]).frontmost.unwrap().captured, Some(true));
        assert_eq!(build(&snap, &[CaptureSource::Monitor { id: 1 }]).frontmost.unwrap().captured, Some(false));
    }

    #[test]
    fn unknown_focus_stays_unknown() {
        let snap = Snapshot { method: "test", app: Some("Finder".into()), ..Default::default() };
        assert_eq!(build(&snap, &[CaptureSource::Monitor { id: 1 }]).frontmost.unwrap().captured, None);
    }

    #[test]
    fn a_mirrored_display_reports_the_output_that_was_captured() {
        let snap = Snapshot {
            method: "test",
            displays: vec![Display { ids: vec![10, 11], rect: Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 } }],
            cursor: Some((50.0, 50.0)),
            ..Default::default()
        };
        let c = build(&snap, &[CaptureSource::Monitor { id: 11 }]).cursor.unwrap();
        assert_eq!(c.display, 11);
        assert!(c.in_capture);
    }

    #[cfg(target_os = "linux")]
    mod wayland {
        use super::super::wayland::assemble;
        use super::super::*;
        use crate::compositor::{CompositorState, Focus};
        use crate::pw_cursor::StreamCursor;
        use crate::screencast::StreamInfo;

        fn monitor_stream(node: u32, x: i32, w: i32, h: i32) -> StreamInfo {
            StreamInfo { node_id: node, source_type: Some("monitor".into()), position: Some((x, 0)), size: Some((w, h)) }
        }
        fn window_stream(node: u32) -> StreamInfo {
            StreamInfo { node_id: node, source_type: Some("window".into()), position: None, size: Some((800, 600)) }
        }
        fn cursor(pos: Option<(i32, i32)>, w: u32, h: u32) -> Option<StreamCursor> {
            Some(StreamCursor { position: pos, size: Some((w, h)), has_metadata: true })
        }
        fn gnome(pointer: (f64, f64), focus: Option<(&str, Rect)>) -> CompositorState {
            CompositorState {
                method: "gnome-shell",
                monitors: vec![
                    Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 },
                    Rect { x: 1920.0, y: 0.0, w: 1280.0, h: 1024.0 },
                ],
                pointer: Some(pointer),
                focus: focus.map(|(app, rect)| Focus { app: Some(app.into()), rect: Some(rect) }),
            }
        }
        fn sample(sources: &[CaptureSource], comp: Option<&CompositorState>, obs: &[(StreamInfo, Option<StreamCursor>)]) -> ActivitySample {
            let snap = assemble(sources, comp, obs).expect("a snapshot");
            build(&snap, sources)
        }

        #[test]
        fn a_recorded_monitor_is_named_by_its_stream() {
            let comp = gnome((960.0, 270.0), Some(("org.gnome.Terminal", Rect { x: 100.0, y: 100.0, w: 800.0, h: 600.0 })));
            // The stream reports in physical pixels (2x); the layout is logical.
            let obs = [(monitor_stream(57, 0, 1920, 1080), cursor(Some((1920, 540)), 3840, 2160))];
            let s = sample(&[CaptureSource::PipeWire { id: 57 }], Some(&comp), &obs);
            let c = s.cursor.unwrap();
            assert_eq!((c.display, c.x, c.y, c.in_capture), (57, 0.5, 0.25, true));
            assert_eq!(s.method, "gnome-shell");
            let f = s.frontmost.unwrap();
            assert_eq!((f.app.as_str(), f.captured), ("org.gnome.Terminal", Some(true)));
            assert!(s.windows.is_empty());
        }

        #[test]
        fn a_window_stream_is_the_focused_window_when_the_pointer_agrees() {
            let focused = Rect { x: 2000.0, y: 100.0, w: 800.0, h: 600.0 };
            // Pointer at a quarter across, half down the focused window…
            let comp = gnome((2200.0, 400.0), Some(("code", focused)));
            // …and at the same place in the window stream.
            let obs = [(window_stream(80), cursor(Some((200, 300)), 800, 600))];
            let s = sample(&[CaptureSource::PipeWire { id: 80 }], Some(&comp), &obs);
            assert_eq!(s.windows, vec![WindowFocus { id: 80, active: true }]);
            assert!(s.cursor.unwrap().in_capture);
            assert_eq!(s.frontmost.unwrap().captured, Some(true));
        }

        #[test]
        fn a_window_stream_under_the_pointer_but_elsewhere_than_the_focus_is_not_focused() {
            let comp = gnome((2200.0, 400.0), Some(("firefox", Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 })));
            let obs = [(window_stream(80), cursor(Some((200, 300)), 800, 600))];
            let s = sample(&[CaptureSource::PipeWire { id: 80 }], Some(&comp), &obs);
            assert_eq!(s.windows, vec![WindowFocus { id: 80, active: false }]);
            assert_eq!(s.frontmost.unwrap().captured, Some(false));
        }

        #[test]
        fn a_same_shaped_focus_away_from_the_pointer_stays_unknown() {
            // Pointer off both; the focused window has the stream's shape.
            let comp = gnome((3000.0, 900.0), Some(("code", Rect { x: 100.0, y: 100.0, w: 1600.0, h: 1200.0 })));
            let obs = [(window_stream(80), cursor(None, 800, 600))];
            let s = sample(&[CaptureSource::PipeWire { id: 80 }], Some(&comp), &obs);
            assert!(s.windows.is_empty(), "unknown focus is left out, not guessed");
            assert_eq!(s.frontmost.unwrap().captured, None);
        }

        #[test]
        fn the_pointer_well_inside_another_focused_window_rules_the_stream_out() {
            let comp = gnome((500.0, 400.0), Some(("firefox", Rect { x: 0.0, y: 0.0, w: 1200.0, h: 900.0 })));
            let obs = [(window_stream(80), cursor(None, 1200, 900))];
            let s = sample(&[CaptureSource::PipeWire { id: 80 }], Some(&comp), &obs);
            assert_eq!(s.windows, vec![WindowFocus { id: 80, active: false }]);
        }

        #[test]
        fn without_a_compositor_the_stream_alone_places_the_pointer() {
            let obs = [(monitor_stream(57, 1920, 2560, 1440), cursor(Some((640, 1080)), 2560, 1440))];
            let s = sample(&[CaptureSource::PipeWire { id: 57 }], None, &obs);
            assert_eq!(s.method, "portal");
            let c = s.cursor.unwrap();
            assert_eq!((c.display, c.x, c.y, c.in_capture), (57, 0.25, 0.75, true));
            assert!(s.frontmost.is_none());
        }

        #[test]
        fn sway_rules_out_a_differently_shaped_focus_without_a_pointer() {
            let comp = CompositorState {
                method: "sway",
                monitors: vec![Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 }],
                pointer: None,
                focus: Some(Focus { app: Some("foot".into()), rect: Some(Rect { x: 0.0, y: 0.0, w: 960.0, h: 1080.0 }) }),
            };
            let obs = [(window_stream(80), Some(StreamCursor { position: None, size: Some((800, 600)), has_metadata: false }))];
            let s = sample(&[CaptureSource::PipeWire { id: 80 }], Some(&comp), &obs);
            assert!(s.cursor.is_none());
            assert_eq!(s.windows, vec![WindowFocus { id: 80, active: false }]);
            assert_eq!(s.frontmost.unwrap().app, "foot");
        }

        #[test]
        fn nothing_known_is_no_sample() {
            let obs = [(window_stream(80), None)];
            assert!(assemble(&[CaptureSource::PipeWire { id: 80 }], None, &obs).is_none());
        }
    }

    #[test]
    fn the_sample_serializes_to_the_wire_shape() {
        let snap = Snapshot {
            method: "ax",
            displays: two_displays(),
            cursor: Some((960.0, 540.0)),
            under_cursor: vec![3],
            focused: vec![3],
            app: Some("com.apple.Terminal".into()),
            ..Default::default()
        };
        let mut s = build(&snap, &[CaptureSource::Monitor { id: 1 }]);
        s.at = 1_700_000_000_000;
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({
                "at": 1_700_000_000_000i64,
                "method": "ax",
                "cursor": { "display": 1, "x": 0.5, "y": 0.5, "inCapture": true },
                "frontmost": { "app": "com.apple.Terminal", "captured": null },
            })
        );
    }
}

/// A real sample from this machine, for checking a platform by hand:
/// `cargo test -p lookout-core live_activity -- --ignored --nocapture`.
/// `LOOKOUT_ACTIVITY_SOURCES` is the capture-source JSON (default: no
/// sources, so nothing counts as captured).
#[cfg(test)]
#[test]
#[ignore]
fn live_activity() {
    let sources: Vec<CaptureSource> = std::env::var("LOOKOUT_ACTIVITY_SOURCES")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let snap = platform::snapshot(&sources, &[]);
    println!("snapshot: {snap:#?}");
    println!("sample: {}", serde_json::to_string_pretty(&snap.map(|s| build(&s, &sources))).unwrap());
}
