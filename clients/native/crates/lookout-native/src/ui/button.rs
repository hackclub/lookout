//! `Button` — a faithful port of `clients/react/src/ui/Button.tsx`.
//!
//! The Tauri button is a transparent `motion.button` whose *background
//! layer* (an absolutely positioned div at `inset: -1`, clipped to a
//! squircle with corner smoothing 0.7) animates between three variants with
//! one spring `{stiffness: 1500, damping: 60}`:
//!
//! | variant | scale | fill / border |
//! |---------|-------|---------------|
//! | idle    | 1     | idle colours  |
//! | hover   | 1     | hover colours |
//! | active  | 0.96  | hover colours |
//!
//! Only the background scales — the label stays put, which is what gives the
//! press its "squish". Disabled/loading buttons drop to 60% opacity and get
//! no hover or tap. We paint the squircle ourselves with the press baked
//! into the geometry (GPUI has no element transforms). Unlike a CSS scale,
//! the press moves every edge in by the same amount, taken from the shorter
//! side, so wide buttons don't pinch in more at the ends; we drive hover and
//! press from separate springs so a quick tap that leaves the pointer
//! hovering keeps the hover fill, exactly as framer-motion's variants do.

use std::rc::Rc;

use gpui_kit::gpui::{
    AnyElement, App, ClickEvent, CursorStyle, ElementId, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, PathBuilder, RenderOnce, SharedString, StatefulInteractiveElement,
    Styled, Window, canvas, div, px, relative,
};

use crate::motion::{Anim, Curve, Spring};
use crate::theme::{self, Theme, theme};
use crate::ui::a11y::prelude::*;
use gpui_kit::gpui::prelude::FluentBuilder;
use crate::ui::paint;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Success,
    Danger,
    Warning,
    Secondary,
    Ghost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtnSize {
    Sm,
    Md,
    Lg,
}

impl BtnSize {
    /// `(padding-y, padding-x, font-size)` of the label span.
    fn metrics(self) -> (f32, f32, f32) {
        match self {
            BtnSize::Sm => (6.0, 12.0, 12.0),
            BtnSize::Md => (8.0, 16.0, 13.0),
            BtnSize::Lg => (12.0, 24.0, 15.0),
        }
    }
}

/// The press/hover state kept per button across frames.
pub struct PressState {
    pub hovered: bool,
    pub pressed: bool,
    pub hover: Anim,
    pub press: Anim,
}

impl PressState {
    pub fn new() -> Self {
        Self { hovered: false, pressed: false, hover: Anim::new(0.0), press: Anim::new(0.0) }
    }
    /// Advance; returns whether anything is still moving.
    pub fn tick(&mut self) -> bool {
        let a = self.hover.tick();
        let b = self.press.tick();
        a | b
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    variant: Variant,
    size: BtnSize,
    loading: bool,
    disabled: bool,
    full_width: bool,
    flex_1: bool,
    radius: f32,
    bg: Option<Hsla>,
    border: Option<Hsla>,
    color: Option<Hsla>,
    on_click: Option<ClickHandler>,
    fixed_size: Option<(f32, f32)>,
    pad: Option<(f32, f32)>,
    font_size: Option<f32>,
    gap: f32,
    tooltip: Option<SharedString>,
    press_scale: f32,
    hover_bg: Option<Hsla>,
    drop: Option<Hsla>,
    medium: bool,
    aria_label: Option<SharedString>,
    aria_description: Option<SharedString>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let mut b = Self::empty(id);
        b.label = Some(label.into());
        b
    }

