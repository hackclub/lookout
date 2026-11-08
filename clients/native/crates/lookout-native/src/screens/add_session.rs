//! `AddSessionPage.tsx` — start a recording from a program, or paste a
//! `lookout://` link.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, Context, Entity, EventEmitter, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Window, div, img, px,
};

use crate::images::{self, Remote};
use crate::programs::{OpenOutcome, Program};
use crate::screens::layout::page_layout;
use crate::theme::{self, Theme, theme};
use crate::ui::button::{BtnSize, Button};
use crate::ui::input::{InputEvent, TextInput, new_input};
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

pub enum AddEvent {
    Back,
    /// A pasted session link resolved to a token.
    Start(String),
    /// A pasted pairing callback.
    DeepLink(String),
    OpenProgram(Program),
}

pub struct AddSessionPage {
    programs: Option<Vec<Program>>,
    pub launched: Option<String>,
    link: Entity<TextInput>,
    error: Option<String>,
    loading: bool,
    focused: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<AddEvent> for AddSessionPage {}

impl AddSessionPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let link = new_input("Paste a lookout:// link here", cx);
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&link, |this, _, ev: &InputEvent, cx| {
            match ev {
                InputEvent::Changed => this.error = None,
                InputEvent::Submit => {
                    if !this.loading {
                        this.start(cx)
                    }
                }
                InputEvent::Focus => this.focused = true,
                InputEvent::Blur => this.focused = false,
                _ => {}
            }
            cx.notify();
        }));
        subs.push(cx.observe(&images::cache(cx), |_, _, cx| cx.notify()));
        cx.spawn(async move |this, cx| {
            let list = crate::programs::fetch(cx, None).await.unwrap_or_default();
            this.update(cx, |p, cx| {
                p.programs = Some(list);
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self { programs: None, launched: None, link, error: None, loading: false, focused: false, _subs: subs }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let raw = self.link.read(cx).text();
        let trimmed = raw.trim().to_string();
        if crate::deeplink::parse_pair_callback(&trimmed).is_some() {
            cx.emit(AddEvent::DeepLink(trimmed));
            return;
        }
        match crate::deeplink::extract_token(&trimmed) {
            Some(token) => cx.emit(AddEvent::Start(token)),
            None => {
                self.error = Some("Couldn't find a valid session token or pair link in that URL.".into());
                cx.notify();
            }
        }
    }

    /// Called by the main window with the outcome of `openProgram`.
    pub fn program_outcome(&mut self, label: String, outcome: OpenOutcome, cx: &mut Context<Self>) {
        match outcome {
            OpenOutcome::Failed => {
                self.error = Some("Couldn't open your browser. Try the link option below.".into());
                self.launched = None;
            }
            OpenOutcome::Browser => {
                self.launched = Some(label.clone());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(15_000)).await;
                    this.update(cx, |p, cx| {
                        if p.launched.as_deref() == Some(label.as_str()) {
                            p.launched = None;
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
            OpenOutcome::Started => {}
        }
        cx.notify();
    }
}

fn note(t: &Theme) -> gpui_kit::gpui::Div {
    div().ts_lh(12.0, 1.5).text_color(t.text_tertiary).text_center()
}

impl Render for AddSessionPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let me = cx.entity();
        let mut body = div().w_full().mt(px(16.0)).flex().flex_col().gap(px(12.0));
        match self.programs.clone() {
            None => body = body.child(div().flex().justify_center().child(Spinner::new(SpinnerSize::Md))),
            Some(list) if !list.is_empty() => {
                let mut col = div().flex().flex_col().gap(px(8.0));
                for p in list {
                    let label = p.label();
                    let loading_this = self.launched.as_deref() == Some(label.as_str());
                    let disabled = self.loading || (self.launched.is_some() && !loading_this);
                    let icon: Option<AnyElement> = p.icon_url.as_deref().and_then(|u| match images::get(u, cx) {
                        Remote::Ready(i, _) => Some(img(i).size(px(18.0)).rounded(px(5.0)).mr(px(0.0)).into_any_element()),
                        _ => None,
                    });
                    let me2 = me.clone();
                    let program = p.clone();
                    let mut b = Button::empty(theme::eid("program", &p.name))
                        .size(BtnSize::Lg)
                        .full_width()
                        .loading(loading_this)
                        .disabled(disabled)
                        .on_click(move |_, _, cx| {
                            let p = program.clone();
                            me2.update(cx, |page, cx| {
                                page.launched = Some(p.label());
                                cx.emit(AddEvent::OpenProgram(p));
                                cx.notify();
                            })
                        });
                    if let Some(icon) = icon {
                        b = b.child(icon);
                    }
                    b = b.child(label.clone());
                    col = col.child(b);
                }
                body = body.child(col);
                body = body.child(match &self.launched {
                    Some(l) => note(&t)
                        .text_color(t.text_secondary)
                        .child(format!("Opened {l} in your browser — finish there and you'll be sent back here to start recording.")),
                    None => note(&t).child("Not all programs support this. You may need to start the session on the program's website."),
                });
            }
            Some(_) => {
                body = body.child(note(&t).child("No programs available right now. Open Lookout from a Hack Club site, or paste a link below."))
            }
        }
        let link_empty = self.link.read(cx).text().trim().is_empty();
        let me_back = cx.entity();
        let me_start = cx.entity();
        let actions: Vec<AnyElement> = vec![
            self.error.clone().map(|e| note(&t).text_color(theme::status::danger()).child(e).into_any_element()),
            Some(
                crate::ui::input::field_box(&self.link)
                    .w_full()
                    .h(px(48.0))
                    .px(px(16.0))
                    .ts(13.0)
                    .w500()
                    .text_color(t.text_primary)
                    .bg(t.bg_sunken)
                    .border_1()
                    .border_color(if self.error.is_some() { theme::status::danger() } else { t.border_default })
                    .rounded(px(10.0))
                    .when(self.loading, |d| d.opacity(0.5))
                    .child(self.link.clone())
                    .into_any_element(),
            ),
            Some(
                Button::new("start-from-link", "Start from link")
                    .size(BtnSize::Lg)
                    .full_width()
                    .disabled(link_empty || self.loading)
                    .loading(self.loading)
                    .on_click(move |_, _, cx| me_start.update(cx, |p, cx| p.start(cx)))
                    .into_any_element(),
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        let el = page_layout(
            &t,
            Some(Rc::new(move |_: &mut Window, cx: &mut App| me_back.update(cx, |_, cx| cx.emit(AddEvent::Back)))),
            None,
            // The header bar already says "Start a recording".
            (!crate::theme::header_bar_shown()).then(|| "Start a recording".into()),
            Some("Pick a program to start a timelapse".into()),
            None,
            vec![body.into_any_element()],
            actions,
        )
        .into_any_element();
        el
    }
}

#[allow(unused)]
fn _u(_: SharedString) {}
