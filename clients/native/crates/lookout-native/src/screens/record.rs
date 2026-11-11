//! `RecordPage.tsx` — checks the session is still recordable, then shows
//! the source picker or the recorder, sliding between them like the route
//! transition (`AnimatePresence mode="sync"`, direction +1 on select).
//!
//! "Change source…" (the recorder's ⋯ menu, or a lost source) keeps the
//! recording open and slides the picker in over it (+1), so the session
//! keeps its clock and backlog; picking switches the recording's source and
//! slides back (−1), and Back returns to the recorder unchanged.

use std::rc::Rc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, AnyView, App, AppContext, Bounds, Context, Entity, EventEmitter, IntoElement, ParentElement,
    Pixels, Render, SharedString, Styled, Subscription, Window, canvas, div, px,
};
use lookout_core::{ControlMode, RecorderPhase, RecordingSource, SessionConfig};

use crate::app::{self, api_base};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::screens::modals::{CaptureErrorDialog, CaptureErrorEvent, NamingEvent, NamingModal};
use crate::screens::recorder::{RecorderView, RecorderViewEvent};
use crate::screens::source_picker::{PickerEvent, Sel, SourcePicker};
use crate::theme::{self, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::header_bar::{HeaderBtn, HeaderBtnKind};
use crate::ui::icon::Icon;
use crate::ui::overlay::{Entrance, ModalWidth};
use crate::ui::primitives::{Skeleton, back_button, page_icon};
use crate::ui::text::TextExt;

pub enum RecordEvent {
    Back,
    ViewSession(String),
    OpenEditor(String),
    /// The picker is on screen (TipDrawer's "source" moment).
    PickerShown,
    /// The recorder's ⋯ header button: open its menu under `anchor` (the
    /// button's bounds, window coords including any frame margin).
    Menu { anchor: Bounds<Pixels>, can_change_source: bool },
}

enum Check {
    Loading,
    Ok,
    Finished(&'static str),
    Error(String),
}

enum Flow {
    Picker,
    Recorder(Entity<RecorderView>),
}

struct FlowSlot {
    flow: Flow,
    x: Anim,
    opacity: Anim,
    leaving: bool,
}

pub type ModalReq = (AnyView, Entrance, ModalWidth, Option<Rc<dyn Fn(&mut Window, &mut App)>>);

pub struct RecordPage {
    pub token: String,
    check: Check,
    session_status: Option<String>,
    picker: Entity<SourcePicker>,
    slots: Vec<FlowSlot>,
    /// The open recording's view. Kept while the picker is up for a source
    /// change, so the recording (and its clock) carries on underneath.
    recorder: Option<Entity<RecorderView>>,
    /// The picker is changing the open recording's source, not starting it.
    switching: bool,
    stopping: bool,
    /// Where the recorder's ⋯ button is, for its menu.
    menu_anchor: Bounds<Pixels>,
    naming: Option<Entity<NamingModal>>,
    capture_error: Option<Entity<CaptureErrorDialog>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RecordEvent> for RecordPage {}

impl RecordPage {
    pub fn new(token: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|cx| SourcePicker::new("Start Capture".into(), cx));
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&picker, window, |this, _, ev: &PickerEvent, window, cx| match ev {
            PickerEvent::Submit(sel) => this.start_recorder(sel.clone(), window, cx),
            PickerEvent::CaptureError { error, detecting } => {
                let title = if *detecting { "Couldn't detect your displays" } else { "Couldn't start the capture" };
                let (token, base) = (this.token.clone(), api_base(cx));
                let d = cx.new(|cx| CaptureErrorDialog::new(error.clone(), Some(title.into()), true, cx).with_session(&token, &base));
                let picker = this.picker.clone();
                this._subs.push(cx.subscribe(&d, move |this, _, ev: &CaptureErrorEvent, cx| {
                    match ev {
                        CaptureErrorEvent::Retry => picker.update(cx, |p, cx| p.refresh(cx)),
                        CaptureErrorEvent::Dismiss => {}
                    }
                    this.capture_error = None;
                    cx.notify();
                }));
                this.capture_error = Some(d);
                cx.notify();
            }
        }));
        let mut page = Self {
            token,
            check: Check::Loading,
            session_status: None,
            picker,
            slots: vec![FlowSlot { flow: Flow::Picker, x: Anim::new(0.0), opacity: Anim::new(1.0), leaving: false }],
            recorder: None,
            switching: false,
            stopping: false,
            menu_anchor: Bounds::default(),
            naming: None,
            capture_error: None,
            _subs: subs,
        };
        page.check_session(cx);
        // While a source change is up, keep the picker's button honest
        // (a tray pause/resume changes it) and hand the window back to the
        // recorder when it needs the user: a stop from the tray.
        let model = app::state(cx).recorder.clone();
        page._subs.push(cx.observe(&model, |this, model, cx| {
            if !this.switching {
                return;
            }
            let phase = model.read(cx).state.as_ref().map(|s| s.phase);
            if matches!(phase, Some(RecorderPhase::StopPausing | RecorderPhase::StopPrompting | RecorderPhase::StopFinishing)) {
                this.cancel_switch(cx);
            } else {
                this.sync_switch_label(cx);
            }
        }));
        page
    }

    fn check_session(&mut self, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let cfg = SessionConfig { token: self.token.clone(), api_base_url: api_base(cx) };
        let fut = app::tokio(cx, async move { core.session_status(&cfg).await });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |p, cx| {
                match res {
                    Ok(v) => {
                        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        p.check = match status.as_str() {
                            "complete" => Check::Finished("Complete"),
                            "compiling" => Check::Finished("Compiling"),
                            "failed" => Check::Finished("Failed"),
                            "stopped" => Check::Finished("Stopped"),
                            _ => Check::Ok,
                        };
                        p.session_status = Some(status);
                        p.picker.update(cx, |pk, cx| {
                            pk.submit_label = if matches!(p.session_status.as_deref(), Some("active" | "paused")) {
                                "Resume Session".into()
                            } else {
                                "Start Capture".into()
                            };
                            cx.notify();
                        });
                        if matches!(p.check, Check::Ok) {
                            cx.emit(RecordEvent::PickerShown);
                        }
                    }
                    Err(e) => p.check = Check::Error(e.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn transition(&mut self, flow: Flow, dir: f32) {
        for s in self.slots.iter_mut().filter(|s| !s.leaving) {
            s.leaving = true;
            s.x.set(-14.0 * dir, Curve::Spring(Spring::ROUTE));
            s.opacity.set(0.0, Curve::tween(140.0, Ease::EASE_OUT));
        }
        // A slot coming back while it is still animating out (the picker, or
        // the same recorder after a source change) re-enters from its
        // current values (AnimatePresence, same key).
        let same = |s: &FlowSlot| match (&s.flow, &flow) {
            (Flow::Picker, Flow::Picker) => true,
            (Flow::Recorder(a), Flow::Recorder(b)) => a == b,
            _ => false,
        };
        if let Some(i) = self.slots.iter().position(|s| s.leaving && same(s)) {
            let mut s = self.slots.remove(i);
            s.leaving = false;
            s.x.set(0.0, Curve::Spring(Spring::ROUTE));
            s.opacity.set_delayed(1.0, Curve::tween(160.0, Ease::EASE_OUT), 40.0);
            self.slots.push(s);
            return;
        }
        let mut opacity = Anim::new(0.0);
        opacity.set_delayed(1.0, Curve::tween(160.0, Ease::EASE_OUT), 40.0);
        self.slots.push(FlowSlot { flow, x: Anim::from_to(14.0 * dir, 0.0, Curve::Spring(Spring::ROUTE)), opacity, leaving: false });
    }

    fn start_recorder(&mut self, sel: Vec<Sel>, window: &mut Window, cx: &mut Context<Self>) {
        if self.switching {
            if let Some(rec) = self.recorder.clone() {
                self.switch_to(rec, sel, cx);
                return;
            }
        }
        let token = self.token.clone();
        let rec = cx.new(|cx| RecorderView::new(token, sel, window, cx));
        self._subs.push(cx.subscribe(&rec, |this, _, ev: &RecorderViewEvent, cx| match ev {
            RecorderViewEvent::Back => cx.emit(RecordEvent::Back),
            RecorderViewEvent::Finished => {
                let t = this.token.clone();
                cx.emit(RecordEvent::ViewSession(t));
            }
            RecorderViewEvent::Reselect => this.change_source(true, cx),
            RecorderViewEvent::OpenEditor(t) => cx.emit(RecordEvent::OpenEditor(t.clone())),
        }));
        self.recorder = Some(rec.clone());
        self.transition(Flow::Recorder(rec), 1.0);
        cx.notify();
    }

    /// Show the picker over the open recording. The recording is left as it
    /// is (still capturing, if it was) until a new source is picked, so
    /// browsing the picker costs no time and Back has nothing to undo.
    /// `lost`: the old source is dead; on Wayland its casts are too, so
    /// they're released and dropped from the selection.
    fn change_source(&mut self, lost: bool, cx: &mut Context<Self>) {
        if self.switching || self.recorder.is_none() {
            return;
        }
        if lost {
            release_screencast(cx);
            self.picker.update(cx, |p, cx| p.forget_casts(cx));
        }
        self.switching = true;
        self.sync_switch_label(cx);
        self.transition(Flow::Picker, 1.0);
        cx.notify();
    }

    /// Back to the recorder, source unchanged.
    pub fn cancel_switch(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recorder.clone().filter(|_| self.switching) else { return };
        self.switching = false;
        self.transition(Flow::Recorder(rec), -1.0);
        cx.notify();
    }

    /// The source change's pick: switch the recording over and slide back.
    fn switch_to(&mut self, rec: Entity<RecorderView>, sel: Vec<Sel>, cx: &mut Context<Self>) {
        let source = match RecordingSource::from_picked(sel.iter().map(|s| s.picked()).collect()) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[record] {e}");
                return;
            }
        };
        let state = app::state(cx).recorder.read(cx).state.clone();
        // Re-picking what is already recording: nothing to switch.
        if !switch_needed(state.as_ref(), &source) {
            self.cancel_switch(cx);
            return;
        }
        rec.update(cx, |r, cx| r.switch_source(sel, source, cx));
        self.cancel_switch(cx);
    }

    /// The picker's button while changing source: it switches a running
    /// recording over, or resumes a paused one on the pick.
    fn sync_switch_label(&mut self, cx: &mut Context<Self>) {
        let state = app::state(cx).recorder.read(cx).state.clone();
        let label: SharedString = switch_label(state.as_ref()).into();
        self.picker.update(cx, |p, cx| {
            if p.submit_label != label {
                p.submit_label = label;
                cx.notify();
            }
        });
    }

    /// The picker is up to change the open recording's source.
    pub fn switching(&self) -> bool {
        self.switching
    }

    /// "Change source…" from the recorder's menu (re-checked: the phase may
    /// have moved on while the menu was open).
    pub fn menu_change_source(&mut self, cx: &mut Context<Self>) {
        let Some(rec) = self.recorder.clone().filter(|_| self.is_recording_view()) else { return };
        if rec.read(cx).change_source_offered(cx) == Some(true) {
            self.change_source(false, cx);
        }
    }

    fn stop_from_picker(&mut self, name: Option<String>, edit: bool, cx: &mut Context<Self>) {
        self.stopping = true;
        if let Some(n) = &self.naming {
            n.update(cx, |n, cx| {
                n.loading = true;
                cx.notify();
            });
        }
        let core = app::state(cx).core.clone();
        let cfg = SessionConfig { token: self.token.clone(), api_base_url: api_base(cx) };
        let fut = app::tokio(cx, async move { core.session_finish(&cfg, name.as_deref(), edit).await });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |p, cx| {
                p.stopping = false;
                p.naming = None;
                match res {
                    Ok(_) => {
                        release_screencast(cx);
                        if edit {
                            cx.emit(RecordEvent::OpenEditor(p.token.clone()));
                        }
                        cx.emit(RecordEvent::ViewSession(p.token.clone()));
                    }
                    Err(e) => log::error!("[record] stop failed: {}", e.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn modal(&self, cx: &App) -> Option<ModalReq> {
        if let Some(d) = &self.capture_error {
            let e = d.clone();
            return Some((
                d.clone().into(),
                Entrance::Overlay,
                ModalWidth::Max(460.0),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| e.update(cx, |_, cx| cx.emit(CaptureErrorEvent::Dismiss)))),
            ));
        }
        if let Some(n) = &self.naming {
            return Some((n.clone().into(), Entrance::Naming, ModalWidth::Bare(400.0), None));
        }
        for s in self.slots.iter().rev() {
            if let (Flow::Recorder(r), false) = (&s.flow, s.leaving) {
                return r.read(cx).modal();
            }
        }
        None
    }

    pub fn is_recording_view(&self) -> bool {
        self.slots.iter().any(|s| !s.leaving && matches!(s.flow, Flow::Recorder(_)))
    }

    /// Whether Stop Session is offered: on the picker of a session that has
    /// already recorded something.
    fn can_stop(&self) -> bool {
        matches!(self.check, Check::Ok)
            && !self.is_recording_view()
            && !self.switching
            && self.session_status.is_some()
            && self.session_status.as_deref() != Some("pending")
    }

    fn request_stop(&mut self, cx: &mut Context<Self>) {
        let n = cx.new(|cx| NamingModal::new(false, cx));
        self._subs.push(cx.subscribe(&n, |p, _, ev: &NamingEvent, cx| match ev {
            NamingEvent::Confirm(name) => p.stop_from_picker(name.clone(), false, cx),
            NamingEvent::EditAndSave(name) => p.stop_from_picker(name.clone(), true, cx),
            NamingEvent::Resume => {
                p.naming = None;
                cx.notify();
            }
        }));
        self.naming = Some(n);
        cx.notify();
    }

    /// The header bar's title for this page's current step.
    pub fn header_title(&self) -> &'static str {
        if !matches!(self.check, Check::Ok) || self.is_recording_view() {
            "Recording"
        } else if self.switching {
            "Change source"
        } else if self.session_status.as_deref() == Some("pending") {
            "What should Lookout capture?"
        } else {
            "Select source"
        }
    }

    /// Under the title while changing source: the recording hasn't stopped
    /// just because its timer is off screen.
    pub fn header_subtitle(&self, cx: &App) -> Option<&'static str> {
        if !self.switching {
            return None;
        }
        let state = app::state(cx).recorder.read(cx).state.clone();
        Some(switch_subtitle(state.as_ref()))
    }

    /// Stop Session in the header bar, beside the step title, rather than on
    /// a title row of its own under the bar.
    pub fn header_actions(&self, me: Entity<Self>, cx: &App) -> Vec<AnyElement> {
        if self.is_recording_view() {
            // The recorder's secondary actions (Change source…) live in a ⋯
            // menu beside the title, hung from this button like the + menu.
            let Some(rec) = self.recorder.clone() else { return Vec::new() };
            let Some(enabled) = rec.read(cx).change_source_offered(cx) else { return Vec::new() };
            let (open, probe) = (me.clone(), me);
            return vec![
                div()
                    .relative()
                    .child(
                        HeaderBtn::icon("hb-recorder-menu", HeaderBtnKind::Action, Icon::DotsThreeBold, 20.0, move |_, _, cx| {
                            open.update(cx, |p, cx| cx.emit(RecordEvent::Menu { anchor: p.menu_anchor, can_change_source: enabled }))
                        })
                        .tooltip("More"),
                    )
                    .child(
                        canvas(move |b, _, cx| probe.update(cx, |p, _| p.menu_anchor = b), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    )
                    .into_any_element(),
            ];
        }
        if !self.can_stop() {
            return Vec::new();
        }
        vec![
            HeaderBtn::text("hb-stop-session", if self.stopping { "Stopping…" } else { "Stop Session" }, move |_, _, cx| {
                me.update(cx, |p, cx| p.request_stop(cx))
            })
            .destructive()
            .disabled(self.stopping)
            .into_any_element(),
        ]
    }

    fn picker_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx).clone();
        let pending = self.session_status.as_deref() == Some("pending");
        if crate::theme::header_bar_shown() {
            // The bar carries the title (`header_title`) and Stop Session.
            return None;
        }
        // No header bar: the old in-page row with Back, title and Stop.
        let e_back = cx.entity();
        let e_stop = cx.entity();
        let title = if self.switching { "Change source" } else if pending { "What should Lookout capture?" } else { "Select source" };
        let switching = self.switching;
        Some(
            div()
                .max_w(px(480.0))
                .mx_auto()
                .p(px(16.0))
                .pb(px(0.0))
                .flex()
                .justify_between()
                .items_center()
                .flex_none()
                .w_full()
                .relative()
                .h(px(16.0 + 30.0))
                .child(back_button("picker-back", if switching { "Recording" } else { "Gallery" }, &t, move |_, _, cx| {
                    e_back.update(cx, |p, cx| if p.switching { p.cancel_switch(cx) } else { cx.emit(RecordEvent::Back) })
                }))
                .child(
                    div()
                        .absolute()
                        .left(px(if pending { 0.0 } else { 100.0 }))
                        .right(px(if pending { 0.0 } else { 100.0 }))
                        .top(px(16.0))
                        .bottom(px(0.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .ts(14.0)
                        .w700()
                        .text_color(t.text_primary)
                        .child(title),
                )
                .when(self.can_stop(), |d| {
                    d.child(div().ml_auto().child(
                        Button::new("picker-stop", "Stop Session")
                            .variant(Variant::Danger)
                            .size(BtnSize::Md)
                            .loading(self.stopping)
                            .on_click(move |_, _, cx| e_stop.update(cx, |p, cx| p.request_stop(cx))),
                    ))
                })
                .into_any_element(),
        )
    }
}

/// Whether picking `source` during a source change has anything to do:
/// not when it is what is already recording, fine and running.
fn switch_needed(state: Option<&lookout_core::RecorderState>, source: &RecordingSource) -> bool {
    !state.is_some_and(|s| &s.source == source && s.source_lost.is_none() && s.control_mode == ControlMode::Recording)
}

/// The picker's button during a source change: "Switch Source" over a
/// running recording, "Resume Session" over a paused one (picking resumes).
fn switch_label(state: Option<&lookout_core::RecorderState>) -> &'static str {
    if state.is_some_and(|s| s.control_mode == ControlMode::Recording) { "Switch Source" } else { "Resume Session" }
}

/// See [`RecordPage::header_subtitle`].
fn switch_subtitle(state: Option<&lookout_core::RecorderState>) -> &'static str {
    if state.is_some_and(|s| s.control_mode == ControlMode::Recording) { "Still recording until you switch" } else { "Recording paused" }
}

pub fn release_screencast(cx: &mut App) {
    #[cfg(target_os = "linux")]
    {
        let core = app::state(cx).core.clone();
        core.release_screencast();
    }
    let _ = cx;
}

impl Render for RecordPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        match &self.check {
            Check::Loading => {
                return div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .max_w(px(480.0))
                            .mx_auto()
                            .p(px(16.0))
                            .w_full()
                            .flex_none()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(Skeleton::new().w(80.0).h(32.0))
                            .child(Skeleton::new().w(180.0).h(24.0))
                            .child(div().w(px(80.0))),
                    )
                    .child(
                        div()
                            .max_w(px(480.0))
                            .mx_auto()
                            .p(px(16.0))
                            .pt(px(0.0))
                            .flex_1()
                            .w_full()
                            .child(div().mb(px(16.0)).child(Skeleton::new().aspect(2.0).radius(10.0)))
                            .child(div().mb(px(12.0)).child(Skeleton::new().h(36.0)))
                            .child(div().mb(px(4.0)).child(Skeleton::new().h(48.0)))
                            .child(div().mb(px(4.0)).child(Skeleton::new().h(48.0)))
                            .child(div().mb(px(4.0)).child(Skeleton::new().h(48.0)))
                            .child(div().mt(px(16.0)).child(Skeleton::new().h(48.0).radius(10.0))),
                    )
                    .into_any_element();
            }
            Check::Error(msg) => {
                let e = cx.entity();
                return crate::screens::layout::page_layout(
                    &t,
                    (!crate::theme::header_bar_shown()).then(|| -> Rc<dyn Fn(&mut Window, &mut App)> {
                        Rc::new(move |_, cx| e.update(cx, |_, cx| cx.emit(RecordEvent::Back)))
                    }),
                    Some(page_icon(Icon::LkErrorCircle.el(32.0, theme::status::danger())).into_any_element()),
                    Some("Session Error".into()),
                    Some(SharedString::from(if msg.is_empty() { "Unknown error".to_string() } else { msg.clone() })),
                    None,
                    vec![],
                    vec![],
                )
                .into_any_element();
            }
            Check::Finished(label) => {
                let e = cx.entity();
                let e2 = cx.entity();
                let token = self.token.clone();
                return crate::screens::layout::page_layout(
                    &t,
                    (!crate::theme::header_bar_shown()).then(|| -> Rc<dyn Fn(&mut Window, &mut App)> {
                        Rc::new(move |_, cx| e.update(cx, |_, cx| cx.emit(RecordEvent::Back)))
                    }),
                    Some(page_icon(Icon::LkCheckLine.el(32.0, t.text_tertiary)).into_any_element()),
                    Some(format!("Session Already {label}").into()),
                    Some("This session is no longer recordable.".into()),
                    None,
                    vec![],
                    vec![
                        Button::new("view-timelapse", "View Timelapse")
                            .size(BtnSize::Lg)
                            .full_width()
                            .on_click(move |_, _, cx| {
                                let t = token.clone();
                                e2.update(cx, |_, cx| cx.emit(RecordEvent::ViewSession(t)))
                            })
                            .into_any_element(),
                    ],
                )
                .into_any_element();
            }
            Check::Ok => {}
        }

        let mut moving = false;
        for s in &mut self.slots {
            moving |= s.x.tick() | s.opacity.tick();
        }
        self.slots.retain(|s| !(s.leaving && !s.opacity.is_animating()));
        if moving {
            window.request_animation_frame();
        }
        let flows: Vec<(bool, Option<Entity<RecorderView>>, f32, f32)> = self
            .slots
            .iter()
            .map(|s| match &s.flow {
                Flow::Picker => (true, None, s.x.get(), s.opacity.get()),
                Flow::Recorder(r) => (false, Some(r.clone()), s.x.get(), s.opacity.get()),
            })
            .collect();
        let mut layers: Vec<AnyElement> = Vec::new();
        for (is_picker, rec, x, o) in flows {
            let content: AnyElement = if is_picker {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(self.picker_header(cx))
                    .child(div().flex_1().min_h(px(0.0)).child(self.picker.clone()))
                    .into_any_element()
            } else {
                div().size_full().children(rec).into_any_element()
            };
            layers.push(div().absolute().top_0().bottom_0().w_full().left(px(x)).opacity(o).child(content).into_any_element());
        }
        div().relative().size_full().overflow_hidden().children(layers).into_any_element()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use lookout_core::recorder::{Recording, SourceLoss, SourceLossReason};
    use lookout_core::{CaptureSource, RecorderState};

    fn screen(id: u32) -> RecordingSource {
        RecordingSource::Screen { sources: vec![CaptureSource::Monitor { id }] }
    }

    fn state(source: RecordingSource, phase: RecorderPhase, lost: bool) -> RecorderState {
        let cfg = SessionConfig { token: "tok".into(), api_base_url: "http://x".into() };
        let mut rec = Recording::new(cfg, source, 0);
        rec.phase = phase;
        rec.set_status(if phase == RecorderPhase::Recording { "active" } else { "paused" });
        if lost {
            rec.lose_source(SourceLoss { reason: SourceLossReason::Revoked, message: String::new(), detail: None });
        }
        rec.snapshot(0)
    }

    #[test]
    fn re_picking_the_running_source_switches_nothing() {
        let running = state(screen(1), RecorderPhase::Recording, false);
        assert!(!switch_needed(Some(&running), &screen(1)));
        assert!(switch_needed(Some(&running), &screen(2)));
    }

    #[test]
    fn re_picking_a_paused_or_lost_source_still_switches_so_it_resumes() {
        let paused = state(screen(1), RecorderPhase::Paused, false);
        assert!(switch_needed(Some(&paused), &screen(1)));
        let lost = state(screen(1), RecorderPhase::Recording, true);
        assert!(switch_needed(Some(&lost), &screen(1)));
        assert!(switch_needed(None, &screen(1)));
    }

    #[test]
    fn the_picker_button_says_what_picking_will_do() {
        assert_eq!(switch_label(Some(&state(screen(1), RecorderPhase::Recording, false))), "Switch Source");
        assert_eq!(switch_label(Some(&state(screen(1), RecorderPhase::Paused, false))), "Resume Session");
        // A lost source reads as paused: picking resumes.
        assert_eq!(switch_label(Some(&state(screen(1), RecorderPhase::Recording, true))), "Resume Session");
        assert_eq!(switch_label(None), "Resume Session");
    }

    #[test]
    fn the_subtitle_says_whether_the_recording_is_still_running() {
        assert_eq!(switch_subtitle(Some(&state(screen(1), RecorderPhase::Recording, false))), "Still recording until you switch");
        assert_eq!(switch_subtitle(Some(&state(screen(1), RecorderPhase::Paused, false))), "Recording paused");
        assert_eq!(switch_subtitle(Some(&state(screen(1), RecorderPhase::Recording, true))), "Recording paused");
    }
}
