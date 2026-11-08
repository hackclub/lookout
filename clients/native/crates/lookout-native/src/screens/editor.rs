//! `TimelapseEditor.tsx` — trim (and mask) a timelapse before it's published.
//!
//! The stage plays the uncut original at its own aspect ratio. Under it, the
//! timeline: a timecode ruler, a filmstrip where dragging marks a stretch to
//! remove, one lane per stack of overlapping masks, and an overview bar that
//! pans the timeline once it's zoomed. Then the toolbar: play, zoom, add a
//! cut or mask, the kept total and Publish.
//!
//! Cuts are integer unit ranges (one unit = one video second = one real
//! minute), merged when they touch, and saved as capture-time intervals.
//! Masks (lookout PR #198) are boxes on the frame over a span of video
//! seconds that the worker pixelates; they only appear when the server's
//! `GET /units` returns a `masks` list, since older servers reject them.
//!
//! The timeline zooms (buttons, `=`/`-`/`0`, pinch, Ctrl/⌘-scroll, a mouse
//! wheel) and pans (two-finger or Shift-scroll, the overview bar), and
//! follows the playhead while playing.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, AppContext, Bounds, Context, Corners, CursorStyle, DispatchPhase, Entity, EventEmitter,
    FocusHandle, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathBuilder, PinchEvent, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollEvent,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div, point, px,
};
use lookout_core::SessionConfig;

use crate::app::{self, api_base};
use crate::editor_math::{self as em, CutInterval, Mask, Region, VideoUnit, View};
use crate::filmstrip::{self, Filmstrip};
use crate::motion::{Anim, Curve, Ease};
use crate::theme::{self, Theme, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::feedback::{ErrorBanner, ProgressRing, minutes_flow};
use crate::ui::icon::Icon;
use crate::ui::paint;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;
use crate::ui::video::{VideoEvent, VideoPlayer};

const RULER_H: f32 = 30.0;
const STRIP_H: f32 = 64.0;
const STRIP_R: f32 = 12.0;
const LANE_H: f32 = 16.0;
const LANE_GAP: f32 = 5.0;
const MINI_H: f32 = 12.0;
const STAGE_R: f32 = 12.0;
/// Depth of the timeline's left/right fade while zoomed.
const EDGE_FADE: f32 = 36.0;
/// Toolbar control height.
const CTRL: f32 = 40.0;
/// Deepest zoom: one unit (a real minute) at most this wide.
const MAX_UNIT_PX: f32 = 640.0;
/// Keep at most this many decoded thumbnails.
const TILE_CACHE: usize = 700;
/// Lane colours, matched by each mask's box on the video.
const LANE_COLORS: [u32; 3] = [0x3b82f6, 0x8b5cf6, 0xeab308];
const QUART: Ease = Ease::EASE_OUT_QUART;

/// `editorStyles.ts` reduced-motion block: every editor transition collapses
/// to 1ms under `prefers-reduced-motion: reduce`.
fn ed_ms(ms: f32) -> f32 {
    if crate::motion::reduce_motion() { 1.0 } else { ms }
}

/// …and the grip / playhead hover transforms are removed.
fn ed_hover(scale: f32) -> f32 {
    if crate::motion::reduce_motion() { 1.0 } else { scale }
}

fn lane_color(lane: usize) -> Hsla {
    theme::hex(LANE_COLORS[lane % LANE_COLORS.len()])
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
    CutsChanged { removed: usize, masks: usize, dirty: bool },
    SessionName(String),
}

enum Phase {
    Loading,
    Preparing { units: usize },
    Failed(String),
    Ready,
}

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Move,
    Start,
    End,
}

/// Which edges of a mask box a drag moves (none = the whole box).
#[derive(Clone, Copy, Default, PartialEq)]
struct Grip {
    l: bool,
    r: bool,
    t: bool,
    b: bool,
}

enum Drag {
    /// Playhead / ruler.
    Scrub,
    /// Pressed on the filmstrip: a click seeks, a drag marks a cut.
    Maybe { down: f64 },
    /// Moving or resizing a cut.
    Region { index: usize, mode: DragMode, grab: f64, anchor: i64 },
    /// The overview bar's thumb; `grab` = pointer unit − view start.
    Minimap { grab: f64 },
    /// A mask's span on its lane.
    MaskSpan { id: String, mode: DragMode, grab: f64 },
    /// Pressed on the video: a click plays/pauses, a drag draws a mask.
    Stage { at: Point<Pixels> },
    /// Drawing a new mask from `origin` (normalized frame coordinates).
    MaskDraw { id: String, origin: (f32, f32) },
    /// Moving / resizing a mask's box.
    MaskBox { id: String, grip: Grip, at: (f32, f32), orig: (f32, f32, f32, f32) },
}

/// A zoom/pan animation. With an anchor, the anchored unit stays put while
/// the zoom eases (in log space); without, the window slides.
struct ViewAnim {
    from: View,
    to: View,
    anchor: Option<(f64, f64)>,
    p: Anim,
}

