//! About Lookout — the app icon, name, version and a few links, laid out
//! like a native About panel. Reached from the header's info button and the
//! macOS app menu (Lookout → About Lookout).

use std::sync::Arc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    App, Context, InteractiveElement, IntoElement, ParentElement, Render, RenderImage, SharedString,
    StatefulInteractiveElement, Styled, Window, div, img, px,
};

use crate::theme::{Theme, theme};
use crate::ui::text::TextExt;

const WEBSITE: &str = "https://lookout.hackclub.com";
const DOCS: &str = "https://fallout.hackclub.com/docs";
const GITHUB: &str = "https://github.com/hackclub/lookout/";

thread_local! {
    /// Decoded once: a dropped `RenderImage` would otherwise leak its atlas
    /// tile every time the page is rebuilt.
    static ICON: Option<Arc<RenderImage>> = crate::images::decode_fit(crate::assets::APP_ICON_PNG, 192, 192).ok();
}

pub struct InfoPage;

impl InfoPage {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self
    }
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

/// A plain text link: secondary text, brightens on hover.
fn link(id: &'static str, label: &'static str, url: &'static str, t: &Theme) -> impl IntoElement {
    let hover = t.text_primary;
    div()
        .id(id)
        .ts(12.0)
        .text_color(t.text_secondary)
        .hover(move |s| s.text_color(hover))
        .child(label)
        .on_click(move |_, _, cx| cx.open_url(url))
}

impl Render for InfoPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let icon = ICON.with(|i| i.clone());

        let mut version = format!("Version {}", crate::updater::installed_version());
        if crate::updater::supported() && crate::updater::beta_enabled() {
            version.push_str(" (Beta)");
        }
        let platform = format!("{} · {}", os_name(), std::env::consts::ARCH);
        let server = server_note(cx);

        let dot = || div().ts(12.0).text_color(t.text_tertiary).child("·");

        div().size_full().flex().flex_col().items_center().p(px(24.0)).child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .when_some(icon, |d, i| d.child(img(i).size(px(96.0)).mb(px(16.0))))
                .child(div().ts(22.0).w600().text_color(t.text_primary).child("Lookout"))
                .child(div().mt(px(4.0)).ts(13.0).text_color(t.text_secondary).child(SharedString::from(version)))
                .child(div().mt(px(2.0)).ts(12.0).text_color(t.text_tertiary).child(SharedString::from(platform)))
                .when_some(server, |d, s| d.child(div().mt(px(2.0)).ts(12.0).text_color(t.text_tertiary).child(s)))
                .child(
                    div()
                        .mt(px(20.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(link("about-website", "Website", WEBSITE, &t))
                        .child(dot())
                        .child(link("about-docs", "Docs", DOCS, &t))
                        .child(dot())
                        .child(link("about-github", "GitHub", GITHUB, &t)),
                ),
        )
        .child(div().flex_none().ts(11.0).text_color(t.text_tertiary).child("Made by Hack Club"))
    }
}

/// Only worth mentioning when it isn't the default server.
fn server_note(cx: &App) -> Option<SharedString> {
    let store = crate::app::store(cx);
    if store.is_default_api_base() {
        return None;
    }
    let host = store.api_base();
    Some(format!("Server: {}", host.trim_start_matches("https://").trim_start_matches("http://")).into())
}
