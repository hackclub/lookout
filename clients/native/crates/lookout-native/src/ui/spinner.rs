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
}

impl Spinner {
    pub fn new(size: SpinnerSize) -> Self {
        Self { size, color: None, base: None }
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
        div().size(px(s)).flex_none().child(
            canvas(
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
            .size_full(),
        )
    }
}
