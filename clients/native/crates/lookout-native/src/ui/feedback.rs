//! `ErrorDisplay` (page / inline / banner), `ProgressRing`, `NumberFlow`
//! rolling digits and `MinutesFlow`.

use std::rc::Rc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, canvas, div, px,
};

use crate::motion::{self, Anim, Ease};
use crate::theme::{self, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::paint;
use crate::ui::text::{MONO, TextExt};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

// ── ErrorDisplay ────────────────────────────────────────────────────────

#[derive(IntoElement)]
pub struct ErrorBanner {
    id: ElementId,
    title: Option<SharedString>,
    error: SharedString,
    reassurance: Option<SharedString>,
    action: Option<(SharedString, Handler)>,
    on_copy: Option<Handler>,
    on_dismiss: Option<Handler>,
}

impl ErrorBanner {
    pub fn new(id: impl Into<ElementId>, error: impl Into<SharedString>) -> Self {
        Self { id: id.into(), title: None, error: error.into(), reassurance: None, action: None, on_copy: None, on_dismiss: None }
    }
    pub fn title(mut self, t: impl Into<SharedString>) -> Self {
        self.title = Some(t.into());
        self
    }
    pub fn reassurance(mut self, r: impl Into<SharedString>) -> Self {
        self.reassurance = Some(r.into());
        self
    }
    pub fn action(mut self, label: impl Into<SharedString>, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.action = Some((label.into(), Rc::new(f)));
        self
    }
    pub fn on_copy(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_copy = Some(Rc::new(f));
        self
    }
}

/// The banner's small outlined buttons (an outline layer that springs to
/// 0.96 on tap).
fn banner_btn(id: impl Into<ElementId>, label: &str, color: Hsla, f: Handler) -> Button {
    Button::new(id, SharedString::from(label.to_string()))
        .variant(Variant::Ghost)
        .bg(theme::transparent())
        .border(color)
        .text_color(color)
        .radius(6.0)
        .padding(2.0, 8.0)
        .font_size(11.0)
        .on_click(move |e, w, cx| f(e, w, cx))
}

impl RenderOnce for ErrorBanner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let err = t.text_error;
        let mut text = div()
            .flex_1()
            .min_w(px(0.0))
            .ts(11.0)
            .max_h(px(120.0))
            .overflow_hidden();
        if let Some(title) = self.title.clone() {
            text = text.child(div().w700().mb(px(4.0)).child(title));
        }
        text = text.child(div().font_family(MONO).child(self.error.clone()));
        if let Some(r) = self.reassurance.clone() {
            text = text.child(div().mt(px(14.0)).opacity(0.8).child(r));
        }
        let id = self.id.clone();
        div()
            .id(self.id)
            .flex()
            .items_start()
            .gap(px(8.0))
            .px(px(16.0))
            .py(px(12.0))
            .mb(px(12.0))
            .bg(theme::rgba8(239, 68, 68, 0.15))
            .border_1()
            .border_color(theme::status::danger())
            .rounded(px(8.0))
            .text_color(err)
            .ts(13.0)
            .child(text)
            .children(self.action.map(|(l, f)| banner_btn(SharedString::from(format!("{id:?}-action")), &l, err, f)))
            .children(self.on_copy.map(|f| banner_btn(SharedString::from(format!("{id:?}-copy")), "Copy", err, f)))
            .children(self.on_dismiss.map(|f| banner_btn(SharedString::from(format!("{id:?}-x")), "×", err, f).font_size(16.0)))
    }
}

/// `ErrorDisplay variant="page"`: red title, monospace error, primary action
/// + "Copy Error".
pub fn error_page(
    title: impl Into<SharedString>,
    error: impl Into<SharedString>,
    action: Option<(SharedString, Handler)>,
    on_copy: Option<Handler>,
    t: &crate::theme::Theme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .text_center()
        .child(div().ts(20.0).w700().text_color(theme::status::danger()).mb(px(8.0)).child(title.into()))
        .child(
            div()
                .font_family(MONO)
                .ts(11.0)
                .max_w(px(400.0))
                .max_h(px(150.0))
                .overflow_hidden()
                .text_color(t.text_error)
                .mb(px(16.0))
                .child(error.into()),
        )
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .justify_center()
                .children(action.map(|(label, f)| {
                    Button::new("errpage-action", label).variant(Variant::Primary).size(BtnSize::Md).on_click(move |e, w, cx| f(e, w, cx))
                }))
                .children(on_copy.map(|f| {
                    Button::new("errpage-copy", "Copy Error").variant(Variant::Secondary).size(BtnSize::Md).on_click(move |e, w, cx| f(e, w, cx))
                })),
        )
}

