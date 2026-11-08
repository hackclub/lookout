//! Small primitives from `@lookout/react`: Badge, Skeleton (with the
//! shimmer), Card (with its 0.99 tap), PressScale (shrink-on-press for
//! whole surfaces), scroll-edge fades and the page scaffolds.

use std::rc::Rc;

use gpui_kit::gpui::{
    AnyElement, App, Bounds, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window, canvas, div, fill, linear_color_stop, linear_gradient, point, px, size,
};

use crate::motion::{self, Curve, Spring};
use crate::theme::{self, Theme, theme};
use crate::ui::button::{PressState, press_values};
use crate::ui::text::TextExt;

// ── Badge ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BadgeSize {
    Sm,
    Md,
    Lg,
}

/// `Badge` (status pill): white 600 text on the status colour, radius 999;
/// the overlay variant adds a 1px black/10% ring.
pub fn badge(status: &str, size: BadgeSize, overlay: bool, t: &Theme) -> impl IntoElement {
    let (label, color) = theme::status_config(status, t);
    let label: SharedString = if label.is_empty() { status.to_string().into() } else { label.into() };
    let (fs, py, pxl) = match size {
        BadgeSize::Sm => (10.0, 2.0, 8.0),
        BadgeSize::Md => (12.0, 4.0, 12.0),
        BadgeSize::Lg => (13.0, 6.0, 16.0),
    };
    let mut el = div()
        .flex_none()
        .px(px(pxl))
        .py(px(py))
        .rounded(px(999.0))
        .bg(color)
        .text_color(theme::hex(0xffffff))
        .ts(fs)
        .w600()
        .child(label);
    if overlay {
        el = el.shadow(vec![theme::sh(0.0, 0.0, 0.0, 1.0, theme::black(0.1))]);
    }
    el
}

// ── Skeleton ────────────────────────────────────────────────────────────

/// `Skeleton`: a rounded block in `skeleton.bg` with the shimmer gradient
/// (`transparent → shimmer → transparent`, 200% wide, `shimmer 1.5s
/// ease-in-out infinite`). The Tauri app only defined the `shimmer`
/// keyframes after the first recorder mounted, so most skeletons sat still;
/// here they always shimmer (a deliberate fix).
#[derive(IntoElement)]
pub struct Skeleton {
    w: Option<f32>,
    w_frac: Option<f32>,
    h: Option<f32>,
    aspect: Option<f32>,
    radius: f32,
    flex_1: bool,
}

impl Skeleton {
    pub fn new() -> Self {
        Self { w: None, w_frac: None, h: Some(20.0), aspect: None, radius: 8.0, flex_1: false }
    }
    pub fn w(mut self, w: f32) -> Self {
        self.w = Some(w);
        self
    }
    pub fn w_frac(mut self, f: f32) -> Self {
        self.w_frac = Some(f);
        self
    }
    pub fn h(mut self, h: f32) -> Self {
        self.h = Some(h);
        self
    }
    /// `aspectRatio` (w/h) — height follows the width.
    pub fn aspect(mut self, a: f32) -> Self {
        self.aspect = Some(a);
        self.h = None;
        self
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = r;
        self
    }
    pub fn flex_1(mut self) -> Self {
        self.flex_1 = true;
        self
    }
}

impl RenderOnce for Skeleton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let bg = t.skeleton_bg;
        let hi = t.skeleton_shimmer;
        let radius = self.radius;
        window.request_animation_frame();
        let phase = motion::shimmer_phase();
        let aspect = self.aspect;
        let mut el = div().relative().flex_none();
        el = match (self.w, self.w_frac) {
            (Some(w), _) => el.w(px(w)),
            (None, Some(f)) => el.w(gpui_kit::gpui::relative(f)),
            _ => el.w_full(),
        };
        if self.flex_1 {
            el = el.flex_1();
        }
        if let Some(h) = self.h {
            el = el.h(px(h));
        }
        let shimmer = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| paint_shimmer(bounds, radius, bg, hi, phase, window),
        )
        .absolute()
        .inset_0();
        if let Some(a) = aspect {
            // Aspect-ratio box: an inner spacer whose height follows width.
            el.child(AspectBox::new(a)).child(shimmer)
        } else {
            el.child(shimmer)
        }
    }
}

