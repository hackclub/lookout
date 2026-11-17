//! Keyboard operation for the screens' own pressable surfaces (the kit's
//! `Button`, `HeaderBtn` and `PressScale` handle this themselves), built on
//! `ui::focus`.

use gpui_kit::gpui::{App, Div, ElementId, FocusHandle, ParentElement, Stateful, Window};

use crate::ui::focus;

/// A focus handle that lives as long as the element keyed `id` keeps
/// rendering.
pub fn keyed_focus(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> FocusHandle {
    focus::keyed_handle(&id.into(), window, cx)
}

pub trait KbdPress: Sized {
    /// A button-like surface: a Tab stop where buttons are (on macOS only
    /// with Full Keyboard Access), pressed by Enter/Space, with the focus
    /// ring (`radius` = its corners; the element must be `.relative()`).
    fn kbd_press(self, focus: &FocusHandle, radius: f32, f: impl Fn(&mut Window, &mut App) + 'static) -> Self;
}

impl KbdPress for Stateful<Div> {
    fn kbd_press(self, handle: &FocusHandle, radius: f32, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        focus::focusable_control(self, handle, focus::buttons_are_tab_stops(), None, f).child(focus::ring(handle, radius))
    }
}