// ── ProgressRing ────────────────────────────────────────────────────────

/// `ProgressRing`: track + accent arc from 12 o'clock with round caps; the
/// arc follows `progress` with `stroke-dashoffset 0.25s linear`.
#[derive(IntoElement)]
pub struct ProgressRing {
    id: ElementId,
    progress: f32,
    size: f32,
    stroke: f32,
    show_percent: bool,
    color: Option<Hsla>,
    track: Option<Hsla>,
    transition_ms: f32,
    ease: Ease,
}

impl ProgressRing {
    pub fn new(id: impl Into<ElementId>, progress: f32) -> Self {
        Self {
            id: id.into(),
            progress,
            size: 72.0,
            stroke: 5.0,
            show_percent: false,
            color: None,
            track: None,
            transition_ms: 250.0,
            ease: Ease::Linear,
        }
    }
    pub fn size(mut self, s: f32, stroke: f32) -> Self {
        self.size = s;
        self.stroke = stroke;
        self
    }
    pub fn show_percent(mut self) -> Self {
        self.show_percent = true;
        self
    }
    pub fn color(mut self, c: Hsla) -> Self {
        self.color = Some(c);
        self
    }
    pub fn track(mut self, c: Hsla) -> Self {
        self.track = Some(c);
        self
    }
    /// UpdatePill's ring animates over 200ms `ease`.
    pub fn transition(mut self, ms: f32, ease: Ease) -> Self {
        self.transition_ms = ms;
        self.ease = ease;
        self
    }
}

struct RingState(Anim);

impl RenderOnce for ProgressRing {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let p = self.progress.clamp(0.0, 1.0);
        let st = window.use_keyed_state(self.id.clone(), cx, |_, _| RingState(Anim::new(p)));
        let (ms, ease) = (self.transition_ms, self.ease);
        let moving = st.update(cx, |s, _| {
            if (s.0.target() - p).abs() > 1e-4 {
                // A CSS `transition: stroke-dashoffset …`.
                s.0.set_css(p, ms, ease);
            }
            s.0.tick()
        });
        if moving {
            window.request_animation_frame();
        }
        let shown = st.read(cx).0.get();
        let color = self.color.unwrap_or(t.accent);
        let track = self.track.unwrap_or(t.spinner_base);
        let (size, stroke) = (self.size, self.stroke);
        let percent = ((p * 100.0).floor() as i64).min(99);
        div()
            .relative()
            .size(px(size))
            .flex_none()
            .child(
                canvas(|_, _, _| (), move |bounds, _, window, _| {
                    let c = paint::bounds_center(bounds);
                    let r = (size - stroke) / 2.0;
                    if let Some(path) = paint::ring(c, r, stroke) {
                        window.paint_path(path, track);
                    }
                    if shown > 0.0005 {
                        let a0 = -std::f32::consts::FRAC_PI_2;
                        let a1 = a0 + std::f32::consts::TAU * shown.min(0.9999);
                        if let Some(path) = paint::round_capped_arc(c, r, stroke, a0, a1) {
                            window.paint_path(path, color);
                        }
                    }
                })
                .absolute()
                .inset_0(),
            )
            .when(self.show_percent, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .ts(13.0)
                        .w600()
                        .text_color(t.text_secondary)
                        .child(NumberFlow::new((self.id.clone(), "pct"), percent).suffix("%")),
                )
            })
    }
}

// ── NumberFlow ──────────────────────────────────────────────────────────

