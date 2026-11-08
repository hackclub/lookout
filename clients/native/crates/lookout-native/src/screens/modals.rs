//! The recording flow's modals: `NamingModal`, `SourceLostPrompt` and
//! `CaptureErrorDialog`.

use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, Focusable, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use lookout_core::capture_diagnostics::CaptureEnvironment;
use lookout_core::{SourceLoss, SourceLossReason};

use crate::diagnosis::{self, Diagnosis};
use crate::theme::{self, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::input::{InputEvent, TextInput, new_input};
use crate::ui::text::{MONO, TextExt};

// ── NamingModal ─────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum NamingEvent {
    Confirm(Option<String>),
    EditAndSave(Option<String>),
    Resume,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Stop,
    Edit,
}

pub struct NamingModal {
    input: Entity<TextInput>,
    pub loading: bool,
    pub too_short: bool,
    choice: Option<Choice>,
    focused_once: bool,
    _sub: Subscription,
}

impl EventEmitter<NamingEvent> for NamingModal {}

fn trimmed(s: String) -> Option<String> {
    let t = s.trim().to_string();
    (!t.is_empty()).then_some(t)
}

impl NamingModal {
    pub fn new(too_short: bool, cx: &mut Context<Self>) -> Self {
        let input = new_input("My timelapse", cx);
        input.update(cx, |i, _| i.max_len = 255);
        let sub = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Submit = ev {
                if !this.loading {
                    this.choice = Some(Choice::Stop);
                    let name = trimmed(input.read(cx).text());
                    cx.emit(NamingEvent::Confirm(name));
                }
            }
        });
        Self { input, loading: false, too_short, choice: None, focused_once: false, _sub: sub }
    }
}

impl Render for NamingModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        // `setTimeout(() => input.focus(), 50)` on open.
        if !self.focused_once && !self.too_short {
            self.focused_once = true;
            let input = self.input.clone();
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                cx.update(|window, cx| {
                    let fh = input.read(cx).focus_handle(cx);
                    window.focus(&fh, cx);
                })
                .ok();
            })
            .detach();
        }
        let loading = self.loading;
        self.input.update(cx, |i, _| i.disabled = loading);
        let choice = self.choice;
        let e = cx.entity();
        let title = if self.too_short { "Nothing to save yet" } else { "Name your timelapse" };
        let body = if self.too_short {
            "A timelapse needs a minute of recording. Stop now and this session ends without one."
        } else {
            "Give it a name, or skip to use the default."
        };
        let mut card = div()
            .w_full()
            .p(px(24.0))
            .bg(t.bg_panel)
            .border_1()
            .border_color(t.border_default)
            .rounded(px(10.0))
            .text_center()
            .shadow(vec![theme::sh(0.0, 10.0, 40.0, 0.0, theme::black(0.5))])
            .child(div().ts(16.0).w700().text_color(t.text_primary).mb(px(8.0)).child(title))
            .child(div().ts(13.0).text_color(t.text_secondary).mb(px(16.0)).child(body));
        if !self.too_short {
            card = card.child(
                crate::ui::input::field_box(&self.input)
                    .w_full()
                    .px(px(16.0))
                    .py(px(12.0))
                    .ts(14.0)
                    .w500()
                    .text_color(t.text_primary)
                    .bg(t.bg_sunken)
                    .border_1()
                    .border_color(t.border_default)
                    .rounded(px(8.0))
                    .mb(px(16.0))
                    .text_left()
                    .when(loading, |d| d.opacity(0.5))
                    .child(self.input.clone()),
            );
        }
        if self.too_short {
            let (e1, e2) = (e.clone(), e.clone());
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        Button::new("naming-keep", "Keep recording")
                            .variant(Variant::Success)
                            .size(BtnSize::Lg)
                            .full_width()
                            .disabled(loading)
                            .on_click(move |_, _, cx| e1.update(cx, |_, cx| cx.emit(NamingEvent::Resume))),
                    )
                    .child(
                        Button::new("naming-stop-anyway", "Stop anyway")
                            .variant(Variant::Ghost)
                            .size(BtnSize::Md)
                            .full_width()
                            .loading(loading && choice == Some(Choice::Stop))
                            .disabled(loading && choice != Some(Choice::Stop))
                            .on_click(move |_, _, cx| {
                                e2.update(cx, |m, cx| {
                                    m.choice = Some(Choice::Stop);
                                    let name = trimmed(m.input.read(cx).text());
                                    cx.emit(NamingEvent::Confirm(name));
                                })
                            }),
                    ),
            );
        } else {
            let (e1, e2, e3) = (e.clone(), e.clone(), e.clone());
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        Button::new("naming-edit", "Edit & Save")
                            .size(BtnSize::Lg)
                            .full_width()
                            .loading(loading && choice == Some(Choice::Edit))
                            .disabled(loading && choice != Some(Choice::Edit))
                            .on_click(move |_, _, cx| {
                                e1.update(cx, |m, cx| {
                                    m.choice = Some(Choice::Edit);
                                    let name = trimmed(m.input.read(cx).text());
                                    cx.emit(NamingEvent::EditAndSave(name));
                                })
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(12.0))
                            .child(
                                Button::new("naming-save", "Save & Stop")
                                    .variant(Variant::Secondary)
                                    .size(BtnSize::Lg)
                                    .flex_1()
                                    .loading(loading && choice == Some(Choice::Stop))
                                    .disabled(loading && choice != Some(Choice::Stop))
                                    .on_click(move |_, _, cx| {
                                        e2.update(cx, |m, cx| {
                                            m.choice = Some(Choice::Stop);
                                            let name = trimmed(m.input.read(cx).text());
                                            cx.emit(NamingEvent::Confirm(name));
                                        })
                                    }),
                            )
                            .child(
                                Button::new("naming-resume", "Resume")
                                    .variant(Variant::Success)
                                    .size(BtnSize::Lg)
                                    .flex_1()
                                    .disabled(loading)
                                    .on_click(move |_, _, cx| e3.update(cx, |_, cx| cx.emit(NamingEvent::Resume))),
                            ),
                    ),
            );
        }
        card
    }
}

