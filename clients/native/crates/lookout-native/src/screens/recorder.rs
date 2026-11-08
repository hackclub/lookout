//! `DesktopRecorder.tsx` — a pure view over the engine's `RecorderState`.
//! Opens the recording on mount, closes it when the page goes away, and
//! draws whatever the core publishes; every button is one recorder action.

use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyView, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, canvas, div, px,
};
use lookout_core::{ControlMode, RecorderError, RecorderPhase, RecorderState, RecordingSource, SessionConfig};

use crate::app::{self, RecorderEvent, RecorderModel, api_base};
use crate::motion;
use crate::previews;
use crate::screens::modals::{
    CaptureErrorDialog, CaptureErrorEvent, NamingEvent, NamingModal, SourceLostEvent, SourceLostPrompt,
};
use crate::screens::source_picker::Sel;
use crate::theme::{self, is_mac, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::feedback::{ErrorBanner, error_page};
use crate::ui::icon::Icon;
use crate::ui::overlay::{Entrance, ModalWidth};
use crate::ui::paint;
use crate::ui::primitives::{AspectBox, Skeleton};
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

pub enum RecorderViewEvent {
    Back,
    /// Phase `finished` — go to the session view.
    Finished,
    /// The source is gone and the user wants to pick again.
    Reselect,
    OpenEditor(String),
}

pub struct RecorderView {
    pub token: String,
    sources: Vec<Sel>,
    model: Entity<RecorderModel>,
    naming: Option<Entity<NamingModal>>,
    lost: Option<Entity<SourceLostPrompt>>,
    capture_detail: Option<Entity<CaptureErrorDialog>>,
    opened: bool,
    finished_emitted: bool,
    window_focused: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RecorderViewEvent> for RecorderView {}

/// `describeRecorderError` (recorder.ts:123-138).
pub fn describe(err: &RecorderError) -> String {
    match err {
        RecorderError::NoRecording => "No recording is open.".into(),
        RecorderError::InvalidPhase { phase } => {
            format!("That doesn't apply right now ({}).", serde_json::to_value(phase).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default())
        }
        RecorderError::SourceLost => "The capture source is gone — pick one again.".into(),
        RecorderError::Api { message, .. } => message.clone(),
        RecorderError::Other { message } => message.clone(),
    }
}

impl RecorderView {
    pub fn new(token: String, sources: Vec<Sel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = app::state(cx).recorder.clone();
        let mut subs = Vec::new();
        subs.push(cx.observe(&model, |this, _, cx| {
            this.sync_modals(cx);
            cx.notify();
        }));
        subs.push(cx.subscribe(&model, |_, _, ev: &RecorderEvent, cx| {
            if let RecorderEvent::Notice { title, body } = ev {
                crate::platform::notify(title, body, cx);
            }
        }));
        subs.push(cx.observe(&previews::entity(cx), |_, _, cx| cx.notify()));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            this.window_focused = window.is_window_active();
            cx.notify();
        }));
        let mut v = Self {
            token,
            sources,
            model,
            naming: None,
            lost: None,
            capture_detail: None,
            opened: false,
            finished_emitted: false,
            window_focused: window.is_window_active(),
            _subs: subs,
        };
        v.open(cx);
        v
    }

    fn open(&mut self, cx: &mut Context<Self>) {
        if self.opened {
            return;
        }
        self.opened = true;
        let picked = self.sources.iter().map(|s| s.picked()).collect::<Vec<_>>();
        let source = match RecordingSource::from_picked(picked) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[recorder] {e}");
                return;
            }
        };
        let cfg = SessionConfig { token: self.token.clone(), api_base_url: api_base(cx) };
        let core = app::state(cx).core.clone();
        let fut = app::tokio(cx, async move { core.recorder_open(cfg, source).await });
        cx.spawn(async move |_, _| {
            if let Err(e) = fut.await {
                log::error!("[recorder] open failed: {}", describe(&e));
            }
        })
        .detach();
    }

    fn state<'a>(&self, cx: &'a App) -> Option<&'a RecorderState> {
        self.model.read(cx).state.as_ref().filter(|s| s.token == self.token)
    }

    fn action(&mut self, which: &'static str, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let fut = app::tokio(cx, async move {
            match which {
                "pause" => core.recorder_pause().await.map(|_| ()),
                "resume" => core.recorder_resume().await.map(|_| ()),
                "stop" => core.recorder_begin_stop().await.map(|_| ()),
                "cancel_stop" => core.recorder_cancel_stop().await.map(|_| ()),
                _ => Ok(()),
            }
        });
        cx.spawn(async move |_, _| {
            if let Err(e) = fut.await {
                log::warn!("[recorder] {which} failed: {}", describe(&e));
            }
        })
        .detach();
    }

    fn finish(&mut self, name: Option<String>, edit: bool, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let token = self.token.clone();
        let fut = app::tokio(cx, async move { core.recorder_finish_stop(name, edit).await });
        cx.spawn(async move |this, cx| match fut.await {
            Ok(outcome) => {
                if outcome.edit {
                    this.update(cx, |_, cx| cx.emit(RecorderViewEvent::OpenEditor(token))).ok();
                }
            }
            Err(e) => log::warn!("[recorder] finish failed: {}", describe(&e)),
        })
        .detach();
    }

    /// Keep the naming / source-lost modals in step with the state.
    fn sync_modals(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.state(cx).cloned() else { return };
        let prompting = matches!(state.phase, RecorderPhase::StopPrompting | RecorderPhase::StopFinishing);
        if prompting {
            let loading = state.phase == RecorderPhase::StopFinishing;
            match &self.naming {
                Some(n) => n.update(cx, |n, cx| {
                    if n.loading != loading {
                        n.loading = loading;
                        cx.notify();
                    }
                }),
                None => {
                    let too_short = state.too_short_to_compile;
                    let n = cx.new(|cx| NamingModal::new(too_short, cx));
                    self._subs.push(cx.subscribe(&n, |this, _, ev: &NamingEvent, cx| match ev {
                        NamingEvent::Confirm(name) => this.finish(name.clone(), false, cx),
                        NamingEvent::EditAndSave(name) => this.finish(name.clone(), true, cx),
                        NamingEvent::Resume => {
                            // "Keep recording": back to the picker if the source is gone.
                            let lost = this.state(cx).map(|s| s.source_lost.is_some()).unwrap_or(false);
                            if lost {
                                this.action("cancel_stop", cx);
                                cx.emit(RecorderViewEvent::Reselect);
                            } else {
                                this.action("cancel_stop", cx);
                            }
                        }
                    }));
                    self.naming = Some(n);
                }
            }
        } else {
            self.naming = None;
        }
        let stop_loading = matches!(state.phase, RecorderPhase::StopPausing | RecorderPhase::StopFinishing);
        match (&state.source_lost, prompting) {
            (Some(loss), false) => match &self.lost {
                Some(l) => l.update(cx, |l, cx| {
                    l.loss = loss.clone();
                    l.stopping = stop_loading;
                    cx.notify();
                }),
                None => {
                    let l = cx.new(|_| SourceLostPrompt { loss: loss.clone(), stopping: stop_loading });
                    self._subs.push(cx.subscribe(&l, |this, _, ev: &SourceLostEvent, cx| match ev {
                        SourceLostEvent::Reselect => cx.emit(RecorderViewEvent::Reselect),
                        SourceLostEvent::Stop => this.action("stop", cx),
                        SourceLostEvent::Details => {
                            let detail = this.state(cx).and_then(|s| s.source_lost.as_ref().and_then(|l| l.detail.clone())).unwrap_or_default();
                            this.show_capture_detail(detail, cx);
                        }
                    }));
                    self.lost = Some(l);
                }
            },
            _ => self.lost = None,
        }
        if state.phase == RecorderPhase::Finished && !self.finished_emitted {
            self.finished_emitted = true;
            cx.emit(RecorderViewEvent::Finished);
        }
    }

    fn show_capture_detail(&mut self, error: String, cx: &mut Context<Self>) {
        let d = cx.new(|cx| CaptureErrorDialog::new(error, Some("The recording hit a capture error".into()), false, cx));
        self._subs.push(cx.subscribe(&d, |this, _, ev: &CaptureErrorEvent, cx| {
            if let CaptureErrorEvent::Dismiss = ev {
                this.capture_detail = None;
                cx.notify();
            }
        }));
        self.capture_detail = Some(d);
        cx.notify();
    }

    /// The modal this page wants on top (rendered by the main window).
    pub fn modal(&self) -> Option<(AnyView, Entrance, ModalWidth, Option<std::rc::Rc<dyn Fn(&mut Window, &mut App)>>)> {
        if let Some(d) = &self.capture_detail {
            let e = d.clone();
            return Some((
                d.clone().into(),
                Entrance::Overlay,
                ModalWidth::Max(460.0),
                Some(std::rc::Rc::new(move |_: &mut Window, cx: &mut App| e.update(cx, |_, cx| cx.emit(CaptureErrorEvent::Dismiss)))),
            ));
        }
        if let Some(n) = &self.naming {
            return Some((n.clone().into(), Entrance::Naming, ModalWidth::Bare(400.0), None));
        }
        if let Some(l) = &self.lost {
            return Some((l.clone().into(), Entrance::Overlay, ModalWidth::Max(420.0), None));
        }
        None
    }
}