/// `<NumberFlow value>` — each digit is a vertical 0–9 strip that rolls to
/// the new value over 900ms on NumberFlow's spring-sampled `linear()`
/// curve, upward when the number grows and downward when it shrinks.
/// Entering/leaving digits fade over 450ms `ease-out`, and the strip is
/// masked so digits fade in and out at the top and bottom edges.
#[derive(IntoElement)]
pub struct NumberFlow {
    id: ElementId,
    value: i64,
    min_digits: usize,
    suffix: Option<SharedString>,
}

impl NumberFlow {
    pub fn new(id: impl Into<ElementId>, value: i64) -> Self {
        Self { id: id.into(), value, min_digits: 1, suffix: None }
    }
    /// `format={{ minimumIntegerDigits }}`.
    pub fn min_digits(mut self, n: usize) -> Self {
        self.min_digits = n;
        self
    }
    pub fn suffix(mut self, s: impl Into<SharedString>) -> Self {
        self.suffix = Some(s.into());
        self
    }
}

/// NumberFlow's `transformTiming` / `opacityTiming`.
const NF_SPIN_MS: f64 = 900.0;
const NF_FADE_MS: f64 = 450.0;

/// One digit place (keyed from the right, like NumberFlow's `integer:<n>`).
struct DigitState {
    place: usize,
    value: u8,
    /// Running spins `(start, delta)`. NumberFlow animates each update as
    /// `--_number-flow-d: -delta → 0` with `composite: accumulate`, so a new
    /// update *adds* to the ones still running instead of retargeting.
    spins: Vec<(f64, f32)>,
    /// Entering (`animateIn`) or leaving fade `(start, entering)`.
    fade: Option<(f64, bool)>,
    /// Popped out of the layout (a removed place): spins to 0, fades out,
    /// and is dropped once the flow's animations finish.
    exiting: bool,
    /// Where a popped place stood, from the box's left edge, so it fades out
    /// in place while the remaining digits slide to their new positions.
    popped_left: f32,
}

impl DigitState {
    fn new(place: usize, value: u8) -> Self {
        Self { place, value, spins: Vec::new(), fade: None, exiting: false, popped_left: 0.0 }
    }
    /// NumberFlow `Digit.getDelta` with the flow's trend.
    fn spin_to(&mut self, value: u8, trend: i64, now: f64) {
        let (v, p) = (value as i64, self.value as i64);
        let t = v - p;
        let e = if trend != 0 { trend } else { t.signum() };
        let delta = if e < 0 && v > p { v - 10 - p } else if e > 0 && v < p { 10 - p + v } else { t };
        if delta != 0 {
            self.spins.push((now, delta as f32));
        }
        self.value = value;
    }
    /// Continuous strip position now.
    fn pos(&self, now: f64) -> f32 {
        let mut pos = self.value as f32;
        for (start, delta) in &self.spins {
            let p = ((now - start) / NF_SPIN_MS).clamp(0.0, 1.0) as f32;
            pos -= delta * (1.0 - Ease::NUMBER_FLOW.apply(p));
        }
        pos
    }
    fn opacity(&self, now: f64) -> f32 {
        match self.fade {
            None => 1.0,
            Some((start, entering)) => {
                let e = Ease::EASE_OUT.apply(((now - start) / NF_FADE_MS).clamp(0.0, 1.0) as f32);
                if entering { 1.0 - 0.9999 * (1.0 - e) } else { 1.0 - 0.999 * e }
            }
        }
    }
    /// Drop finished animations; returns whether any is still running.
    fn prune(&mut self, now: f64) -> bool {
        self.spins.retain(|(start, _)| now - start < NF_SPIN_MS);
        if let Some((start, entering)) = self.fade {
            if now - start >= NF_FADE_MS && entering {
                self.fade = None;
            }
        }
        let fading = self.fade.is_some_and(|(start, _)| now - start < NF_FADE_MS);
        !self.spins.is_empty() || fading
    }
}

struct FlowState {
    value: i64,
    digits: Vec<DigitState>,
    /// Digit-box width `(start, from, to)`. When a place is added or removed
    /// NumberFlow moves everything to its new position over the transform
    /// timing (900ms, its `linear()` curve) instead of snapping; digits are
    /// anchored to the box's right edge, so animating the width slides them
    /// (and the suffix after them) exactly that way.
    width: Option<(f64, f32, f32)>,
}

