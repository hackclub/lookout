//! Tooltips — the Tauri app relied on the browser's native `title`
//! tooltips. GPUI has none, so this is a small, quiet pill in the system's
//! idiom: shown after the platform hover delay, no animation (tooltips are
//! high-frequency; design-guidance §2).

use gpui_kit::gpui::{AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px};

use crate::theme::theme;
use crate::ui::text::TextExt;

pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }

    pub fn build(self, _window: &mut Window, cx: &mut App) -> AnyView {
        cx.new(|_| self).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div().pl(px(8.0)).pt(px(10.0)).child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(t.bg_panel)
                .border_1()
                .border_color(t.border_default)
                .shadow_md()
                .text_color(t.text_primary)
                .ts(12.0)
                .child(self.text.clone()),
        )
    }
}
