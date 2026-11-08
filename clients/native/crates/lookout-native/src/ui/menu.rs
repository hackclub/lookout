//! Popup menus — the "+" add menu (`AddMenuPopup.tsx` on Windows/Linux,
//! the SwiftUI `AddMenuView` panel on macOS) and the gallery card's
//! context menu (the native popup menu in the Tauri app), drawn in-window
//! with each platform's look:
//!
//! | | macOS (SwiftUI panel) | Windows | Linux (Adwaita popover) |
//! |---|---|---|---|
//! | radius | 14 | 14 | 12 + 20×10 tail |
//! | row | 7/9 pad, r9, 13.5/500 | 7/9 pad, r9, 13.5/500 | 0/12 pad, min-h 32, r6, 14.5/400 |
//! | enter | scale 0.85→1 (spring 1200/46) + fade 150ms | scale 0.92→1 (spring 1800/56) + fade 100ms easeOut | same as Windows, from the tail |
//! | exit | fade 170ms | fade 100ms easeOut | fade 100ms easeOut |
//!
//! Highlight changes are instant (no hover transition), arrows move the
//! selection (wrapping, skipping separators), Enter activates, Escape,
//! click-away and window blur dismiss.

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, Bounds, Context, EventEmitter, FocusHandle, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, PathBuilder, Pixels, Render, SharedString, StatefulInteractiveElement, Styled,
    Window, canvas, div, img, point, px,
};

use crate::images::{self, Remote};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::theme::{self, Theme, is_linux, is_mac, theme};
use crate::ui::icon::Icon;
use crate::ui::paint;
use crate::ui::text::TextExt;

#[derive(Clone)]
pub enum MenuItem {
    Item { id: String, label: String, icon_url: Option<String>, fallback: Icon },
    Separator,
}

impl MenuItem {
    pub fn item(id: impl Into<String>, label: impl Into<String>, fallback: Icon) -> Self {
        MenuItem::Item { id: id.into(), label: label.into(), icon_url: None, fallback }
    }
    fn is_item(&self) -> bool {
        matches!(self, MenuItem::Item { .. })
    }
}

pub enum MenuEvent {
    /// `None` = dismissed.
    Select(Option<String>),
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Mac,
    Windows,
    Linux,
}

pub struct MenuPopup {
    items: Vec<MenuItem>,
    /// The button (or click point) the menu hangs from, in window coords.
    anchor: Bounds<Pixels>,
    /// Context menus open at the pointer, top-left aligned.
    at_pointer: bool,
    selection: Option<usize>,
    scale: Anim,
    opacity: Anim,
    pub closing: bool,
    focus: FocusHandle,
    measured: Bounds<Pixels>,
    look: Look,
}

impl EventEmitter<MenuEvent> for MenuPopup {}

const GAP: f32 = 6.0;
const EDGE: f32 = 8.0;
const TAIL_W: f32 = 20.0;
const TAIL_H: f32 = 10.0;
const TAIL_INSET: f32 = 14.0;

impl MenuPopup {
    pub fn new(items: Vec<MenuItem>, anchor: Bounds<Pixels>, at_pointer: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let look = if is_mac() {
            Look::Mac
        } else if is_linux() && !at_pointer {
            Look::Linux
        } else {
            Look::Windows
        };
        let (from, spring, fade) = match look {
            Look::Mac => (0.85, Spring::new(1200.0, 46.0), Curve::tween(150.0, Ease::EASE_IN_OUT)),
            _ => (0.92, Spring::new(1800.0, 56.0), Curve::tween(100.0, Ease::EASE_OUT)),
        };
        cx.observe(&images::cache(cx), |_, _, cx| cx.notify()).detach();
        Self {
            items,
            anchor,
            at_pointer,
            selection: None,
            scale: Anim::from_to(from, 1.0, Curve::Spring(spring)),
            opacity: Anim::from_to(0.0, 1.0, fade),
            closing: false,
            focus,
            measured: Bounds::default(),
            look,
        }
    }

    pub fn dismiss(&mut self, choice: Option<String>, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        let ms = if self.look == Look::Mac { 170.0 } else { 100.0 };
        let ease = if self.look == Look::Mac { Ease::EASE_IN_OUT } else { Ease::EASE_OUT };
        self.opacity.set(0.0, Curve::tween(ms, ease));
        cx.emit(MenuEvent::Select(choice));
        cx.notify();
    }

    pub fn finished(&self) -> bool {
        self.closing && !self.opacity.is_animating()
    }

