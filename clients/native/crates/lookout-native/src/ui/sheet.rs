//! `Sheet` — the vaul bottom sheet (`Sheet.tsx` over vaul 1.1.2).
//!
//! Opens by sliding up from 100% of its own height and closes back down,
//! both over 500ms `cubic-bezier(0.32, 0.72, 0, 1)`, with the backdrop
//! fading on the same curve. It can be dragged: downward follows the
//! pointer 1:1, upward over-drag is damped by `8·(ln(v+1) − 2)`, and the
//! backdrop dims with `1 − dragged/height`. On release it closes when the
//! flick was faster than 0.4 px/ms or it travelled ≥ 25% of its height,
//! otherwise it springs back over 500ms. Escape, the X button and a click
//! on the backdrop close it too.

use std::time::Instant;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyView, App, Bounds, Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Render, StatefulInteractiveElement, Styled, Window, canvas,
    div, px,
};

use crate::motion::{Anim, Curve, Ease};
use crate::theme::{self, is_windows, theme};
use crate::ui::icon::Icon;

pub enum SheetEvent {
    /// The user asked to close (X, backdrop, Escape, swipe).
    Dismiss,
    /// The exit animation finished; the host may drop the sheet.
    Closed,
}

const VAUL: Curve = Curve::tween(500.0, Ease::VAUL);

/// `Standard` is `Sheet.tsx` (24px inset, radius 24, X button); `Panel` is
/// the ProgramPanel drawer (full width, 14px top corners, its own header).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SheetStyle {
    Standard,
    Panel,
}

pub struct Sheet {
    pub style: SheetStyle,
    hero: Option<AnyView>,
    body: AnyView,
    /// 0 = hidden (translated down by its height), 1 = open.
    open: Anim,
    pub is_open: bool,
    height: f32,
    drag: Option<Drag>,
    /// Pixel offset while dragging (positive = down).
    drag_offset: f32,
    focus: FocusHandle,
    closed_emitted: bool,
    /// vaul's `openTime`: a press within 500ms of opening never drags
    /// ("allow scrolling when animating", `shouldDrag`).
    opened_at: Option<Instant>,
}

struct Drag {
    start_y: f32,
    start_time: Instant,
    last_y: f32,
}

impl EventEmitter<SheetEvent> for Sheet {}

impl Sheet {
    pub fn new(hero: Option<AnyView>, body: AnyView, cx: &mut Context<Self>) -> Self {
        Self {
            style: SheetStyle::Standard,
            hero,
            body,
            open: Anim::new(0.0),
            is_open: false,
            height: 0.0,
            drag: None,
            drag_offset: 0.0,
            focus: cx.focus_handle(),
            closed_emitted: true,
            opened_at: None,
        }
    }

    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open == self.is_open {
            return;
        }
        self.is_open = open;
        self.drag = None;
        if open {
            self.closed_emitted = false;
            self.opened_at = Some(Instant::now());
            // Fold any leftover drag into the animation's start.
            self.open.set(1.0, VAUL);
            window.focus(&self.focus, cx);
        } else {
            let from = self.progress();
            self.drag_offset = 0.0;
            self.open.jump(from);
            self.open.set(0.0, VAUL);
        }
        cx.notify();
    }

    pub fn set_content(&mut self, hero: Option<AnyView>, body: AnyView) {
        self.hero = hero;
        self.body = body;
    }

    /// Effective open fraction including a drag.
    fn progress(&self) -> f32 {
        let base = self.open.get();
        if self.height <= 0.0 {
            return base;
        }
        (base - self.drag_offset.max(0.0) / self.height).clamp(0.0, 1.0)
    }

    pub fn visible(&self) -> bool {
        self.is_open || self.open.is_animating()
    }

    fn release(&mut self, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        let dist = y - d.start_y;
        let ms = d.start_time.elapsed().as_secs_f32() * 1000.0;
        let velocity = dist.abs() / ms.max(1.0);
        if dist > 0.0 && (velocity > 0.4 || dist >= self.height * 0.25) {
            cx.emit(SheetEvent::Dismiss);
            self.set_open(false, window, cx);
        } else {
            // Snap back from wherever the drag left it.
            let from = self.progress();
            let over = self.drag_offset.min(0.0);
            self.drag_offset = 0.0;
            self.open.jump(from);
            self.open.set(1.0, VAUL);
            let _ = over;
            cx.notify();
        }
    }
}