impl Drop for RecorderView {
    fn drop(&mut self) {
        // `recorder_close` on unmount: flush the partial minute, release the
        // camera; the session itself stays open, keeping its time.
        log::info!("[recorder] closing recording for {}…", &self.token[..8.min(self.token.len())]);
    }
}

pub fn close_recording(cx: &mut App) {
    let core = app::state(cx).core.clone();
    let rt = app::state(cx).rt.clone();
    rt.spawn(async move { core.recorder_close().await });
}

impl Render for RecorderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.state(cx).cloned();
        let shell = || div().size_full().max_w(px(480.0)).mx_auto().p(px(16.0)).flex().flex_col();

        let Some(state) = state.filter(|s| s.phase != RecorderPhase::Loading) else {
            return shell()
                .child(div().flex_none().mb(px(16.0)).child(Skeleton::new().h(56.0)))
                .child(div().flex_1().child(Skeleton::new().aspect(16.0 / 9.0).radius(10.0)))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(12.0))
                        .mt(px(16.0))
                        .child(div().flex_1().child(Skeleton::new().h(48.0).radius(10.0)))
                        .child(div().flex_1().child(Skeleton::new().h(48.0).radius(10.0))),
                )
                .into_any_element();
        };

        if state.phase == RecorderPhase::LoadError {
            let e = cx.entity();
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(error_page(
                    "Session Error",
                    state.session_error.clone().unwrap_or_else(|| "Unknown error".into()),
                    Some((
                        if is_mac() { "← Gallery".into() } else { "Back to Gallery".into() },
                        std::rc::Rc::new(move |_: &gpui_kit::gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                            e.update(cx, |_, cx| cx.emit(RecorderViewEvent::Back))
                        }),
                    )),
                    None,
                    &t,
                ))
                .into_any_element();
        }
        if state.phase == RecorderPhase::Finished {
            return div().size_full().flex().items_center().justify_center().child(Spinner::new(SpinnerSize::Md)).into_any_element();
        }

        // Clock: re-derive every frame while running and not held; repaint
        // only when the second changes (the rAF loop in useRecorderClock).
        let now_ms = chrono::Utc::now().timestamp_millis();
        let display = state.clock.display_seconds(now_ms);
        if state.clock.running && state.clock.hold_at_unix_ms.is_none() {
            let next_tick = 1000 - ((now_ms - state.clock.anchor_at_unix_ms).rem_euclid(1000)) as u64;
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(next_tick.max(16))).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }

        let recording = state.control_mode == ControlMode::Recording;
        if recording {
            window.request_animation_frame();
        }
        let dot_opacity = motion::pulse_opacity(1500.0);
        let status = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px(px(20.0))
            .py(px(12.0))
            .bg(t.bg_surface)
            .rounded(px(8.0))
            .border_1()
            .border_color(t.border_default)
            .mb(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .when(recording, |d| {
                        d.child(div().size(px(10.0)).rounded_full().bg(theme::status::danger()).opacity(dot_opacity).flex_none())
                    })
                    .when(!recording, |d| d.child(Icon::LkPauseGlyph.el(14.0, t.text_tertiary)))
                    .child(div().ts(32.0).w700().text_color(t.text_primary).child(crate::fmt::format_time(display))),
            )
            .child(div().ts(13.0).text_color(t.text_secondary).child(format!(
                "{} {}",
                state.capture_count,
                if state.capture_count == 1 { "capture" } else { "captures" }
            )));

        // Preview: core-pushed frames win; otherwise poll each source at 1fps.
        let camera = matches!(state.source, RecordingSource::Camera { .. });
        let live = self.window_focused && !camera;
        let label = if self.window_focused && state.capturing { "Preview" } else { "Latest capture" };
        let core_preview = self.model.read(cx).preview.clone();
        let preview_area = if let Some(img) = core_preview {
            let (w, h) = (img.size(0).width.0 as f32, img.size(0).height.0 as f32);
            let aspect = w / h.max(1.0);
            preview_frame(img, aspect, false, Some(label), &t)
                .into_any_element()
        } else {
            let srcs: Vec<_> = self.sources.iter().filter_map(|s| s.capture()).collect();
            let multi = srcs.len() > 1;
            let mut row = div().flex().gap(px(if multi { 4.0 } else { 0.0 })).size_full();
            for src in srcs {
                let img = previews::get(&src, Duration::from_millis(1000), live, cx);
                row = row.child(match img {
                    Some((img, aspect)) => preview_frame(img, aspect, multi, (!multi).then_some(label), &t).into_any_element(),
                    None => {
                        let mut d = div()
                            .flex_1()
                            .min_w(px(0.0))
                            .rounded(px(10.0))
                            .bg(t.bg_sunken)
                            .border_1()
                            .border_color(t.border_default)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Spinner::new(SpinnerSize::Sm));
                        if !multi {
                            d.style().aspect_ratio = Some(16.0 / 9.0);
                        }
                        d.into_any_element()
                    }
                });
            }
            row.into_any_element()
        };

        let e = cx.entity();
        let stop_loading = matches!(state.phase, RecorderPhase::StopPausing | RecorderPhase::StopFinishing);
        let prompting = matches!(state.phase, RecorderPhase::StopPrompting | RecorderPhase::StopFinishing);
        let (e1, e2) = (e.clone(), e.clone());
        let buttons = if recording {
            div()
                .flex_none()
                .flex()
                .gap(px(12.0))
                .child(
                    Button::new("rec-pause", "Pause")
                        .variant(Variant::Warning)
                        .size(BtnSize::Lg)
                        .flex_1()
                        .loading(state.phase == RecorderPhase::Pausing)
                        .disabled(stop_loading || prompting)
                        .on_click(move |_, _, cx| e1.update(cx, |v, cx| v.action("pause", cx))),
                )
                .child(
                    Button::new("rec-stop", "Stop")
                        .variant(Variant::Danger)
                        .size(BtnSize::Lg)
                        .flex_1()
                        .loading(stop_loading)
                        .disabled(state.phase == RecorderPhase::Pausing || prompting)
                        .on_click(move |_, _, cx| e2.update(cx, |v, cx| v.action("stop", cx))),
                )
        } else {
            div()
                .flex_none()
                .flex()
                .gap(px(12.0))
                .child(
                    Button::new("rec-resume", "Resume")
                        .variant(Variant::Success)
                        .size(BtnSize::Lg)
                        .flex_1()
                        .loading(state.phase == RecorderPhase::Resuming)
                        .disabled(stop_loading || prompting || state.source_lost.is_some())
                        .on_click(move |_, _, cx| e1.update(cx, |v, cx| v.action("resume", cx))),
                )
                .child(
                    Button::new("rec-stop", "Stop")
                        .variant(Variant::Danger)
                        .size(BtnSize::Lg)
                        .flex_1()
                        .loading(stop_loading)
                        .disabled(state.phase == RecorderPhase::Resuming || prompting)
                        .on_click(move |_, _, cx| e2.update(cx, |v, cx| v.action("stop", cx))),
                )
        };

        let capture_error = state.capture_error.clone();
        let stale = state.stale && capture_error.is_none();
        let e3 = e.clone();
        shell()
            .child(status)
            .child(div().flex_1().min_h(px(0.0)).flex().mb(px(16.0)).child(preview_area))
            .when(stale, |d| {
                d.child(
                    div()
                        .px(px(12.0))
                        .py(px(8.0))
                        .bg(theme::hexa(0xf59e0b, 0x1a as f32 / 255.0))
                        .border_1()
                        .border_color(theme::status::warning())
                        .rounded(px(8.0))
                        .mb(px(12.0))
                        .ts(12.0)
                        .text_color(theme::status::warning())
                        .child("Screenshots haven't been captured in a while. Your recording may not be saving."),
                )
            })
            .when_some(capture_error.clone(), |d, err| {
                let report = crate::logbuf::report(&err);
                let err_detail = err.clone();
                d.child(
                    ErrorBanner::new("capture-error", err)
                        .reassurance("Your earlier progress is saved. Restart the app if this keeps happening.")
                        .action("Details", move |_, _, cx| {
                            let err = err_detail.clone();
                            e3.update(cx, |v, cx| v.show_capture_detail(err, cx))
                        })
                        .on_copy(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(report.clone()))),
                )
            })
            .when_some(state.action_error.clone().filter(|_| capture_error.is_none()), |d, err| {
                let report = crate::logbuf::report(&err);
                d.child(
                    ErrorBanner::new("action-error", err)
                        .reassurance("The recording is unchanged — try again.")
                        .on_copy(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(report.clone()))),
                )
            })
            .child(buttons)
            .into_any_element()
    }
}

/// A preview frame: radius 10, sunken fill, 1px border, image contained
/// (covered when tiled), with the "Preview" / "Latest capture" label.
fn preview_frame(
    img: std::sync::Arc<gpui_kit::gpui::RenderImage>,
    aspect: f32,
    cover: bool,
    label: Option<&'static str>,
    t: &crate::theme::Theme,
) -> impl IntoElement {
    div()
        .relative()
        .flex_1()
        .min_w(px(0.0))
        .rounded(px(10.0))
        .overflow_hidden()
        .bg(t.bg_sunken)
        .border_1()
        .border_color(t.border_default)
        .child(
            canvas(|_, _, _| (), move |bounds, _, window, _| {
                paint::paint_fit_image(paint::inset(bounds, 1.0), img.clone(), aspect, cover, 1.0, 9.0, window)
            })
            .absolute()
            .inset_0(),
        )
        .when_some(label, |d, label| {
            d.child(
                div()
                    .absolute()
                    .bottom(px(6.0))
                    .right(px(6.0))
                    .ts(11.0)
                    .text_color(t.badge_overlay_text)
                    .bg(t.badge_overlay_bg)
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .child(label),
            )
        })
}

#[allow(unused)]
fn _keep(_: AspectBox, _: SharedString) {}
