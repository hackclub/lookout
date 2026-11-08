//! Modals — `Overlay.tsx`, `NamingModal`'s backdrop, and the native
//! `confirm()` dialogs, drawn in-window.
//!
//! The Tauri overlay: backdrop `colors.bg.backdrop` over the whole window
//! (instant), panel 14px radius, 1px default border, `0 24px 64px
//! rgba(0,0,0,.45)`, entering with `y: 10 → 0` on a 420/34/0.7 spring and
//! `opacity 0 → 1` over 160ms easeOut. It unmounted instantly; here the
//! panel and backdrop fade out over 120ms instead — a deliberate
//! improvement, since an instant vanish reads as a glitch next to an
//! animated entrance.

use std::rc::Rc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, AnyView, App, Context, EntityId, FocusHandle, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, SharedString, Styled, Window, div, px,
};

use crate::motion::{Anim, Curve, Ease, Spring};
use crate::theme::{self, Theme, is_windows, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::text::TextExt;

/// How a modal entered (the two Tauri variants differ by 2px of travel).
#[derive(Clone, Copy)]
pub enum Entrance {
    /// `Overlay`: y 10 → 0.
    Overlay,
    /// `NamingModal`: y 12 → 0, backdrop uses the Windows gradient fill.
    Naming,
}

pub struct ModalSlot {
    pub id: EntityId,
    pub view: AnyView,
    pub y: Anim,
    pub opacity: Anim,
    /// Overlay only: `scale 0.99 → 1`. Overlay's transition object names
    /// only `y` and `opacity`, so motion hands `scale` the whole object
    /// (get-value-transition.mjs) — a *defined* transition with no type or
    /// duration: a 300ms tween on the base ease "easeOut".
    pub scale: Anim,
    pub leaving: bool,
    pub entrance: Entrance,
    pub on_dismiss: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    pub width: ModalWidth,
}

#[derive(Clone, Copy)]
pub enum ModalWidth {
    /// `min(<px>, 92vw)` panel width.
    Max(f32),
    /// NamingModal's card: `width 100%, maxWidth 400` (the card draws itself).
    Bare(f32),
}

impl ModalSlot {
    pub fn new(view: AnyView, entrance: Entrance, width: ModalWidth, on_dismiss: Option<Rc<dyn Fn(&mut Window, &mut App)>>) -> Self {
        let travel = match entrance {
            Entrance::Overlay => 10.0,
            Entrance::Naming => 12.0,
        };
        Self {
            id: view.entity_id(),
            view,
            y: Anim::from_to(travel, 0.0, Curve::Spring(Spring::OVERLAY)),
            opacity: Anim::from_to(0.0, 1.0, Curve::tween(160.0, Ease::EASE_OUT)),
            scale: match entrance {
                Entrance::Overlay => Anim::from_to(0.99, 1.0, Curve::tween(300.0, Ease::EASE_OUT)),
                Entrance::Naming => Anim::new(1.0),
            },
            leaving: false,
            entrance,
            on_dismiss,
            width,
        }
    }

    pub fn leave(&mut self) {
        if !self.leaving {
            self.leaving = true;
            self.opacity.set(0.0, Curve::tween(120.0, Ease::EASE_OUT));
        }
    }

    pub fn tick(&mut self) -> bool {
        self.y.tick() | self.opacity.tick() | self.scale.tick()
    }

    pub fn finished(&self) -> bool {
        self.leaving && !self.opacity.is_animating()
    }
}

/// Render the modal stack (topmost last). Backdrop fades with the panel on
/// exit; on entry it appears instantly, as before.
pub fn render_modals(slots: &[ModalSlot], t: &Theme, window_w: f32) -> Vec<AnyElement> {
    slots
        .iter()
        .map(|slot| {
            let o = slot.opacity.get();
            let backdrop_alpha = if slot.leaving { o } else { 1.0 };
            let dismiss = slot.on_dismiss.clone();
            let fill = t.bg_backdrop;
            let gradient = matches!(slot.entrance, Entrance::Naming) && is_windows();
            let mut backdrop = div()
                .id(("modal-backdrop", slot.id))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(20.0))
                .opacity(backdrop_alpha)
                .occlude();
            backdrop = if gradient {
                backdrop.bg(gpui_kit::gpui::linear_gradient(
                    180.0,
                    gpui_kit::gpui::linear_color_stop(theme::fade(fill, 0.0), 0.0),
                    gpui_kit::gpui::linear_color_stop(fill, 1.0),
                ))
            } else {
                backdrop.bg(fill)
            };
            if let Some(d) = dismiss {
                backdrop = backdrop.on_mouse_down(MouseButton::Left, move |_, window, cx| d(window, cx));
            }
            let width = match slot.width {
                ModalWidth::Max(w) => w.min(window_w * 0.92),
                ModalWidth::Bare(w) => w.min(window_w - 40.0),
            };
            let panel = div()
                .id(("modal-panel", slot.id))
                .w(px(width))
                .max_h_full()
                .mt(px(slot.y.get() * 2.0))
                .opacity(o)
                // Clicks inside the panel never reach the backdrop.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .map(|d| match slot.width {
                    ModalWidth::Max(_) => {
                        // The panel chrome (shadow, fill, border) is painted
                        // with the 0.99 → 1 scale baked into its geometry.
                        // GPUI can't scale the content itself; it stays at 1
                        // — a ≤2px difference at the edges while the panel is
                        // still fading in (see docs/animation-audit.md).
                        let scale = slot.scale.get();
                        let (fill, border) = (t.bg_panel, t.border_default);
                        d.relative()
                            .flex()
                            .flex_col()
                            .min_h(px(0.0))
                            .child(
                                gpui_kit::gpui::canvas(|_, _, _| (), move |bounds, _, window, _| {
                                    let b = crate::ui::paint::scale_bounds(bounds, scale);
                                    window.paint_drop_shadows(
                                        b,
                                        gpui_kit::gpui::Corners::all(px(14.0 * scale)),
                                        &[theme::sh(0.0, 24.0, 64.0, 0.0, theme::black(0.45))],
                                    );
                                    crate::ui::paint::paint_rounded_bg(bounds, 14.0, scale, fill, border, window);
                                })
                                .absolute()
                                .inset_0(),
                            )
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .min_h(px(0.0))
                                    .max_h_full()
                                    .border_1()
                                    .border_color(theme::transparent())
                                    .rounded(px(14.0))
                                    .overflow_hidden()
                                    .child(slot.view.clone()),
                            )
                    }
                    ModalWidth::Bare(_) => d.child(slot.view.clone()),
                });
            backdrop.child(panel).into_any_element()
        })
        .collect()
}