impl Render for Sheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        if self.open.tick() {
            window.request_animation_frame();
        }
        if !self.visible() && !self.closed_emitted {
            self.closed_emitted = true;
            cx.emit(SheetEvent::Closed);
        }
        let p = self.progress();
        // Over-drag upward is damped: 8·(ln(v+1) − 2).
        let up = if self.drag_offset < 0.0 { -(8.0 * ((-self.drag_offset) + 1.0).ln() - 16.0).max(0.0) } else { 0.0 };
        let translate = (1.0 - p) * self.height + up;
        let backdrop = t.bg_backdrop;
        let window_radius = crate::windows::main_window::window_corner_radius(window);
        let me_bg = cx.entity();
        let me_x = cx.entity();
        let probe = cx.entity();

        let panel = self.style == SheetStyle::Panel;
        let sheet = if panel {
            div()
                .id("sheet-card")
                .relative()
                .flex()
                .flex_col()
                .bg(t.bg_panel)
                .border_1()
                .border_color(t.border_default)
                .rounded_t(px(14.0))
                .rounded_b(px(window_radius))
                .overflow_hidden()
                .child(self.body.clone())
                .child(div().h(px(10.0)).flex_none())
        } else { div()
            .id("sheet-card")
            .relative()
            .flex()
            .flex_col()
            .bg(t.bg_panel)
            .border_1()
            .border_color(t.border_default)
            .rounded(px(24.0))
            .overflow_hidden()
            .shadow(vec![theme::sh(0.0, 24.0, 64.0, 0.0, theme::black(0.45))])
            .children(self.hero.clone())
            .child(
                div()
                    .id("sheet-close")
                    .absolute()
                    .top(px(20.0))
                    .right(px(18.0))
                    .size(px(34.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.badge_overlay_bg)
                    .cursor_pointer()
                    .child(Icon::XBold.el(15.0, t.badge_overlay_text))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        me_x.update(cx, |s, cx| {
                            cx.emit(SheetEvent::Dismiss);
                            s.set_open(false, window, cx);
                        })
                    }),
            )
            .child(div().p(px(24.0)).flex().flex_col().items_start().flex_none().child(self.body.clone())) };

        div()
            .id("sheet-layer")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|s, ev: &gpui_kit::gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    cx.emit(SheetEvent::Dismiss);
                    s.set_open(false, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .id("sheet-backdrop")
                    .rounded(px(window_radius))
                    .absolute()
                    .inset_0()
                    .opacity(p)
                    .map(|d| {
                        if is_windows() {
                            d.bg(gpui_kit::gpui::linear_gradient(
                                180.0,
                                gpui_kit::gpui::linear_color_stop(theme::fade(backdrop, 0.0), 0.0),
                                gpui_kit::gpui::linear_color_stop(backdrop, 1.0),
                            ))
                        } else {
                            d.bg(backdrop)
                        }
                    })
                    .occlude()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        me_bg.update(cx, |s, cx| {
                            cx.emit(SheetEvent::Dismiss);
                            s.set_open(false, window, cx);
                        })
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(if panel { 0.0 } else { 24.0 }))
                    .right(px(if panel { 0.0 } else { 24.0 }))
                    .bottom(px(-translate))
                    .pb(px(if panel { 0.0 } else { 24.0 }))
                    .child(
                        canvas(move |b: Bounds<Pixels>, _, cx| probe.update(cx, |s, _| s.height = f32::from(b.size.height)), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    )
                    .on_mouse_down(MouseButton::Left, cx.listener(|s, ev: &MouseDownEvent, _, cx| {
                        let y = f32::from(ev.position.y);
                        let settling = s.opened_at.is_some_and(|t| t.elapsed().as_millis() < 500);
                        if !settling {
                            s.drag = Some(Drag { start_y: y, start_time: Instant::now(), last_y: y });
                        }
                        cx.stop_propagation();
                    }))
                    .on_mouse_move(cx.listener(|s, ev: &MouseMoveEvent, _, cx| {
                        if let Some(d) = &mut s.drag {
                            let y = f32::from(ev.position.y);
                            d.last_y = y;
                            s.drag_offset = y - d.start_y;
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|s, ev: &gpui_kit::gpui::MouseUpEvent, window, cx| {
                        s.release(f32::from(ev.position.y), window, cx);
                    }))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|s, ev: &gpui_kit::gpui::MouseUpEvent, window, cx| {
                        s.release(f32::from(ev.position.y), window, cx);
                    }))
                    // vaul's `[data-vaul-drawer]::after`: the drawer's own
                    // background continues 200% below it, so an upward
                    // over-drag never shows a gap. Only the panel drawer has
                    // a background on its Content (Sheet.tsx's is
                    // transparent).
                    .when(panel, |d| {
                        let h = self.height;
                        d.child(div().absolute().left_0().right_0().top_full().h(px(h * 2.0)).bg(t.bg_panel))
                    })
                    .child(sheet),
            )
    }
}

#[allow(unused)]
fn _u(_: &App) {}