    /// A button with only custom children (icons, mixed content).
    pub fn empty(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            children: Vec::new(),
            variant: Variant::Primary,
            size: BtnSize::Md,
            loading: false,
            disabled: false,
            full_width: false,
            flex_1: false,
            radius: 8.0,
            bg: None,
            border: None,
            color: None,
            on_click: None,
            fixed_size: None,
            pad: None,
            font_size: None,
            gap: 8.0,
            tooltip: None,
            press_scale: 0.96,
            hover_bg: None,
            drop: None,
            medium: false,
            aria_label: None,
            aria_description: None,
        }
    }

    pub fn variant(mut self, v: Variant) -> Self {
        self.variant = v;
        self
    }
    pub fn size(mut self, s: BtnSize) -> Self {
        self.size = s;
        self
    }
    pub fn loading(mut self, on: bool) -> Self {
        self.loading = on;
        self
    }
    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    pub fn flex_1(mut self) -> Self {
        self.flex_1 = true;
        self
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = r;
        self
    }
    /// Caller `style.background` — used for both idle and hover (so such a
    /// button only shows the press, never a hover tint).
    pub fn bg(mut self, c: Hsla) -> Self {
        self.bg = Some(c);
        self
    }
    pub fn border(mut self, c: Hsla) -> Self {
        self.border = Some(c);
        self
    }
    pub fn text_color(mut self, c: Hsla) -> Self {
        self.color = Some(c);
        self
    }
    /// `cardButtonStyle`: surface fill, default border, radius 10.
    pub fn card_style(self, t: &Theme) -> Self {
        self.bg(t.bg_surface).border(t.border_default).radius(10.0)
    }
    pub fn fixed(mut self, w: f32, h: f32) -> Self {
        self.fixed_size = Some((w, h));
        self
    }
    pub fn padding(mut self, y: f32, x: f32) -> Self {
        self.pad = Some((y, x));
        self
    }
    pub fn font_size(mut self, s: f32) -> Self {
        self.font_size = Some(s);
        self
    }
    pub fn gap(mut self, g: f32) -> Self {
        self.gap = g;
        self
    }
    pub fn tooltip(mut self, t: impl Into<SharedString>) -> Self {
        self.tooltip = Some(t.into());
        self
    }
    pub fn press_scale(mut self, s: f32) -> Self {
        self.press_scale = s;
        self
    }
    /// A hover fill for a [`Self::bg`] button (which otherwise only presses).
    pub fn hover_bg(mut self, c: Hsla) -> Self {
        self.hover_bg = Some(c);
        self
    }
    /// A crisp 1px drop under the shape (`0 1px 2px` in the tactile recipe),
    /// pressed along with it.
    pub fn drop(mut self, c: Hsla) -> Self {
        self.drop = Some(c);
        self
    }
    /// Medium label instead of semibold.
    pub fn medium(mut self) -> Self {
        self.medium = true;
        self
    }
    /// The name screen readers announce. Defaults to the text label, then the
    /// tooltip; icon-only buttons with neither must set one.
    pub fn aria_label(mut self, l: impl Into<SharedString>) -> Self {
        self.aria_label = Some(l.into());
        self
    }
    /// Extra spoken detail, read after the name.
    pub fn aria_description(mut self, d: impl Into<SharedString>) -> Self {
        self.aria_description = Some(d.into());
        self
    }
    pub fn child(mut self, el: impl IntoElement) -> Self {
        self.children.push(el.into_any_element());
        self
    }
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

/// Colours for one variant: `(idle bg, idle border, hover bg, hover border, text)`.
fn variant_colors(v: Variant, t: &Theme) -> (Hsla, Hsla, Hsla, Hsla, Hsla) {
    let clear = theme::transparent();
    match v {
        Variant::Primary => (t.accent, clear, t.accent_hover, clear, t.on_accent),
        Variant::Success => (theme::status::success(), clear, theme::status::success(), clear, theme::hex(0xffffff)),
        Variant::Danger => (theme::status::danger(), clear, theme::status::danger(), clear, theme::hex(0xffffff)),
        Variant::Warning => (theme::status::warning(), clear, theme::status::warning(), clear, theme::hex(0x000000)),
        Variant::Secondary => (clear, t.border_hover, t.bg_surface, t.border_hover, t.text_secondary),
        Variant::Ghost => (clear, clear, t.bg_selected, clear, t.text_secondary),
    }
}

