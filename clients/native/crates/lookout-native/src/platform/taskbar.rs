//! Windows: Lookout's taskbar button while a timelapse is open.
//!
//! - An overlay badge on the icon: a red dot while recording, an amber pause
//!   badge while paused (with a description for screen readers).
//! - Pause/Resume and Stop buttons under the taskbar thumbnail preview, like
//!   a media player's; they do what the tray / menu-bar item's buttons do.
//! - The button's progress bar runs (indeterminate) while a timelapse
//!   compiles on its page.
//!
//! Glyphs are drawn here at the taskbar's DPI and in the taskbar's own light
//! or dark colour. Button clicks reach us as `WM_COMMAND` on the main window,
//! caught by a window subclass and handed to the app through a channel, so
//! nothing touches the app from inside the window procedure.
//!
//! Elsewhere every function here is a no-op.

use gpui_kit::gpui::App;

/// What the taskbar should show for the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Recording {
    #[default]
    None,
    Live,
    Paused,
}

/// Follow the recorder (called on every recorder update).
pub fn sync(cx: &mut App) {
    #[cfg(target_os = "windows")]
    imp::sync(recording(cx), cx);
    #[cfg(not(target_os = "windows"))]
    let _ = cx;
}

/// A timelapse page saw its session compiling (or stop compiling / close).
pub fn set_compiling(token: &str, compiling: bool, cx: &mut App) {
    #[cfg(target_os = "windows")]
    imp::set_compiling(token, compiling, cx);
    #[cfg(not(target_os = "windows"))]
    let _ = (token, compiling, cx);
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn recording(cx: &App) -> Recording {
    use lookout_core::RecorderPhase;
    let rec = crate::app::state(cx).recorder.read(cx);
    match rec.state.as_ref().map(|s| s.phase) {
        Some(RecorderPhase::Recording | RecorderPhase::Resuming) => Recording::Live,
        Some(RecorderPhase::Paused | RecorderPhase::Pausing) => Recording::Paused,
        _ => Recording::None,
    }
}

/// Anti-aliased glyphs as straight (non-premultiplied) RGBA, `size`² pixels.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) mod glyph {
    #[derive(Clone, Copy)]
    pub enum Shape {
        /// The recording badge: a red disc in a white ring.
        RecordBadge,
        /// The paused badge: two white bars on an amber disc in a white ring.
        PausedBadge,
        Pause,
        Play,
        Stop,
    }

    /// Coverage of the shape at `(x, y)` in a 0..1 square.
    fn inside(shape: Shape, x: f32, y: f32) -> [f32; 4] {
        let (dx, dy) = (x - 0.5, y - 0.5);
        let r = (dx * dx + dy * dy).sqrt();
        let white = [1.0, 1.0, 1.0, 1.0];
        let none = [0.0; 4];
        match shape {
            Shape::RecordBadge => {
                if r <= 0.36 {
                    [0xef as f32 / 255.0, 0x44 as f32 / 255.0, 0x44 as f32 / 255.0, 1.0]
                } else if r <= 0.5 {
                    white
                } else {
                    none
                }
            }
            Shape::PausedBadge => {
                let inner = r <= 0.36;
                let bar = inner && ((0.36..0.46).contains(&x) || (0.54..0.64).contains(&x)) && (0.33..0.67).contains(&y);
                if bar {
                    white
                } else if inner {
                    [0xf5 as f32 / 255.0, 0x9e as f32 / 255.0, 0x0b as f32 / 255.0, 1.0]
                } else if r <= 0.5 {
                    white
                } else {
                    none
                }
            }
            Shape::Pause => {
                let on = ((0.22..0.4).contains(&x) || (0.6..0.78).contains(&x)) && (0.15..0.85).contains(&y);
                if on { white } else { none }
            }
            Shape::Play => {
                // A right-pointing triangle, 0.25..0.8 wide, centred.
                let on = x >= 0.27 && x <= 0.82 && (y - 0.5).abs() <= (0.82 - x) * 0.62;
                if on { white } else { none }
            }
            Shape::Stop => {
                let on = (0.2..0.8).contains(&x) && (0.2..0.8).contains(&y);
                if on { white } else { none }
            }
        }
    }

    /// Rasterise with 4×4 supersampling; `tint` replaces white (for glyphs on
    /// a light taskbar).
    pub fn rgba(shape: Shape, size: usize, tint: Option<[u8; 3]>) -> Vec<u8> {
        const S: usize = 4;
        let mut out = vec![0u8; size * size * 4];
        for py in 0..size {
            for px in 0..size {
                let mut acc = [0.0f32; 4];
                for sy in 0..S {
                    for sx in 0..S {
                        let x = (px as f32 + (sx as f32 + 0.5) / S as f32) / size as f32;
                        let y = (py as f32 + (sy as f32 + 0.5) / S as f32) / size as f32;
                        let c = inside(shape, x, y);
                        for k in 0..3 {
                            acc[k] += c[k] * c[3];
                        }
                        acc[3] += c[3];
                    }
                }
                let a = acc[3] / (S * S) as f32;
                let i = (py * size + px) * 4;
                if a > 0.0 {
                    let mut rgb = [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3]];
                    if let Some(t) = tint {
                        if rgb.iter().all(|v| *v > 0.99) {
                            rgb = [t[0] as f32 / 255.0, t[1] as f32 / 255.0, t[2] as f32 / 255.0];
                        }
                    }
                    out[i] = (rgb[0] * 255.0).round() as u8;
                    out[i + 1] = (rgb[1] * 255.0).round() as u8;
                    out[i + 2] = (rgb[2] * 255.0).round() as u8;
                    out[i + 3] = (a * 255.0).round() as u8;
                }
            }
        }
        out
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::sync::OnceLock;

    use gpui_kit::gpui::App;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::Shell::{
        DefSubclassProc, ITaskbarList3, SetWindowSubclass, TBPF_INDETERMINATE, TBPF_NOPROGRESS, THB_FLAGS, THB_ICON,
        THB_TOOLTIP, THBF_ENABLED, THBF_HIDDEN, THBN_CLICKED, THUMBBUTTON, TaskbarList,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIcon, DestroyIcon, HICON, RegisterWindowMessageW, SM_CXSMICON, WM_COMMAND, WM_SETTINGCHANGE,
    };
    use windows::core::{PCWSTR, w};
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Recording;
    use super::glyph::{self, Shape};
    use crate::platform::tray::TrayAction;

    const PAUSE_ID: u32 = 1;
    const STOP_ID: u32 = 2;
    const SUBCLASS_ID: usize = 0x4c4b_5442; // "LKTB"

    enum Event {
        Clicked(u32),
        /// Explorer (re)created the taskbar button: buttons must be added again.
        ButtonCreated,
        ThemeChanged,
    }

    fn channel() -> &'static (flume::Sender<Event>, flume::Receiver<Event>) {
        static CH: OnceLock<(flume::Sender<Event>, flume::Receiver<Event>)> = OnceLock::new();
        CH.get_or_init(flume::unbounded)
    }

    fn button_created_msg() -> u32 {
        static MSG: OnceLock<u32> = OnceLock::new();
        *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) })
    }

    struct Icons {
        light: bool,
        record: HICON,
        paused: HICON,
        pause: HICON,
        play: HICON,
        stop: HICON,
    }

    impl Drop for Icons {
        fn drop(&mut self) {
            for h in [self.record, self.paused, self.pause, self.play, self.stop] {
                if !h.is_invalid() {
                    let _ = unsafe { DestroyIcon(h) };
                }
            }
        }
    }

    struct State {
        hwnd: HWND,
        list: ITaskbarList3,
        icons: Option<Icons>,
        buttons_added: bool,
        shown: Option<Recording>,
        compiling: HashSet<String>,
        progress_on: bool,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
        static LISTENING: RefCell<bool> = const { RefCell::new(false) };
    }

    unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
        if msg == WM_COMMAND && ((wparam.0 >> 16) & 0xffff) as u32 == THBN_CLICKED {
            let _ = channel().0.send(Event::Clicked((wparam.0 & 0xffff) as u32));
        } else if msg == button_created_msg() {
            let _ = channel().0.send(Event::ButtonCreated);
        } else if msg == WM_SETTINGCHANGE {
            let _ = channel().0.send(Event::ThemeChanged);
        }
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    fn main_hwnd(cx: &mut App) -> Option<HWND> {
        let h = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0)?;
        h.update(cx, |_, window, _| {
            let handle = HasWindowHandle::window_handle(window).ok()?;
            match handle.as_raw() {
                RawWindowHandle::Win32(w) => Some(HWND(w.hwnd.get() as *mut _)),
                _ => None,
            }
        })
        .ok()
        .flatten()
    }

    /// The taskbar is light when Windows' "system" mode is light.
    fn taskbar_light() -> bool {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let ok = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(std::ptr::from_mut(&mut value).cast()),
                Some(&mut size),
            )
        };
        ok.is_ok() && value != 0
    }

    fn make_icon(shape: Shape, size: i32, tint: Option<[u8; 3]>) -> HICON {
        let n = size as usize;
        let rgba = glyph::rgba(shape, n, tint);
        // CreateIcon takes device-dependent, top-down rows: 32bpp BGRA with
        // alpha for the colour, and an all-zero (opaque) AND mask.
        let mut bgra = Vec::with_capacity(rgba.len());
        for p in rgba.chunks_exact(4) {
            bgra.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        let mask_row = n.div_ceil(16) * 2;
        let mask = vec![0u8; mask_row * n];
        unsafe { CreateIcon(None, size, size, 1, 32, mask.as_ptr(), bgra.as_ptr()) }.unwrap_or_default()
    }

    fn icons(hwnd: HWND) -> Icons {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.max(16);
        let light = taskbar_light();
        let tint = light.then_some([0x1f, 0x1f, 0x1f]);
        Icons {
            light,
            record: make_icon(Shape::RecordBadge, size, None),
            paused: make_icon(Shape::PausedBadge, size, None),
            pause: make_icon(Shape::Pause, size, tint),
            play: make_icon(Shape::Play, size, tint),
            stop: make_icon(Shape::Stop, size, tint),
        }
    }

    fn tip(s: &str) -> [u16; 260] {
        let mut out = [0u16; 260];
        for (i, c) in s.encode_utf16().take(259).enumerate() {
            out[i] = c;
        }
        out
    }

    fn buttons(st: &State, rec: Recording) -> [THUMBBUTTON; 2] {
        let icons = st.icons.as_ref().expect("icons");
        let hidden = if rec == Recording::None { THBF_HIDDEN } else { THBF_ENABLED };
        let (icon, label) = if rec == Recording::Paused { (icons.play, "Resume recording") } else { (icons.pause, "Pause recording") };
        [
            THUMBBUTTON { dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS, iId: PAUSE_ID, hIcon: icon, szTip: tip(label), dwFlags: hidden, ..Default::default() },
            THUMBBUTTON { dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS, iId: STOP_ID, hIcon: icons.stop, szTip: tip("Stop recording"), dwFlags: hidden, ..Default::default() },
        ]
    }

    /// Set up for the current main window (again, if it changed).
    fn ensure(cx: &mut App) -> bool {
        let Some(hwnd) = main_hwnd(cx) else { return false };
        let same = STATE.with_borrow(|s| s.as_ref().is_some_and(|s| s.hwnd == hwnd));
        if same {
            return true;
        }
        let Ok(list) = (unsafe { CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER) }) else {
            log::warn!("[taskbar] ITaskbarList3 unavailable");
            return false;
        };
        if unsafe { list.HrInit() }.is_err() {
            return false;
        }
        let _ = button_created_msg();
        unsafe {
            let _ = SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, 0);
        }
        let compiling = STATE.with_borrow_mut(|s| s.take().map(|s| s.compiling).unwrap_or_default());
        STATE.with_borrow_mut(|s| {
            *s = Some(State { hwnd, list, icons: None, buttons_added: false, shown: None, compiling, progress_on: false });
        });
        listen(cx);
        true
    }

    /// Act on button clicks and taskbar changes, once.
    fn listen(cx: &mut App) {
        if LISTENING.with_borrow(|l| *l) {
            return;
        }
        LISTENING.with_borrow_mut(|l| *l = true);
        let rx = channel().1.clone();
        cx.spawn(async move |cx| {
            while let Ok(ev) = rx.recv_async().await {
                cx.update(|cx| match ev {
                    Event::Clicked(PAUSE_ID) => {
                        let action = if super::recording(cx) == Recording::Paused { TrayAction::Resume } else { TrayAction::Pause };
                        crate::windows::handle_tray_action(action, cx);
                    }
                    Event::Clicked(STOP_ID) => crate::windows::handle_tray_action(TrayAction::Stop, cx),
                    Event::Clicked(_) => {}
                    Event::ButtonCreated => {
                        STATE.with_borrow_mut(|s| {
                            if let Some(s) = s {
                                s.buttons_added = false;
                                s.shown = None;
                                s.progress_on = false;
                            }
                        });
                        apply(super::recording(cx));
                    }
                    Event::ThemeChanged => {
                        let changed = STATE.with_borrow(|s| {
                            s.as_ref().and_then(|s| s.icons.as_ref()).is_some_and(|i| i.light != taskbar_light())
                        });
                        if changed {
                            STATE.with_borrow_mut(|s| {
                                if let Some(s) = s {
                                    s.icons = None;
                                    s.shown = None;
                                }
                            });
                            apply(super::recording(cx));
                        }
                    }
                });
            }
        })
        .detach();
    }

    /// Push `rec` (and the progress state) to the taskbar if it changed.
    fn apply(rec: Recording) {
        STATE.with_borrow_mut(|s| {
            let Some(st) = s.as_mut() else { return };
            if st.icons.is_none() {
                st.icons = Some(icons(st.hwnd));
            }
            if st.shown != Some(rec) {
                let b = buttons(st, rec);
                let ok = if st.buttons_added {
                    unsafe { st.list.ThumbBarUpdateButtons(st.hwnd, &b) }.is_ok()
                } else {
                    // Fails until Explorer has made the button; `ButtonCreated` retries.
                    let ok = unsafe { st.list.ThumbBarAddButtons(st.hwnd, &b) }.is_ok();
                    st.buttons_added = ok;
                    ok
                };
                let icons = st.icons.as_ref().expect("icons");
                let (badge, desc) = match rec {
                    Recording::Live => (icons.record, w!("Recording")),
                    Recording::Paused => (icons.paused, w!("Recording paused")),
                    Recording::None => (HICON::default(), PCWSTR::null()),
                };
                let badged = unsafe { st.list.SetOverlayIcon(st.hwnd, badge, desc) }.is_ok();
                if ok && badged {
                    st.shown = Some(rec);
                }
            }
            let compiling = !st.compiling.is_empty();
            if compiling != st.progress_on {
                let flag = if compiling { TBPF_INDETERMINATE } else { TBPF_NOPROGRESS };
                if unsafe { st.list.SetProgressState(st.hwnd, flag) }.is_ok() {
                    st.progress_on = compiling;
                }
            }
        });
    }

    pub fn sync(rec: Recording, cx: &mut App) {
        if ensure(cx) {
            apply(rec);
        }
    }

    pub fn set_compiling(token: &str, compiling: bool, cx: &mut App) {
        if !ensure(cx) {
            return;
        }
        STATE.with_borrow_mut(|s| {
            if let Some(s) = s {
                if compiling {
                    s.compiling.insert(token.to_string());
                } else {
                    s.compiling.remove(token);
                }
            }
        });
        apply(super::recording(cx));
    }
}