impl FlowState {
    fn width_now(&self, now: f64, target: f32) -> f32 {
        match self.width {
            Some((start, from, to)) => {
                let p = ((now - start) / NF_SPIN_MS).clamp(0.0, 1.0) as f32;
                from + (to - from) * Ease::NUMBER_FLOW.apply(p)
            }
            None => target,
        }
    }
}

impl RenderOnce for NumberFlow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let value = self.value.max(0);
        let text = format!("{value:0width$}", width = self.min_digits);
        // Places from the right: places[0] is the units digit.
        let places: Vec<u8> = text.bytes().rev().map(|b| b - b'0').collect();
        let style = window.text_style();
        let fs = f32::from(style.font_size.to_pixels(window.rem_size()));
        let font = style.font();
        let color = style.color;
        let run = gpui_kit::gpui::TextRun { len: 1, font, color, background_color: None, underline: None, strikethrough: None };
        let cell_w = f32::from(window.text_system().shape_line("0".into(), px(fs), &[run], None).width);
        let target_w = cell_w * places.len() as f32;

        let st = window.use_keyed_state(self.id.clone(), cx, |_, _| FlowState {
            value,
            digits: places.iter().enumerate().map(|(i, d)| DigitState::new(i, *d)).collect(),
            width: None,
        });
        // `respectMotionPreference`: no animation at all under reduced motion.
        let animated = !motion::reduce_motion();
        let now = motion::now();
        let moving = st.update(cx, |s, _| {
            if s.value != value {
                let old_present = s.digits.iter().filter(|d| !d.exiting).count();
                let w_before = s.width_now(now, cell_w * old_present as f32);
                let trend = (value - s.value).signum();
                let mut next: Vec<DigitState> = Vec::with_capacity(places.len());
                let mut old = std::mem::take(&mut s.digits);
                for (place, d) in places.iter().enumerate() {
                    let ds = match old.iter().position(|o| o.place == place && !o.exiting) {
                        Some(i) => {
                            let mut o = old.remove(i);
                            if animated { o.spin_to(*d, trend, now) } else { o.value = *d }
                            o
                        }
                        None => {
                            // A new place: starts at 0, spins to its value
                            // and fades in (`startDigitsAtZero`, `animateIn`).
                            let mut n = DigitState::new(place, if animated { 0 } else { *d });
                            if animated {
                                n.spin_to(*d, trend, now);
                                n.fade = Some((now, true));
                            }
                            n
                        }
                    };
                    next.push(ds);
                }
                if animated {
                    // Removed places: popped where they stood, spin to 0 and
                    // fade out over `opacityTiming`.
                    for mut o in old {
                        if !o.exiting {
                            o.exiting = true;
                            o.popped_left = w_before - (o.place as f32 + 1.0) * cell_w;
                            o.spin_to(0, trend, now);
                            o.fade = Some((now, false));
                        }
                        next.push(o);
                    }
                }
                s.digits = next;
                s.value = value;
            }
            // Width follows the number of places (the font can change too).
            let current = s.width_now(now, target_w);
            let settled_to = s.width.map_or(current, |(_, _, to)| to);
            if (settled_to - target_w).abs() > 0.01 {
                s.width = animated.then_some((now, current, target_w));
            }
            let mut any = false;
            if let Some((start, _, _)) = s.width {
                if now - start < NF_SPIN_MS {
                    any = true;
                } else {
                    s.width = None;
                }
            }
            for d in &mut s.digits {
                any |= d.prune(now);
            }
            if !any {
                // `animationsfinish` removes the popped places.
                s.digits.retain(|d| !d.exiting);
            }
            any
        });
        if moving {
            window.request_animation_frame();
        }

        let s = st.read(cx);
        let box_w = s.width_now(now, target_w);
        let mut row = div().relative().flex().flex_row().items_center().line_height(px(fs)).h(px(fs));
        // Present places left → right in normal flow.
        let mut present: Vec<&DigitState> = s.digits.iter().filter(|d| !d.exiting).collect();
        present.sort_by(|a, b| b.place.cmp(&a.place));
        let mut digits_box = div().relative().flex().flex_row().h(px(fs)).w(px(box_w));
        for d in present {
            let right = d.place as f32 * cell_w;
            digits_box = digits_box.child(digit_strip(d.pos(now), d.opacity(now), right, cell_w, fs));
        }
        // Popped places keep their offset from the section's right edge.
        for d in s.digits.iter().filter(|d| d.exiting) {
            let strip = digit_strip(d.pos(now), d.opacity(now), 0.0, cell_w, fs);
            // A cell-wide wrapper pinned from the left; the strip fills it.
            digits_box = digits_box.child(div().absolute().top_0().left(px(d.popped_left)).w(px(cell_w)).h(px(fs)).child(strip));
        }
        row = row.child(digits_box);
        row.children(self.suffix.map(|s| div().child(s)))
    }
}