/// Paint the squircle background with a 1px inner border, pressed to
/// `scale`: every edge moves in by the same amount (the shorter side's
/// shrink, see [`paint::press_inset`]) and the corner radius with it. Used
/// by Button and by every other "pressable" surface that mirrored it.
pub fn paint_squircle_bg(
    bounds: gpui_kit::gpui::Bounds<gpui_kit::gpui::Pixels>,
    radius: f32,
    smoothing: f32,
    scale: f32,
    fill: Hsla,
    border: Hsla,
    window: &mut Window,
) {
    let d = paint::press_inset(bounds, scale);
    let outer = paint::inset(bounds, d);
    let radius = (radius - d).max(0.0);
    if fill.a > 0.001 {
        let inner = paint::inset(outer, if border.a > 0.001 { 1.0 } else { 0.0 });
        let r = if border.a > 0.001 { (radius - 1.0).max(0.0) } else { radius };
        if let Some(p) = paint::squircle(inner, r, smoothing, 1.0) {
            window.paint_path(p, fill);
        }
    }
    if border.a > 0.001 {
        let inner = paint::inset(outer, 1.0);
        let mut b = PathBuilder::fill();
        paint::add_squircle(&mut b, outer, radius, smoothing, 1.0);
        paint::add_squircle(&mut b, inner, (radius - 1.0).max(0.0), smoothing, 1.0);
        if let Ok(p) = b.build() {
            window.paint_path(p, border);
        }
    }
}