pub struct TimelapseEditor {
    token: String,
    phase: Phase,
    units: Vec<VideoUnit>,
    saved_cuts: Vec<CutInterval>,
    regions: Vec<Region>,
    selected: Option<usize>,
    masks_supported: bool,
    masks: Vec<Mask>,
    saved_masks: Vec<Mask>,
    sel_mask: Option<String>,
    hover_mask: Option<String>,
    mask_seq: u64,
    video: Option<Entity<VideoPlayer>>,
    /// The picture's aspect and length, once the player knows them.
    aspect: Option<f32>,
    video_duration: Option<f64>,
    time: f64,
    playing: bool,
    drag: Option<Drag>,
    /// The filmstrip's bounds (= the timeline's width), window coordinates.
    strip: Bounds<Pixels>,
    stage: Bounds<Pixels>,
    mini: Bounds<Pixels>,
    view: View,
    view_anim: Option<ViewAnim>,
    film: Option<Filmstrip>,
    film_cache: BTreeMap<u64, Arc<RenderImage>>,
    film_asked: HashSet<u64>,
    film_dead: bool,
    film_sent: Vec<u64>,
    _film_rx: Option<Task<()>>,
    _film_debounce: Option<Task<()>>,
    saving: bool,
    save_error: Option<String>,
    progress: f32,
    prep_started: Instant,
    real_anchor: Option<(f32, Instant)>,
    focus: FocusHandle,
    cut_ring: Anim,
    hover_region: Option<usize>,
    region_hover: Anim,
    hover_handle: Option<(usize, bool)>,
    grip: Anim,
    head_hover: bool,
    head_scale: Anim,
    head_tip: Anim,
    lane_h: Anim,
    /// Edge fades while zoomed: content runs on past the left / right edge.
    fade_l: Anim,
    fade_r: Anim,
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
            masks_supported: false,
            masks: Vec::new(),
            saved_masks: Vec::new(),
            sel_mask: None,
            hover_mask: None,
            mask_seq: 0,
            video: None,
            aspect: None,
            video_duration: None,
            time: 0.0,
            playing: false,
            drag: None,
            strip: Bounds::default(),
            stage: Bounds::default(),
            mini: Bounds::default(),
            view: View::FIT,
            view_anim: None,
            film: None,
            film_cache: BTreeMap::new(),
            film_asked: HashSet::new(),
            film_dead: false,
            film_sent: Vec::new(),
            _film_rx: None,
            _film_debounce: None,
            saving: false,
            save_error: None,
            progress: 0.0,
            prep_started: Instant::now(),
            real_anchor: None,
            focus: cx.focus_handle(),
            cut_ring: Anim::new(0.0),
            hover_region: None,
            region_hover: Anim::new(0.0),
            hover_handle: None,
            grip: Anim::new(1.0),
            head_hover: false,
            head_scale: Anim::new(1.0),
            head_tip: Anim::new(0.0),
            lane_h: Anim::new(0.0),
            fade_l: Anim::new(0.0),
            fade_r: Anim::new(0.0),
            lease_lost: false,
            _poll: None,
            _lease: None,
            _tick: None,
            _subs: Vec::new(),
        };
        e.load(cx);
        e.start_lease(cx);
        // Closing the editor frees its filmstrip tiles from the atlas.
        cx.on_release(|e, cx| {
            for (_, img) in std::mem::take(&mut e.film_cache) {
                cx.drop_image(img, None);
            }
        })
        .detach();
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
                // A server with the masking API always returns the list.
                self.masks_supported = v.get("masks").is_some_and(|m| m.is_array());
                if let Some(m) = v.get("masks") {
                    self.masks = em::masks_from_json(m, &self.units);
                    self.saved_masks = self.masks.clone();
                }
                let player = cx.new(|cx| {
                    let mut p = VideoPlayer::new(url.clone(), cx);
                    p.controls = false;
                    p.radius = STAGE_R;
                    p
                });
                self._subs.push(cx.subscribe(&player, |e, _, ev: &VideoEvent, cx| e.on_video(ev, cx)));
                self.video = Some(player);
                self.start_filmstrip(url, cx);
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

    // ── Playback ───────────────────────────────────────────────────────

    fn on_video(&mut self, ev: &VideoEvent, cx: &mut Context<Self>) {
        match ev {
            VideoEvent::Time(t) => {
                // While dragging, the pointer owns the playhead: the
                // player's reports trail it (and Media Foundation can briefly
                // report the pre-seek time), which made the head jump back.
                if self.drag.is_some() {
                    return;
                }
                self.time = *t;
                if self.playing {
                    // Playback skips cut stretches; a cut that runs to the end stops.
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
                    self.follow(*t);
                }
            }
            VideoEvent::Playing(p) => {
                self.playing = *p;
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

    // ── Edits ──────────────────────────────────────────────────────────

    fn cut_here(&mut self, cx: &mut Context<Self>) {
        if self.n() == 0 {
            return;
        }
        let at = em::unit_at_time(self.time, self.n());
        let mut next = self.regions.clone();
        next.push(Region { start: at, end: at + 1 });
        self.regions = em::normalize(&next);
        self.selected = self.regions.iter().position(|r| at >= r.start && at < r.end);
        self.sel_mask = None;
        self.emit_cuts(cx);
    }

    fn next_mask_id(&mut self) -> String {
        self.mask_seq += 1;
        format!("mask-{}-{}", chrono::Utc::now().timestamp_millis(), self.mask_seq)
    }

    /// The span a new mask at the playhead gets: the unit under it.
    fn new_mask_span(&self) -> (f64, f64) {
        let n = self.n() as f64;
        let start = self.time.floor().clamp(0.0, (n - 1.0).max(0.0));
        (start, (start + 1.0).min(n))
    }

    /// Room for another mask at the playhead (lanes, and the server's cap).
    fn can_add_mask(&self) -> bool {
        let (s, e) = self.new_mask_span();
        self.masks_supported
            && self.n() > 0
            && self.masks.len() < em::MAX_MASKS
            && em::masks_overlapping(&self.masks, s, e, None) < em::MAX_MASK_LANES
    }

    /// "Add mask": a box in the middle of the frame, ready to move.
    fn add_mask(&mut self, cx: &mut Context<Self>) {
        if !self.can_add_mask() {
            return;
        }
        let (start, end) = self.new_mask_span();
        let id = self.next_mask_id();
        self.masks.push(Mask { id: id.clone(), start, end, x: 0.35, y: 0.38, w: 0.3, h: 0.24 });
        self.select_mask(Some(id));
        self.emit_cuts(cx);
    }

    fn select_mask(&mut self, id: Option<String>) {
        if id.is_some() {
            self.selected = None;
        }
        self.sel_mask = id;
    }

    fn mask_mut(&mut self, id: &str) -> Option<&mut Mask> {
        self.masks.iter_mut().find(|m| m.id == id)
    }

    fn delete_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.sel_mask.take() {
            self.masks.retain(|m| m.id != id);
            self.emit_cuts(cx);
        } else if let Some(i) = self.selected.take() {
            if i < self.regions.len() {
                self.regions.remove(i);
            }
            self.emit_cuts(cx);
        }
    }

    fn clear_all(&mut self, cx: &mut Context<Self>) {
        self.regions.clear();
        self.masks.clear();
        self.selected = None;
        self.sel_mask = None;
        self.emit_cuts(cx);
    }

    fn emit_cuts(&mut self, cx: &mut Context<Self>) {
        let cuts = em::regions_to_cuts(&self.normalized(), &self.units);
        let dirty = cuts != self.saved_cuts || self.masks != self.saved_masks;
        cx.emit(EditorEvent::CutsChanged { removed: self.normalized().len(), masks: self.masks.len(), dirty });
        cx.notify();
    }

    pub fn current_cuts(&self) -> Vec<CutInterval> {
        em::regions_to_cuts(&self.normalized(), &self.units)
    }

    /// The masks to save, or `None` for a server without the masking API.
    pub fn current_masks(&self) -> Option<serde_json::Value> {
        self.masks_supported.then(|| em::masks_to_json(&self.masks, &self.units))
    }

    // ── Timeline geometry ──────────────────────────────────────────────

    fn width(&self) -> f32 {
        f32::from(self.strip.size.width).max(1.0)
    }

    fn max_zoom(&self) -> f64 {
        em::max_zoom(self.n(), self.width(), MAX_UNIT_PX)
    }

    /// Unit → px from the timeline's left edge.
    fn x_of(&self, u: f64) -> f32 {
        ((u - self.view.start) / self.view.visible(self.n()).max(1e-9)) as f32 * self.width()
    }

    /// Window x → unit (clamped to the timeline).
    fn unit_at_x(&self, x: Pixels) -> f64 {
        let frac = ((f32::from(x) - f32::from(self.strip.origin.x)) / self.width()) as f64;
        (self.view.start + frac * self.view.visible(self.n())).clamp(0.0, self.n() as f64)
    }

    fn set_view(&mut self, v: View) {
        self.view = v.clamped(self.n(), self.max_zoom());
    }

    /// Zoom by `factor` about window x `at` (or the playhead if it's in
    /// view, else the middle), optionally eased.
    fn zoom_by(&mut self, factor: f64, at: Option<Pixels>, animate: bool, cx: &mut Context<Self>) {
        let n = self.n();
        if n == 0 {
            return;
        }
        let base = self.view_anim.as_ref().map(|a| a.to).unwrap_or(self.view);
        let target_zoom = (base.zoom * factor).clamp(1.0, self.max_zoom());
        let (anchor, frac) = match at {
            Some(x) => {
                let frac = ((f32::from(x) - f32::from(self.strip.origin.x)) / self.width()).clamp(0.0, 1.0) as f64;
                (self.view.start + frac * self.view.visible(n), frac)
            }
            None => {
                let head = (self.time - self.view.start) / self.view.visible(n);
                if (0.0..=1.0).contains(&head) {
                    (self.time, head)
                } else {
                    (self.view.start + self.view.visible(n) / 2.0, 0.5)
                }
            }
        };
        let to = self.view.zoomed(n, target_zoom, anchor, frac, self.max_zoom());
        if animate && !crate::motion::reduce_motion() {
            self.view_anim = Some(ViewAnim { from: self.view, to, anchor: Some((anchor, frac)), p: Anim::from_to(0.0, 1.0, Curve::tween(260.0, QUART)) });
        } else {
            self.view_anim = None;
            self.view = to;
        }
        cx.notify();
    }

    fn zoom_fit(&mut self, cx: &mut Context<Self>) {
        let to = View::FIT;
        if crate::motion::reduce_motion() {
            self.view = to;
        } else {
            self.view_anim = Some(ViewAnim { from: self.view, to, anchor: None, p: Anim::from_to(0.0, 1.0, Curve::tween(300.0, QUART)) });
        }
        cx.notify();
    }

    /// Slide the window to start at `start`, eased.
    fn pan_to(&mut self, start: f64) {
        let to = View { start, zoom: self.view.zoom }.clamped(self.n(), self.max_zoom());
        if (to.start - self.view.start).abs() < 1e-6 {
            return;
        }
        if crate::motion::reduce_motion() {
            self.view = to;
        } else {
            self.view_anim = Some(ViewAnim { from: self.view, to, anchor: None, p: Anim::from_to(0.0, 1.0, Curve::tween(320.0, QUART)) });
        }
    }

    /// Pan by `dx` px (positive moves the content right, back in time).
    fn pan_px(&mut self, dx: f32, cx: &mut Context<Self>) {
        self.view_anim = None;
        let du = dx as f64 / self.width() as f64 * self.view.visible(self.n());
        self.set_view(View { start: self.view.start - du, zoom: self.view.zoom });
        cx.notify();
    }

    /// Keep the playhead on screen while playing: page ahead when it nears
    /// the right edge.
    fn follow(&mut self, t: f64) {
        if self.view.zoom <= 1.0 + 1e-6 || self.view_anim.is_some() {
            return;
        }
        let vis = self.view.visible(self.n());
        if t > self.view.start + vis * 0.92 || t < self.view.start {
            self.pan_to(t - vis * 0.08);
        }
    }

    /// After a keyboard seek, bring the playhead into view.
    fn reveal(&mut self, t: f64) {
        let vis = self.view.visible(self.n());
        if t < self.view.start || t > self.view.start + vis {
            self.pan_to(t - vis / 2.0);
        }
    }

    fn tick_view(&mut self) -> bool {
        let n = self.n();
        let max_zoom = self.max_zoom();
        let Some(a) = &mut self.view_anim else { return false };
        let moving = a.p.tick();
        let p = a.p.get() as f64;
        let (fz, tz) = (a.from.zoom.ln(), a.to.zoom.ln());
        let zoom = (fz + (tz - fz) * p).exp();
        let v = match a.anchor {
            Some((anchor, frac)) => View { zoom, start: anchor - frac * n as f64 / zoom },
            None => View { zoom, start: a.from.start + (a.to.start - a.from.start) * p },
        };
        let done = !moving || p >= 1.0;
        let to = a.to;
        self.view = if done { to } else { v }.clamped(n, max_zoom);
        if done {
            self.view_anim = None;
        }
        !done
    }

    fn on_scroll(&mut self, ev: &ScrollEvent, cx: &mut Context<Self>) {
        if self.n() == 0 || self.drag.is_some() {
            return;
        }
        let zoom_mod = ev.modifiers.control || ev.modifiers.platform;
        match ev.delta {
            // Trackpads (and precise wheels).
            ScrollDelta::Pixels(p) => {
                let (dx, dy) = (f32::from(p.x), f32::from(p.y));
                if zoom_mod {
                    self.zoom_by((dy as f64 * 0.01).exp(), Some(ev.position.x), false, cx);
                } else {
                    // Either axis pans: horizontal swipes, or vertical for
                    // one-axis scrolling.
                    self.pan_px(if dx.abs() >= dy.abs() { dx } else { dy }, cx);
                }
            }
            // Notched mouse wheels: up/down zooms, Shift or tilt pans.
            ScrollDelta::Lines(l) => {
                if ev.modifiers.shift || l.x.abs() > l.y.abs() {
                    let d = if l.x.abs() > l.y.abs() { l.x } else { l.y };
                    self.pan_px(d * 48.0, cx);
                } else {
                    // ~1.1× a line: X11 and Windows send 3 lines a notch.
                    self.zoom_by(1.1f64.powf(l.y as f64), Some(ev.position.x), false, cx);
                }
            }
        }
    }

    fn on_pinch(&mut self, ev: &PinchEvent, cx: &mut Context<Self>) {
        if self.n() == 0 || self.drag.is_some() {
            return;
        }
        self.zoom_by((1.0 + ev.delta as f64).max(0.2), Some(ev.position.x), false, cx);
    }

    // ── Pointer ────────────────────────────────────────────────────────

    /// A window position as normalized frame coordinates (clamped).
    fn frame_xy(&self, p: Point<Pixels>) -> (f32, f32) {
        let r = self.video_rect();
        let w = f32::from(r.size.width).max(1.0);
        let h = f32::from(r.size.height).max(1.0);
        (
            ((f32::from(p.x) - f32::from(r.origin.x)) / w).clamp(0.0, 1.0),
            ((f32::from(p.y) - f32::from(r.origin.y)) / h).clamp(0.0, 1.0),
        )
    }

    /// The picture's rect on the stage (window coordinates).
    fn video_rect(&self) -> Bounds<Pixels> {
        let aspect = self.video.as_ref().and_then(|_| self.aspect).unwrap_or(16.0 / 9.0);
        fit(self.stage, aspect)
    }

    fn on_down(&mut self, drag: Drag, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.view_anim = None;
        match &drag {
            Drag::Scrub => {
                let u = self.unit_at_x(pos.x);
                self.seek(u, cx);
            }
            Drag::Minimap { .. } | Drag::MaskSpan { .. } | Drag::MaskBox { .. } => {}
            _ => {}
        }
        self.drag = Some(drag);
        cx.notify();
    }

    fn on_move(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        let u = self.unit_at_x(pos.x);
        let n = self.n() as i64;
        let next = match drag {
            Drag::Scrub => {
                self.seek(u, cx);
                Some(Drag::Scrub)
            }
            Drag::Minimap { grab } => {
                let mw = f32::from(self.mini.size.width).max(1.0);
                let mu = ((f32::from(pos.x) - f32::from(self.mini.origin.x)) / mw) as f64 * n as f64;
                self.set_view(View { start: mu - grab, zoom: self.view.zoom });
                cx.notify();
                Some(Drag::Minimap { grab })
            }
            Drag::Maybe { down } => {
                // Past a third of a unit (or 4px), the press becomes a cut.
                let px_moved = ((u - down) / self.view.visible(self.n()) * self.width() as f64).abs();
                if (u - down).abs() < 0.34 && px_moved < 4.0 {
                    Some(Drag::Maybe { down })
                } else {
                    let a = u.min(down).floor() as i64;
                    let b = u.max(down).ceil() as i64;
                    self.regions.push(Region { start: a, end: b.max(a + 1) });
                    let index = self.regions.len() - 1;
                    self.selected = Some(index);
                    self.sel_mask = None;
                    self.emit_cuts(cx);
                    Some(Drag::Region { index, mode: if u >= down { DragMode::End } else { DragMode::Start }, grab: 0.0, anchor: down.floor() as i64 })
                }
            }
            Drag::Region { index, mode, grab, anchor } => {
                if let Some(mut r) = self.regions.get(index).copied() {
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
                    self.emit_cuts(cx);
                }
                Some(Drag::Region { index, mode, grab, anchor })
            }
            Drag::MaskSpan { id, mode, grab } => {
                self.drag_mask_span(&id, mode, grab, u, cx);
                Some(Drag::MaskSpan { id, mode, grab })
            }
            Drag::Stage { at } => {
                let moved = (f32::from(pos.x - at.x)).hypot(f32::from(pos.y - at.y));
                if moved < 4.0 || !self.can_add_mask() {
                    Some(Drag::Stage { at })
                } else {
                    // Drawing a mask: it covers the unit under the playhead.
                    let origin = self.frame_xy(at);
                    let (start, end) = self.new_mask_span();
                    let id = self.next_mask_id();
                    self.masks.push(Mask { id: id.clone(), start, end, x: origin.0, y: origin.1, w: 0.0, h: 0.0 });
                    self.select_mask(Some(id.clone()));
                    let d = Drag::MaskDraw { id, origin };
                    self.drag = Some(d);
                    return self.on_move(pos, cx);
                }
            }
            Drag::MaskDraw { id, origin } => {
                let p = self.frame_xy(pos);
                if let Some(m) = self.mask_mut(&id) {
                    m.x = origin.0.min(p.0);
                    m.y = origin.1.min(p.1);
                    m.w = (p.0 - origin.0).abs();
                    m.h = (p.1 - origin.1).abs();
                }
                cx.notify();
                Some(Drag::MaskDraw { id, origin })
            }
            Drag::MaskBox { id, grip, at, orig } => {
                let p = self.frame_xy(pos);
                let (dx, dy) = (p.0 - at.0, p.1 - at.1);
                let (ox, oy, ow, oh) = orig;
                const MIN: f32 = 0.02;
                if let Some(m) = self.mask_mut(&id) {
                    if grip == Grip::default() {
                        m.x = (ox + dx).clamp(0.0, 1.0 - ow);
                        m.y = (oy + dy).clamp(0.0, 1.0 - oh);
                    } else {
                        let (mut l, mut t, mut r, mut b) = (ox, oy, ox + ow, oy + oh);
                        if grip.l {
                            l = (ox + dx).clamp(0.0, r - MIN);
                        }
                        if grip.r {
                            r = (ox + ow + dx).clamp(l + MIN, 1.0);
                        }
                        if grip.t {
                            t = (oy + dy).clamp(0.0, b - MIN);
                        }
                        if grip.b {
                            b = (oy + oh + dy).clamp(t + MIN, 1.0);
                        }
                        m.x = l;
                        m.y = t;
                        m.w = r - l;
                        m.h = b - t;
                    }
                }
                cx.notify();
                Some(Drag::MaskBox { id, grip, at, orig })
            }
        };
        self.drag = next;
    }

    fn drag_mask_span(&mut self, id: &str, mode: DragMode, grab: f64, u: f64, cx: &mut Context<Self>) {
        let n = self.n() as f64;
        // Edges snap to unit boundaries within 6px.
        let snap_u = 6.0 / self.width() as f64 * self.view.visible(self.n());
        let snap = |v: f64| if (v - v.round()).abs() < snap_u { v.round() } else { v };
        let others: Vec<Mask> = self.masks.iter().filter(|m| m.id != id).cloned().collect();
        let Some(m) = self.masks.iter().find(|m| m.id == id).cloned() else { return };
        let min_len = 0.1;
        let (mut s, mut e) = (m.start, m.end);
        let mut seek_to = None;
        match mode {
            DragMode::Move => {
                let len = m.end - m.start;
                s = snap(u - grab).clamp(0.0, (n - len).max(0.0));
                e = s + len;
            }
            DragMode::Start => {
                s = snap(u).clamp(0.0, m.end - min_len);
                seek_to = Some(s + 0.02);
            }
            DragMode::End => {
                e = snap(u).clamp(m.start + min_len, n);
                seek_to = Some((e - 0.03).max(s));
            }
        }
        // Never more than the lane limit at once.
        if em::masks_overlapping(&others, s, e, None) >= em::MAX_MASK_LANES {
            return;
        }
        if let Some(mm) = self.mask_mut(id) {
            mm.start = s;
            mm.end = e;
        }
        if let Some(t) = seek_to {
            self.seek(t, cx);
        }
        cx.notify();
    }

    fn on_up(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Maybe { down } => {
                self.seek(down, cx);
                self.selected = None;
                self.sel_mask = None;
            }
            Drag::Region { index, .. } => {
                let dragged = self.regions.get(index).copied();
                self.regions = em::normalize(&self.regions);
                self.selected = dragged.and_then(|d| self.regions.iter().position(|r| d.start >= r.start && d.start < r.end));
                self.emit_cuts(cx);
            }
            Drag::Stage { .. } => {
                // A plain click: deselect a box, else play/pause.
                if self.sel_mask.take().is_none() {
                    self.toggle_play(cx);
                }
            }
            Drag::MaskDraw { id, .. } => {
                // Too small to mean anything: a click, not a box.
                let tiny = self.masks.iter().find(|m| m.id == id).is_none_or(|m| m.w < 0.015 || m.h < 0.015);
                if tiny {
                    self.masks.retain(|m| m.id != id);
                    self.sel_mask = None;
                }
                self.emit_cuts(cx);
            }
            Drag::MaskBox { .. } | Drag::MaskSpan { .. } => self.emit_cuts(cx),
            Drag::Scrub | Drag::Minimap { .. } => {}
        }
        self.head_scale.set_css(if self.head_hover { ed_hover(1.15) } else { 1.0 }, ed_ms(120.0), QUART);
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.saving = true;
        self.save_error = None;
        let cuts = self.current_cuts();
        let masks = self.current_masks();
        let core = app::state(cx).core.clone();
        let cfg = self.cfg(cx);
        let fut = app::tokio(cx, async move {
            let body = serde_json::to_value(&cuts).unwrap_or_default();
            core.session_set_edits(&cfg, body, masks).await?;
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

    // ── Filmstrip ──────────────────────────────────────────────────────

    fn start_filmstrip(&mut self, url: String, cx: &mut Context<Self>) {
        let (film, rx) = Filmstrip::spawn(url, (STRIP_H * 2.0) as u32);
        self.film = Some(film);
        self._film_rx = Some(cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv_async().await {
                let mut batch = vec![first];
                batch.extend(rx.try_iter());
                let alive = this.update(cx, |e, cx| {
                    for (k, img) in batch {
                        match img {
                            Some(img) => {
                                e.film_cache.insert(k, img);
                            }
                            None if k == u64::MAX => e.film_dead = true,
                            None => {}
                        }
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    /// The thumbnail cells in view: `(x, width, key)` in timeline px. Cells
    /// tile the whole (zoomed) timeline at the frame's own aspect.
    fn tile_cells(&self, tile_w: f32) -> (Vec<(f32, f32, u64)>, usize) {
        let n = self.n();
        let w = self.width();
        if n == 0 || w <= 1.0 {
            return (Vec::new(), 0);
        }
        let content_w = w as f64 * self.view.zoom;
        let count = ((content_w / tile_w as f64).ceil() as usize).max(1);
        let cell = content_w / count as f64;
        let offset = self.view.start / n as f64 * content_w;
        let i0 = (offset / cell).floor().max(0.0) as usize;
        let i1 = (((offset + w as f64) / cell).ceil() as usize).min(count);
        let duration = self.duration();
        let cells = (i0..i1)
            .map(|i| {
                let t = ((i as f64 + 0.5) / count as f64 * n as f64).min(duration - 0.05).max(0.0);
                ((i as f64 * cell - offset) as f32, cell as f32, filmstrip::key(t))
            })
            .collect();
        (cells, count)
    }

    /// Video length in seconds; one unit is one second when the stream
    /// can't tell us.
    fn duration(&self) -> f64 {
        self.video_duration.filter(|d| *d > 0.0).unwrap_or(self.n() as f64)
    }

    /// Ask the decoder for missing thumbnails once the view settles.
    fn want_tiles(&mut self, keys: &[u64], cx: &mut Context<Self>) {
        if self.film_dead || self.film.is_none() {
            return;
        }
        let missing: Vec<u64> = keys.iter().copied().filter(|k| !self.film_cache.contains_key(k) && !self.film_asked.contains(k)).collect();
        if missing.is_empty() || missing == self.film_sent {
            return;
        }
        self.film_sent = missing.clone();
        // While zooming/panning the wanted set changes every frame; only
        // ask once it holds still for a beat (nearest cached frames stand
        // in meanwhile).
        let settle = if self.film_cache.is_empty() { 0 } else { 120 };
        self._film_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(settle)).await;
            this.update(cx, |e, _| {
                // Middle out: what's under the eye first.
                let mut keys = missing;
                let mid = keys[keys.len() / 2] as i64;
                keys.sort_by_key(|k| (*k as i64 - mid).abs());
                for k in &keys {
                    e.film_asked.insert(*k);
                }
                if let Some(f) = &e.film {
                    f.request(keys);
                }
            })
            .ok();
        }));
    }

    /// Drop the thumbnails farthest from the view once the cache is full.
    /// Trimmed tiles leave GPUI's sprite atlas too: a `RenderImage` stays
    /// uploaded until `drop_image`, so just forgetting the `Arc` leaks it.
    fn trim_tiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.film_cache.len() <= TILE_CACHE {
            return;
        }
        let mid = filmstrip::key(self.view.start + self.view.visible(self.n()) / 2.0) as i64;
        let mut keys: Vec<u64> = self.film_cache.keys().copied().collect();
        keys.sort_by_key(|k| std::cmp::Reverse((*k as i64 - mid).abs()));
        for k in keys.into_iter().take(self.film_cache.len() - TILE_CACHE * 85 / 100) {
            if let Some(img) = self.film_cache.remove(&k) {
                cx.drop_image(img, Some(window));
            }
            self.film_asked.remove(&k);
        }
    }

    /// The decoded frame for `key`, else the nearest one decoded so far.
    fn tile_for(&self, key: u64) -> Option<Arc<RenderImage>> {
        if let Some(img) = self.film_cache.get(&key) {
            return Some(img.clone());
        }
        let after = self.film_cache.range(key..).next();
        let before = self.film_cache.range(..key).next_back();
        match (before, after) {
            (Some(b), Some(a)) => Some(if key - b.0 <= a.0 - key { b.1.clone() } else { a.1.clone() }),
            (Some(x), None) | (None, Some(x)) => Some(x.1.clone()),
            (None, None) => None,
        }
    }

    // ── Keys ───────────────────────────────────────────────────────────

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.platform || k.modifiers.control {
            return;
        }
        match k.key.as_str() {
            "space" | "k" => self.toggle_play(cx),
            "x" | "c" => self.cut_here(cx),
            "m" => self.add_mask(cx),
            "delete" | "backspace" => self.delete_selection(cx),
            "escape" => {
                self.selected = None;
                self.sel_mask = None;
            }
            "=" | "+" => self.zoom_by(1.5, None, true, cx),
            "-" => self.zoom_by(1.0 / 1.5, None, true, cx),
            "0" => self.zoom_fit(cx),
            "left" | "right" => {
                let step = if k.modifiers.shift { 10.0 } else { 1.0 };
                let t = self.time + if k.key == "left" { -step } else { step };
                self.seek(t, cx);
                self.reveal(self.time);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// The largest rect of `aspect` centred in `b`.
fn fit(b: Bounds<Pixels>, aspect: f32) -> Bounds<Pixels> {
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    if w <= 0.0 || h <= 0.0 || aspect <= 0.0 {
        return b;
    }
    let (fw, fh) = if w / h > aspect { (h * aspect, h) } else { (w, w / aspect) };
    Bounds {
        origin: point(b.origin.x + px((w - fw) / 2.0), b.origin.y + px((h - fh) / 2.0)),
        size: gpui_kit::gpui::size(px(fw), px(fh)),
    }
}

fn corners(tl: f32, tr: f32, br: f32, bl: f32) -> Corners<Pixels> {
    Corners { top_left: px(tl), top_right: px(tr), bottom_right: px(br), bottom_left: px(bl) }
}

/// Label/minor steps (units) for the ruler at `px_per_unit`: labels at
/// least ~110px apart, minor ticks at least ~12px.
fn ruler_steps(px_per_unit: f64) -> (f64, f64) {
    const STEPS: [f64; 17] = [0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 180.0, 360.0, 720.0, 1440.0, 2880.0, 5760.0, 11520.0];
    let label = STEPS.iter().copied().find(|s| s * px_per_unit >= 110.0).unwrap_or(11520.0);
    let minor = [5.0, 4.0, 2.0, 1.0]
        .iter()
        .map(|d| label / d)
        .find(|m| m * px_per_unit >= 12.0)
        .unwrap_or(label);
    (label, minor)
}

/// Paint a mask's "pixelated" preview: a mosaic of greys, so it reads as
/// hidden without sampling the video.
fn paint_mosaic(b: Bounds<Pixels>, window: &mut Window) {
    let cell = 9.0f32;
    let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    let cols = (w / cell).ceil() as i32;
    let rows = (h / cell).ceil() as i32;
    for j in 0..rows {
        for i in 0..cols {
            // Cheap hash → a stable grey per cell.
            let hsh = ((i * 73_856_093) ^ (j * 19_349_663)).wrapping_mul(83_492_791) as u32;
            let l = 0.30 + (hsh % 1000) as f32 / 1000.0 * 0.22;
            let cx0 = x0 + i as f32 * cell;
            let cy0 = y0 + j as f32 * cell;
            let cb = Bounds {
                origin: point(px(cx0), px(cy0)),
                size: gpui_kit::gpui::size(px(cell.min(x0 + w - cx0)), px(cell.min(y0 + h - cy0))),
            };
            window.paint_quad(gpui_kit::gpui::fill(cb, gpui_kit::gpui::hsla(0.6, 0.06, l, 0.94)));
        }
    }
}

/// 45° hatch: 5px stripe, 5px gap, clipped to `clip`.
fn paint_hatch(b: Bounds<Pixels>, clip: Bounds<Pixels>, stripe: Hsla, window: &mut Window) {
    window.with_content_mask(Some(gpui_kit::gpui::ContentMask { bounds: clip }), |window| {
        let h = f32::from(b.size.height);
        let x0 = f32::from(b.origin.x) - h;
        let x1 = f32::from(b.origin.x) + f32::from(b.size.width);
        let y0 = f32::from(b.origin.y);
        let step = 10.0 * std::f32::consts::SQRT_2;
        let mut x = x0 - x0.rem_euclid(step);
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
            x += step;
        }
    });
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
        if let Some(v) = &self.video {
            let v = v.read(cx);
            self.aspect = v.aspect();
            self.video_duration = (v.duration > 0.0).then_some(v.duration);
        }
        let n = self.n();
        let normalized = self.normalized();
        let in_cut = em::region_at_time(self.time, &normalized).is_some();
        if self.cut_ring.target() != if in_cut { 1.0 } else { 0.0 } {
            self.cut_ring.set(if in_cut { 1.0 } else { 0.0 }, Curve::tween(120.0, Ease::EASE_OUT));
        }
        let tip_on = self.head_hover || matches!(self.drag, Some(Drag::Scrub));
        if self.head_tip.target() != if tip_on { 1.0 } else { 0.0 } {
            self.head_tip.set_css(if tip_on { 1.0 } else { 0.0 }, ed_ms(140.0), QUART);
        }
        let lanes = em::assign_lanes(&self.masks);
        let lane_count = if self.masks.is_empty() { 0 } else { lanes.iter().copied().max().unwrap_or(0) + 1 };
        let lanes_target = if lane_count == 0 { 0.0 } else { 6.0 + lane_count as f32 * (LANE_H + LANE_GAP) - LANE_GAP };
        if self.lane_h.target() != lanes_target {
            self.lane_h.set_css(lanes_target, ed_ms(200.0), QUART);
        }
        // Fade an edge only while there's more timeline past it.
        let more_l = self.view.start > 1e-3;
        let more_r = self.view.start + self.view.visible(n) < n as f64 - 1e-3;
        for (a, on) in [(&mut self.fade_l, more_l), (&mut self.fade_r, more_r)] {
            let to = if on { EDGE_FADE } else { 0.0 };
            if a.target() != to {
                a.set_css(to, ed_ms(180.0), QUART);
            }
        }
        let mut moving = self.tick_view();
        for a in [
            &mut self.cut_ring,
            &mut self.region_hover,
            &mut self.grip,
            &mut self.head_scale,
            &mut self.head_tip,
            &mut self.lane_h,
            &mut self.fade_l,
            &mut self.fade_r,
        ] {
            moving |= a.tick();
        }
        if moving {
            window.request_animation_frame();
        }

        // ── Stage ──
        let stage_probe = cx.entity();
        let vr = self.video_rect();
        let so = self.stage.origin;
        let rel = |b: Bounds<Pixels>| (f32::from(b.origin.x - so.x), f32::from(b.origin.y - so.y), f32::from(b.size.width), f32::from(b.size.height));
        let (vx, vy, vw, vh) = rel(vr);
        let have_stage = f32::from(self.stage.size.width) > 0.0;
        let ring_o = self.cut_ring.get();
        let video_box = |d: gpui_kit::gpui::Div| if have_stage { d.absolute().left(px(vx)).top(px(vy)).w(px(vw)).h(px(vh)) } else { d.absolute().inset_0() };
        let drawing = matches!(self.drag, Some(Drag::MaskDraw { .. }));
        let mut overlay = video_box(div()).id("editor-overlay");
        for m in self.masks.iter().filter(|m| m.active_at(self.time) || matches!(&self.drag, Some(Drag::MaskDraw { id, .. }) if *id == m.id)) {
            let lane = self.masks.iter().position(|x| x.id == m.id).map(|i| lanes[i]).unwrap_or(0);
            overlay = overlay.child(self.render_mask_box(m, lane, vw, vh, &t, cx));
        }
        if ring_o > 0.001 {
            overlay = overlay.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(STAGE_R))
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
            );
        }
        let stage = div()
            .id("editor-stage")
            .flex_1()
            .min_h(px(0.0))
            .relative()
            .when(self.masks_supported, |d| d.cursor(if drawing { CursorStyle::Crosshair } else { CursorStyle::Arrow }))
            .child(canvas(move |b, _, cx| stage_probe.update(cx, |e, _| e.stage = b), |_, _, _, _| {}).absolute().inset_0())
            .children(self.video.clone().map(|v| {
                video_box(div())
                    .rounded(px(STAGE_R))
                    .shadow(vec![theme::sh(0.0, 8.0, 28.0, -6.0, theme::black(if t.dark { 0.55 } else { 0.25 }))])
                    .child(v)
            }))
            .child(overlay)
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| e.on_down(Drag::Stage { at: ev.position }, ev.position, cx)));

        // ── Timeline ──
        let w = self.width();
        let have_w = f32::from(self.strip.size.width) > 1.0;
        let vis = self.view.visible(n).max(1e-9);
        let ppu = w as f64 / vis;

        // Ruler: timecodes over dense ticks; owns scrubbing.
        let (label_step, minor) = ruler_steps(ppu);
        let mut ruler = div()
            .id("editor-ruler")
            .relative()
            .h(px(RULER_H))
            .overflow_hidden()
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| e.on_down(Drag::Scrub, ev.position, cx)));
        if have_w && n > 0 {
            let from = (self.view.start / minor).floor() * minor;
            let mut u = from;
            let tnum = gpui_kit::gpui::FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
            while u <= self.view.start + vis + minor {
                let x = self.x_of(u);
                let major = ((u / label_step) - (u / label_step).round()).abs() < 1e-6;
                if (-60.0..w + 60.0).contains(&x) && u <= n as f64 + 1e-6 {
                    if major {
                        let lw = 76.0;
                        let lx = (x - lw / 2.0).clamp(0.0, w - lw);
                        let align_left = lx <= 0.0;
                        let align_right = lx >= w - lw;
                        ruler = ruler.child(
                            div()
                                .absolute()
                                .top_0()
                                .left(px(lx))
                                .w(px(lw))
                                .flex()
                                .map(|d| if align_left { d.justify_start() } else if align_right { d.justify_end() } else { d.justify_center() })
                                .ts(12.0)
                                .w500()
                                .font_features(tnum.clone())
                                .text_color(t.text_tertiary)
                                .whitespace_nowrap()
                                .child(em::timecode(u)),
                        );
                    }
                    ruler = ruler.child(
                        div()
                            .absolute()
                            .bottom(px(3.0))
                            .left(px(x.clamp(0.0, w - 1.0) - 0.5))
                            .w(px(1.0))
                            .h(px(if major { 9.0 } else { 5.0 }))
                            .rounded(px(0.5))
                            .bg(if major { t.text_tertiary } else { t.text_quaternary }),
                    );
                }
                u += minor;
            }
        }

        // Filmstrip.
        let aspect = self.aspect.unwrap_or(16.0 / 9.0);
        let tile_w = (STRIP_H * aspect).round().max(24.0);
        let (cells, count) = self.tile_cells(tile_w);
        let keys: Vec<u64> = cells.iter().map(|c| c.2).collect();
        // Overview tiles first, so zooming out always has something.
        if have_w {
            self.want_tiles(&keys, cx);
            self.trim_tiles(window, cx);
        }
        let painted: Vec<(f32, f32, bool, bool, Option<Arc<RenderImage>>)> = cells
            .iter()
            .map(|&(x, cw, k)| {
                let i = ((x as f64 + self.view.start / n.max(1) as f64 * w as f64 * self.view.zoom) / cw as f64).round() as usize;
                (x, cw, i == 0, i + 1 == count, self.tile_for(k))
            })
            .collect();
        let probe = cx.entity();
        let track = t.track;
        let gaps: Vec<f32> = em::gap_indices(&self.units).into_iter().map(|g| self.x_of(g as f64)).filter(|x| (0.0..=w).contains(x)).collect();
        let gap_c = t.text_quaternary;
        let mut strip = div()
            .id("editor-strip")
            .relative()
            .h(px(STRIP_H))
            .cursor(CursorStyle::Crosshair)
            .child(
                canvas(move |b, _, cx| probe.update(cx, |e, _| e.strip = b), move |b, _, window, _| {
                    let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
                    let h = f32::from(b.size.height);
                    window.paint_quad(gpui_kit::gpui::fill(b, track).corner_radii(px(STRIP_R)));
                    for (x, cw, first, last, img) in &painted {
                        let Some(img) = img else { continue };
                        // The cell, clipped to the strip; rounded at the
                        // timeline's two ends.
                        let l = (bx + x).max(bx);
                        let r = (bx + x + cw).min(bx + f32::from(b.size.width));
                        if r <= l {
                            continue;
                        }
                        let cell = Bounds { origin: point(px(l), b.origin.y), size: gpui_kit::gpui::size(px(r - l), b.size.height) };
                        let rl = if *first { STRIP_R } else { 0.0 };
                        let rr = if *last { STRIP_R } else { 0.0 };
                        // Cover: fill the cell, crop the sides.
                        let s = img.size(0);
                        let (iw, ih) = (s.width.0 as f32, s.height.0 as f32);
                        let scale = (cw / iw).max(h / ih);
                        let (dw, dh) = (iw * scale, ih * scale);
                        let ib = Bounds {
                            origin: point(px(bx + x + (cw - dw) / 2.0), px(by + (h - dh) / 2.0)),
                            size: gpui_kit::gpui::size(px(dw), px(dh)),
                        };
                        let _ = window.paint_image(cell, ib, corners(rl, rr, rr, rl), img.clone(), 0, false);
                    }
                    // Recording gaps: dashed lines.
                    for gx in &gaps {
                        let mut y = by;
                        while y < by + h {
                            let seg = Bounds { origin: point(px(bx + gx - 1.0), px(y)), size: gpui_kit::gpui::size(px(2.0), px(3.0f32.min(by + h - y))) };
                            window.paint_quad(gpui_kit::gpui::fill(seg, gap_c));
                            y += 6.0;
                        }
                    }
                })
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                let u = e.unit_at_x(ev.position.x);
                e.on_down(Drag::Maybe { down: u }, ev.position, cx)
            }));

        // Cut regions: red hatch over dimmed frames, white grips.
        let rh = self.region_hover.get();
        let grip_scale = self.grip.get();
        for (i, r) in self.regions.clone().into_iter().enumerate() {
            let left = self.x_of(r.start as f64);
            let right = self.x_of(r.end as f64);
            if right < -12.0 || left > w + 12.0 || !have_w {
                continue;
            }
            let rw = right - left;
            // Draw only the part in view; grips only on edges in view.
            let (vl, vr) = (left.max(0.0), right.min(w));
            let selected = self.selected == Some(i);
            let hovered = self.hover_region == Some(i);
            let fill = if hovered { theme::mix(t.cut_fill, t.cut_fill_hover, rh) } else { t.cut_fill };
            let stripe = t.cut_stripe;
            let border = t.cut_border;
            let ring = if selected { 4.0 } else { 3.0 };
            let label = (vr - vl >= 52.0).then(|| em::span_label((r.end - r.start) as f64));
            let (lo, ro) = (left - vl, right - vl);
            let handle = |start: bool, cx: &mut Context<Self>| {
                let hov = self.hover_handle == Some((i, start));
                let gs = if hov { grip_scale } else { 1.0 };
                div()
                    .id(("handle", i * 2 + start as usize))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .w(px(20.0))
                    .left(px(if start { lo } else { ro } - 10.0))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(10.0 * gs.min(1.2)))
                            .h(px(30.0 * gs.min(1.15)))
                            .rounded(px(5.0))
                            .bg(theme::hex(0xffffff))
                            .shadow(vec![theme::sh(0.0, 1.0, 4.0, 0.0, theme::black(0.4))])
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().w(px(2.0)).h(px(12.0)).rounded(px(1.0)).bg(theme::hexa(0x1f2937, 0.75))),
                    )
                    .on_hover(cx.listener(move |e, h: &bool, _, cx| {
                        if *h {
                            e.hover_handle = Some((i, start));
                            e.grip.jump(1.0);
                            e.grip.set_css(ed_hover(1.15), ed_ms(140.0), QUART);
                        } else if e.hover_handle == Some((i, start)) {
                            e.grip.set_css(1.0, ed_ms(140.0), QUART);
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let Some(r) = e.regions.get(i).copied() else { return };
                        e.selected = Some(i);
                        e.sel_mask = None;
                        let grab = e.unit_at_x(ev.position.x) - r.start as f64;
                        e.on_down(
                            Drag::Region { index: i, mode: if start { DragMode::Start } else { DragMode::End }, grab, anchor: r.start },
                            ev.position,
                            cx,
                        );
                    }))
            };
            let h_start = (left >= -10.0).then(|| handle(true, cx));
            let h_end = (right <= w + 10.0).then(|| handle(false, cx));
            strip = strip.child(
                div()
                    .id(("region", i))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(vl))
                    .w(px((vr - vl).max(2.0)))
                    .cursor(CursorStyle::OpenHand)
                    .child(
                        canvas(|_, _, _| (), move |b, _, window, _| {
                            // The whole region, clipped to the part in view.
                            let full = Bounds {
                                origin: point(b.origin.x + px(lo), b.origin.y),
                                size: gpui_kit::gpui::size(px(rw.max(2.0)), b.size.height),
                            };
                            window.with_content_mask(Some(gpui_kit::gpui::ContentMask { bounds: b }), |window| {
                                let rr = px(8.0);
                                window.paint_quad(gpui_kit::gpui::fill(full, theme::black(0.45)).corner_radii(rr));
                                window.paint_quad(gpui_kit::gpui::fill(full, fill).corner_radii(rr));
                                paint_hatch(full, paint::inset(full, 1.0), stripe, window);
                                window.paint_quad(gpui_kit::gpui::quad(full, rr, gpui_kit::gpui::transparent_black(), px(ring), border, Default::default()));
                            });
                        })
                        .absolute()
                        .inset_0(),
                    )
                    .children(label.map(|l| {
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .ts(14.0)
                            .w700()
                            .text_color(theme::hex(0xffffff))
                            .child(l)
                    }))
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
                        e.sel_mask = None;
                        let grab = e.unit_at_x(ev.position.x) - r.start as f64;
                        e.on_down(Drag::Region { index: i, mode: DragMode::Move, grab, anchor: r.start }, ev.position, cx);
                    }))
                    .children(h_start)
                    .children(h_end),
            );
        }

        // Mask lanes.
        let lanes_h = self.lane_h.get();
        // `lanes_h` includes a 6px gap above the first lane.
        let mut lanes_el = div().relative().h(px(lanes_h)).overflow_hidden();
        if have_w {
            for (mi, m) in self.masks.clone().into_iter().enumerate() {
                let lane = lanes[mi];
                let left = self.x_of(m.start);
                let right = self.x_of(m.end);
                if right < -10.0 || left > w + 10.0 {
                    continue;
                }
                lanes_el = lanes_el.child(self.render_mask_bar(&m, lane, left, right, w, cx));
            }
        }

        // Playhead: a pin on the ruler, a line through strip and lanes.
        let hs = self.head_scale.get();
        let head_x = self.x_of(self.time.min(n as f64));
        let head_in = have_w && n > 0 && (-1.0..=w + 1.0).contains(&head_x);
        let tip_o = self.head_tip.get();
        let cur_unit = em::unit_at_time(self.time, n);
        let clock = self.units.get(cur_unit as usize).map(|u| crate::fmt::clock_time_ms(em::parse_ms(&u.captured_at))).unwrap_or_default();
        let line_h = STRIP_H + lanes_h + 4.0;
        let head_color = t.text_primary;
        let playhead = head_in.then(|| {
            div()
                .absolute()
                .top_0()
                .left(px(head_x))
                .w(px(0.0))
                .h(px(RULER_H + line_h))
                .child(
                    div()
                        .absolute()
                        .top(px(RULER_H - 2.0))
                        .left(px(-1.0))
                        .w(px(2.0))
                        .h(px(line_h))
                        .rounded(px(1.0))
                        .bg(head_color)
                        .shadow(vec![theme::sh(0.0, 0.0, 3.0, 0.0, theme::black(0.45))]),
                )
                .child(
                    div()
                        .id("playhead-hit")
                        .absolute()
                        .top(px(RULER_H - 22.0))
                        .left(px(-14.0))
                        .w(px(28.0))
                        .h(px(26.0))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .child(
                            canvas(|_, _, _| (), move |b, _, window, _| {
                                // A rounded tab with a point, like a map pin.
                                let c = paint::bounds_center(b);
                                let (pw, ph) = (14.0 * hs, 15.0 * hs);
                                let x0 = c.x - pw / 2.0;
                                let y1 = f32::from(b.origin.y) + f32::from(b.size.height) - 2.0;
                                let y0 = y1 - ph;
                                let mut p = PathBuilder::fill();
                                let r = 4.0 * hs;
                                p.move_to(point(px(x0 + r), px(y0)));
                                p.line_to(point(px(x0 + pw - r), px(y0)));
                                p.curve_to(point(px(x0 + pw), px(y0 + r)), point(px(x0 + pw), px(y0)));
                                p.line_to(point(px(x0 + pw), px(y0 + ph * 0.58)));
                                p.line_to(point(px(c.x), px(y1)));
                                p.line_to(point(px(x0), px(y0 + ph * 0.58)));
                                p.line_to(point(px(x0), px(y0 + r)));
                                p.curve_to(point(px(x0 + r), px(y0)), point(px(x0), px(y0)));
                                p.close();
                                if let Ok(p) = p.build() {
                                    window.paint_path(p, head_color);
                                }
                            })
                            .absolute()
                            .inset_0(),
                        )
                        .on_hover(cx.listener(|e, h: &bool, _, cx| {
                            e.head_hover = *h;
                            if e.drag.is_none() {
                                e.head_scale.set_css(if *h { ed_hover(1.15) } else { 1.0 }, ed_ms(120.0), QUART);
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            e.head_scale.set_css(ed_hover(1.08), ed_ms(120.0), QUART);
                            e.on_down(Drag::Scrub, ev.position, cx)
                        })),
                )
                // While scrubbing: what time this frame was recorded.
                .when(tip_o > 0.01, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(-30.0 + 4.0 * (1.0 - tip_o)))
                            .left(px(-60.0))
                            .w(px(120.0))
                            .flex()
                            .justify_center()
                            .opacity(tip_o)
                            .child(
                                div()
                                    .px(px(8.0))
                                    .py(px(3.0))
                                    .rounded(px(6.0))
                                    .bg(t.popover_bg)
                                    .border_1()
                                    .border_color(t.popover_border)
                                    .shadow(vec![theme::sh(0.0, 4.0, 12.0, 0.0, theme::black(0.3))])
                                    .ts(12.0)
                                    .w600()
                                    .text_color(t.text_primary)
                                    .whitespace_nowrap()
                                    .child(format!("{} · {clock}", em::timecode(self.time))),
                            ),
                    )
                })
        });

        // Overview bar: where the view sits; drag it to pan.
        let zoomed = self.view.zoom > 1.0 + 1e-3;
        let mini_probe = cx.entity();
        let mini_marks: Vec<(f32, f32)> = normalized.iter().map(|r| (r.start as f32 / n.max(1) as f32, r.end as f32 / n.max(1) as f32)).collect();
        let (thumb_l, thumb_w) = ((self.view.start / n.max(1) as f64) as f32, (vis / n.max(1) as f64) as f32);
        let head_f = (self.time / n.max(1) as f64) as f32;
        let (mark_c, thumb_c, grip_c, head_c) = (
            theme::mix(t.cut_border, t.track, 0.35),
            if zoomed { t.text_primary } else { t.text_primary.opacity(0.14) },
            t.bg_body.opacity(0.7),
            t.text_secondary,
        );
        let minimap = div()
            .id("editor-minimap")
            .relative()
            .mt(px(12.0))
            .h(px(MINI_H))
            .when(zoomed, |d| d.cursor(CursorStyle::OpenHand))
            .child(
                canvas(move |b, _, cx| mini_probe.update(cx, |e, _| e.mini = b), move |b, _, window, _| {
                    let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
                    let (bw, bh) = (f32::from(b.size.width), f32::from(b.size.height));
                    window.paint_quad(gpui_kit::gpui::fill(b, track).corner_radii(px(bh / 2.0)));
                    for (s, e) in &mini_marks {
                        let mb = Bounds { origin: point(px(bx + s * bw), px(by + 3.0)), size: gpui_kit::gpui::size(px(((e - s) * bw).max(bh - 6.0)), px(bh - 6.0)) };
                        window.paint_quad(gpui_kit::gpui::fill(mb, mark_c).corner_radii(px((bh - 6.0) / 2.0)));
                    }
                    let tw = (thumb_w * bw).max(28.0).min(bw);
                    let tl = (thumb_l * bw).clamp(0.0, bw - tw);
                    let tb = Bounds { origin: point(px(bx + tl), b.origin.y), size: gpui_kit::gpui::size(px(tw), b.size.height) };
                    window.paint_quad(gpui_kit::gpui::fill(tb, thumb_c).corner_radii(px(bh / 2.0)));
                    if zoomed && tw > 24.0 {
                        let cxm = bx + tl + tw / 2.0;
                        for k in -1..=1 {
                            let gb = Bounds { origin: point(px(cxm + k as f32 * 4.0 - 0.75), px(by + 3.0)), size: gpui_kit::gpui::size(px(1.5), px(bh - 6.0)) };
                            window.paint_quad(gpui_kit::gpui::fill(gb, grip_c).corner_radii(px(0.75)));
                        }
                    }
                    let hx = bx + head_f * bw;
                    let hb = Bounds { origin: point(px(hx - 1.0), px(by)), size: gpui_kit::gpui::size(px(2.0), b.size.height) };
                    window.paint_quad(gpui_kit::gpui::fill(hb, head_c).corner_radii(px(1.0)));
                })
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                if e.view.zoom <= 1.0 + 1e-3 {
                    return;
                }
                let n = e.n() as f64;
                let mw = f32::from(e.mini.size.width).max(1.0);
                let mu = ((f32::from(ev.position.x) - f32::from(e.mini.origin.x)) / mw) as f64 * n;
                let vis = e.view.visible(e.n());
                // On the thumb: grab it where pressed. Elsewhere: centre
                // the view there, then drag from the middle.
                let grab = if mu >= e.view.start && mu <= e.view.start + vis {
                    mu - e.view.start
                } else {
                    e.set_view(View { start: mu - vis / 2.0, zoom: e.view.zoom });
                    vis / 2.0
                };
                e.on_down(Drag::Minimap { grab }, ev.position, cx);
            }));

        // While a drag is live, follow the pointer anywhere in the window:
        // element mouse-move listeners only fire while hovered, so drifting
        // off the thin strip froze the drag until the pointer came back.
        let capture = self.drag.is_some().then(|| {
            let (mv, up) = (cx.entity(), cx.entity());
            canvas(|_, _, _| (), move |_, _, window, _| {
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        mv.update(cx, |e, cx| e.on_move(ev.position, cx));
                    }
                });
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                        up.update(cx, |e, cx| e.on_up(cx));
                    }
                });
            })
            .absolute()
            .size_0()
        });

        // Ruler, strip, lanes and playhead scroll together, so they fade
        // together at whichever edge has more timeline past it.
        let track_area = div().relative().child(ruler).child(strip).child(lanes_el).children(playhead);
        let timeline = div()
            .id("editor-timeline")
            .relative()
            .on_scroll(cx.listener(|e, ev: &ScrollEvent, _, cx| e.on_scroll(ev, cx)))
            .on_pinch(cx.listener(|e, ev: &PinchEvent, _, cx| e.on_pinch(ev, cx)))
            .child(crate::ui::primitives::edge_faded_x(track_area, self.fade_l.get(), self.fade_r.get()))
            .child(minimap)
            .children(capture);

        // ── Toolbar ──
        let cuts = em::regions_to_cuts(&normalized, &self.units);
        let removed = em::count_cut_units(&self.units, &cuts);
        let kept = n - removed;
        let all_cut = n > 0 && kept == 0;
        let has_selection = self.selected.is_some() || self.sel_mask.is_some();
        let has_edits = !normalized.is_empty() || !self.masks.is_empty();
        let max_zoom = self.max_zoom();
        let tool = |id: &'static str| {
            Button::empty(id)
                .variant(Variant::Secondary)
                .radius(10.0)
                .text_color(t.text_primary)
                .border(t.border_hover)
        };
        let icon_btn = |id: &'static str, icon: Icon, tip: &'static str| tool(id).fixed(CTRL, CTRL).padding(0.0, 0.0).tooltip(tip).child(icon.el(17.0, t.text_primary));
        let text_btn = |id: &'static str, icon: Icon, label: &'static str| {
            tool(id).padding(10.0, 16.0).font_size(14.0).gap(8.0).child(icon.el(16.0, t.text_primary)).child(label)
        };
        let (e_play, e_in, e_out, e_cut, e_mask, e_del, e_clear, e_pub) =
            (cx.entity(), cx.entity(), cx.entity(), cx.entity(), cx.entity(), cx.entity(), cx.entity(), cx.entity());
        let can_mask = self.can_add_mask();
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .mt(px(6.0))
            .child(
                icon_btn("editor-play", if self.playing { Icon::VpPause } else { Icon::VpPlay }, if self.playing { "Pause (Space)" } else { "Play (Space)" })
                    .on_click(move |_, _, cx| e_play.update(cx, |e, cx| e.toggle_play(cx))),
            )
            .child(
                icon_btn("editor-zoom-in", Icon::ZoomIn, "Zoom in (=)")
                    .disabled(self.view.zoom >= max_zoom - 1e-3)
                    .on_click(move |_, _, cx| e_in.update(cx, |e, cx| e.zoom_by(1.6, None, true, cx))),
            )
            .child(
                icon_btn("editor-zoom-out", Icon::ZoomOut, "Zoom out (−)")
                    .disabled(!zoomed)
                    .on_click(move |_, _, cx| e_out.update(cx, |e, cx| e.zoom_by(1.0 / 1.6, None, true, cx))),
            )
            .child(div().w(px(4.0)))
            .child(
                text_btn("editor-add-cut", Icon::Scissors, "Add cut")
                    .tooltip("Remove the minute under the playhead (X). Or drag across the filmstrip.")
                    .on_click(move |_, _, cx| e_cut.update(cx, |e, cx| e.cut_here(cx))),
            )
            .when(self.masks_supported, |d| {
                d.child(
                    text_btn("editor-add-mask", Icon::Selection, "Add mask")
                        .disabled(!can_mask)
                        .tooltip(if can_mask {
                            "Pixelate part of the screen (M). Or drag a box on the video."
                        } else {
                            "Up to 3 masks can overlap at once"
                        })
                        .on_click(move |_, _, cx| e_mask.update(cx, |e, cx| e.add_mask(cx))),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .ml(px(10.0))
                    .ts(14.0)
                    .text_color(t.text_secondary)
                    .whitespace_nowrap()
                    .child(Icon::ClockFill.el(16.0, t.text_secondary))
                    .child(div().flex().w700().text_color(t.text_primary).child(minutes_flow("kept", kept as f64, t.text_primary)))
                    .child("after cuts")
                    .when(removed > 0, |d| {
                        d.child(div().flex().w600().text_color(t.cut_border).child("−").child(minutes_flow("removed", removed as f64, t.cut_border)))
                    }),
            )
            .child(div().flex_1().min_w(px(12.0)))
            .when(has_selection, |d| {
                d.child(
                    tool("editor-delete")
                        .padding(10.0, 14.0)
                        .font_size(14.0)
                        .gap(7.0)
                        .tooltip("Delete (⌫)")
                        .child(Icon::TrashBold.el(15.0, t.text_primary))
                        .child(if self.sel_mask.is_some() { "Delete mask" } else { "Remove cut" })
                        .on_click(move |_, _, cx| e_del.update(cx, |e, cx| e.delete_selection(cx))),
                )
            })
            .when(has_edits && !has_selection, |d| {
                d.child(
                    Button::new("editor-clear", "Clear all")
                        .variant(Variant::Ghost)
                        .radius(10.0)
                        .padding(10.0, 14.0)
                        .font_size(14.0)
                        .on_click(move |_, _, cx| e_clear.update(cx, |e, cx| e.clear_all(cx))),
                )
            })
            .child({
                let mut b = Button::new("editor-publish", if self.saving { "Publishing…" } else { "Publish" })
                    .size(BtnSize::Lg)
                    .radius(10.0)
                    .padding(10.0, 26.0)
                    .font_size(14.0)
                    .loading(self.saving)
                    .disabled(all_cut)
                    .on_click(move |_, _, cx| e_pub.update(cx, |e, cx| e.save(cx)));
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
            .gap(px(14.0))
            .child(stage)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(timeline)
                    .child(toolbar)
                    .when_some(self.save_error.clone(), |d, err| d.child(div().mt(px(8.0)).child(ErrorBanner::new("save-error", err).title("Couldn't save your edits")))),
            )
            .into_any_element()
    }
}

impl TimelapseEditor {
    /// A mask's box on the video: mosaic fill, lane-coloured border, and
    /// eight grips when selected.
    fn render_mask_box(&self, m: &Mask, lane: usize, vw: f32, vh: f32, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let color = lane_color(lane);
        let selected = self.sel_mask.as_deref() == Some(m.id.as_str());
        let (x, y, w, h) = (m.x * vw, m.y * vh, (m.w * vw).max(1.0), (m.h * vh).max(1.0));
        let id = m.id.clone();
        let mut el = div()
            .id(SharedString::from(format!("mask-box-{}", m.id)))
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
            .cursor(CursorStyle::OpenHand)
            .child(
                canvas(|_, _, _| (), move |b, _, window, _| {
                    window.with_content_mask(Some(gpui_kit::gpui::ContentMask { bounds: b }), |window| paint_mosaic(b, window));
                    window.paint_quad(gpui_kit::gpui::quad(b, px(3.0), gpui_kit::gpui::transparent_black(), px(if selected { 2.0 } else { 1.5 }), color, Default::default()));
                })
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let Some(m) = e.masks.iter().find(|m| m.id == id).cloned() else { return };
                e.select_mask(Some(id.clone()));
                let at = e.frame_xy(ev.position);
                e.on_down(Drag::MaskBox { id: id.clone(), grip: Grip::default(), at, orig: (m.x, m.y, m.w, m.h) }, ev.position, cx);
            }));
        if selected {
            let grips: [(Grip, f32, f32, CursorStyle); 8] = [
                (Grip { l: true, t: true, ..Default::default() }, 0.0, 0.0, CursorStyle::ResizeUpLeftDownRight),
                (Grip { t: true, ..Default::default() }, 0.5, 0.0, CursorStyle::ResizeUpDown),
                (Grip { r: true, t: true, ..Default::default() }, 1.0, 0.0, CursorStyle::ResizeUpRightDownLeft),
                (Grip { r: true, ..Default::default() }, 1.0, 0.5, CursorStyle::ResizeLeftRight),
                (Grip { r: true, b: true, ..Default::default() }, 1.0, 1.0, CursorStyle::ResizeUpLeftDownRight),
                (Grip { b: true, ..Default::default() }, 0.5, 1.0, CursorStyle::ResizeUpDown),
                (Grip { l: true, b: true, ..Default::default() }, 0.0, 1.0, CursorStyle::ResizeUpRightDownLeft),
                (Grip { l: true, ..Default::default() }, 0.0, 0.5, CursorStyle::ResizeLeftRight),
            ];
            for (gi, (grip, fx, fy, cursor)) in grips.into_iter().enumerate() {
                let id = m.id.clone();
                el = el.child(
                    div()
                        .id(SharedString::from(format!("mask-grip-{}-{gi}", m.id)))
                        .absolute()
                        .left(px(fx * w - 9.0))
                        .top(px(fy * h - 9.0))
                        .size(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor(cursor)
                        .child(div().size(px(10.0)).rounded(px(3.0)).bg(theme::hex(0xffffff)).border(px(1.5)).border_color(color).shadow(vec![theme::sh(0.0, 1.0, 3.0, 0.0, theme::black(0.4))]))
                        .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let Some(m) = e.masks.iter().find(|m| m.id == id).cloned() else { return };
                            let at = e.frame_xy(ev.position);
                            e.on_down(Drag::MaskBox { id: id.clone(), grip, at, orig: (m.x, m.y, m.w, m.h) }, ev.position, cx);
                        })),
                );
            }
        }
        let _ = t;
        el.into_any_element()
    }

    /// A mask's span on its lane.
    fn render_mask_bar(&self, m: &Mask, lane: usize, left: f32, right: f32, w: f32, cx: &mut Context<Self>) -> AnyElement {
        let color = lane_color(lane);
        let selected = self.sel_mask.as_deref() == Some(m.id.as_str());
        let hovered = self.hover_mask.as_deref() == Some(m.id.as_str());
        let top = 6.0 + lane as f32 * (LANE_H + LANE_GAP);
        // Only the part in view; rounded ends only where the span ends.
        let (vl, vr) = (left.max(-1.0), right.min(w + 1.0));
        let width = (vr - vl).max(6.0);
        let (lo, ro) = (left - vl, right - vl);
        let fill = color.opacity(if selected { 0.6 } else if hovered { 0.42 } else { 0.3 });
        let grip = |start: bool, cx: &mut Context<Self>| {
            let id = m.id.clone();
            div()
                .id(SharedString::from(format!("mask-span-{}-{}", m.id, start as u8)))
                .absolute()
                .top_0()
                .bottom_0()
                .w(px(14.0))
                // The hit area straddles the edge; the grip sits just inside.
                .left(px(if start { lo } else { ro } - 7.0))
                .cursor(CursorStyle::ResizeLeftRight)
                .flex()
                .items_center()
                .map(|d| if start { d.justify_end().pr(px(2.0)) } else { d.justify_start().pl(px(2.0)) })
                .when(selected || hovered, |d| d.child(div().w(px(2.0)).h(px(LANE_H - 8.0)).rounded(px(1.0)).bg(theme::white(0.95))))
                .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    e.select_mask(Some(id.clone()));
                    e.on_down(Drag::MaskSpan { id: id.clone(), mode: if start { DragMode::Start } else { DragMode::End }, grab: 0.0 }, ev.position, cx);
                }))
        };
        let (g0, g1) = ((left >= -7.0).then(|| grip(true, cx)), (right <= w + 7.0).then(|| grip(false, cx)));
        let id = m.id.clone();
        let id_h = m.id.clone();
        div()
            .id(SharedString::from(format!("mask-span-{}", m.id)))
            .absolute()
            .top(px(top))
            .left(px(vl))
            .w(px(width))
            .h(px(LANE_H))
            .rounded(px(5.0))
            .bg(fill)
            .border(px(if selected { 1.5 } else { 1.0 }))
            .border_color(color)
            .cursor(CursorStyle::OpenHand)
            .on_hover(cx.listener(move |e, h: &bool, _, cx| {
                if *h {
                    e.hover_mask = Some(id_h.clone());
                } else if e.hover_mask.as_deref() == Some(id_h.as_str()) {
                    e.hover_mask = None;
                }
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(move |e, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let Some(m) = e.masks.iter().find(|m| m.id == id).cloned() else { return };
                e.select_mask(Some(id.clone()));
                // Show its box: put the playhead inside the span.
                if !m.active_at(e.time) {
                    e.seek(m.start + 0.02, cx);
                }
                let grab = e.unit_at_x(ev.position.x) - m.start;
                e.on_down(Drag::MaskSpan { id: id.clone(), mode: DragMode::Move, grab }, ev.position, cx);
            }))
            .children(g0)
            .children(g1)
            .into_any_element()
    }
}

#[allow(unused)]
fn _u(_: AnyElement, _: SharedString, _: Hsla, _: &Theme) {}
