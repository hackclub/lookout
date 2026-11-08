use gpui_kit::gpui::{Context, IntoElement, ParentElement, Render, SharedString, Styled, Window, div};

pub struct Placeholder {
    pub name: SharedString,
}

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().items_center().justify_center().child(self.name.clone())
    }
}