/// Wire press/hover tracking onto an element with the given state. Returns
/// the current (hover progress, scale) and requests frames while moving.
pub fn press_values(state: &gpui_kit::gpui::Entity<PressState>, window: &mut Window, cx: &mut App) -> (f32, f32) {
    let moving = state.update(cx, |s, _| s.tick());
    if moving {
        window.request_animation_frame();
    }
    let s = state.read(cx);
    (s.hover.get(), 1.0 - s.press.get())
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let (idle_bg, idle_border, hover_bg, hover_border, variant_text) = variant_colors(self.variant, &t);
        let idle_bg = self.bg.unwrap_or(idle_bg);
        let hover_bg = self.hover_bg.or(self.bg).unwrap_or(hover_bg);
        let idle_border = self.border.unwrap_or(idle_border);
        let hover_border = self.border.unwrap_or(hover_border);
        let text_color = self.color.unwrap_or(variant_text);
        let inert = self.disabled || self.loading;
        let press_depth = 1.0 - self.press_scale;

        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| PressState::new());
        // Disabled buttons have no whileHover/whileTap: they animate back to idle.
        if inert {
            state.update(cx, |s, _| {
                if s.hover.target() != 0.0 {
                    s.hover.set(0.0, Curve::Spring(Spring::PRESS));
                }
                if s.press.target() != 0.0 {
                    s.press.set(0.0, Curve::Spring(Spring::PRESS));
                }
                s.pressed = false;
            });
        }
        let (hover, scale) = press_values(&state, window, cx);
        let fill = theme::mix(idle_bg, hover_bg, hover);
        let border = theme::mix(idle_border, hover_border, hover);
        let radius = self.radius;
        let drop = self.drop;

        let (py, pxl, fs) = self.size.metrics();
        let (py, pxl) = self.pad.unwrap_or((py, pxl));
        let fs = self.font_size.unwrap_or(fs);

        let a11y_name = self.aria_label.clone().or_else(|| self.label.clone()).or_else(|| self.tooltip.clone());
        let a11y_desc = self.aria_description.clone().or_else(|| self.tooltip.clone().filter(|t| Some(t) != a11y_name.as_ref()));
        let mut label = div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(self.gap))
            .w_full()
            .h_full()
            .py(px(py))
            .px(px(pxl))
            .ts(fs)
            .font_weight(if self.medium { gpui_kit::gpui::FontWeight::MEDIUM } else { gpui_kit::gpui::FontWeight::SEMIBOLD })
            .text_color(text_color)
            .whitespace_nowrap();
        if self.loading {
            let c = if self.variant == Variant::Warning { theme::hex(0x000000) } else { theme::hex(0xffffff) };
            label = label.child(Spinner::new(SpinnerSize::Sm).color(c));
        }
        if let Some(l) = self.label {
            label = label.child(l);
        }
        label = label.children(self.children);

        let on_click = self.on_click.clone();
        let mut root = div()
            .id(self.id.clone())
            .role(Role::Button)
            .when_some(a11y_name, |d, l| d.aria_label(l))
            .when_some(a11y_desc, |d, l| d.aria_description(l))
            .a11y_flags(NodeFlags { disabled: self.disabled, busy: self.loading, ..Default::default() })
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .border_1()
            .border_color(theme::transparent())
            .when_some_opacity(inert)
            .child(
                canvas(|_, _, _| (), move |bounds, _, window, _| {
                    if let Some(d) = drop {
                        let mut below = bounds;
                        below.origin.y += px(1.0);
                        paint_squircle_bg(below, radius, 0.7, scale, d, theme::transparent(), window);
                    }
                    paint_squircle_bg(bounds, radius, 0.7, scale, fill, border, window);
                })
                .absolute()
                .top(px(-1.0))
                .left(px(-1.0))
                .right(px(-1.0))
                .bottom(px(-1.0)),
            )
            .child(label);

        if let Some((w, h)) = self.fixed_size {
            root = root.w(px(w)).h(px(h));
        }
        if self.full_width {
            root = root.w_full();
        }
        if self.flex_1 {
            root = root.flex_1().min_w(px(0.0));
        }
        // Native feel: buttons keep the arrow (only text fields get a
        // different cursor).
        root = root.cursor(CursorStyle::Arrow);
        if let Some(tip) = self.tooltip.clone() {
            root = root.tooltip(move |window, cx| crate::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx));
        }

        if !inert {
            let s1 = state.clone();
            let s2 = state.clone();
            let s3 = state.clone();
            let s4 = state.clone();
            root = root
                .on_hover(move |hovered, _window, cx| {
                    s1.update(cx, |s, cx| {
                        s.hovered = *hovered;
                        s.hover.set(if *hovered || s.pressed { 1.0 } else { 0.0 }, Curve::Spring(Spring::PRESS));
                        cx.notify();
                    });
                })
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    s2.update(cx, |s, cx| {
                        s.pressed = true;
                        s.press.set(press_depth, Curve::Spring(Spring::PRESS));
                        s.hover.set(1.0, Curve::Spring(Spring::PRESS));
                        cx.notify();
                    });
                })
                .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                    s3.update(cx, |s, cx| {
                        s.pressed = false;
                        s.press.set(0.0, Curve::Spring(Spring::PRESS));
                        cx.notify();
                    });
                })
                .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                    s4.update(cx, |s, cx| {
                        s.pressed = false;
                        s.press.set(0.0, Curve::Spring(Spring::PRESS));
                        if !s.hovered {
                            s.hover.set(0.0, Curve::Spring(Spring::PRESS));
                        }
                        cx.notify();
                    });
                });
            if let Some(f) = on_click {
                let f2 = f.clone();
                root = root
                    .on_click(move |ev, window, cx| f(ev, window, cx))
                    .on_a11y_click(move |window, cx| f2(&ClickEvent::default(), window, cx));
            }
        }
        root
    }
}

trait OpacityWhen: Styled + Sized {
    fn when_some_opacity(self, inert: bool) -> Self {
        if inert { self.opacity(0.6) } else { self }
    }
}
impl<T: Styled + Sized> OpacityWhen for T {}

// Keep `relative` imported for downstream helpers.
#[allow(unused)]
fn _unused() {
    let _ = relative(1.0);
}
