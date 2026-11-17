//! The recording flow's modals: `NamingModal`, `SourceLostPrompt` and
//! `CaptureErrorDialog`.

use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use lookout_core::capture_diagnostics::CaptureEnvironment;
use lookout_core::{SourceLoss, SourceLossReason};

use crate::diagnosis::{self, Diagnosis};
use crate::theme::{is_windows, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::a11y::prelude::*;
use crate::screens::kbd::KbdPress;
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

/// The prompt that stops a recording. It is laid out like a desktop alert
/// rather than a web card: title and text left-aligned, a regular-height
/// field, one row of regular buttons in the platform's order (default
/// button trailing on macOS and Linux, leading on Windows), Return for the
/// default button and Escape to keep recording.
pub struct NamingModal {
    input: Entity<TextInput>,
    pub loading: bool,
    pub too_short: bool,
    choice: Option<Choice>,
    focused_once: bool,
    /// Resume was sent; Escape can arrive from both the field and the
    /// dialog, and must only resume once.
    resumed: bool,
    focus: FocusHandle,
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
        input.update(cx, |i, _| {
            i.max_len = 255;
            i.a11y_label = Some("Timelapse name".into());
        });
        let sub = cx.subscribe(&input, |this, _, ev: &InputEvent, cx| match ev {
            // Return takes the default button, "Edit & Save".
            InputEvent::Submit => this.choose(Choice::Edit, cx),
            InputEvent::Cancel => this.resume(cx),
            _ => {}
        });
        Self { input, loading: false, too_short, choice: None, focused_once: false, resumed: false, focus: cx.focus_handle(), _sub: sub }
    }

    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        if self.loading || self.resumed {
            return;
        }
        self.choice = Some(choice);
        let name = if self.too_short { None } else { trimmed(self.input.read(cx).text()) };
        cx.emit(match choice {
            Choice::Edit => NamingEvent::EditAndSave(name),
            Choice::Stop => NamingEvent::Confirm(name),
        });
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        if self.loading || self.resumed {
            return;
        }
        self.resumed = true;
        cx.emit(NamingEvent::Resume);
    }
}

impl Render for NamingModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        if !self.focused_once {
            self.focused_once = true;
            if self.too_short {
                // No field: the dialog takes focus so Return and Escape work.
                window.focus(&self.focus, cx);
            } else {
                // `setTimeout(() => input.focus(), 50)` on open.
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
        }
        let loading = self.loading;
        self.input.update(cx, |i, _| i.disabled = loading);
        let choice = self.choice;
        let too_short = self.too_short;
        let e = cx.entity();
        let title = if too_short { "Nothing to save yet" } else { "Name your timelapse" };
        let body = if too_short {
            "A timelapse needs a minute of recording. Stop now and this session ends without one."
        } else {
            "Leave it blank to use the default name."
        };
        crate::ui::overlay::set_dialog_title(cx.entity_id(), title, too_short);

        let button = |id: &'static str, label: &'static str, variant: Variant, which: Option<Choice>| {
            let e = e.clone();
            Button::new(id, label)
                .variant(variant)
                .size(BtnSize::Md)
                .loading(loading && which.is_some() && choice == which)
                .disabled(loading && (which.is_none() || choice != which))
                .on_click(move |_, _, cx| {
                    e.update(cx, |m, cx| match which {
                        Some(c) => m.choose(c, cx),
                        None => m.resume(cx),
                    })
                })
        };
        // (default, cancel, alternative)
        let (default, cancel, alt) = if too_short {
            (button("naming-keep", "Keep recording", Variant::Primary, None), None, Some(button("naming-stop-anyway", "Stop anyway", Variant::Secondary, Some(Choice::Stop))))
        } else {
            (
                button("naming-edit", "Edit & Save", Variant::Primary, Some(Choice::Edit)),
                Some(button("naming-resume", "Resume", Variant::Secondary, None)),
                Some(button("naming-save", "Save & Stop", Variant::Secondary, Some(Choice::Stop))),
            )
        };
        let row = div().flex().items_center().gap(px(8.0));
        let row = if is_windows() {
            // Windows: right-aligned, default first, cancel last.
            row.justify_end().child(default).children(alt).children(cancel)
        } else {
            // macOS / GNOME: default trailing, cancel beside it, the
            // alternative action on its own at the leading edge (like
            // "Don't Save").
            row.children(alt).child(div().flex_1()).children(cancel).child(default)
        };