#[cfg(test)]
mod tests {
    use super::glyph::{Shape, rgba};

    fn alpha(px: &[u8], size: usize, x: usize, y: usize) -> u8 {
        px[(y * size + x) * 4 + 3]
    }

    #[test]
    fn glyphs_have_the_right_shapes() {
        let n = 32;
        let rec = rgba(Shape::RecordBadge, n, None);
        // Red in the middle, clear in the corner, white ring near the edge.
        assert_eq!(&rec[(16 * n + 16) * 4..(16 * n + 16) * 4 + 4], &[0xef, 0x44, 0x44, 0xff]);
        assert_eq!(alpha(&rec, n, 0, 0), 0);
        let ring = (16 * n + 1) * 4;
        assert!(rec[ring + 3] > 200 && rec[ring] == 0xff && rec[ring + 1] == 0xff, "white ring at the left edge");

        let pause = rgba(Shape::Pause, n, None);
        assert_eq!(alpha(&pause, n, 16, 16), 0, "gap between the bars");
        assert_eq!(alpha(&pause, n, 9, 16), 255, "left bar");
        assert_eq!(alpha(&pause, n, 22, 16), 255, "right bar");

        let play = rgba(Shape::Play, n, Some([0x1f, 0x1f, 0x1f]));
        let mid = (16 * n + 12) * 4;
        assert_eq!(&play[mid..mid + 4], &[0x1f, 0x1f, 0x1f, 0xff], "tinted for a light taskbar");
        assert_eq!(alpha(&play, n, 26, 4), 0, "point of the triangle stays clear above");

        let stop = rgba(Shape::Stop, n, None);
        assert_eq!(alpha(&stop, n, 16, 16), 255);
        assert_eq!(alpha(&stop, n, 2, 2), 0);
    }
}