    fn move_sel(&mut self, down: bool) {
        let idx: Vec<usize> = self.items.iter().enumerate().filter(|(_, i)| i.is_item()).map(|(i, _)| i).collect();
        if idx.is_empty() {
            return;
        }
        let pos = self.selection.and_then(|s| idx.iter().position(|i| *i == s));
        let next = match (pos, down) {
            (None, true) => 0,
            (None, false) => idx.len() - 1,
            (Some(p), true) => (p + 1) % idx.len(),
            (Some(p), false) => (p + idx.len() - 1) % idx.len(),
        };
        self.selection = Some(idx[next]);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.dismiss(None, cx),
            "down" => self.move_sel(true),
            "up" => self.move_sel(false),
            "enter" => {
                if let Some(MenuItem::Item { id, .. }) = self.selection.and_then(|s| self.items.get(s)).cloned() {
                    self.dismiss(Some(id), cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for MenuPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        if self.scale.tick() | self.opacity.tick() {
            window.request_animation_frame();
        }
        if !self.closing && !window.is_window_active() {
            self.dismiss(None, cx);
        }
        let look = self.look;
        let win = window.bounds().size;
        let (ww, wh) = (f32::from(win.width), f32::from(win.height));
        let a = self.anchor;
        let (ax, ay, aw, ah) = (f32::from(a.origin.x), f32::from(a.origin.y), f32::from(a.size.width), f32::from(a.size.height));
        let mw = f32::from(self.measured.size.width).max(220.0);
        let mh = f32::from(self.measured.size.height);
        let tail = if look == Look::Linux { TAIL_H } else { 0.0 };
        // Position: right edges aligned under the anchor; flip above when it
        // would leave the window; context menus open at the pointer.
        let (left, top, flipped, tail_right);
        if self.at_pointer {
            left = ax.min(ww - mw - EDGE).max(EDGE);
            let below = ay + GAP;
            flipped = below + mh > wh - EDGE;
            top = if flipped { (ay - mh).max(EDGE) } else { below };
            tail_right = 0.0;
        } else {
            let aligned_right = (ww - (ax + aw)).max(EDGE);
            let centre_from_aligned = ww - (ax + aw / 2.0) - aligned_right;
            let shift = if look == Look::Linux { (TAIL_INSET + TAIL_W / 2.0 - centre_from_aligned).max(0.0) } else { 0.0 };
            let right = (aligned_right - shift).max(EDGE);
            left = ww - right - mw;
            let below = ay + ah + GAP + tail;
            flipped = below + mh > wh - EDGE;
            top = if flipped { ay - GAP - tail - mh } else { below };
            tail_right = (ww - (ax + aw / 2.0) - right - TAIL_W / 2.0).max(TAIL_INSET);
        }
        // Scale origin: the top-right corner (Linux: the tail tip; context
        // menus: the top-left).
        let (ox, oy) = if self.at_pointer {
            (0.0, if flipped { 1.0 } else { 0.0 })
        } else if look == Look::Linux {
            (1.0 - (tail_right + TAIL_W / 2.0) / mw, if flipped { 1.0 } else { 0.0 })
        } else {
            (1.0, if flipped { 1.0 } else { 0.0 })
        };
        let s = self.scale.get();
        let o = self.opacity.get().clamp(0.0, 1.0);
        let (radius, border, bg_top, bg_bottom, shadow): (f32, Hsla, Hsla, Hsla, Vec<gpui_kit::gpui::BoxShadow>) = match look {
            Look::Linux => (
                12.0,
                t.popover_border,
                t.popover_bg_top,
                t.popover_bg,
                vec![
                    theme::sh(0.0, 1.0, 3.0, 0.0, theme::black(0.08)),
                    theme::sh(0.0, 4.0, 12.0, 0.0, theme::black(0.1)),
                    theme::sh(0.0, 10.0, 28.0, 0.0, theme::black(0.12)),
                ],
            ),
            Look::Mac => (14.0, theme::fade(t.text_primary, 0.12), t.bg_panel, t.bg_panel, vec![theme::sh(0.0, 6.0, 32.0, 0.0, theme::black(0.28))]),
            Look::Windows => (14.0, theme::fade(t.text_primary, 0.12), t.bg_panel, t.bg_panel, vec![theme::sh(0.0, 6.0, 32.0, 0.0, theme::black(0.28))]),
        };
        let me = cx.entity();
        let probe = cx.entity();
        let rows: Vec<AnyElement> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| match item {
                MenuItem::Separator => div()
                    .h(px(1.0))
                    .bg(if look == Look::Linux { t.popover_separator } else { t.border_default })
                    .map(|d| if look == Look::Linux { d.my(px(5.0)) } else { d.my(px(4.0)).mx(px(10.0)) })
                    .into_any_element(),
                MenuItem::Item { id, label, icon_url, fallback } => {
                    let highlighted = self.selection == Some(i);
                    let fg = if highlighted { t.text_primary } else { t.text_secondary };
                    let icon: AnyElement = match icon_url.as_deref().map(|u| images::get(u, cx)) {
                        Some(Remote::Ready(img_data, _)) => img(img_data).size(px(18.0)).rounded(px(5.0)).into_any_element(),
                        _ => fallback.el(15.0, fg).into_any_element(),
                    };
                    let hl = match look {
                        Look::Linux => t.popover_hover,
                        _ => theme::fade(t.text_primary, 0.09),
                    };
                    let id = id.clone();
                    let me2 = me.clone();
                    let me3 = me.clone();
                    div()
                        .id(("menu-row", i))
                        .flex()
                        .items_center()
                        .map(|d| match look {
                            Look::Linux => d.gap(px(12.0)).min_h(px(32.0)).px(px(12.0)).rounded(px(6.0)),
                            _ => d.gap(px(9.0)).px(px(9.0)).py(px(7.0)).rounded(px(9.0)),
                        })
                        .when(highlighted, |d| d.bg(hl))
                        .child(div().size(px(18.0)).flex().items_center().justify_center().flex_none().child(icon))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(t.text_primary)
                                .map(|d| if look == Look::Linux { d.ts(14.5).w400() } else { d.ts(13.5).w500() })
                                .child(SharedString::from(label.clone())),
                        )
                        .on_hover(move |hov, _, cx| {
                            me2.update(cx, |m, cx| {
                                if *hov {
                                    m.selection = Some(i);
                                } else if m.selection == Some(i) {
                                    m.selection = None;
                                }
                                cx.notify();
                            })
                        })
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            me3.update(cx, |m, cx| m.dismiss(Some(id), cx))
                        })
                        .into_any_element()
                }
            })
            .collect();

