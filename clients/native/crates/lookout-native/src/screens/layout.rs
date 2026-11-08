//! `PageLayout` — the desktop's centred page scaffold (permission gates,
//! session errors, "Start a recording").

use std::rc::Rc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window, div, px};

use crate::theme::{Theme, is_linux};
use crate::ui::primitives::back_button;
use crate::ui::text::TextExt;

#[allow(clippy::too_many_arguments)]
pub fn page_layout(
    t: &Theme,
    on_back: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    icon: Option<AnyElement>,
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    hint: Option<SharedString>,
    children: Vec<AnyElement>,
    actions: Vec<AnyElement>,
) -> impl IntoElement {
    let has_hint = hint.is_some();
    div()
        .flex()
        .flex_col()
        .size_full()
        .max_w(px(480.0))
        .mx_auto()
        .p(px(16.0))
        .when_some(on_back.filter(|_| !is_linux()), |d, f| {
            d.child(div().flex_none().child(back_button("page-back", "Back", t, move |_, w, cx| f(w, cx))))
        })
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .text_center()
                .px(px(24.0))
                .children(icon)
                .when_some(title, |d, title| d.child(div().ts(20.0).w700().text_color(t.text_primary).mb(px(8.0)).child(title)))
                .when_some(subtitle, |d, s| {
                    d.child(
                        div()
                            .ts_lh(13.0, 1.6)
                            .text_color(t.text_secondary)
                            .when(has_hint, |d| d.mb(px(16.0)))
                            .child(s),
                    )
                })
                .when_some(hint, |d, h| d.child(div().ts_lh(12.0, 1.5).text_color(t.text_tertiary).child(h)))
                .children(children),
        )
        .when(!actions.is_empty(), |d| d.child(div().flex_none().flex().flex_col().gap(px(12.0)).children(actions)))
}