// ── Confirm dialog (Tauri `confirm()` / `message()`) ────────────────────

pub struct ConfirmDialog {
    pub title: SharedString,
    pub message: SharedString,
    pub ok_label: SharedString,
    pub cancel_label: Option<SharedString>,
    pub destructive: bool,
    pub on_ok: Rc<dyn Fn(&mut Window, &mut App)>,
    pub on_cancel: Rc<dyn Fn(&mut Window, &mut App)>,
    pub focus: FocusHandle,
}

impl ConfirmDialog {
    pub fn new(
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        ok_label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Window, &mut App) + 'static,
        on_cancel: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            ok_label: ok_label.into(),
            cancel_label: Some("Cancel".into()),
            destructive: false,
            on_ok: Rc::new(on_ok),
            on_cancel: Rc::new(on_cancel),
            focus: cx.focus_handle(),
        }
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        let ok = self.on_ok.clone();
        let cancel = self.on_cancel.clone();
        let cancel2 = self.on_cancel.clone();
        let ok2 = self.on_ok.clone();
        let mut buttons = div().flex().gap(px(8.0)).justify_end();
        // Windows puts the primary button first; elsewhere it trails.
        let ok_btn = Button::new("confirm-ok", self.ok_label.clone())
            .variant(if self.destructive { Variant::Danger } else { Variant::Primary })
            .size(BtnSize::Md)
            .on_click(move |_, window, cx| ok(window, cx));
        let cancel_btn = self.cancel_label.clone().map(|l| {
            Button::new("confirm-cancel", l)
                .variant(Variant::Secondary)
                .size(BtnSize::Md)
                .on_click(move |_, window, cx| cancel(window, cx))
        });
        if is_windows() {
            buttons = buttons.child(ok_btn).children(cancel_btn);
        } else {
            buttons = buttons.children(cancel_btn).child(ok_btn);
        }
        div()
            .track_focus(&self.focus)
            .on_key_down(move |ev, window, cx| match ev.keystroke.key.as_str() {
                "escape" => cancel2(window, cx),
                "enter" => ok2(window, cx),
                _ => {}
            })
            .flex()
            .flex_col()
            .p(px(24.0))
            .gap(px(16.0))
            .child(
                div()
                    .child(div().ts(18.0).w600().text_color(t.text_primary).child(self.title.clone()))
                    .child(
                        div()
                            .mt(px(8.0))
                            .ts_lh(14.0, 1.5)
                            .text_color(t.text_secondary)
                            .children(self.message.split("\n\n").enumerate().map(|(i, p)| {
                                div().when(i > 0, |d| d.mt(px(10.0))).child(SharedString::from(p.to_string()))
                            })),
                    ),
            )
            .child(buttons)
    }
}
