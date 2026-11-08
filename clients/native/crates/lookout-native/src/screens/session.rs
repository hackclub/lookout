//! `SessionDetail.tsx` — one timelapse: the video (or its compile state),
//! the edit-hold panel, the name (double-click to rename), and stats.

use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, AnyView, App, AppContext, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, Window, div, px,
};
use lookout_core::SessionConfig;

use crate::app::{self, api_base};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::theme::{self, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::feedback::{ErrorBanner, ProgressRing};
use crate::ui::header_bar::HeaderBtn;
use crate::ui::icon::Icon;
use crate::ui::input::{InputEvent, TextInput, new_input};
use crate::ui::primitives::{AspectBox, BadgeSize, Skeleton, back_button, badge, card};
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;
use crate::ui::video::VideoPlayer;

pub enum SessionEvent {
    Back,
    Archive,
    Edit,
    /// `onRecordingFinished` (fires once per mount when not held).
    RecordingFinished { redirect_url: Option<String>, panel_url: Option<String>, panel_resolved: bool },
    /// `onComplete` (observed stopped|compiling → complete).
    Complete { redirect_url: Option<String>, panel_url: Option<String> },
}

#[derive(Clone, Default)]
struct Status {
    status: String,
    progress: Option<f32>,
    tracked: f64,
    redirect_url: Option<String>,
    panel_url: Option<String>,
    panel_resolved: bool,
    editable: bool,
    edit_hold_until: Option<String>,
}

fn parse_status(v: &serde_json::Value) -> Status {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    Status {
        status: s("status").unwrap_or_default(),
        progress: v.get("progress").and_then(|p| p.as_f64()).map(|p| p as f32),
        tracked: v.get("trackedSeconds").and_then(|p| p.as_f64()).unwrap_or(0.0),
        redirect_url: s("redirectUrl"),
        panel_url: s("panelUrl"),
        panel_resolved: v.get("panelResolved").and_then(|p| p.as_bool()).unwrap_or(false),
        editable: v.get("editable").and_then(|p| p.as_bool()).unwrap_or(false),
        edit_hold_until: s("editHoldUntil"),
    }
}

pub struct SessionPage {
    pub token: String,
    name: Option<String>,
    created_at: Option<String>,
    status: Option<Status>,
    prev_status: Option<String>,
    video_url: Option<String>,
    video: Option<Entity<VideoPlayer>>,
    error: Option<String>,
    finished_fired: bool,
    complete_fired: bool,
    // Rename
    renaming: bool,
    renaming_anim: bool,
    rename_input: Entity<TextInput>,
    pad_l: Anim,
    pad_r: Anim,
    input_pad: Anim,
    input_fill: Anim,
    pencil: Anim,
    date_y: Anim,
    pencil_press: Anim,
    // Hold
    publishing: bool,
    build_progress: f32,
    hold_started: Option<std::time::Instant>,
    saw_real_progress: bool,
    pub below_video: Option<AnyView>,
    pub title_action: Option<AnyView>,
    scroll: ScrollHandle,
    _poll: Option<Task<()>>,
    _lease: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SessionEvent> for SessionPage {}

impl SessionPage {
    pub fn new(token: String, cx: &mut Context<Self>) -> Self {
        let rename_input = new_input("", cx);
        let mut subs = Vec::new();
        subs.push(cx.subscribe(&rename_input, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::Submit => this.submit_rename(cx),
            InputEvent::Cancel | InputEvent::Blur => this.end_rename(cx),
            _ => {}
        }));
        let mut p = Self {
            token,
            name: None,
            created_at: None,
            status: None,
            prev_status: None,
            video_url: None,
            video: None,
            error: None,
            finished_fired: false,
            complete_fired: false,
            renaming: false,
            renaming_anim: false,
            rename_input,
            pad_l: Anim::new(0.0),
            pad_r: Anim::new(8.0),
            input_pad: Anim::new(0.0),
            input_fill: Anim::new(0.0),
            pencil: Anim::new(1.0),
            date_y: Anim::new(-4.0),
            pencil_press: Anim::new(0.0),
            publishing: false,
            build_progress: 0.0,
            hold_started: None,
            saw_real_progress: false,
            below_video: None,
            title_action: None,
            scroll: ScrollHandle::new(),
            _poll: None,
            _lease: None,
            _subs: subs,
        };
        p.load_session(cx);
        p.fetch_status(cx);
        p
    }

    fn cfg(&self, cx: &App) -> SessionConfig {
        SessionConfig { token: self.token.clone(), api_base_url: api_base(cx) }
    }

    fn load_session(&mut self, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let fut = app::tokio(cx, async move { core.session_get(&cfg).await });
        cx.spawn(async move |this, cx| {
            if let Ok(v) = fut.await {
                this.update(cx, |p, cx| {
                    p.name = v.get("name").and_then(|n| n.as_str()).map(String::from);
                    p.created_at = v.get("createdAt").and_then(|n| n.as_str()).map(String::from);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn fetch_status(&mut self, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let fut = app::tokio(cx, async move { core.session_status(&cfg).await });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |p, cx| {
                match res {
                    Ok(v) => p.on_status(parse_status(&v), cx),
                    Err(e) => p.error = Some(e.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_status(&mut self, s: Status, cx: &mut Context<Self>) {
        let prev = self.prev_status.replace(s.status.clone());
        let finished = matches!(s.status.as_str(), "stopped" | "compiling" | "complete");
        let held = s.edit_hold_until.is_some() && s.status != "failed";
        if finished && !held && !self.finished_fired {
            self.finished_fired = true;
            cx.emit(SessionEvent::RecordingFinished {
                redirect_url: s.redirect_url.clone(),
                panel_url: s.panel_url.clone(),
                panel_resolved: s.panel_resolved,
            });
        }
        if s.status == "complete" && matches!(prev.as_deref(), Some("stopped" | "compiling")) && !self.complete_fired {
            self.complete_fired = true;
            cx.emit(SessionEvent::Complete { redirect_url: s.redirect_url.clone(), panel_url: s.panel_url.clone() });
        }
        if let Some(p) = s.progress {
            self.saw_real_progress = true;
            self.build_progress = self.build_progress.max(p);
        }
        if s.status == "complete" && self.video_url.is_none() {
            let core = app::state(cx).core.clone();
            let cfg = self.cfg(cx);
            let fut = app::tokio(cx, async move { core.session_video(&cfg).await });
            cx.spawn(async move |this, cx| {
                if let Ok(v) = fut.await {
                    if let Some(url) = v.get("videoUrl").and_then(|u| u.as_str()) {
                        let url = url.to_string();
                        this.update(cx, |p, cx| {
                            p.video_url = Some(url.clone());
                            p.video = Some(cx.new(|cx| VideoPlayer::new(url, cx)));
                            cx.notify();
                        })
                        .ok();
                    }
                }
            })
            .detach();
        }
        // Poll every 3s while it compiles.
        if matches!(s.status.as_str(), "stopped" | "compiling") {
            if self._poll.is_none() {
                self._poll = Some(cx.spawn(async move |this, cx| loop {
                    cx.background_executor().timer(Duration::from_millis(3000)).await;
                    let alive = this.update(cx, |p, cx| p.fetch_status(cx)).is_ok();
                    if !alive {
                        break;
                    }
                }));
            }
        } else {
            self._poll = None;
        }
        // Edit lease: heartbeat now and every 30s while the hold panel shows.
        if held && self._lease.is_none() && !self.publishing {
            let core = app::state(cx).core.clone();
            let cfg = self.cfg(cx);
            let rt = app::state(cx).rt.clone();
            self._lease = Some(cx.spawn(async move |_, cx| loop {
                let core = core.clone();
                let cfg = cfg.clone();
                let _ = rt.spawn(async move { core.session_heartbeat_editing(&cfg).await }).await;
                cx.background_executor().timer(Duration::from_secs(30)).await;
            }));
        }
        if !held {
            self._lease = None;
        }
        self.status = Some(s);
    }

    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming {
            return;
        }
        self.renaming = true;
        self.renaming_anim = true;
        let name = self.name.clone().unwrap_or_default();
        self.rename_input.update(cx, |i, cx| {
            i.set_text(name, cx);
            i.select_all_text(cx);
        });
        let fh = gpui_kit::gpui::Focusable::focus_handle(self.rename_input.read(cx), cx);
        window.focus(&fh, cx);
        let sp = Curve::Spring(Spring::new(500.0, 30.0));
        self.pad_l.set(16.0, sp);
        self.pad_r.set(16.0, sp);
        self.input_pad.set(8.0, sp);
        self.input_fill.set(1.0, Curve::tween(150.0, Ease::EASE_OUT));
        self.pencil.set(0.0, Curve::tween(150.0, Ease::EASE_OUT));
        self.date_y.set(0.0, sp);
        cx.notify();
    }

    fn end_rename(&mut self, cx: &mut Context<Self>) {
        if !self.renaming {
            return;
        }
        self.renaming = false;
        let sp = Curve::Spring(Spring::new(500.0, 30.0));
        self.pad_l.set(0.0, sp);
        self.pad_r.set(8.0, sp);
        self.input_pad.set(0.0, sp);
        self.input_fill.set(0.0, Curve::tween(150.0, Ease::EASE_OUT));
        self.date_y.set(-4.0, sp);
        // The pencil returns 600ms later, once the layout has settled.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            this.update(cx, |p, cx| {
                if !p.renaming {
                    p.renaming_anim = false;
                    p.pencil.set(1.0, Curve::tween(150.0, Ease::EASE_OUT));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn submit_rename(&mut self, cx: &mut Context<Self>) {
        let new_name = self.rename_input.read(cx).text().trim().to_string();
        let old = self.name.clone().unwrap_or_default();
        self.end_rename(cx);
        if new_name.is_empty() || new_name == old {
            return;
        }
        self.name = Some(new_name.clone());
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let fut = app::tokio(cx, async move { core.session_rename(&cfg, &new_name).await });
        cx.spawn(async move |this, cx| {
            match fut.await {
                Ok(v) => {
                    let n = v.get("name").and_then(|n| n.as_str()).map(String::from);
                    this.update(cx, |p, cx| {
                        if let Some(n) = n {
                            p.name = Some(n);
                        }
                        cx.notify();
                    })
                    .ok();
                }
                Err(e) => {
                    let msg = if e.status.is_some() { "Failed to rename session." } else { "Error renaming session." };
                    log::warn!("[session] rename failed: {}", e.message);
                    this.update(cx, |p, cx| {
                        p.name = Some(old);
                        p.error = Some(msg.into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        self.publishing = true;
        self._lease = None;
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let fut = app::tokio(cx, async move { core.session_apply_cuts(&cfg).await });
        cx.spawn(async move |this, cx| {
            let _ = fut.await;
            this.update(cx, |p, cx| {
                p.publishing = false;
                p.fetch_status(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn hold_panel(&mut self, s: &Status, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let editable = s.editable;
        if !editable && !self.saw_real_progress {
            let started = *self.hold_started.get_or_insert_with(std::time::Instant::now);
            let est = 1.0 - (-2.2 * started.elapsed().as_secs_f32() / 30.0).exp();
            self.build_progress = self.build_progress.max(est);
            // `setInterval(tick, 250)`
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        let (e1, e2) = (cx.entity(), cx.entity());
        card(&t)
            .p(px(16.0))
            .mb(px(16.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.0))
                    .when(!editable, |d| d.child(ProgressRing::new("hold-ring", self.build_progress).size(48.0, 4.0)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .ts(14.0)
                                    .w700()
                                    .text_color(t.text_primary)
                                    .child(if editable { "Ready to review" } else { "Preparing your timelapse…" }),
                            )
                            .child({
                                // One paragraph with an inline tertiary span.
                                let lead = if editable {
                                    "Cut out anything you don't want to share, then save. Nothing is published until you do. "
                                } else {
                                    "Your recording is compiling. You'll be able to trim it in a moment. "
                                };
                                let tail = "If you close Lookout, it publishes as recorded.";
                                let text = format!("{lead}{tail}");
                                let range = lead.len()..text.len();
                                div().ts(13.0).text_color(t.text_secondary).mt(px(4.0)).child(
                                    gpui_kit::gpui::StyledText::new(text).with_highlights(vec![(
                                        range,
                                        gpui_kit::gpui::HighlightStyle { color: Some(t.text_tertiary), ..Default::default() },
                                    )]),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .mt(px(16.0))
                    .child(
                        Button::new("hold-edit", "Edit & save")
                            .size(BtnSize::Md)
                            .disabled(!editable || self.publishing)
                            .on_click(move |_, _, cx| e1.update(cx, |_, cx| cx.emit(SessionEvent::Edit))),
                    )
                    .child(
                        Button::new("hold-publish", "Publish as recorded")
                            .variant(Variant::Secondary)
                            .size(BtnSize::Md)
                            .loading(self.publishing)
                            .on_click(move |_, _, cx| e2.update(cx, |p, cx| p.publish(cx))),
                    ),
            )
            .into_any_element()
    }

    fn processing(&self, s: &Status, t: &crate::theme::Theme) -> AnyElement {
        if s.status == "complete" {
            if let Some(v) = &self.video {
                return div()
                    .bg(t.bg_sunken)
                    .rounded(px(10.0))
                    .overflow_hidden()
                    .child(AspectBox::new(16.0 / 9.0).child(div().absolute().inset_0().child(v.clone())))
                    .into_any_element();
            }
        }
        if s.status == "failed" {
            return div()
                .bg(t.bg_sunken)
                .rounded(px(10.0))
                .child(
                    AspectBox::new(16.0 / 9.0).child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(12.0))
                            .child(div().ts(14.0).w700().text_color(theme::status::danger()).child("Compilation failed"))
                            .child(div().ts(13.0).text_color(t.text_secondary).child("It will be retried automatically.")),
                    ),
                )
                .into_any_element();
        }
        // Same 16:9 box as the video, so the page doesn't jump when it loads.
        div()
            .bg(t.bg_sunken)
            .rounded(px(10.0))
            .child(
                AspectBox::new(16.0 / 9.0).child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(12.0))
                        .child(Spinner::new(SpinnerSize::Lg))
                        .child(
                            div()
                                .ts(14.0)
                                .w600()
                                .text_color(t.text_primary)
                                .child(if s.status == "complete" { "Loading video..." } else { "Compiling your timelapse..." }),
                        )
                        .child(
                            div()
                                .ts(13.0)
                                .text_color(t.text_secondary)
                                .child(format!("Tracked time: {}", crate::fmt::format_tracked(s.tracked))),
                        ),
                ),
            )
            .into_any_element()
    }
}

impl SessionPage {
    /// Archive in the header bar, as a word: an archive-✕ glyph next to the
    /// window's close reads as a second close.
    pub fn header_actions(&self, me: Entity<Self>) -> Vec<AnyElement> {
        vec![
            HeaderBtn::text("hb-archive", "Archive", move |_, _, cx| me.update(cx, |_, cx| cx.emit(SessionEvent::Archive)))
                .into_any_element(),
        ]
    }
}

impl Render for SessionPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut moving = false;
        for a in [
            &mut self.pad_l,
            &mut self.pad_r,
            &mut self.input_pad,
            &mut self.input_fill,
            &mut self.pencil,
            &mut self.date_y,
            &mut self.pencil_press,
        ] {
            moving |= a.tick();
        }
        if moving {
            window.request_animation_frame();
        }

        // With the header bar (every platform today), Back and Archive live in
        // it; this row is only the fallback for a bar-less window.
        let header = (!crate::theme::header_bar_shown()).then(|| {
            let (e_back, e_archive) = (cx.entity(), cx.entity());
            div()
                .flex()
                .justify_between()
                .items_center()
                .mb(px(16.0))
                .child(back_button("session-back", "Back", &t, move |_, _, cx| {
                    e_back.update(cx, |_, cx| cx.emit(SessionEvent::Back))
                }))
                .child(
                    Button::new("session-archive", "Archive")
                        .variant(Variant::Secondary)
                        .size(BtnSize::Sm)
                        .card_style(&t)
                        .on_click(move |_, _, cx| e_archive.update(cx, |_, cx| cx.emit(SessionEvent::Archive))),
                )
        });

        let mut col = div().p(px(16.0)).children(header);
        if let Some(err) = &self.error {
            col = col.child(ErrorBanner::new("session-error", err.clone()).title("Error"));
        }
        let Some(status) = self.status.clone() else {
            if self.error.is_none() {
                col = col
                    .child(div().mb(px(16.0)).child(Skeleton::new().aspect(16.0 / 9.0).radius(10.0)))
                    .child(div().mb(px(4.0)).child(Skeleton::new().w_frac(0.5).h(18.0)))
                    .child(div().mb(px(16.0)).child(Skeleton::new().w_frac(0.3).h(12.0)))
                    .child(
                        div()
                            .flex()
                            .gap(px(16.0))
                            .items_center()
                            .child(div().flex_1().child(Skeleton::new().h(64.0)))
                            .child(div().flex_1().child(Skeleton::new().h(64.0))),
                    );
            }
            return div().id("session-scroll").size_full().overflow_y_scroll().child(col).into_any_element();
        };
        let in_hold = status.edit_hold_until.is_some() && status.status != "failed";
        if in_hold {
            col = col.child(self.hold_panel(&status, window, cx));
        } else {
            col = col.child(div().mb(px(16.0)).rounded(px(10.0)).overflow_hidden().child(self.processing(&status, &t)));
        }
        col = col.children(self.below_video.clone());

        if let Some(name) = self.name.clone() {
            let e_dbl = cx.entity();
            let e_pencil = cx.entity();
            let pencil_o = self.pencil.get();
            let fill = self.input_fill.get();
            let ip = self.input_pad.get();
            let name_el: AnyElement = if self.renaming {
                div()
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .px(px(ip))
                    .min_w(px(300.0))
                    .flex_1()
                    .bg(theme::mix(theme::transparent(), t.bg_surface, fill))
                    .border_1()
                    .border_color(theme::mix(theme::transparent(), t.border_selected, fill))
                    .rounded(px(8.0))
                    .ts(16.0)
                    .w700()
                    .text_color(t.text_primary)
                    .child(self.rename_input.clone())
                    .into_any_element()
            } else {
                div()
                    .id("session-name")
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .px(px(ip))
                    .border_1()
                    .border_color(theme::mix(theme::transparent(), t.border_selected, fill))
                    .bg(theme::mix(theme::transparent(), t.bg_surface, fill))
                    .rounded(px(8.0))
                    .ts(16.0)
                    .w700()
                    .text_color(t.text_primary)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(SharedString::from(name))
                    .on_click(move |ev, window, cx| {
                        if ev.click_count() >= 2 {
                            e_dbl.update(cx, |p, cx| p.start_rename(window, cx));
                        }
                    })
                    .into_any_element()
            };
            let pencil = (pencil_o > 0.001 && !self.renaming).then(|| {
                let scale = 0.8 + 0.2 * pencil_o;
                let press = 1.0 - 0.1 * self.pencil_press.get();
                div()
                    .id("rename-pencil")
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(pencil_o)
                    .tooltip(|w, cx| crate::ui::tooltip::Tooltip::new("Rename session").build(w, cx))
                    .child(Icon::LkPencil.scaled(12.0, t.text_tertiary, scale * press))
                    .on_mouse_down(MouseButton::Left, cx.listener(|p, _, _, cx| {
                        p.pencil_press.set_css(1.0, 100.0, Ease::EASE_IN_OUT);
                        cx.notify();
                    }))
                    .on_mouse_up(MouseButton::Left, cx.listener(|p, _, _, cx| {
                        p.pencil_press.set_css(0.0, 100.0, Ease::EASE_IN_OUT);
                        cx.notify();
                    }))
                    .on_click(move |_, window, cx| e_pencil.update(cx, |p, cx| p.start_rename(window, cx)))
            });
            col = col.child(
                div()
                    .mb(px(16.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .h(px(32.0))
                            .child(div().pl(px(self.pad_l.get() - if self.renaming { 16.0 } else { 0.0 })).flex().min_w(px(0.0)).child(name_el))
                            .child(div().size(px(24.0)).flex_none().flex().children(pencil))
                            .children(self.title_action.clone().map(|a| div().ml_auto().flex().items_center().child(a))),
                    )
                    .child(
                        div()
                            .ts(11.0)
                            .text_color(t.text_tertiary)
                            .mt(px(2.0 + self.date_y.get()))
                            .child(self.created_at.as_deref().map(|c| crate::fmt::short_date(c, true)).unwrap_or_default()),
                    ),
            );
        }

        let stat = |value: AnyElement, label: &'static str| {
            card(&t)
                .flex_1()
                .px(px(24.0))
                .py(px(12.0))
                .items_center()
                .gap(px(4.0))
                .child(div().h(px(32.0)).flex().items_center().child(value))
                .child(div().ts(11.0).text_color(t.text_tertiary).child(label))
        };
        col = col.child(
            div()
                .flex()
                .gap(px(16.0))
                .justify_center()
                .child(stat(
                    div().ts(18.0).w700().text_color(t.text_primary).child(crate::fmt::format_tracked(status.tracked)).into_any_element(),
                    "Tracked time",
                ))
                .child(stat(badge(&status.status, BadgeSize::Lg, false, &t).into_any_element(), "Status")),
        );
        div().id("session-scroll").size_full().overflow_y_scroll().track_scroll(&self.scroll).child(col).into_any_element()
    }
}
