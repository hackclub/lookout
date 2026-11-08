//! `UpdatePill` — the Ghostty-style titlebar pill: download progress while
//! an update streams in, then "Restart to Complete Update".
//!
//! `AnimatePresence` + `motion.button`: enters on a 450/32 spring (opacity,
//! y ∓8 from the anchored edge, scale 0.95 → 1) and exits over 150ms
//! ease-out. GPUI has no transforms, so the scale is faked the way Button
//! does it: the background capsule (and the ring / icon) are painted
//! scaled about their centres while the label stays put. Colours cross-fade
//! over 150ms `ease` (the CSS `transition`); the hover tint (16% → 19%)
//! only applies when the pill is clickable.

use gpui_kit::gpui::{
    Context, CursorStyle, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, canvas, div, px,
};

use crate::motion::{Anim, Curve, Ease, Spring};
use crate::theme::{self, theme};
use crate::ui::feedback::NumberFlow;
use crate::ui::icon::Icon;
use crate::ui::paint;
use crate::ui::text::TextExt;
use crate::updater::{UpdatePhase, Updater};

const ENTER: Spring = Spring::new(450.0, 32.0);
const EXIT: Curve = Curve::tween(150.0, Ease::EASE_OUT);
const COLORS: Curve = Curve::tween(150.0, Ease::CSS_EASE);
const RING: Curve = Curve::tween(200.0, Ease::CSS_EASE);

pub struct UpdatePill {
    updater: Entity<Updater>,
    /// `origin="bottom"` slides up from below (Windows); `"top"` slides down.
    bottom: bool,
    /// The last non-idle phase, still drawn while the pill exits.
    shown: UpdatePhase,
    present: bool,
    exiting: bool,
    opacity: Anim,
    y: Anim,
    scale: Anim,
    solid: Anim,
    hover: Anim,
    ring: Anim,
    hovered: bool,
}

impl UpdatePill {
    pub fn new(updater: Entity<Updater>, bottom: bool, cx: &mut Context<Self>) -> Self {
        cx.observe(&updater, |_, _, cx| cx.notify()).detach();
        Self {
            updater,
            bottom,
            shown: UpdatePhase::Idle,
            present: false,
            exiting: false,
            opacity: Anim::new(0.0),
            y: Anim::new(0.0),
            scale: Anim::new(0.95),
            solid: Anim::new(0.0),
            hover: Anim::new(0.0),
            ring: Anim::new(0.0),
            hovered: false,
        }
    }

    /// Whether the pill draws anything (in flight, or still exiting).
    pub fn visible(&self, cx: &gpui_kit::gpui::App) -> bool {
        self.present || self.updater.read(cx).phase != UpdatePhase::Idle
    }

    /// The pill's place unmounted (left the gallery): the next mount enters
    /// afresh, as `AnimatePresence` would.
    pub fn unmount(&mut self) {
        self.present = false;
        self.exiting = false;
        self.hovered = false;
        self.hover.jump(0.0);
    }

    fn offset(&self) -> f32 {
        if self.bottom { 8.0 } else { -8.0 }
    }
}

fn is_solid(p: &UpdatePhase) -> bool {
    matches!(p, UpdatePhase::Ready { .. } | UpdatePhase::Installing { .. })
}