fn paint_shimmer(bounds: Bounds<Pixels>, radius: f32, bg: Hsla, hi: Hsla, phase: f32, window: &mut Window) {
    window.paint_quad(fill(bounds, bg).corner_radii(px(radius)));
    let w = f32::from(bounds.size.width);
    if w <= 0.0 {
        return;
    }
    // Tile period 2W; offset runs -2W → +2W.
    let o = -2.0 * w + 4.0 * w * phase;
    for k in -3..=3 {
        let c = o + w + 2.0 * w * k as f32;
        for (a0, a1, rising) in [(c - w, c, true), (c, c + w, false)] {
            let lo = a0.max(0.0);
            let hi_x = a1.min(w);
            if hi_x - lo <= 0.5 {
                continue;
            }
            let alpha_at = |x: f32| if rising { (x - a0) / w } else { 1.0 - (x - a0) / w };
            let (s, e) = (alpha_at(lo), alpha_at(hi_x));
            let quad_bounds = Bounds {
                origin: point(bounds.origin.x + px(lo), bounds.origin.y),
                size: size(px(hi_x - lo), bounds.size.height),
            };
            let left_r = if lo <= 0.01 { radius } else { 0.0 };
            let right_r = if hi_x >= w - 0.01 { radius } else { 0.0 };
            let grad = linear_gradient(
                90.0,
                linear_color_stop(theme::fade(hi, s), 0.0),
                linear_color_stop(theme::fade(hi, e), 1.0),
            );
            window.paint_quad(fill(quad_bounds, grad).corner_radii(gpui_kit::gpui::Corners {
                top_left: px(left_r),
                bottom_left: px(left_r),
                top_right: px(right_r),
                bottom_right: px(right_r),
            }));
        }
    }
}

/// A box whose height is `width / aspect` (CSS `aspect-ratio`).
#[derive(IntoElement)]
pub struct AspectBox {
    aspect: f32,
    children: Vec<AnyElement>,
}

impl AspectBox {
    pub fn new(aspect: f32) -> Self {
        Self { aspect, children: Vec::new() }
    }
}

impl ParentElement for AspectBox {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for AspectBox {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // Taffy supports aspect_ratio on the style directly.
        let mut d = div().w_full().relative();
        d.style().aspect_ratio = Some(self.aspect);
        d.children(self.children)
    }
}

// ── Press scale for whole surfaces (Card, rows) ─────────────────────────

/// Wrap `content` so it shrinks toward its centre while pressed, the way a
/// `motion.div whileTap={{scale}}` did. GPUI can't scale arbitrary
/// content, so the wrapper insets it by `(1 - scale)/2` of its measured size
/// on each side — at 0.99 on a card this is visually indistinguishable.
#[derive(IntoElement)]
pub struct PressScale {
    id: ElementId,
    scale: f32,
    content: AnyElement,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    on_right_click: Option<Rc<dyn Fn(gpui_kit::gpui::Point<Pixels>, &mut Window, &mut App)>>,
    full: bool,
}

impl PressScale {
    pub fn new(id: impl Into<ElementId>, scale: f32, content: impl IntoElement) -> Self {
        Self { id: id.into(), scale, content: content.into_any_element(), on_click: None, on_right_click: None, full: false }
    }
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
    pub fn on_right_click(mut self, f: impl Fn(gpui_kit::gpui::Point<Pixels>, &mut Window, &mut App) + 'static) -> Self {
        self.on_right_click = Some(Rc::new(f));
        self
    }
    pub fn full(mut self) -> Self {
        self.full = true;
        self
    }
}

struct Measured(Bounds<Pixels>);