// ── SourceLostPrompt ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum SourceLostEvent {
    Reselect,
    Stop,
    Details,
}

pub struct SourceLostPrompt {
    pub loss: SourceLoss,
    pub stopping: bool,
}

impl EventEmitter<SourceLostEvent> for SourceLostPrompt {}

impl Render for SourceLostPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let title = match self.loss.reason {
            SourceLossReason::Revoked => "Screen sharing was stopped",
            SourceLossReason::Failed => "Lost the screen being captured",
        };
        let body = format!(
            "{} Pick a source again to carry on recording — everything captured so far is saved, and the session keeps its time.",
            self.loss.message
        );
        let stopping = self.stopping;
        let has_details = self.loss.detail.is_some();
        let e = cx.entity();
        let (e1, e2, e3) = (e.clone(), e.clone(), e);
        div()
            .flex()
            .flex_col()
            .p(px(24.0))
            .gap(px(16.0))
            .child(
                div()
                    .child(div().ts(18.0).w600().text_color(t.text_primary).child(title))
                    .child(div().mt(px(8.0)).ts_lh(14.0, 1.5).text_color(t.text_secondary).child(body)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(
                        Button::new("lost-reselect", "Select source again")
                            .size(BtnSize::Lg)
                            .full_width()
                            .disabled(stopping)
                            .on_click(move |_, _, cx| e1.update(cx, |_, cx| cx.emit(SourceLostEvent::Reselect))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(
                                Button::new("lost-stop", "Stop recording")
                                    .variant(Variant::Secondary)
                                    .size(BtnSize::Md)
                                    .flex_1()
                                    .loading(stopping)
                                    .on_click(move |_, _, cx| e2.update(cx, |_, cx| cx.emit(SourceLostEvent::Stop))),
                            )
                            .when(has_details, |d| {
                                d.child(
                                    Button::new("lost-details", "Details")
                                        .variant(Variant::Ghost)
                                        .size(BtnSize::Md)
                                        .disabled(stopping)
                                        .on_click(move |_, _, cx| e3.update(cx, |_, cx| cx.emit(SourceLostEvent::Details))),
                                )
                            }),
                    ),
            )
    }
}

// ── CaptureErrorDialog ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum CaptureErrorEvent {
    Retry,
    Dismiss,
}

pub struct CaptureErrorDialog {
    pub error: String,
    pub fallback_title: Option<String>,
    pub can_retry: bool,
    env: Option<CaptureEnvironment>,
    probed: bool,
    copied_command: bool,
    copied_report: bool,
    details_open: bool,
}

impl EventEmitter<CaptureErrorEvent> for CaptureErrorDialog {}