        let e_key = e.clone();
        div()
            .id("naming-dialog")
            .track_focus(&self.focus)
            .on_key_down(move |ev: &gpui_kit::gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.modifiers.modified() {
                    return;
                }
                match ev.keystroke.key.as_str() {
                    "escape" => {
                        cx.stop_propagation();
                        e_key.update(cx, |m, cx| m.resume(cx));
                    }
                    // Return on the dialog itself (not on a focused button).
                    "enter" if too_short => {
                        cx.stop_propagation();
                        e_key.update(cx, |m, cx| m.resume(cx));
                    }
                    _ => {}
                }
            })
            .flex()
            .flex_col()
            .p(px(20.0))
            .gap(px(16.0))
            .child(
                div()
                    .child(div().a11y_heading("naming-title", title, 2).ts(15.0).w600().text_color(t.text_primary).child(title))
                    .child(div().a11y_text("naming-body", body).mt(px(4.0)).ts_lh(13.0, 1.45).text_color(t.text_secondary).child(body)),
            )
            .when(!too_short, |d| {
                d.child(
                    crate::ui::input::field_box(&self.input)
                        .w_full()
                        .px(px(10.0))
                        .py(px(6.0))
                        .ts(13.0)
                        .text_color(t.text_primary)
                        .bg(t.bg_sunken)
                        .border_1()
                        .border_color(t.border_default)
                        .rounded(px(6.0))
                        .when(loading, |d| d.opacity(0.5))
                        .relative()
                        .child(crate::ui::input::field_ring(&self.input, 6.0, cx))
                        .child(self.input.clone()),
                )
            })
            .child(row)
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
        crate::ui::overlay::set_dialog_title(cx.entity_id(), title, true);
        div()
            .flex()
            .flex_col()
            .p(px(24.0))
            .gap(px(16.0))
            .child(
                div()
                    .child(div().a11y_heading("lost-title", title, 2).ts(18.0).w600().text_color(t.text_primary).child(title))
                    .child(div().a11y_text("lost-body", body.clone()).mt(px(8.0)).ts_lh(14.0, 1.5).text_color(t.text_secondary).child(body)),
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
    /// This session in Lookout's web recorder, offered when capture can't
    /// work on this machine at all (ChromeOS's Linux environment).
    web_recorder_url: Option<String>,
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
            #[allow(unused_mut)]
            let mut env = fut.await;
            // Debug builds can pretend to be ChromeOS's Linux environment,
            // to see this dialog without a Chromebook.
            #[cfg(debug_assertions)]
            if std::env::var("LOOKOUT_DEV_CROSTINI").as_deref() == Ok("1") {
                env.chromeos = Some(true);
            }
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
            web_recorder_url: None,
            env: None,
            probed: false,
            copied_command: false,
            copied_report: false,
            details_open: false,
        }
    }

    /// The session this error belongs to, so the dialog can offer to record
    /// it in the browser when the desktop can't.
    pub fn with_session(mut self, token: &str, api_base: &str) -> Self {
        self.web_recorder_url = Some(diagnosis::web_recorder_url(api_base, token));
        self
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        crate::ui::overlay::set_dialog_title(cx.entity_id(), title.clone(), true);
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
                    .child(div().a11y_heading("cap-title", title.clone(), 2).ts(18.0).w600().text_color(t.text_primary).child(title))
                    .child(div().a11y_status("cap-summary", summary.clone(), Live::Polite).mt(px(8.0)).ts_lh(14.0, 1.5).text_color(t.text_secondary).child(summary)),
            );
        if let Some(d) = &d {
            let mut ol = div().id("cap-steps").role(Role::List).aria_label("Steps").flex().flex_col().gap(px(8.0)).pl(px(4.0));
            for (i, step) in d.steps.iter().enumerate() {
                ol = ol.child(
                    div()
                        .id(("cap-step", i))
                        .role(Role::ListItem)
                        .aria_label(format!("{}. {}", i + 1, step))
                        .flex()
                        .gap(px(8.0))
                        .ts_lh(14.0, 1.5)
                        .text_color(t.text_primary)
                        .child(div().w(px(12.0)).flex_none().text_right().child(format!("{}.", i + 1)))
                        .child(div().flex_1().min_w(px(0.0)).child(step.clone())),
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
                        .child(div().a11y_text("cap-command", cmd.clone()).flex_1().min_w(px(0.0)).font_family(MONO).ts(13.0).text_color(t.text_primary).overflow_hidden().whitespace_nowrap().child(cmd.clone()))
                        .child(
                            Button::new("copy-cmd", if self.copied_command { "Copied" } else { "Copy" })
                                .aria_label(if self.copied_command { "Copied" } else { "Copy command" })
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
        let e_key = e.clone();
        let details_focus = crate::screens::kbd::keyed_focus("tech-details", window, cx);
        col = col.child(
            div()
                .child(
                    div()
                        .id("tech-details")
                        .role(Role::Button)
                        .aria_label("Technical details")
                        .aria_expanded(self.details_open)
                        .relative()
                        .kbd_press(&details_focus, 4.0, move |_, cx| e_key.update(cx, |d, cx| {
                            d.details_open = !d.details_open;
                            cx.notify();
                        }))
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
                            .a11y_text("tech-details-text", self.error.clone())
                            .child(self.error.clone()),
                    )
                }),
        );
        let report = diagnosis::build_report(&self.error, self.env.as_ref(), d.as_ref());
        // Retrying can't help where capture is impossible; the browser can.
        let browser = self.web_recorder_url.clone().filter(|_| d.as_ref().is_some_and(|d| d.record_in_browser));
        let retry = self.can_retry && browser.is_none();
        let (e_retry, e_copy, e_close) = (e.clone(), e.clone(), e);
        col.child(
            div()
                .flex()
                .gap(px(8.0))
                .when_some(browser, |dd, url| {
                    dd.child(
                        Button::new("cap-browser", "Record in Chrome")
                            .size(BtnSize::Md)
                            .flex_1()
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                })
                .when(retry, |dd| {
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