/// One digit: a 0–9 strip at continuous position `pos`, absolutely placed
/// `right` px from the digits' right edge.
///
/// NumberFlow spaces the numerals one padded box apart (`1em + 2s`, with
/// `s = round(0.125em)`), moves them rigidly, and hides what leaves the line
/// with a CSS mask: opaque over the line box, fading to transparent over
/// `2s` px above and below it. GPUI has no gradient masks, so the fade
/// zones are drawn as 1px clipped bands at stepped opacity — the same
/// coverage at whole-pixel resolution.
fn digit_strip(pos: f32, opacity: f32, right: f32, cell_w: f32, fs: f32) -> gpui_kit::gpui::Div {
    let s = (fs * 0.125).round().max(0.0);
    let m = 2.0 * s; // fade zone height
    let step = fs + 2.0 * s;
    let mut col = div().absolute().top_0().right(px(right)).w(px(cell_w)).h(px(fs)).opacity(opacity);
    let base = pos.floor();
    for k in 0..=1 {
        let n = (base as i64 + k).rem_euclid(10);
        let offset = k as f32 - (pos - base);
        if offset.abs() >= 1.0 {
            continue;
        }
        let y = offset * step;
        let glyph = |top: f32| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(top))
                .h(px(fs))
                .flex()
                .justify_center()
                .child(SharedString::from(n.to_string()))
        };
        // Fully opaque region: the line box.
        col = col.child(div().absolute().left_0().right_0().top_0().h(px(fs)).overflow_hidden().child(glyph(y)));
        if y.abs() > 0.01 && m >= 1.0 {
            let bands = m.round() as i32;
            for b in 0..bands {
                let alpha = (b as f32 + 0.5) / bands as f32;
                // Above the line: alpha rises toward the line box.
                let top = -m + b as f32;
                if y < 0.0 {
                    col = col.child(
                        div().absolute().left_0().right_0().top(px(top)).h(px(1.0)).overflow_hidden().opacity(alpha).child(glyph(y - top)),
                    );
                }
                // Below the line: alpha falls away from it.
                let top = fs + b as f32;
                if y > 0.0 {
                    col = col.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(top))
                            .h(px(1.0))
                            .overflow_hidden()
                            .opacity(1.0 - alpha)
                            .child(glyph(y - top)),
                    );
                }
            }
        }
    }
    col
}

/// `MinutesFlow`: `<m>m` or `<h>h <m>m` with rolling numerals.
pub fn minutes_flow(id: &str, minutes: f64, color: Hsla) -> impl IntoElement {
    let safe = minutes.round().max(0.0) as i64;
    let h = safe / 60;
    let m = safe % 60;
    // Keyed by position, as React reconciles `MinutesFlow`'s two branches:
    // at 59m → 1h 0m the *first* NumberFlow rolls 59 → 1 and the minutes
    // one mounts fresh.
    let k = |i: usize| SharedString::from(format!("{id}-{i}"));
    let mut row = div().flex().items_center().text_color(color);
    if h > 0 {
        row = row
            .child(NumberFlow::new(k(0), h))
            .child("h ")
            .child(NumberFlow::new(k(1), m))
            .child("m");
    } else {
        row = row.child(NumberFlow::new(k(0), m)).child("m");
    }
    row
}