impl Render for UpdatePill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let phase = self.updater.read(cx).phase.clone();
        let off = self.offset();

        if phase != UpdatePhase::Idle {
            let progress = match &phase {
                UpdatePhase::Downloading { progress, .. } => *progress as f32 / 100.0,
                _ => 1.0,
            };
            if !self.present {
                self.opacity.jump(0.0);
                self.y.jump(off);
                self.scale.jump(0.95);
                self.solid.jump(if is_solid(&phase) { 1.0 } else { 0.0 });
                self.ring.jump(progress);
            }
            if !self.present || self.exiting {
                self.opacity.set(1.0, Curve::Spring(ENTER));
                self.y.set(0.0, Curve::Spring(ENTER));
                self.scale.set(1.0, Curve::Spring(ENTER));
                self.present = true;
                self.exiting = false;
            }
            self.solid.set(if is_solid(&phase) { 1.0 } else { 0.0 }, COLORS);
            self.ring.set(progress, RING);
            self.shown = phase.clone();
        } else if self.present && !self.exiting {
            self.exiting = true;
            self.opacity.set(0.0, EXIT);
            self.y.set(off, EXIT);
            self.scale.set(0.95, EXIT);
        }
        let clickable = matches!(phase, UpdatePhase::Ready { .. });
        self.hover.set(if clickable && self.hovered { 1.0 } else { 0.0 }, COLORS);

        let moving = crate::keep_animating!(window, self.opacity, self.y, self.scale, self.solid, self.hover, self.ring);
        if self.exiting && !moving {
            self.present = false;
            self.exiting = false;
        }
        if !self.present {
            return div().into_any_element();
        }

        let (solid, hover, scale) = (self.solid.get(), self.hover.get(), self.scale.get());
        let fill = theme::mix(
            theme::mix(t.bg_surface, theme::fade(t.text_primary, 0.16), solid),
            theme::fade(t.text_primary, 0.19),
            hover,
        );
        let border = theme::mix(t.border_default, theme::transparent(), solid);
        let color = theme::mix(t.text_secondary, t.text_primary, solid);
        let tip = match &self.shown {
            UpdatePhase::Downloading { version, .. } => format!("Downloading v{version}"),
            UpdatePhase::Installing { version } => format!("Installing v{version}"),
            UpdatePhase::Ready { version } => format!("Restart to update to v{version}"),
            UpdatePhase::Idle => String::new(),
        };

        let mut pill = div()
            .id("update-pill")
            .relative()
            .top(px(self.y.get()))
            .opacity(self.opacity.get().clamp(0.0, 1.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .h(px(22.0))
            .px(px(10.0))
            .border_1()
            .border_color(theme::transparent())
            .text_color(color)
            .ts(11.0)
            .w500()
            .whitespace_nowrap()
            .cursor(CursorStyle::Arrow)
            .child(
                canvas(|_, _, _| (), move |bounds, _, window, _| {
                    paint::paint_rounded_bg(bounds, 11.0, scale, fill, border, window);
                })
                .absolute()
                .top(px(-1.0))
                .left(px(-1.0))
                .right(px(-1.0))
                .bottom(px(-1.0)),
            );

        pill = match &self.shown {
            UpdatePhase::Downloading { progress, .. } => {
                let shown = self.ring.get();
                let track = t.border_default;
                pill.child(
                    canvas(|_, _, _| (), move |bounds, _, window, _| {
                        let c = paint::bounds_center(bounds);
                        let (r, w) = (5.0 * scale, 1.5 * scale);
                        if let Some(p) = paint::ring(c, r, w) {
                            window.paint_path(p, track);
                        }
                        if shown > 0.0005 {
                            let a0 = -std::f32::consts::FRAC_PI_2;
                            let a1 = a0 + std::f32::consts::TAU * shown.min(0.9999);
                            if let Some(p) = paint::round_capped_arc(c, r, w, a0, a1) {
                                window.paint_path(p, color);
                            }
                        }
                    })
                    .size(px(12.0))
                    .flex_none(),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child("Downloading Update…\u{a0}")
                        .child(NumberFlow::new("update-pill-pct", *progress as i64).suffix("%")),
                )
            }
            UpdatePhase::Installing { .. } | UpdatePhase::Ready { .. } => pill
                .child(Icon::LkPower.scaled(11.0, color, scale).mt(px(0.5)))
                .child(div().child(if matches!(self.shown, UpdatePhase::Installing { .. }) {
                    "Installing Update…"
                } else {
                    "Restart to Complete Update"
                })),
            UpdatePhase::Idle => pill,
        };

        let updater = self.updater.clone();
        pill.tooltip(move |window, cx| crate::ui::tooltip::Tooltip::new(tip.clone()).build(window, cx))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hovered = *hovered;
                cx.notify();
            }))
            // `-webkit-app-region: no-drag` inside the macOS drag strip.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, _, cx| {
                if clickable {
                    updater.update(cx, |u, cx| u.restart(cx));
                }
            })
            .into_any_element()
    }
}
