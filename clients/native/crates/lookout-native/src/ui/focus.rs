//! Keyboard focus: Tab order, the focus ring, focus traps and restoring
//! focus when a layer closes.
//!
//! - **Tab / Shift-Tab** move through tab stops in every window
//!   ([`install`] adds one keystroke interceptor; nothing else binds Tab).
//!   A widget that wants Tab for itself sets `.key_context(OWN_TAB)`.
//! - **Tab stops.** `Button`, `HeaderBtn` and `PressScale` are tab stops
//!   whenever they're clickable and enabled; Enter and Space activate them.
//!   On macOS buttons follow the system: they're only reached with Tab when
//!   "Keyboard navigation" (Full Keyboard Access) is on, as in AppKit; text
//!   fields always are. `LOOKOUT_FULL_KEYBOARD=1|0` overrides.
//! - **The ring** shows only for keyboard focus (CSS `:focus-visible`), drawn
//!   the way each platform draws its own: macOS a 3px accent halo hugging
//!   the control, Windows the two-tone 2px + 1px focus visual, GNOME a 2px
//!   accent outline just outside the edge.
//! - **Traps.** A modal layer (sheet, dialog, menu) carries
//!   `.key_context(TRAP)` on the element that holds focus while it's open;
//!   while focus is inside it, Tab cycles within it. Layers remember the
//!   focus they took over ([`remember`]) and hand it back on close
//!   ([`restore`]).

use std::sync::atomic::{AtomicU8, Ordering};

use gpui_kit::gpui::{
    App, Bounds, ElementId, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, Pixels, Stateful, Styled,
    Window, canvas, point, px, quad, size,
};

use crate::theme::{self, Os, Theme, theme};

/// Key context for a widget that handles Tab itself.
pub const OWN_TAB: &str = "OwnsTab";
/// Key context for a modal layer's focus-holding element: Tab stays inside.
pub const TRAP: &str = "FocusTrap";

// 0 = follow the OS, 1 = on, 2 = off.
static FULL_KEYBOARD: AtomicU8 = AtomicU8::new(0);

/// Whether buttons and other non-text controls are Tab stops. Always on
/// Windows and Linux; on macOS it follows Full Keyboard Access.
pub fn buttons_are_tab_stops() -> bool {
    match std::env::var("LOOKOUT_FULL_KEYBOARD").as_deref() {
        Ok("1") | Ok("on") | Ok("true") => return true,
        Ok("0") | Ok("off") | Ok("false") => return false,
        _ => {}
    }
    match FULL_KEYBOARD.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => theme::OS != Os::Mac,
    }
}

