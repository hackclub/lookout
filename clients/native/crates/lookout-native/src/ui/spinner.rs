//! `Spinner` (clients/react/src/ui/Spinner.tsx): a 24-unit viewBox with two
//! r=10 circles, stroke 2 — a faint base ring and a quarter arc (dasharray
//! 62.83 / offset 47.12 → 90°, starting at 3 o'clock) — rotating with
//! `spin 1s linear infinite`.

use gpui_kit::gpui::{App, Hsla, IntoElement, RenderOnce, Styled, Window, canvas, div, px, ParentElement};


use crate::motion;
use crate::theme::theme;
use crate::ui::paint;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinnerSize {
    Sm,
    Md,
    Lg,
}

impl SpinnerSize {
    pub fn px(self) -> f32 {
        match self {
            SpinnerSize::Sm => 16.0,
            SpinnerSize::Md => 24.0,
            SpinnerSize::Lg => 40.0,
        }
    }
}

#[derive(IntoElement)]
pub struct Spinner {
    size: SpinnerSize,
    color: Option<Hsla>,
    base: Option<Hsla>,
    a11y: Option<(gpui_kit::gpui::ElementId, gpui_kit::gpui::SharedString)>,
}

impl Spinner {
    pub fn new(size: SpinnerSize) -> Self {
        Self { size, color: None, base: None, a11y: None }
    }
    /// Expose the spinner to screen readers as a busy progress indicator
    /// named `label` ("Loading timelapses"). Unlabelled spinners stay out
    /// of the tree (inside a busy Button, the button reports busy).
    pub fn labelled(mut self, id: impl Into<gpui_kit::gpui::ElementId>, label: impl Into<gpui_kit::gpui::SharedString>) -> Self {
        self.a11y = Some((id.into(), label.into()));
        self
    }
    pub fn color(mut self, c: Hsla) -> Self {
        self.color = Some(c);
        self
    }
    pub fn base(mut self, c: Hsla) -> Self {
        self.base = Some(c);
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let arc = self.color.unwrap_or(t.spinner_track);
        let base = self.base.unwrap_or(t.spinner_base);
        let s = self.size.px();
        window.request_animation_frame();
        let angle = motion::spin_angle(1000.0);
        let ring = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let scale = f32::from(bounds.size.width) / 24.0;
                let c = paint::bounds_center(bounds);
                let (r, w) = (10.0 * scale, 2.0 * scale);
                if let Some(p) = paint::ring(c, r, w) {
                    window.paint_path(p, base);
                }
                let a0 = angle;
                let a1 = angle + std::f32::consts::FRAC_PI_2;
                if let Some(p) = paint::round_capped_arc(c, r, w, a0, a1) {
                    window.paint_path(p, arc);
                }
            },
        )
        .size_full();
        let el = div().size(px(s)).flex_none().child(ring);
        match self.a11y {
            Some((id, label)) => {
                use crate::ui::a11y::prelude::*;
                el.id(id)
                    .role(Role::ProgressIndicator)
                    .aria_label(label)
                    .a11y_flags(NodeFlags { busy: true, ..Default::default() })
                    .into_any_element()
            }
            None => el.into_any_element(),
        }
    }
}
