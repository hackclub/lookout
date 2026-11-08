//! `TimelapseEditor.tsx` — trim a timelapse before it's published.
//!
//! The stage plays the uncut original; the dock has the transport, a ruler,
//! a filmstrip track where dragging marks a stretch to remove, the
//! kept/removed totals and Save. Cuts are integer unit ranges (one unit =
//! one video second = one real minute), merged when they touch, and saved
//! as capture-time intervals.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, AppContext, Bounds, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement,
    PathBuilder, Pixels, Render, RenderImage, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, canvas, div, point, px,
};
use lookout_core::SessionConfig;

use crate::app::{self, api_base};
use crate::editor_math::{self as em, CutInterval, Region, VideoUnit};
use crate::motion::{Anim, Curve, Ease};
use crate::theme::{self, Theme, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::feedback::{ErrorBanner, ProgressRing, minutes_flow};
use crate::ui::icon::Icon;
use crate::ui::paint;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;
use crate::ui::video::{VideoEvent, VideoPlayer};

const STRIP_HEIGHT: f32 = 56.0;
const RULER_HEIGHT: f32 = 22.0;
const HEAD_W: f32 = 9.0;
const HEAD_H: f32 = 13.0;
const HEAD_HIT: f32 = 22.0;
const QUART: Ease = Ease::EASE_OUT_QUART;

/// `editorStyles.ts` reduced-motion block: every editor transition collapses
/// to 1ms under `prefers-reduced-motion: reduce`.
fn ed_ms(ms: f32) -> f32 {
    if crate::motion::reduce_motion() { 1.0 } else { ms }
}

/// …and the grip / playhead hover transforms are removed (`transform: none`;
/// the reduced rule also outranks the playhead's `:active` scale).
fn ed_hover(scale: f32) -> f32 {
    if crate::motion::reduce_motion() { 1.0 } else { scale }
}

#[derive(Clone, Debug)]
pub struct ApplyResult {
    pub status: Option<String>,
    pub redirect_url: Option<String>,
    pub panel_url: Option<String>,
    pub panel_resolved: bool,
}

pub enum EditorEvent {
    Applied(ApplyResult),
    CutsChanged { removed: usize, dirty: bool },
    SessionName(String),
}

enum Phase {
    Loading,
    Preparing { units: usize },
    Failed(String),
    Ready,
}

#[derive(Clone, Copy)]
enum DragMode {
    Move,
    Start,
    End,
}

enum Drag {
    Scrub,
    Maybe { down: f64 },
    Region { index: usize, mode: DragMode, grab: f64, anchor: i64 },
}

pub struct TimelapseEditor {
    token: String,
    phase: Phase,
    units: Vec<VideoUnit>,
    saved_cuts: Vec<CutInterval>,
    regions: Vec<Region>,
    selected: Option<usize>,
    video: Option<Entity<VideoPlayer>>,
    time: f64,
    playing: bool,
    drag: Option<Drag>,
    strip: Bounds<Pixels>,
    tiles: Vec<Option<Arc<RenderImage>>>,
    tile_w: f32,
    film_key: Option<(i32, usize)>,
    film_task: Option<Task<()>>,
    saving: bool,
    save_error: Option<String>,
    progress: f32,
    prep_started: Instant,
    real_anchor: Option<(f32, Instant)>,
    focus: FocusHandle,
    play_overlay: Anim,
    cut_ring: Anim,
    hover_region: Option<usize>,
    region_hover: Anim,
    hover_handle: Option<(usize, bool)>,
    grip: Anim,
    head_hover: bool,
    head_scale: Anim,
    btn_hover: Anim,
    btn_press: Anim,
    lease_lost: bool,
    _poll: Option<Task<()>>,
    _lease: Option<Task<()>>,
    _tick: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for TimelapseEditor {}

impl TimelapseEditor {
    pub fn new(token: String, cx: &mut Context<Self>) -> Self {
        let mut e = Self {
            token,
            phase: Phase::Loading,
            units: Vec::new(),
            saved_cuts: Vec::new(),
            regions: Vec::new(),
            selected: None,
            video: None,
            time: 0.0,
            playing: false,
            drag: None,
            strip: Bounds::default(),
            tiles: Vec::new(),
            tile_w: (56.0f32 * 16.0 / 9.0).round(),
            film_key: None,
            film_task: None,
            saving: false,
            save_error: None,
            progress: 0.0,
            prep_started: Instant::now(),
            real_anchor: None,
            focus: cx.focus_handle(),
            play_overlay: Anim::new(1.0),
            cut_ring: Anim::new(0.0),
            hover_region: None,
            region_hover: Anim::new(0.0),
            hover_handle: None,
            grip: Anim::new(1.0),
            head_hover: false,
            head_scale: Anim::new(1.0),
            btn_hover: Anim::new(0.0),
            btn_press: Anim::new(0.0),
            lease_lost: false,
            _poll: None,
            _lease: None,
            _tick: None,
            _subs: Vec::new(),
        };
        e.load(cx);
        e.start_lease(cx);
        e
    }

    fn cfg(&self, cx: &App) -> SessionConfig {
        SessionConfig { token: self.token.clone(), api_base_url: api_base(cx) }
    }

    /// Edit lease: heartbeat now and every 30s; losing it ends editing.
    fn start_lease(&mut self, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        self._lease = Some(cx.spawn(async move |this, cx| loop {
            let core2 = core.clone();
            let cfg2 = cfg.clone();
            let held = app::tokio_async(cx, async move { core2.session_heartbeat_editing(&cfg2).await }).await;
            if let Ok(v) = held {
                if v.get("held").and_then(|h| h.as_bool()) == Some(false) {
                    let stop = this
                        .update(cx, |e, cx| {
                            if !e.saving {
                                e.lease_lost = true;
                                e.phase = Phase::Failed("This timelapse was already published, so it can no longer be edited.".into());
                                cx.notify();
                            }
                        })
                        .is_err();
                    if stop {
                        break;
                    }
                }
            }
            cx.background_executor().timer(Duration::from_secs(30)).await;
        }));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let core_n = core.clone();
        let cfg_n = cfg.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(v) = app::tokio_async(cx, async move { core_n.session_get(&cfg_n).await }).await {
                if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    let name = name.to_string();
                    this.update(cx, |_, cx| cx.emit(EditorEvent::SessionName(name))).ok();
                }
            }
        })
        .detach();
        let fut = app::tokio(cx, async move { core.session_units(&cfg).await });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |e, cx| match res {
                Ok(v) => e.on_units(v, cx),
                Err(err) => {
                    e.phase = Phase::Failed(err.message);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn fail_message(reason: &str) -> String {
        match reason {
            "published" => "This timelapse has already been published, so it can't be edited.".into(),
            "failed" => "This timelapse couldn't be compiled, so there's nothing to edit.".into(),
            "recompiles_exhausted" => "This timelapse has reached its edit limit.".into(),
            _ => "This timelapse isn't available for editing.".into(),
        }
    }

    fn on_units(&mut self, v: serde_json::Value, cx: &mut Context<Self>) {
        let editable = v.get("editable").and_then(|b| b.as_bool()).unwrap_or(false);
        let url = v.get("originalVideoUrl").and_then(|u| u.as_str()).map(String::from);
        let reason = v.get("editableReason").and_then(|r| r.as_str()).unwrap_or("").to_string();
        if editable {
            if let Some(url) = url {
                self.units = v.get("units").and_then(|u| serde_json::from_value(u.clone()).ok()).unwrap_or_default();
                self.saved_cuts = v.get("cuts").and_then(|c| serde_json::from_value(c.clone()).ok()).unwrap_or_default();
                self.regions = em::cuts_to_regions(&self.saved_cuts, &self.units);
                let player = cx.new(|cx| {
                    let mut p = VideoPlayer::new(url, cx);
                    p.controls = false;
                    p
                });
                self._subs.push(cx.subscribe(&player, |e, _, ev: &VideoEvent, cx| e.on_video(ev, cx)));
                self.video = Some(player);
                self.phase = Phase::Ready;
                self._poll = None;
                self._tick = None;
                cx.notify();
                return;
            }
        }
        if reason == "preparing" || reason == "no_original" {
            let n = v.get("expectedUnits").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            if !matches!(self.phase, Phase::Preparing { .. }) {
                self.prep_started = Instant::now();
            }
            self.phase = Phase::Preparing { units: n };
            self.start_status_poll(cx);
            self.start_progress_tick(cx);
        } else {
            self.phase = Phase::Failed(Self::fail_message(&reason));
        }
        cx.notify();
    }

    fn start_status_poll(&mut self, cx: &mut Context<Self>) {
        if self._poll.is_some() {
            return;
        }
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        self._poll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(2000)).await;
            let (c, f) = (core.clone(), cfg.clone());
            let Ok(v) = app::tokio_async(cx, async move { c.session_status(&f).await }).await else { continue };
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let progress = v.get("progress").and_then(|p| p.as_f64()).map(|p| p as f32);
            let editable = v.get("editable").and_then(|e| e.as_bool()).unwrap_or(false);
            let alive = this.update(cx, |e, cx| {
                if let Some(p) = progress {
                    e.real_anchor = Some((p, Instant::now()));
                }
                if status == "complete" {
                    e.phase = Phase::Failed(Self::fail_message("published"));
                    e._poll = None;
                } else if status == "failed" {
                    e.phase = Phase::Failed(Self::fail_message("failed"));
                    e._poll = None;
                } else if editable {
                    e._poll = None;
                    e.load(cx);
                }
                cx.notify();
            });
            if alive.is_err() {
                break;
            }
        }));
    }

    /// Build progress, ticked every 200ms: max(previous, estimate, real),
    /// capped at 95% until the build lands.
    fn start_progress_tick(&mut self, cx: &mut Context<Self>) {
        if self._tick.is_some() {
            return;
        }
        self._tick = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let alive = this.update(cx, |e, cx| {
                if let Phase::Preparing { units } = e.phase {
                    let est = em::build_progress::estimate_ms(units);
                    let estimated = em::build_progress::estimate(e.prep_started.elapsed().as_secs_f32() * 1000.0, est);
                    let real = e
                        .real_anchor
                        .map(|(a, since)| em::build_progress::interpolate(a, since.elapsed().as_secs_f32() * 1000.0, est))
                        .unwrap_or(0.0);
                    e.progress = e.progress.max(estimated).max(real).min(em::build_progress::CAP);
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        }));
    }

    fn n(&self) -> usize {
        self.units.len()
    }

    fn normalized(&self) -> Vec<Region> {
        em::normalize(&self.regions)
    }

    fn on_video(&mut self, ev: &VideoEvent, cx: &mut Context<Self>) {
        match ev {
            VideoEvent::Time(t) => {
                self.time = *t;
                // Playback skips cut stretches; a cut that runs to the end stops.
                if self.playing && self.drag.is_none() {
                    if let Some(r) = em::region_at_time(*t, &self.normalized()) {
                        let n = self.n() as i64;
                        if let Some(v) = &self.video {
                            if r.end < n {
                                v.update(cx, |v, cx| v.seek(r.end as f64, cx));
                            } else {
                                v.update(cx, |v, cx| {
                                    v.pause(cx);
                                    v.seek(r.start as f64, cx)
                                });
                            }
                        }
                    }
                }
            }
            VideoEvent::Playing(p) => {
                self.playing = *p;
                self.play_overlay.set(if *p { 0.0 } else { 1.0 }, Curve::tween(160.0, QUART));
            }
            _ => {}
        }
        cx.notify();
    }

    fn seek(&mut self, t: f64, cx: &mut Context<Self>) {
        let n = self.n() as f64;
        let t = t.clamp(0.0, (n - 0.05).max(0.0));
        self.time = t;
        if let Some(v) = &self.video {
            v.update(cx, |v, cx| v.seek(t, cx));
        }
        cx.notify();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let Some(v) = self.video.clone() else { return };
        if self.playing {
            v.update(cx, |v, cx| v.pause(cx));
        } else {
            // Play from inside a cut starts after it.
            if let Some(r) = em::region_at_time(self.time, &self.normalized()) {
                if r.end < self.n() as i64 {
                    self.seek(r.end as f64, cx);
                }
            }
            v.update(cx, |v, cx| v.play(cx));
        }
    }

    fn cut_here(&mut self, cx: &mut Context<Self>) {
        if self.n() == 0 {
            return;
        }
        let at = em::unit_at_time(self.time, self.n());
        let mut next = self.regions.clone();
        next.push(Region { start: at, end: at + 1 });
        self.regions = em::normalize(&next);
        self.selected = self.regions.iter().position(|r| at >= r.start && at < r.end);
        self.emit_cuts(cx);
    }

    fn emit_cuts(&mut self, cx: &mut Context<Self>) {
        let cuts = em::regions_to_cuts(&self.normalized(), &self.units);
        let removed = em::count_cut_units(&self.units, &cuts);
        let dirty = cuts != self.saved_cuts;
        cx.emit(EditorEvent::CutsChanged { removed: self.normalized().len(), dirty });
        let _ = removed;
        cx.notify();
    }

    pub fn current_cuts(&self) -> Vec<CutInterval> {
        em::regions_to_cuts(&self.normalized(), &self.units)
    }

    fn unit_at_x(&self, x: Pixels) -> f64 {
        let w = f32::from(self.strip.size.width).max(1.0);
        let frac = ((f32::from(x) - f32::from(self.strip.origin.x)) / w).clamp(0.0, 1.0);
        frac as f64 * self.n() as f64
    }

    fn on_down(&mut self, drag: Drag, x: Pixels, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if let Drag::Scrub = drag {
            let u = self.unit_at_x(x);
            self.seek(u, cx);
        }
        self.drag = Some(drag);
        cx.notify();
    }

    fn on_move(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        let u = self.unit_at_x(x);
        let n = self.n() as i64;
        let next = match drag {
            Drag::Scrub => {
                self.seek(u, cx);
                Drag::Scrub
            }
            Drag::Maybe { down } => {
                if (u - down).abs() < 0.34 {
                    Drag::Maybe { down }
                } else {
                    let a = u.min(down).floor() as i64;
                    let b = u.max(down).ceil() as i64;
                    self.regions.push(Region { start: a, end: b.max(a + 1) });
                    let index = self.regions.len() - 1;
                    self.selected = Some(index);
                    Drag::Region { index, mode: if u >= down { DragMode::End } else { DragMode::Start }, grab: 0.0, anchor: down.floor() as i64 }
                }
            }
            Drag::Region { index, mode, grab, anchor } => {
                if let Some(r) = self.regions.get(index).copied() {
                    let mut r = r;
                    let mut seek_to = None;
                    match mode {
                        DragMode::Move => {
                            let width = r.end - r.start;
                            let start = ((u - grab).round() as i64).clamp(0, (n - width).max(0));
                            r.start = start;
                            r.end = start + width;
                        }
                        DragMode::Start => {
                            r.start = (u.round() as i64).clamp(0, r.end - 1);
                            seek_to = Some(r.start as f64 + 0.02);
                        }
                        DragMode::End => {
                            let rounded = u.round() as i64;
                            if rounded <= anchor {
                                r.start = rounded.max(0);
                                r.end = anchor + 1;
                                seek_to = Some(r.start as f64 + 0.02);
                            } else {
                                r.end = n.min((r.start + 1).max(rounded));
                                seek_to = Some((n as f64 - 0.05).min(r.end as f64 + 0.02));
                            }
                        }
                    }
                    self.regions[index] = r;
                    self.selected = Some(index);
                    if let Some(t) = seek_to {
                        self.seek(t, cx);
                    }
                }
                Drag::Region { index, mode, grab, anchor }
            }
        };
        self.drag = Some(next);
        self.emit_cuts(cx);
    }

    fn on_up(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Maybe { down } => {
                self.seek(down, cx);
                self.selected = None;
            }
            Drag::Region { index, .. } => {
                let dragged = self.regions.get(index).copied();
                self.regions = em::normalize(&self.regions);
                self.selected = dragged.and_then(|d| self.regions.iter().position(|r| d.start >= r.start && d.start < r.end));
                self.emit_cuts(cx);
            }
            Drag::Scrub => {}
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.saving = true;
        self.save_error = None;
        let cuts = self.current_cuts();
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let fut = app::tokio(cx, async move {
            let body = serde_json::to_value(&cuts).unwrap_or_default();
            core.session_set_cuts(&cfg, body).await?;
            core.session_apply_cuts(&cfg).await
        });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |e, cx| match res {
                Ok(v) => {
                    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
                    cx.emit(EditorEvent::Applied(ApplyResult {
                        status: s("status"),
                        redirect_url: s("redirectUrl"),
                        panel_url: s("panelUrl"),
                        panel_resolved: v.get("panelResolved").and_then(|b| b.as_bool()).unwrap_or(false),
                    }));
                }
                Err(err) => {
                    e.save_error = Some(err.message);
                    e.saving = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Kick off (debounced) filmstrip decoding for the current strip width.
    fn ensure_filmstrip(&mut self, cx: &mut Context<Self>) {
        let w = f32::from(self.strip.size.width);
        if w <= 0.0 {
            return;
        }
        let Some(video) = &self.video else { return };
        let (url, duration) = {
            let v = video.read(cx);
            // One unit is one second of video; trust that when the stream
            // can't tell us its length.
            (v.url.clone(), if v.duration > 0.0 { v.duration } else { self.units.len() as f64 })
        };
        if duration <= 0.0 {
            return;
        }
        let aspect = video.read(cx).frame().map(|(_, a)| a).unwrap_or(16.0 / 9.0);
        let tile_w = (STRIP_HEIGHT * aspect).round().max(24.0);
        let count = ((w / tile_w).ceil() as usize).clamp(1, 48);
        let key = (w.round() as i32, count);
        if self.film_key == Some(key) {
            return;
        }
        self.film_key = Some(key);
        self.tile_w = tile_w;
        let times: Vec<f64> = (0..count)
            .map(|i| {
                let f = (((i as f32 + 0.5) * tile_w) / w).min(1.0) as f64;
                (f * duration).min(duration - 0.05).max(0.0)
            })
            .collect();
        let (tx, rx) = flume::unbounded::<Vec<Option<Arc<RenderImage>>>>();
        let h = (STRIP_HEIGHT * 2.0) as u32;
        self.film_task = Some(cx.spawn(async move |this, cx| {
            // 220ms debounce after a resize / data change.
            cx.background_executor().timer(Duration::from_millis(220)).await;
            std::thread::spawn(move || {
                if let Err(e) = crate::filmstrip::extract(&url, &times, h, &|tiles| {
                    let _ = tx.send(tiles);
                }) {
                    log::warn!("[filmstrip] {e}");
                }
            });
            while let Ok(tiles) = rx.recv_async().await {
                if this.update(cx, |e, cx| {
                    e.tiles = tiles;
                    cx.notify();
                }).is_err() {
                    break;
                }
            }
        }));
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.platform || k.modifiers.control {
            return;
        }
        match k.key.as_str() {
            "space" | "k" => self.toggle_play(cx),
            "x" | "c" => self.cut_here(cx),
            "delete" | "backspace" => {
                if let Some(i) = self.selected.take() {
                    if i < self.regions.len() {
                        self.regions.remove(i);
                    }
                    self.emit_cuts(cx);
                }
            }
            "escape" => self.selected = None,
            "left" | "right" => {
                let step = if k.modifiers.shift { 10.0 } else { 1.0 };
                let t = self.time + if k.key == "left" { -step } else { step };
                self.seek(t, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn pct(u: f64, n: usize) -> f32 {
    if n == 0 { 0.0 } else { (u / n as f64) as f32 }
}

impl Render for TimelapseEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        match &self.phase {
            Phase::Failed(msg) => {
                return div().p(px(20.0)).max_w(px(520.0)).child(ErrorBanner::new("editor-error", msg.clone()).title("Can't edit")).into_any_element();
            }
            Phase::Loading | Phase::Preparing { .. } => {
                let units = if let Phase::Preparing { units } = self.phase { Some(units) } else { None };
                return div()
                    .size_full()
                    .min_h(px(260.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(16.0))
                    .text_center()
                    .p(px(20.0))
                    .child(match units {
                        Some(_) => ProgressRing::new("editor-progress", self.progress).show_percent().into_any_element(),
                        None => Spinner::new(SpinnerSize::Lg).into_any_element(),
                    })
                    .child(
                        div()
                            .child(div().ts(16.0).w600().text_color(t.text_primary).child("Preparing your timelapse"))
                            .child(div().ts_lh(13.0, 1.5).text_color(t.text_secondary).mt(px(4.0)).max_w(px(340.0)).child(match units {
                                Some(n) if n > 0 => format!("Stitching {n} minute{} of footage.", if n == 1 { "" } else { "s" }),
                                // The Tauri app printed this placeholder before the count arrived.
                                _ => "oooooooooooo".to_string(),
                            })),
                    )
                    .into_any_element();
            }
            Phase::Ready => {}
        }
        if !self.focus.is_focused(window) && self.drag.is_none() {
            window.focus(&self.focus, cx);
        }
        self.ensure_filmstrip(cx);
        let n = self.n();
        let normalized = self.normalized();
        let in_cut = em::region_at_time(self.time, &normalized).is_some();
        if self.cut_ring.target() != if in_cut { 1.0 } else { 0.0 } {
            self.cut_ring.set(if in_cut { 1.0 } else { 0.0 }, Curve::tween(120.0, Ease::EASE_OUT));
        }
        let mut moving = false;
        for a in [
            &mut self.play_overlay,
            &mut self.cut_ring,
            &mut self.region_hover,
            &mut self.grip,
            &mut self.head_scale,
            &mut self.btn_hover,
            &mut self.btn_press,
        ] {
            moving |= a.tick();
        }
        if moving {
            window.request_animation_frame();
        }

        // ── Stage ──
        let video = self.video.clone();
        let po = self.play_overlay.get();
        let ring_o = self.cut_ring.get();
        let stage = div()
            .id("editor-stage")
            .flex_1()
            .min_h(px(0.0))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.well)
            .border_1()
            .border_color(t.well_border)
            .rounded(px(12.0))
            .overflow_hidden()
            .cursor(CursorStyle::PointingHand)
            .children(video.map(|v| div().absolute().inset_0().child(v)))
            .on_click(cx.listener(|e, _, _, cx| e.toggle_play(cx)))
            .when(po > 0.001, |d| {
                let s = 0.88 + 0.12 * po;
                d.child(
                    div().absolute().inset_0().flex().items_center().justify_center().opacity(po).child(
                        canvas(|_, _, _| (), move |b, _, window, _| {
                            let c = paint::bounds_center(b);
                            let r = 28.0 * s;
                            let circle = Bounds { origin: point(px(c.x - r), px(c.y - r)), size: gpui_kit::gpui::size(px(2.0 * r), px(2.0 * r)) };
                            window.paint_quad(gpui_kit::gpui::fill(circle, theme::black(0.55)).corner_radii(px(r)));
                            // play triangle (M8 5v14l11-7z in a 20px box)
                            let k = 20.0 / 24.0 * s;
                            let o = (c.x - 10.0 * s, c.y - 10.0 * s);
                            let mut p = PathBuilder::fill();
                            p.move_to(point(px(o.0 + 8.0 * k), px(o.1 + 5.0 * k)));
                            p.line_to(point(px(o.0 + 8.0 * k), px(o.1 + 19.0 * k)));
                            p.line_to(point(px(o.0 + 19.0 * k), px(o.1 + 12.0 * k)));
                            p.close();
                            if let Ok(p) = p.build() {
                                window.paint_path(p, theme::hex(0xffffff));
                            }
                        })
                        .size(px(80.0)),
                    ),
                )
            })
            .when(ring_o > 0.001, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.0))
                        .opacity(ring_o)
                        .bg(theme::rgba8(220, 38, 38, 0.10))
                        .border(px(3.0))
                        .border_color(t.cut_border)
                        .child(
                            div()
                                .absolute()
                                .top(px(12.0))
                                .right(px(12.0))
                                .bg(t.cut_border)
                                .text_color(theme::hex(0xffffff))
                                .ts(11.0)
                                .w600()
                                .px(px(8.0))
                                .py(px(3.0))
                                .rounded(px(6.0))
                                .child("Will be removed"),
                        ),
                )
            });

        // ── Transport ──
        let cur_unit = em::unit_at_time(self.time, n);
        let clock = self.units.get(cur_unit as usize).map(|u| crate::fmt::clock_time_ms(em::parse_ms(&u.captured_at))).unwrap_or_default();
        let bh = self.btn_hover.get();
        let bp = self.btn_press.get();
        let play_btn = div()
            .id("editor-play")
            .size(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                canvas(|_, _, _| (), {
                    let (bg, border) = (theme::mix(theme::transparent(), t.bg_surface, bh), theme::mix(t.border_default, t.border_hover, bh));
                    move |b, _, window, _| paint::paint_rounded_bg(b, 8.0, 1.0 - 0.06 * bp, bg, border, window)
                })
                .absolute()
                .inset_0(),
            )
            .relative()
            .child(if self.playing { Icon::LkPauseBars } else { Icon::LkPlay }.el(12.0, t.text_primary))
            .on_hover(cx.listener(|e, h: &bool, _, cx| {
                e.btn_hover.set_css(if *h { 1.0 } else { 0.0 }, ed_ms(140.0), QUART);
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|e, _, _, cx| {
                e.btn_press.set_css(1.0, ed_ms(140.0), QUART);
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|e, _, _, cx| {
                e.btn_press.set_css(0.0, ed_ms(140.0), QUART);
                cx.notify();
            }))
            .on_click(cx.listener(|e, _, _, cx| e.toggle_play(cx)));
        let transport = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(play_btn)
            .child(
                div()
                    .flex()
                    .ts(13.0)
                    .text_color(t.text_primary)
                    .child(em::elapsed_label(cur_unit as f64, n))
                    .child(div().text_color(t.text_tertiary).child(format!(" of {} · recorded at {clock}", em::elapsed_label(n as f64, n)))),
            )
            .child(div().flex_1());

        // ── Timeline ──
        let strip_w = f32::from(self.strip.size.width);
        let step = em::ruler_step(n, strip_w);
        let ticks = em::ruler_ticks(n, step);
        let mut ruler = div()
            .id("editor-ruler")
            .relative()
            .h(px(RULER_HEIGHT))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| e.on_down(Drag::Scrub, ev.position.x, cx)));
        for (u, major) in &ticks {
            let first = *u == 0.0;
            let last = *u >= n as f64;
            let p = pct(*u, n);
            let mut tick = div().absolute().top_0().bottom_0().left(gpui_kit::gpui::relative(p));
            if *major {
                let label = div().absolute().top_0().ts(11.0).text_color(t.text_tertiary).whitespace_nowrap().child(em::elapsed_label(*u, n));
                let label = if first {
                    label.left_0()
                } else if last {
                    label.right_0()
                } else {
                    label.left(px(-14.0)).w(px(28.0)).flex().justify_center()
                };
                tick = tick.child(label);
            }
            tick = tick.child(
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(if first { 0.0 } else if last { -1.0 } else { -0.5 }))
                    .w(px(1.0))
                    .h(px(if *major { 7.0 } else { 4.0 }))
                    .bg(if *major { t.text_tertiary } else { t.text_quaternary }),
            );
            ruler = ruler.child(tick);
        }

        let probe = cx.entity();
        let tiles = self.tiles.clone();
        let tile_w = self.tile_w;
        let mut strip = div()
            .id("editor-strip")
            .relative()
            .h(px(STRIP_HEIGHT))
            .rounded(px(8.0))
            .overflow_hidden()
            .cursor(CursorStyle::Crosshair)
            .bg(t.track)
            .border_1()
            .border_color(t.border_default)
            .child(
                canvas(move |b, _, cx| probe.update(cx, |e, _| e.strip = b), move |b, _, window, _| {
                    // Filmstrip tiles, left to right at their natural width.
                    let mut x = f32::from(b.origin.x);
                    for tile in tiles.iter() {
                        if let Some(img) = tile {
                            let tb = Bounds { origin: point(px(x), b.origin.y), size: gpui_kit::gpui::size(px(tile_w), b.size.height) };
                            let _ = window.paint_image(b, tb, Default::default(), img.clone(), 0, false);
                        }
                        x += tile_w;
                    }
                })
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                let u = e.unit_at_x(ev.position.x);
                e.on_down(Drag::Maybe { down: u }, ev.position.x, cx)
            }));
        for gap in em::gap_indices(&self.units) {
            strip = strip.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui_kit::gpui::relative(pct(gap as f64, n)))
                    .ml(px(-1.0))
                    .w(px(2.0))
                    .child(canvas(|_, _, _| (), {
                        let c = t.text_quaternary;
                        move |b, _, window, _| {
                            let mut y = f32::from(b.origin.y);
                            let bottom = y + f32::from(b.size.height);
                            while y < bottom {
                                let seg = Bounds { origin: point(b.origin.x, px(y)), size: gpui_kit::gpui::size(b.size.width, px(3.0f32.min(bottom - y))) };
                                window.paint_quad(gpui_kit::gpui::fill(seg, c));
                                y += 6.0;
                            }
                        }
                    }).size_full()),
            );
        }
        let rh = self.region_hover.get();
        let grip_scale = self.grip.get();
        for (i, r) in self.regions.clone().into_iter().enumerate() {
            let selected = self.selected == Some(i);
            let hovered = self.hover_region == Some(i);
            let fill = if hovered { theme::mix(t.cut_fill, t.cut_fill_hover, rh) } else { t.cut_fill };
            let stripe = t.cut_stripe;
            let border = t.cut_border;
            let ring = if selected { 2.0 } else { 1.0 };
            let left = pct(r.start as f64, n);
            let width = pct((r.end - r.start) as f64, n);
            let handle = |start: bool, cx: &mut Context<Self>| {
                let hov = self.hover_handle == Some((i, start));
                let gw = 3.0 * if hov { grip_scale } else { 1.0 };
                div()
                    .id(("handle", i * 2 + start as usize))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(px(12.0))
                    .map(|d| if start { d.left(px(-6.0)) } else { d.right(px(-6.0)) })
                    .cursor(CursorStyle::ResizeLeftRight)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w(px(gw)).h(px(22.0)).rounded(px(2.0)).bg(border))
                    .on_hover(cx.listener(move |e, h: &bool, _, cx| {
                        if *h {
                            e.hover_handle = Some((i, start));
                            e.grip.jump(1.0);
                            e.grip.set_css(ed_hover(1.6), ed_ms(140.0), QUART);
                        } else if e.hover_handle == Some((i, start)) {
                            e.grip.set_css(1.0, ed_ms(140.0), QUART);
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let Some(r) = e.regions.get(i).copied() else { return };
                        e.selected = Some(i);
                        let grab = e.unit_at_x(ev.position.x) - r.start as f64;
                        e.on_down(
                            Drag::Region { index: i, mode: if start { DragMode::Start } else { DragMode::End }, grab, anchor: r.start },
                            ev.position.x,
                            cx,
                        );
                    }))
            };
            let h_start = handle(true, cx);
            let h_end = handle(false, cx);
            strip = strip.child(
                div()
                    .id(("region", i))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui_kit::gpui::relative(left))
                    .w(gpui_kit::gpui::relative(width))
                    .cursor(CursorStyle::OpenHand)
                    .child(
                        canvas(|_, _, _| (), move |b, _, window, _| {
                            window.paint_quad(gpui_kit::gpui::fill(b, fill).corner_radii(px(6.0)));
                            // 45° hatch: 5px stripe, 5px gap.
                            let clip = paint::inset(b, 1.0);
                            window.with_content_mask(Some(gpui_kit::gpui::ContentMask { bounds: clip }), |window| {
                                let h = f32::from(b.size.height);
                                let x0 = f32::from(b.origin.x) - h;
                                let x1 = f32::from(b.origin.x) + f32::from(b.size.width);
                                let y0 = f32::from(b.origin.y);
                                let mut x = x0 - (x0.rem_euclid(10.0));
                                while x < x1 {
                                    let mut p = PathBuilder::fill();
                                    p.move_to(point(px(x), px(y0 + h)));
                                    p.line_to(point(px(x + 5.0 * std::f32::consts::SQRT_2), px(y0 + h)));
                                    p.line_to(point(px(x + 5.0 * std::f32::consts::SQRT_2 + h), px(y0)));
                                    p.line_to(point(px(x + h), px(y0)));
                                    p.close();
                                    if let Ok(p) = p.build() {
                                        window.paint_path(p, stripe);
                                    }
                                    x += 10.0 * std::f32::consts::SQRT_2;
                                }
                            });
                            window.paint_quad(gpui_kit::gpui::quad(b, px(6.0), gpui_kit::gpui::transparent_black(), px(ring), border, Default::default()));
                        })
                        .absolute()
                        .inset_0(),
                    )
                    .on_hover(cx.listener(move |e, h: &bool, _, cx| {
                        if *h {
                            e.hover_region = Some(i);
                            e.region_hover.jump(0.0);
                            e.region_hover.set_css(1.0, ed_ms(140.0), QUART);
                        } else if e.hover_region == Some(i) {
                            e.region_hover.set_css(0.0, ed_ms(140.0), QUART);
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let Some(r) = e.regions.get(i).copied() else { return };
                        e.selected = Some(i);
                        let grab = e.unit_at_x(ev.position.x) - r.start as f64;
                        e.on_down(Drag::Region { index: i, mode: DragMode::Move, grab, anchor: r.start }, ev.position.x, cx);
                    }))
                    .child(h_start)
                    .child(h_end),
            );
        }

        // Playhead
        let hs = self.head_scale.get();
        let head_left = pct(self.time.min(n as f64), n);
        let playhead = (n > 0).then(|| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(gpui_kit::gpui::relative(head_left))
                .w(px(0.0))
                .child(
                    div()
                        .id("playhead-hit")
                        .absolute()
                        .top(px(RULER_HEIGHT - HEAD_H))
                        .left(px(-HEAD_HIT / 2.0))
                        .size(px(HEAD_HIT))
                        .flex()
                        .justify_center()
                        .cursor(CursorStyle::ResizeLeftRight)
                        .child(
                            div()
                                .w(px(HEAD_W * hs))
                                .h(px(HEAD_H))
                                .rounded(px(4.5))
                                .bg(t.text_primary)
                                .shadow(vec![theme::sh(0.0, 1.0, 3.0, 0.0, theme::black(0.45))]),
                        )
                        .on_hover(cx.listener(|e, h: &bool, _, cx| {
                            e.head_hover = *h;
                            e.head_scale.set_css(if *h { ed_hover(1.25) } else { 1.0 }, ed_ms(120.0), QUART);
                            cx.notify();
                        }))
                        .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            e.head_scale.set_css(ed_hover(1.1), ed_ms(120.0), QUART);
                            e.on_down(Drag::Scrub, ev.position.x, cx)
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(RULER_HEIGHT - 4.0))
                        .bottom_0()
                        .left(px(-1.0))
                        .w(px(2.0))
                        .bg(t.text_primary)
                        .shadow(vec![theme::sh(0.0, 0.0, 0.0, 0.5, theme::black(0.35))]),
                )
        });

        let timeline = div()
            .id("editor-timeline")
            .relative()
            .on_mouse_move(cx.listener(|e, ev: &MouseMoveEvent, _, cx| e.on_move(ev.position.x, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|e, _, _, cx| {
                e.head_scale.set_css(if e.head_hover { ed_hover(1.25) } else { 1.0 }, ed_ms(120.0), QUART);
                e.on_up(cx)
            }))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|e, _, _, cx| e.on_up(cx)))
            .child(ruler)
            .child(strip)
            .children(playhead);

        // ── Actions ──
        let cuts = em::regions_to_cuts(&normalized, &self.units);
        let removed = em::count_cut_units(&self.units, &cuts);
        let kept = n - removed;
        let all_cut = n > 0 && kept == 0;
        let has_selection = self.selected.is_some();
        let (e1, e2, e3) = (cx.entity(), cx.entity(), cx.entity());
        let actions = div()
            .flex()
            .items_center()
            .gap(px(12.0))
            .flex_wrap()
            .mt(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .ts(14.0)
                    .w600()
                    .text_color(t.text_primary)
                    .child(minutes_flow("kept", kept as f64, t.text_primary))
                    .child(" kept")
                    .when(removed > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .w500()
                                .text_color(t.cut_border)
                                .child(" · ")
                                .child(minutes_flow("removed", removed as f64, t.cut_border))
                                .child(" removed"),
                        )
                    }),
            )
            .child(div().flex_1().min_w(px(12.0)))
            .when(has_selection, |d| {
                d.child(
                    Button::new("remove-cut", "Remove cut").variant(Variant::Secondary).size(BtnSize::Sm).on_click(move |_, _, cx| {
                        e1.update(cx, |e, cx| {
                            if let Some(i) = e.selected.take() {
                                if i < e.regions.len() {
                                    e.regions.remove(i);
                                }
                                e.emit_cuts(cx);
                            }
                        })
                    }),
                )
            })
            .when(!normalized.is_empty(), |d| {
                d.child(
                    Button::new("clear-all", "Clear all").variant(Variant::Secondary).size(BtnSize::Sm).on_click(move |_, _, cx| {
                        e2.update(cx, |e, cx| {
                            e.regions.clear();
                            e.selected = None;
                            e.emit_cuts(cx);
                        })
                    }),
                )
            })
            .child({
                let mut b = Button::new("editor-save", if self.saving { "Saving…" } else { "Save" })
                    .size(BtnSize::Sm)
                    .loading(self.saving)
                    .disabled(all_cut)
                    .on_click(move |_, _, cx| e3.update(cx, |e, cx| e.save(cx)));
                if all_cut {
                    b = b.tooltip("You can't remove the entire timelapse");
                }
                b
            });

        div()
            .id("timelapse-editor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(stage)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(transport)
                    .child(timeline)
                    .child(actions)
                    .when_some(self.save_error.clone(), |d, err| d.child(ErrorBanner::new("save-error", err).title("Couldn't save your edits"))),
            )
            .into_any_element()
    }
}

#[allow(unused)]
fn _u(_: AnyElement, _: SharedString, _: Hsla, _: &Theme) {}
