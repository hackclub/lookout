//! `HeaderBar` (Linux only, `clients/desktop/src/components/HeaderBar.tsx`):
//! the 47px Adwaita-style title bar the undecorated Linux window draws for
//! itself. Back button (30×30 round action), title/subtitle, a flexible drag
//! spacer, page actions and the 24×24 round close control on whichever
//! edge GNOME's `button-layout` puts it.
//!
//! Hover/active colours transition over 100ms ease-out; the whole bar dims
//! to 55% opacity (160ms ease-out) while the window is in the backdrop.

use std::rc::Rc;

use gpui_kit::gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use gpui_kit::gpui::prelude::FluentBuilder;

use crate::motion::{Anim, Ease};
use crate::theme::{self, theme};
use crate::ui::icon::Icon;
use crate::ui::text::TextExt;

pub const HEADER_BAR_HEIGHT: f32 = 47.0;

/// The round header-bar buttons. `Control` is the filled close button,
/// `Action` the transparent back/gear/+ buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HeaderBtnKind {
    Control,
    Action,
}

struct HoverState {
    hovered: bool,
    pressed: bool,
    hover: Anim,
    press: Anim,
}

#[derive(IntoElement)]
pub struct HeaderBtn {
    id: ElementId,
    kind: HeaderBtnKind,
    content: AnyElement,
    /// An icon drawn in the button's own (transitioning) foreground colour —
    /// Phosphor icons are `currentColor`, so they follow the CSS
    /// `color 100ms ease-out` hover transition.
    icon: Option<(Icon, f32)>,
    on_click: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
    tooltip: Option<SharedString>,
}

