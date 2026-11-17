//! Modals — `Overlay.tsx` (which also hosts the naming prompt) and the native
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
use crate::ui::a11y::prelude::*;
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::text::TextExt;

/// How a modal enters. The Tauri NamingModal had its own variant (12px of
/// travel, a gradient backdrop on Windows, a free-floating card); it now
/// uses the standard dialog like every other modal.
#[derive(Clone, Copy)]
pub enum Entrance {
    /// `Overlay`: y 10 → 0.
    Overlay,
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
    /// What had focus when the modal appeared ([`Self::remember_focus`]);
    /// [`Self::leave_and_restore`] hands it back.
    pub restore: Option<FocusHandle>,
}

#[derive(Clone, Copy)]
pub enum ModalWidth {
    /// `min(<px>, 92vw)` panel width.
    Max(f32),
}

impl ModalSlot {
    pub fn new(view: AnyView, entrance: Entrance, width: ModalWidth, on_dismiss: Option<Rc<dyn Fn(&mut Window, &mut App)>>) -> Self {
        let travel = match entrance {
            Entrance::Overlay => 10.0,
        };
        Self {
            id: view.entity_id(),
            view,
            y: Anim::from_to(travel, 0.0, Curve::Spring(Spring::OVERLAY)),
            opacity: Anim::from_to(0.0, 1.0, Curve::tween(160.0, Ease::EASE_OUT)),
            scale: match entrance {
                Entrance::Overlay => Anim::from_to(0.99, 1.0, crate::motion::spatial(Curve::tween(300.0, Ease::EASE_OUT))),
            },
            leaving: false,
            entrance,
            on_dismiss,
            width,
            restore: None,
        }
    }

    /// Note what has focus now (the control that opened the modal), before
    /// the modal's view takes it. Call when pushing the slot.
    pub fn remember_focus(mut self, window: &Window, cx: &App) -> Self {
        self.restore = crate::ui::focus::remember(window, cx);
        self
    }

    /// [`Self::leave`], handing focus back to what had it before the modal
    /// opened when focus is still inside the modal (or nowhere).
    /// Returns whether it moved focus.
    pub fn leave_and_restore(&mut self, window: &mut Window, cx: &mut App) -> bool {
        let was_leaving = self.leaving;
        self.leave();
        if was_leaving {
            return false;
        }
        let Some(r) = self.restore.take() else { return false };
        let in_modal = window.focused(cx).is_none() || crate::ui::focus::focus_in_trap(window);
        if in_modal {
            window.focus(&r, cx);
        }
        in_modal
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
pub fn render_modals(slots: &[ModalSlot], t: &Theme, window_w: f32, window_radius: f32) -> Vec<AnyElement> {
    slots
        .iter()
        .map(|slot| {
            let o = slot.opacity.get();
            let backdrop_alpha = if slot.leaving { o } else { 1.0 };
            let dismiss = slot.on_dismiss.clone();
            let fill = t.bg_backdrop;
            let mut backdrop = div()
                .id(("modal-backdrop", slot.id))
                .rounded(px(window_radius))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(20.0))
                .opacity(backdrop_alpha)
                .occlude();
            backdrop = backdrop.bg(fill);
            if let Some(d) = dismiss {
                backdrop = backdrop.on_mouse_down(MouseButton::Left, move |_, window, cx| d(window, cx));
            }
            let width = match slot.width {
                ModalWidth::Max(w) => w.min(window_w * 0.92),
            };
            let view_id = slot.id;
            let esc = slot.on_dismiss.clone();
            let panel = div()
                .id(("modal-panel", slot.id))
                .role(Role::Dialog)
                // Tab stays inside the dialog; Escape dismisses it (when the
                // modal's own view doesn't handle Escape first).
                .key_context(crate::ui::focus::TRAP)
                .when_some(esc.filter(|_| !slot.leaving), |d, esc| {
                    d.on_key_down(move |ev: &gpui_kit::gpui::KeyDownEvent, window, cx| {
                        if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                            cx.stop_propagation();
                            esc(window, cx);
                        }
                    })
                })
                // Named by the modal view itself (`set_dialog_title`), which
                // has rendered by the time this node is built.
                .a11y_synthetic_children(move |b| {
                    let node = b.parent_node();
                    NodeFlags { modal: true, ..Default::default() }.apply(node);
                    if let Some((role, title)) = DIALOG_TITLES.with(|m| m.borrow().get(&view_id).cloned()) {
                        node.set_role(role);
                        node.set_label(title.to_string());
                    }
                })
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
                });
            backdrop.child(panel).into_any_element()
        })
        .collect()
}

thread_local! {
    static DIALOG_TITLES: std::cell::RefCell<std::collections::HashMap<gpui_kit::gpui::EntityId, (Role, SharedString)>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Name the modal dialog that shows view `id` (call from the view's render).
/// `alert` makes it an alert dialog (a confirmation or error).
pub fn set_dialog_title(id: gpui_kit::gpui::EntityId, title: impl Into<SharedString>, alert: bool) {
    let role = if alert { Role::AlertDialog } else { Role::Dialog };
    DIALOG_TITLES.with(|m| {
        let mut m = m.borrow_mut();
        // Views come and go; keep the map from growing without bound.
        if m.len() > 64 {
            m.clear();
        }
        m.insert(id, (role, title.into()));
    });
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
        // Take focus when nothing in the dialog has it (Tab may move it to
        // the buttons, which must keep it).
        if !self.focus.contains_focused(window, cx) {
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
        set_dialog_title(cx.entity_id(), self.title.clone(), true);
        div()
            .id("confirm-dialog")
            .role(Role::Group)
            .aria_label(self.message.replace("\n\n", " "))
            .track_focus(&self.focus)
            .on_key_down(move |ev, window, cx| match ev.keystroke.key.as_str() {
                "escape" => {
                    cx.stop_propagation();
                    cancel2(window, cx)
                }
                "enter" => {
                    cx.stop_propagation();
                    ok2(window, cx)
                }
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

/// Whether confirms and alerts use the system's own dialog: NSAlert on macOS,
/// a task dialog on Windows. The Tauri app's `@tauri-apps/plugin-dialog`
/// showed native dialogs there too. Linux keeps the in-app [`ConfirmDialog`]:
/// its native counterpart was a GTK popup that matched nothing around it.
pub fn native_dialogs() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

/// Show a native dialog over `handle`'s window: `title` is the bold message,
/// `message` the detail text. `on_ok` runs if the first button is chosen.
/// Deferred, so it's safe to call while that window is being updated.
pub fn native_dialog(
    handle: gpui_kit::gpui::AnyWindowHandle,
    level: gpui_kit::gpui::PromptLevel,
    title: String,
    message: String,
    ok: String,
    cancel: Option<String>,
    on_ok: impl FnOnce(&mut Window, &mut App) + 'static,
    cx: &mut App,
) {
    use gpui_kit::gpui::PromptButton;
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| {
            let mut answers = vec![PromptButton::Ok(ok.into())];
            if let Some(c) = cancel {
                answers.push(PromptButton::Cancel(c.into()));
            }
            let rx = window.prompt(level, &title, Some(&message), &answers, cx);
            cx.spawn(async move |cx| {
                if let Ok(0) = rx.await {
                    let _ = handle.update(cx, |_, window, cx| on_ok(window, cx));
                }
            })
            .detach();
        });
    });
}