impl CaptureErrorDialog {
    pub fn new(error: String, fallback_title: Option<String>, can_retry: bool, cx: &mut Context<Self>) -> Self {
        let fut = crate::app::tokio(cx, async { lookout_core::capture_diagnostics::capture_environment().await });
        cx.spawn(async move |this, cx| {
            let env = fut.await;
            this.update(cx, |d, cx| {
                d.env = Some(env);
                d.probed = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            error,
            fallback_title,
            can_retry,
            env: None,
            probed: false,
            copied_command: false,
            copied_report: false,
            details_open: false,
        }
    }

    fn diagnosis(&self) -> Option<Diagnosis> {
        diagnosis::diagnose(&self.error, self.env.as_ref())
    }

    pub fn title(&self) -> String {
        self.diagnosis()
            .map(|d| d.title)
            .or_else(|| self.fallback_title.clone())
            .unwrap_or_else(|| "Screen capture failed".into())
    }

    fn flash(&mut self, which: u8, cx: &mut Context<Self>) {
        if which == 0 {
            self.copied_command = true;
        } else {
            self.copied_report = true;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1600)).await;
            this.update(cx, |d, cx| {
                if which == 0 {
                    d.copied_command = false;
                } else {
                    d.copied_report = false;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for CaptureErrorDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let d = self.diagnosis();
        let title = self.title();
        let summary = d.as_ref().map(|d| d.summary.clone()).unwrap_or_else(|| {
            if self.probed {
                "Something in the capture path failed and we couldn't place it. The details below are worth sending along.".into()
            } else {
                "Checking what this system has installed…".into()
            }
        });
        let e = cx.entity();
        let mut col = div()
            .id("capture-error")
            .flex()
            .flex_col()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .p(px(24.0))
            .gap(px(16.0))
            .child(
                div()
                    .child(div().ts(18.0).w600().text_color(t.text_primary).child(title))
                    .child(div().mt(px(8.0)).ts_lh(14.0, 1.5).text_color(t.text_secondary).child(summary)),
            );
        if let Some(d) = &d {
            let mut ol = div().flex().flex_col().gap(px(8.0)).pl(px(4.0));
            for (i, step) in d.steps.iter().enumerate() {
                ol = ol.child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .ts_lh(14.0, 1.5)
                        .text_color(t.text_primary)
                        .child(div().w(px(12.0)).flex_none().text_right().child(format!("{}.", i + 1)))
                        .child(div().flex_1().child(step.clone())),
                );
            }
            col = col.child(ol);
            if let Some(cmd) = d.command.clone() {
                let e1 = e.clone();
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .bg(t.bg_sunken)
                        .border_1()
                        .border_color(t.border_default)
                        .rounded(px(8.0))
                        .pl(px(12.0))
                        .pr(px(8.0))
                        .py(px(8.0))
                        .child(div().flex_1().min_w(px(0.0)).font_family(MONO).ts(13.0).text_color(t.text_primary).overflow_hidden().whitespace_nowrap().child(cmd.clone()))
                        .child(
                            Button::new("copy-cmd", if self.copied_command { "Copied" } else { "Copy" })
                                .variant(Variant::Secondary)
                                .size(BtnSize::Sm)
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(cmd.clone()));
                                    e1.update(cx, |d, cx| d.flash(0, cx));
                                }),
                        ),
                );
            }
        }
        let e_toggle = e.clone();
        col = col.child(
            div()
                .child(
                    div()
                        .id("tech-details")
                        .ts(13.0)
                        .text_color(t.text_tertiary)
                        .cursor_pointer()
                        .child(format!("{} Technical details", if self.details_open { "▾" } else { "▸" }))
                        .on_click(move |_, _, cx| e_toggle.update(cx, |d, cx| {
                            d.details_open = !d.details_open;
                            cx.notify();
                        })),
                )
                .when(self.details_open, |dd| {
                    dd.child(
                        div()
                            .mt(px(8.0))
                            .p(px(12.0))
                            .bg(t.bg_sunken)
                            .rounded(px(8.0))
                            .font_family(MONO)
                            .ts_lh(11.0, 1.5)
                            .text_color(t.text_error)
                            .max_h(px(160.0))
                            .overflow_hidden()
                            .child(self.error.clone()),
                    )
                }),
        );
        let report = diagnosis::build_report(&self.error, self.env.as_ref(), d.as_ref());
        let (e_retry, e_copy, e_close) = (e.clone(), e.clone(), e);
        col.child(
            div()
                .flex()
                .gap(px(8.0))
                .when(self.can_retry, |dd| {
                    dd.child(
                        Button::new("cap-retry", "Try again")
                            .size(BtnSize::Md)
                            .flex_1()
                            .on_click(move |_, _, cx| e_retry.update(cx, |_, cx| cx.emit(CaptureErrorEvent::Retry))),
                    )
                })
                .child(
                    Button::new("cap-report", if self.copied_report { "Copied" } else { "Copy report" })
                        .variant(Variant::Secondary)
                        .size(BtnSize::Md)
                        .flex_1()
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(report.clone()));
                            e_copy.update(cx, |d, cx| d.flash(1, cx));
                        }),
                )
                .child(
                    Button::new("cap-close", "Close")
                        .variant(Variant::Ghost)
                        .size(BtnSize::Md)
                        .on_click(move |_, _, cx| e_close.update(cx, |_, cx| cx.emit(CaptureErrorEvent::Dismiss))),
                ),
        )
    }
}

#[allow(unused)]
fn _u(_: SharedString, _: &mut App) {}