impl HeaderBtn {
    pub fn new(
        id: impl Into<ElementId>,
        kind: HeaderBtnKind,
        content: impl IntoElement,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self { id: id.into(), kind, content: content.into_any_element(), icon: None, on_click: Rc::new(on_click), tooltip: None }
    }
    /// A button whose content is `icon` at `size`, tinted with the button's
    /// animated foreground colour.
    pub fn icon(
        id: impl Into<ElementId>,
        kind: HeaderBtnKind,
        icon: Icon,
        size: f32,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        let mut b = Self::new(id, kind, div(), on_click);
        b.icon = Some((icon, size));
        b
    }
    pub fn tooltip(mut self, t: impl Into<SharedString>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
}


impl RenderOnce for HeaderBtn {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let st = window.use_keyed_state(self.id.clone(), cx, |_, _| HoverState {
            hovered: false,
            pressed: false,
            hover: Anim::new(0.0),
            press: Anim::new(0.0),
        });
        let moving = st.update(cx, |s, _| s.hover.tick() | s.press.tick());
        if moving {
            window.request_animation_frame();
        }
        let (h, p) = {
            let s = st.read(cx);
            (s.hover.get(), s.press.get())
        };
        let (size, idle, hover, active, fg_idle, fg_hover): (f32, Hsla, Hsla, Hsla, Hsla, Hsla) = match self.kind {
            HeaderBtnKind::Control => (
                24.0,
                t.headerbar_control,
                t.headerbar_control_hover,
                t.border_hover,
                t.text_primary,
                t.text_primary,
            ),
            HeaderBtnKind::Action => (
                30.0,
                theme::transparent(),
                t.headerbar_control,
                t.headerbar_control_hover,
                t.text_secondary,
                t.text_primary,
            ),
        };
        let bg = theme::mix(theme::mix(idle, hover, h), active, p);
        let fg = theme::mix(fg_idle, fg_hover, h.max(p));
        let (s1, s2, s3, s4) = (st.clone(), st.clone(), st.clone(), st.clone());
        let f = self.on_click.clone();
        let mut el = div()
            .id(self.id)
            .size(px(size))
            .flex_none()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .text_color(fg)
            .map(|d| match self.icon {
                Some((icon, size)) => d.child(icon.el(size, fg)),
                None => d.child(self.content),
            })
            .on_hover(move |hov, _, cx| {
                s1.update(cx, |s, cx| {
                    s.hovered = *hov;
                    s.hover.set_css(if *hov { 1.0 } else { 0.0 }, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
                s2.update(cx, |s, cx| {
                    s.pressed = true;
                    s.press.set_css(1.0, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                s3.update(cx, |s, cx| {
                    s.pressed = false;
                    s.press.set_css(0.0, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                s4.update(cx, |s, cx| {
                    s.pressed = false;
                    s.press.set_css(0.0, 100.0, Ease::EASE_OUT);
                    cx.notify();
                })
            })
            .on_click(move |ev, window, cx| f(ev, window, cx));
        if let Some(tip) = self.tooltip {
            el = el.tooltip(move |window, cx| crate::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx));
        }
        el
    }
}

/// Header contents for the current page.
#[derive(Clone, Default)]
pub struct HeaderNav {
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    pub on_back: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

#[derive(IntoElement)]
pub struct HeaderBar {
    nav: HeaderNav,
    actions: Vec<AnyElement>,
    controls_on_right: bool,
    backdrop: f32,
    maximizable: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl HeaderBar {
    pub fn new(nav: HeaderNav, controls_on_right: bool, backdrop: f32) -> Self {
        Self { nav, actions: Vec::new(), controls_on_right, backdrop, maximizable: false, on_close: None }
    }
    pub fn actions(mut self, a: Vec<AnyElement>) -> Self {
        self.actions = a;
        self
    }
    pub fn maximizable(mut self, on: bool) -> Self {
        self.maximizable = on;
        self
    }
    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(f));
        self
    }
}

struct DragState {
    should_move: bool,
}

impl RenderOnce for HeaderBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let drag = window.use_keyed_state("headerbar-drag", cx, |_, _| DragState { should_move: false });
        let make_close = |id: &'static str| {
            let on_close = self.on_close.clone();
            HeaderBtn::new(
                id,
                HeaderBtnKind::Control,
                Icon::LkHeaderClose.el(14.0, t.text_primary),
                move |_, window, cx| match &on_close {
                    Some(f) => f(window, cx),
                    None => window.remove_window(),
                },
            )
            .tooltip("Close")
        };
        let has_back = self.nav.on_back.is_some();
        let back = self.nav.on_back.clone().map(|f| {
            HeaderBtn::icon("hb-back", HeaderBtnKind::Action, Icon::LkHeaderBack, 16.0, move |_, window, cx| f(window, cx))
                .tooltip("Back")
        });
        let title_size = if self.nav.subtitle.is_some() { 13.0 } else { 14.0 };
        let title_block = div()
            .flex()
            .flex_col()
            .justify_center()
            .min_w(px(0.0))
            .child(
                div()
                    .text_size(px(title_size))
                    .line_height(px(title_size * 1.15))
                    .w700()
                    .text_color(t.text_primary)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.nav.title.clone()),
            )
            .children(self.nav.subtitle.clone().map(|s| {
                div()
                    .text_size(px(11.0))
                    .line_height(px(11.0 * 1.15))
                    .w400()
                    .text_color(t.text_secondary)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(s)
            }));
        let maximizable = self.maximizable;
        let (d1, d2, d3) = (drag.clone(), drag.clone(), drag.clone());
        div()
            .id("lookout-headerbar")
            .relative()
            .flex_none()
            .h(px(HEADER_BAR_HEIGHT))
            .flex()
            .items_center()
            .gap(px(8.0))
            .map(|d| if has_back { d.px(px(8.0)) } else { d.pl(px(12.0)).pr(px(8.0)) })
            .opacity(self.backdrop)
            // Drag: start only once the pointer moves, so buttons keep their clicks.
            .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                if window.default_prevented() {
                    return;
                }
                if ev.click_count == 2 {
                    if maximizable {
                        window.zoom_window();
                    }
                    return;
                }
                d1.update(cx, |s, _| s.should_move = true);
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| d2.update(cx, |s, _| s.should_move = false))
            .on_mouse_move(move |_, window, cx| {
                let go = d3.update(cx, |s, _| std::mem::replace(&mut s.should_move, false));
                if go {
                    window.start_window_move();
                }
            })
            .on_mouse_down(MouseButton::Right, |ev, window, _| window.show_window_menu(ev.position))
            .map(|d| if !self.controls_on_right { d.child(make_close("hb-close-left")) } else { d })
            .children(back)
            .child(title_block)
            .child(div().flex_1().self_stretch())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .flex_none()
                    .children(self.actions)
                    .map(|d| if self.controls_on_right { d.child(make_close("hb-close")) } else { d }),
            )
    }
}

