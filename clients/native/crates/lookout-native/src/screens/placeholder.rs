use gpui_kit::gpui::{Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div};

use crate::ui::a11y::prelude::*;

pub struct Placeholder {
    pub name: SharedString,
}

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().items_center().justify_center().child(div().a11y_text("placeholder", self.name.clone()).child(self.name.clone()))
    }
}