        // Content slides toward the scale origin so the menu reads as
        // growing out of it (text can't scale in GPUI; the panel does).
        let dx = (1.0 - s) * mw * (ox - 0.5);
        let dy = (1.0 - s) * mh * (oy - 0.5);
        let closing = self.closing;
        let panel = div()
            .id("menu-panel")
            .absolute()
            .left(px(left))
            .top(px(top))
            .min_w(px(220.0))
            .max_w(px(320.0f32.min(ww - 2.0 * EDGE)))
            .opacity(o)
            .when(!closing, |d| d.occlude().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()))
            .child(
                canvas(move |b, _, cx| probe.update(cx, |m, _| m.measured = b), move |bounds, _, window, _| {
                    let sb = paint::scale_bounds_from(bounds, s, ox, oy);
                    window.paint_drop_shadows(sb, gpui_kit::gpui::Corners::all(px(radius * s)), &shadow);
                    let grad = gpui_kit::gpui::linear_gradient(
                        180.0,
                        gpui_kit::gpui::linear_color_stop(bg_top, 0.0),
                        gpui_kit::gpui::linear_color_stop(bg_bottom, 1.0),
                    );
                    window.paint_quad(gpui_kit::gpui::quad(sb, px(radius * s), grad, px(1.0), border, Default::default()));
                    if look == Look::Linux {
                        // The tail: fill over the panel's border, then the open stroke.
                        let tr = f32::from(sb.origin.x) + f32::from(sb.size.width) - tail_right * s - TAIL_W;
                        let ty = if flipped { f32::from(sb.origin.y) + f32::from(sb.size.height) } else { f32::from(sb.origin.y) };
                        let dir = if flipped { 1.0 } else { -1.0 };
                        let mut f = PathBuilder::fill();
                        f.move_to(point(px(tr), px(ty + dir * 0.0 - dir * 0.5)));
                        f.line_to(point(px(tr + 10.0), px(ty + dir * 9.5)));
                        f.line_to(point(px(tr + 20.0), px(ty - dir * 0.5)));
                        f.close();
                        let fill_c = if flipped { bg_bottom } else { bg_top };
                        if let Ok(p) = f.build() {
                            window.paint_path(p, fill_c);
                        }
                        // Cover the panel's whole 1px border row under the
                        // tail's base (the triangle alone reaches half of it,
                        // leaving an antialiased seam between tail and panel).
                        // The tail's stroke, painted next, keeps its edges.
                        let seam_top = if flipped { ty - 1.5 } else { ty - 0.5 };
                        window.paint_quad(gpui_kit::gpui::fill(
                            Bounds { origin: point(px(tr + 1.0), px(seam_top)), size: gpui_kit::gpui::size(px(TAIL_W - 2.0), px(2.0)) },
                            fill_c,
                        ));
                        let mut st = PathBuilder::stroke(px(1.0));
                        st.move_to(point(px(tr + 0.5), px(ty - dir * 1.0)));
                        st.line_to(point(px(tr + 10.0), px(ty + dir * 9.0)));
                        st.line_to(point(px(tr + 19.5), px(ty - dir * 1.0)));
                        if let Ok(p) = st.build() {
                            window.paint_path(p, border);
                        }
                    }
                })
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .relative()
                    .left(px(dx))
                    .top(px(dy))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .p(px(6.0))
                    .children(rows),
            );

        // A closing menu is pointer-transparent: it only fades.
        if self.closing {
            return div().absolute().inset_0().child(panel).into_any_element();
        }
        let me_bg = cx.entity();
        div()
            .id("menu-layer")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| me_bg.update(cx, |m, cx| m.dismiss(None, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|m, _, _, cx| m.dismiss(None, cx)))
            .child(panel)
            .into_any_element()
    }
}

#[allow(unused)]
fn _u(_: &Theme, _: &App) {}