/// Set from `platform::a11y_settings` (macOS `AppleKeyboardUIMode`).
pub fn set_full_keyboard_access(on: Option<bool>) {
    FULL_KEYBOARD.store(
        match on {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        Ordering::Relaxed,
    );
}

/// Install the app-wide Tab / Shift-Tab handling. Call once at startup.
pub fn install(cx: &mut App) {
    cx.intercept_keystrokes(|ev, window, cx| {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        if k.key != "tab" || m.control || m.alt || m.platform || m.function {
            return;
        }
        if ev.context_stack.iter().any(|c| c.contains(OWN_TAB)) {
            return;
        }
        move_focus(!m.shift, window, cx);
        cx.stop_propagation();
    })
    .detach();
}

fn in_trap(window: &Window) -> bool {
    window.context_stack().iter().any(|c| c.contains(TRAP))
}

/// Whether focus is inside a modal layer (an element with `TRAP`).
pub fn focus_in_trap(window: &Window) -> bool {
    in_trap(window)
}

/// Move to the next (or previous) tab stop, staying inside a modal layer
/// when focus is in one. Wraps at the ends, like every platform does.
pub fn move_focus(forward: bool, window: &mut Window, cx: &mut App) {
    let start = window.focused(cx);
    let trapped = in_trap(window);
    for _ in 0..512 {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        let now = window.focused(cx);
        if now.is_none() || now == start {
            // Went all the way round.
            break;
        }
        if !trapped || in_trap(window) {
            return;
        }
    }
    // Nothing focusable in the trap besides where we started: stay put.
    if let Some(s) = start {
        window.focus(&s, cx);
    }
}

/// What has focus now, to hand back with [`restore`] when a layer that's
/// about to take focus closes.
pub fn remember(window: &Window, cx: &App) -> Option<FocusHandle> {
    window.focused(cx)
}

/// Give focus back to what [`remember`] saw, if it's still around.
pub fn restore(handle: Option<&FocusHandle>, window: &mut Window, cx: &mut App) {
    if let Some(h) = handle {
        window.focus(h, cx);
    }
}

thread_local! {
    static TRAPS: std::cell::RefCell<Vec<(gpui_kit::gpui::AnyWindowHandle, Option<FocusHandle>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// For a custom modal layer: remember the current focus and move it to
/// `container` (which should carry `.track_focus(container)` and
/// `.key_context(TRAP)`). Pair with [`pop_trap`] when it closes.
pub fn push_trap(window: &mut Window, cx: &mut App, container: &FocusHandle) {
    let prev = remember(window, cx);
    TRAPS.with(|t| t.borrow_mut().push((window.window_handle(), prev)));
    window.focus(container, cx);
}

/// Close the innermost trap opened with [`push_trap`] in this window and
/// restore the focus it took over.
pub fn pop_trap(window: &mut Window, cx: &mut App) {
    let me = window.window_handle();
    let prev = TRAPS.with(|t| {
        let mut t = t.borrow_mut();
        let i = t.iter().rposition(|(w, _)| *w == me)?;
        Some(t.remove(i).1)
    });
    if let Some(prev) = prev {
        restore(prev.as_ref(), window, cx);
    }
}

/// A focus handle kept per element id (for `RenderOnce` controls).
pub fn keyed_handle(id: &ElementId, window: &mut Window, cx: &mut App) -> FocusHandle {
    window.use_keyed_state((id.clone(), "focus"), cx, |_, cx| cx.focus_handle()).read(cx).clone()
}

/// Whether `handle` has focus that came from the keyboard (draw the ring).
pub fn ring_visible(handle: &FocusHandle, window: &Window) -> bool {
    handle.is_focused(window) && window.last_input_was_keyboard()
}

/// An absolutely positioned child that draws the focus ring around its
/// parent (which must be `.relative()`) while `handle` has keyboard focus.
/// `radius` is the parent's corner radius.
pub fn ring(handle: &FocusHandle, radius: f32) -> impl IntoElement {
    let handle = handle.clone();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, cx| {
            if ring_visible(&handle, window) {
                paint_ring(bounds, radius, window, theme(cx));
            }
        },
    )
    .absolute()
    .inset_0()
}

fn grow(b: Bounds<Pixels>, by: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(b.origin.x - px(by), b.origin.y - px(by)),
        size: size(b.size.width + px(by * 2.0), b.size.height + px(by * 2.0)),
    }
}

fn outline(window: &mut Window, b: Bounds<Pixels>, radius: f32, width: f32, color: gpui_kit::gpui::Hsla) {
    window.paint_quad(quad(b, px(radius.max(0.0)), theme::transparent(), px(width), color, Default::default()));
}

/// Paint the platform's focus ring around `bounds` (corner `radius`).
pub fn paint_ring(bounds: Bounds<Pixels>, radius: f32, window: &mut Window, t: &Theme) {
    let thick = if t.high_contrast { 1.0 } else { 0.0 };
    match theme::OS {
        Os::Mac => {
            let w = 3.0 + thick;
            outline(window, grow(bounds, w), radius + w, w, t.focus_ring);
        }
        Os::Windows => {
            // Fluent's focus visual: 2px primary outside a 1px secondary,
            // with the secondary hugging the control.
            let primary = if t.high_contrast { t.focus_ring } else { t.text_primary };
            let secondary = if t.dark { theme::black(1.0) } else { theme::white(1.0) };
            let secondary = if t.high_contrast { t.bg_body } else { secondary };
            outline(window, grow(bounds, 1.0), radius + 1.0, 1.0, secondary);
            outline(window, grow(bounds, 3.0 + thick), radius + 3.0 + thick, 2.0 + thick, primary);
        }
        Os::Linux => {
            let w = 2.0 + thick;
            outline(window, grow(bounds, 1.0 + w), radius + 1.0 + w, w, t.focus_ring);
        }
    }
}

/// Enter / Space on the focused element itself runs `f` (the element's
/// click). Held keys don't repeat it.
///
/// Runs in the capture phase on purpose: GPUI's own keyboard click (div.rs,
/// "Press enter, space to trigger click") records the key-down in the bubble
/// phase and fires the element's `on_click` on key-up. Stopping the event
/// here means it never records the press, so an element with both this and
/// `on_click` (every kit `Button`) runs once, not twice.
pub fn activate_on_keys(
    el: Stateful<gpui_kit::gpui::Div>,
    handle: &FocusHandle,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<gpui_kit::gpui::Div> {
    let handle = handle.clone();
    el.capture_key_down(move |ev: &KeyDownEvent, window, cx| {
        if !handle.is_focused(window) || ev.keystroke.modifiers.modified() {
            return;
        }
        if matches!(ev.keystroke.key.as_str(), "enter" | "space") {
            cx.stop_propagation();
            if !ev.is_held {
                f(window, cx);
            }
        }
    })
}

/// Make a clickable `RenderOnce` control keyboard-focusable: tracks
/// `handle` (a tab stop unless `tab_stop` says otherwise), activates on
/// Enter/Space and draws the ring. Returns the element.
pub fn focusable_control(
    el: Stateful<gpui_kit::gpui::Div>,
    handle: &FocusHandle,
    tab_stop: bool,
    tab_index: Option<isize>,
    f: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<gpui_kit::gpui::Div> {
    let mut h = handle.clone().tab_stop(tab_stop);
    if let Some(i) = tab_index {
        h = h.tab_index(i);
    }
    activate_on_keys(el.track_focus(&h), &h, f)
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_override_wins() {
        // Not set in tests: follows the platform default.
        if std::env::var("LOOKOUT_FULL_KEYBOARD").is_err() {
            super::set_full_keyboard_access(Some(true));
            assert!(super::buttons_are_tab_stops());
            super::set_full_keyboard_access(Some(false));
            assert!(!super::buttons_are_tab_stops());
            super::set_full_keyboard_access(None);
            assert_eq!(super::buttons_are_tab_stops(), crate::theme::OS != crate::theme::Os::Mac);
        }
    }
}