impl RenderOnce for PressScale {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| PressState::new());
        let measured = window.use_keyed_state((self.id.clone(), "m"), cx, |_, _| Measured(Bounds::default()));
        let (_, scale) = press_values(&state, window, cx);
        let b = measured.read(cx).0;
        let w = f32::from(b.size.width);
        let h = f32::from(b.size.height);
        let ix = w * (1.0 - scale) / 2.0;
        let iy = h * (1.0 - scale) / 2.0;
        let depth = 1.0 - self.scale;
        let s1 = state.clone();
        let s2 = state.clone();
        let s3 = state.clone();
        let m = measured.clone();
        let mut root = div()
            .id(self.id.clone())
            .relative()
            .px(px(ix))
            .py(px(iy))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        m.update(cx, |mm, _| mm.0 = bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(self.content)
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                s1.update(cx, |s, cx| {
                    s.pressed = true;
                    s.press.set(depth, Curve::Spring(Spring::PRESS));
                    cx.notify();
                })
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                s2.update(cx, |s, cx| {
                    s.pressed = false;
                    s.press.set(0.0, Curve::Spring(Spring::PRESS));
                    cx.notify();
                })
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                s3.update(cx, |s, cx| {
                    s.pressed = false;
                    s.press.set(0.0, Curve::Spring(Spring::PRESS));
                    cx.notify();
                })
            });
        if self.full {
            root = root.size_full();
        }
        if let Some(f) = self.on_click {
            root = root.on_click(move |ev, window, cx| {
                if ev.standard_click() {
                    f(ev, window, cx)
                }
            });
        }
        if let Some(f) = self.on_right_click {
            root = root.on_mouse_down(MouseButton::Right, move |ev, window, cx| f(ev.position, window, cx));
        }
        root
    }
}

/// `Card`: surface fill, 1px default border, radius 10, overflow hidden.
pub fn card(t: &Theme) -> gpui_kit::gpui::Div {
    div()
        .flex()
        .flex_col()
        .bg(t.bg_surface)
        .border_1()
        .border_color(t.border_default)
        .rounded(px(10.0))
        .overflow_hidden()
}

// ── Scroll-edge fades (CSS mask-image) ──────────────────────────────────

/// The Tauri lists faded their scrolled-away edges with a `mask-image`
/// gradient. GPUI has no masks; over an opaque window body the same look
/// comes from a gradient of the body colour on top of the edge.
/// A scroll viewport whose content fades out at the top and bottom edges,
/// like the Tauri app's CSS `mask-image` (the gallery's 20px, the source
/// list's 12px). The content itself goes transparent, so window vibrancy or
/// Mica shows through. Uses the vendored GPUI's `Window::with_edge_fade`.
/// Pass 0 for an edge that shouldn't fade (nothing scrolled past it).
pub fn edge_faded(child: impl IntoElement, top: f32, bottom: f32) -> EdgeFaded {
    EdgeFaded { child: child.into_any_element(), top, bottom }
}

pub struct EdgeFaded {
    child: AnyElement,
    top: f32,
    bottom: f32,
}

impl IntoElement for EdgeFaded {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui_kit::gpui::Element for EdgeFaded {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui_kit::gpui::GlobalElementId>,
        _inspector_id: Option<&gpui_kit::gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (gpui_kit::gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui_kit::gpui::GlobalElementId>,
        _inspector_id: Option<&gpui_kit::gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&gpui_kit::gpui::GlobalElementId>,
        _inspector_id: Option<&gpui_kit::gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_edge_fade(bounds, px(self.top), px(self.bottom), |window| child.paint(window, cx));
    }
}

pub fn edge_fade(top: bool, height: f32, bg: Hsla) -> impl IntoElement {
    let (a, b) = (bg, theme::fade(bg, 0.0));
    let grad = if top {
        linear_gradient(180.0, linear_color_stop(a, 0.0), linear_color_stop(b, 1.0))
    } else {
        linear_gradient(180.0, linear_color_stop(b, 0.0), linear_color_stop(a, 1.0))
    };
    let mut d = div().absolute().left_0().right_0().h(px(height)).bg(grad);
    d = if top { d.top_0() } else { d.bottom_0() };
    d
}

// ── PageLayout (desktop page scaffold) ──────────────────────────────────

/// Back button label the way each platform's Tauri build drew it.
pub fn back_button(id: impl Into<ElementId>, label: &'static str, t: &Theme, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> crate::ui::button::Button {
    use crate::ui::button::{BtnSize, Button, Variant};
    let b = Button::empty(id).variant(Variant::Secondary).size(BtnSize::Sm).card_style(t).on_click(on_click);
    if theme::is_mac() {
        b.child(format!("← {label}"))
    } else {
        b.gap(4.0)
            .child(crate::ui::icon::Icon::LkChevronLeft.el(14.0, t.text_secondary))
            .child(label)
    }
}

/// The 72px icon circle used by PageLayout. Its background was invalid CSS
/// in the Tauri app (`var(...)20`), so it rendered transparent — kept.
pub fn page_icon(child: impl IntoElement) -> impl IntoElement {
    div().size(px(72.0)).rounded_full().flex().items_center().justify_center().mb(px(20.0)).flex_none().child(child)
}
