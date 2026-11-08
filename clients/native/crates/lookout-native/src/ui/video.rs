//! `VideoPlayer` — the session's compiled timelapse, played natively.
//!
//! Playback runs on each OS's own media stack (see [`crate::player`]):
//! hardware decode, streaming, and frames drawn straight from GPU memory
//! (macOS) or uploaded into one reused texture (Windows, Linux), so a 4K
//! timelapse plays as smoothly as it would in QuickTime.
//!
//! The chrome follows the video.js v10 skin the Tauri app used: one row of
//! white controls on a soft scrim (play, time, slider, settings, full
//! screen), 36px round buttons, a 4px track with a white fill and a thumb
//! that appears under the pointer, and the hovered time above it. Controls hide after a moment of stillness while
//! playing (taking the cursor with them in full screen). Speed and loop live
//! in the settings menu.
//! - keyboard: Space/K play-pause, ←/→ ±5s, J/L ±10s, ,/. frame step,
//!   0–9 jump, Home/End, F full screen, Esc exit, Shift+,/. speed.
//!
//! The editor reuses the player with `controls = false` and draws its own
//! stage UI, driving it through [`VideoPlayer::play`], [`VideoPlayer::seek`]
//! and the [`VideoEvent`] stream.

use std::sync::Arc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, Bounds, ClickEvent, Context, Corners, DispatchPhase, ElementId, EventEmitter, FocusHandle,
    FontFeatures, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Render, SharedString, StatefulInteractiveElement,
    Styled, Task, Window, anchored, canvas, deferred, div, point, px, relative,
};

use crate::motion::{self, Anim, Curve, Ease};
use crate::player::{self, Backend, Frame, Status};
use crate::theme;
use crate::ui::icon::Icon;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;
use crate::ui::tooltip::Tooltip;

pub enum VideoEvent {
    /// Playback position changed (seconds).
    Time(f64),
    Playing(bool),
    Loaded { duration: f64 },
    Error(String),
}

/// Controls hide after this long without pointer movement while playing.
const IDLE_MS: f64 = 2200.0;
/// The buffering spinner waits this long so quick seeks don't flash it.
const SPINNER_DELAY_MS: f64 = 350.0;
/// "Strong" ease-out used for everything the user causes.
const OUT: Ease = Ease::Bezier(0.23, 1.0, 0.32, 1.0);
/// Frame step when paused (timelapses are 30 fps).
const FRAME_STEP: f64 = 1.0 / 30.0;
/// The settings menu's height (heading, six speeds, divider, Loop), used
/// to keep it inside the window.
const SPEED_MENU_H: f32 = 272.0;

fn tnum() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

fn fmt_clock(s: f64, long: bool) -> String {
    let s = s.max(0.0).floor() as i64;
    if long {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_rate(r: f64) -> String {
    if (r - 1.0).abs() < 1e-6 {
        "Normal".into()
    } else {
        format!("{}×", trim(r))
    }
}

fn trim(r: f64) -> String {
    let s = format!("{r:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
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

pub struct VideoPlayer {
    pub url: String,
    /// Draw our own controls (the session player). The editor draws its own.
    pub controls: bool,
    /// Corner radius of the player surface.
    pub radius: f32,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub error: Option<String>,

    backend: Option<Backend>,
    status: Status,
    frame: Option<Frame>,
    focus: FocusHandle,

    // Reveal / auto-hide.
    hovered: bool,
    reveal_until: f64,
    controls_o: Anim,
    controls_shown: bool,

    // Overlays.
    frame_o: Anim,
    spinner_o: Anim,
    buffering_since: Option<f64>,
    dim_o: Anim,
    pp: Anim,

    // Scrubber.
    bar_bounds: Bounds<Pixels>,
    root_bounds: Bounds<Pixels>,
    speed_bounds: Bounds<Pixels>,
    bar_hover: bool,
    knob: Anim,
    hover_frac: Option<f32>,
    /// Some(was_playing) while dragging the playhead.
    scrub: Option<bool>,

    show_remaining: bool,
    rate: f64,
    looping: bool,
    speed_open: bool,
    speed_o: Anim,

    fullscreen: bool,
    owns_window_fullscreen: bool,
    saw_window_fullscreen: bool,


    emitted_time: f64,
    emitted_playing: bool,
    emitted_loaded: bool,
    emitted_error: bool,

    _pump: Option<Task<()>>,
}

impl EventEmitter<VideoEvent> for VideoPlayer {}

impl VideoPlayer {
    pub fn new(url: String, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = flume::bounded::<()>(1);
        let waker: player::Waker = Arc::new(move || {
            let _ = tx.try_send(());
        });
        let mut error = None;
        let backend = match Backend::open(&url, waker) {
            Ok(b) => Some(b),
            Err(e) => {
                log::warn!("[video] {e}");
                error = Some(e);
                None
            }
        };
        let pump = backend.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                while rx.recv_async().await.is_ok() {
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            url,
            controls: true,
            radius: 10.0,
            playing: false,
            position: 0.0,
            duration: 0.0,
            error,
            backend,
            status: Status::default(),
            frame: None,
            focus: cx.focus_handle(),
            hovered: false,
            reveal_until: 0.0,
            controls_o: Anim::new(0.0),
            controls_shown: false,
            frame_o: Anim::new(0.0),
            spinner_o: Anim::new(0.0),
            buffering_since: None,
            dim_o: Anim::new(0.0),
            pp: Anim::new(0.0),
            bar_bounds: Bounds::default(),
            root_bounds: Bounds::default(),
            speed_bounds: Bounds::default(),
            bar_hover: false,
            knob: Anim::new(0.0),
            hover_frac: None,
            scrub: None,
            show_remaining: false,
            rate: 1.0,
            looping: false,
            speed_open: false,
            speed_o: Anim::new(0.0),
            fullscreen: false,
            owns_window_fullscreen: false,
            saw_window_fullscreen: false,
            emitted_time: -1.0,
            emitted_playing: false,
            emitted_loaded: false,
            emitted_error: false,
            _pump: pump,
        }
    }

    // ── Public API (the editor drives these) ────────────────────────────

    pub fn play(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = &self.backend {
            b.play();
            self.playing = true;
            self.emitted_playing = true;
            cx.emit(VideoEvent::Playing(true));
            cx.notify();
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = &self.backend {
            b.pause();
            self.playing = false;
            self.emitted_playing = false;
            cx.emit(VideoEvent::Playing(false));
            cx.notify();
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.playing { self.pause(cx) } else { self.play(cx) }
    }

    pub fn seek(&mut self, t: f64, cx: &mut Context<Self>) {
        let t = t.clamp(0.0, if self.duration > 0.0 { self.duration } else { f64::MAX });
        self.position = t;
        if let Some(b) = &self.backend {
            b.seek(t);
        }
        self.emitted_time = t;
        cx.emit(VideoEvent::Time(t));
        cx.notify();
    }

    /// The video's aspect ratio once the first frame is in.
    pub fn aspect(&self) -> Option<f32> {
        self.frame.as_ref().map(|f| f.aspect())
    }

    /// The frame on screen (what the editor's mask preview samples).
    pub fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    fn set_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        self.rate = rate;
        if let Some(b) = &self.backend {
            b.set_rate(rate);
        }
        cx.notify();
    }

    fn step_rate(&mut self, up: bool, cx: &mut Context<Self>) {
        let i = player::RATES.iter().position(|&r| (r - self.rate).abs() < 1e-6).unwrap_or(2);
        let j = if up { (i + 1).min(player::RATES.len() - 1) } else { i.saturating_sub(1) };
        self.set_rate(player::RATES[j], cx);
    }

    fn toggle_loop(&mut self, cx: &mut Context<Self>) {
        self.looping = !self.looping;
        if let Some(b) = &self.backend {
            b.set_looping(self.looping);
        }
        cx.notify();
    }

    fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen {
            self.fullscreen = false;
            if self.owns_window_fullscreen && window.is_fullscreen() {
                window.toggle_fullscreen();
            }
            self.owns_window_fullscreen = false;
        } else {
            self.fullscreen = true;
            self.saw_window_fullscreen = window.is_fullscreen();
            self.owns_window_fullscreen = !window.is_fullscreen();
            if self.owns_window_fullscreen {
                crate::platform::fullscreen::prepare(window);
                window.toggle_fullscreen();
            }
            window.focus(&self.focus, cx);
        }
        self.poke();
        cx.notify();
    }

    /// Show the controls for another idle period.
    fn poke(&mut self) {
        self.reveal_until = motion::now() + IDLE_MS;
    }

    // ── Scrubbing ──────────────────────────────────────────────────────

    fn frac_at(&self, x: Pixels) -> f32 {
        let b = self.bar_bounds;
        let w = f32::from(b.size.width).max(1.0);
        ((f32::from(x) - f32::from(b.origin.x)) / w).clamp(0.0, 1.0)
    }

    fn begin_scrub(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if self.duration <= 0.0 {
            return;
        }
        let was_playing = self.playing;
        if was_playing {
            if let Some(b) = &self.backend {
                b.pause();
            }
        }
        self.scrub = Some(was_playing);
        self.scrub_to(x, cx);
    }

    fn scrub_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let f = self.frac_at(x);
        self.hover_frac = Some(f);
        self.seek(f as f64 * self.duration, cx);
    }

    fn end_scrub(&mut self, cx: &mut Context<Self>) {
        if let Some(was_playing) = self.scrub.take() {
            if was_playing {
                if let Some(b) = &self.backend {
                    b.play();
                }
            }
            self.poke();
            cx.notify();
        }
    }

    // ── Keyboard ───────────────────────────────────────────────────────

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.platform || k.modifiers.control || k.modifiers.alt {
            return;
        }
        let d = self.duration;
        match k.key.as_str() {
            "space" | "k" => {
                self.toggle(cx);
                // Keyboard actions don't animate.
                self.pp.jump(if self.playing { 1.0 } else { 0.0 });
            }
            "left" => self.seek(self.position - 5.0, cx),
            "right" => self.seek(self.position + 5.0, cx),
            "j" => self.seek(self.position - 10.0, cx),
            "l" => self.seek(self.position + 10.0, cx),
            "," if k.modifiers.shift => self.step_rate(false, cx),
            "." if k.modifiers.shift => self.step_rate(true, cx),
            "<" => self.step_rate(false, cx),
            ">" => self.step_rate(true, cx),
            "," if !self.playing => self.seek(self.position - FRAME_STEP, cx),
            "." if !self.playing => self.seek(self.position + FRAME_STEP, cx),
            "home" => self.seek(0.0, cx),
            "end" => self.seek(d, cx),
            "f" => self.toggle_fullscreen(window, cx),
            "escape" if self.speed_open => self.speed_open = false,
            "escape" if self.fullscreen => self.toggle_fullscreen(window, cx),
            key if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() && d > 0.0 => {
                let n = (key.as_bytes()[0] - b'0') as f64;
                self.seek(d * n / 10.0, cx);
            }
            _ => return,
        }
        self.poke();
        // Keyboard reveals are instant.
        self.controls_o.jump(1.0);
        cx.stop_propagation();
        cx.notify();
    }

    // ── Per-frame sync with the backend ────────────────────────────────

    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(b) = &self.backend else { return };
        b.tick();
        self.status = b.status();
        self.frame = b.frame();
        let s = &self.status;

        if let Some(e) = &s.error {
            if !self.emitted_error {
                self.emitted_error = true;
                self.error = Some(e.clone());
                cx.emit(VideoEvent::Error(e.clone()));
            }
        }
        if s.duration > 0.0 {
            self.duration = s.duration;
            if !self.emitted_loaded {
                self.emitted_loaded = true;
                cx.emit(VideoEvent::Loaded { duration: s.duration });
            }
        }
        // While dragging, the backend's report trails the pointer; the
        // pointer wins.
        let playing = if self.scrub.is_some() { self.playing } else { s.playing };
        if playing != self.emitted_playing {
            self.emitted_playing = playing;
            cx.emit(VideoEvent::Playing(playing));
        }
        self.playing = playing;
        if self.scrub.is_none() {
            self.position = s.position;
            if (self.position - self.emitted_time).abs() > 1e-4 {
                self.emitted_time = self.position;
                cx.emit(VideoEvent::Time(self.position));
            }
        }
    }

    // ── Animation targets ──────────────────────────────────────────────

    fn update_anims(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let now = motion::now();
        let s = &self.status;
        let has_frame = s.has_frame;
        let error = self.error.is_some();

        set(&mut self.frame_o, if has_frame { 1.0 } else { 0.0 }, Curve::tween(260.0, Ease::EASE_OUT_QUART));

        // Spinner: only after a short wait.
        let buffering = s.buffering && !error;
        if buffering {
            self.buffering_since.get_or_insert(now);
        } else {
            self.buffering_since = None;
        }
        let spin = self.buffering_since.is_some_and(|t| now - t > SPINNER_DELAY_MS);
        set(&mut self.spinner_o, if spin { 1.0 } else { 0.0 }, Curve::tween(if spin { 200.0 } else { 120.0 }, OUT));

        set(&mut self.dim_o, if s.ended { 1.0 } else { 0.0 }, Curve::tween(240.0, OUT));
        set(&mut self.pp, if self.playing { 1.0 } else { 0.0 }, Curve::tween(160.0, OUT));

        let shown = self.controls
            && has_frame
            && !error
            && (!self.playing || self.scrub.is_some() || self.speed_open || self.bar_hover || now < self.reveal_until);
        if shown != self.controls_shown {
            self.controls_shown = shown;
            if !shown && self.fullscreen {
                cx.hide_cursor_until_mouse_moves();
            }
        }
        set(&mut self.controls_o, if shown { 1.0 } else { 0.0 }, Curve::tween(if shown { 140.0 } else { 320.0 }, OUT));

        let grow = self.bar_hover || self.scrub.is_some();
        // video.js thumb: opacity/scale over --media-duration-slider (100ms).
        set(&mut self.knob, if grow { 1.0 } else { 0.0 }, Curve::tween(100.0, Ease::Bezier(0.0, 0.0, 0.2, 1.0)));
        set(&mut self.speed_o, if self.speed_open { 1.0 } else { 0.0 }, Curve::tween(if self.speed_open { 160.0 } else { 110.0 }, OUT));

        let mut moving = false;
        for a in [
            &mut self.frame_o,
            &mut self.spinner_o,
            &mut self.dim_o,
            &mut self.pp,
            &mut self.controls_o,
            &mut self.knob,
            &mut self.speed_o,
        ] {
            moving |= a.tick();
        }
        // Keep the auto-hide deadline and the spinner delay ticking.
        let timers = (self.playing && shown) || (buffering && !spin);
        let _ = window;
        moving || timers || self.status.active
    }
}

/// Retarget only when the target changes (so tweens aren't restarted).
fn set(a: &mut Anim, to: f32, curve: Curve) {
    if (a.target() - to).abs() > f32::EPSILON {
        a.set(to, curve);
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        // Stop decoding now rather than whenever the last clone goes.
        self.backend.take();
    }
}

// ── Rendering ─────────────────────────────────────────────────────────

/// A round icon button in the control bar.
fn ctl_button(id: impl Into<ElementId>, active: bool, tip: Option<&'static str>, child: impl IntoElement) -> gpui_kit::gpui::Stateful<gpui_kit::gpui::Div> {
    // video.js: 36px circular buttons; hover/expanded fill at 10% white.
    let d = div()
        .id(id)
        .size(px(36.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .when(active, |d| d.bg(theme::white(0.1)))
        .hover(|s| s.bg(theme::white(0.1)))
        .active(|s| s.bg(theme::white(0.16)))
        .child(child);
    match tip {
        Some(tip) => d.tooltip(move |window, cx| Tooltip::new(tip).build(window, cx)),
        None => d,
    }
}

impl Render for VideoPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync(cx);

        // Leaving OS full screen some other way (green button, Esc on
        // macOS) ends ours too.
        if self.fullscreen {
            if window.is_fullscreen() {
                self.saw_window_fullscreen = true;
            } else if self.saw_window_fullscreen {
                self.fullscreen = false;
                self.owns_window_fullscreen = false;
            }
        }

        if self.update_anims(window, cx) {
            window.request_animation_frame();
        }

        let content = if self.error.is_some() { self.render_error(cx) } else { self.render_player(window, cx) };
        if self.fullscreen {
            let vp = window.viewport_size();
            return div()
                .size_full()
                .bg(theme::black(1.0))
                .child(
                    deferred(
                        anchored()
                            .position(point(px(0.0), px(0.0)))
                            .child(div().w(vp.width).h(vp.height).bg(theme::black(1.0)).child(content)),
                    )
                    .with_priority(100),
                )
                .into_any_element();
        }
        content
    }
}

impl VideoPlayer {
    fn render_error(&self, _cx: &mut Context<Self>) -> AnyElement {
        let url = self.url.clone();
        let detail = self.error.clone().unwrap_or_default();
        div()
            .id("video-error")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .p(px(24.0))
            .text_center()
            .bg(theme::rgba8(11, 13, 18, 1.0))
            .rounded(px(self.radius))
            .child(Icon::WarningCircleFill.el(28.0, theme::white(0.5)))
            .child(div().ts(17.0).w600().text_color(theme::hex(0xf4f5f7)).child("Can't play this timelapse"))
            .child(div().max_w(px(460.0)).ts_lh(13.0, 1.5).text_color(theme::hex(0xaab0ba)).child(detail))
            .child(
                div()
                    .mt(px(4.0))
                    .flex()
                    .ts(13.0)
                    .text_color(theme::hex(0x8a909a))
                    .child("It saved fine. You can ")
                    .child(
                        div()
                            .id("video-download")
                            .text_color(theme::hex(0x6ea8ff))
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .child("open the file")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(" in another app."),
            )
            .into_any_element()
    }

    fn render_player(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let radius = if self.fullscreen { 0.0 } else { self.radius };
        let frame = self.frame.clone();
        let entity = cx.entity();

        let mut root = div()
            .id("video-player")
            .relative()
            .size_full()
            .bg(theme::black(1.0))
            .rounded(px(radius))
            .overflow_hidden()
            .child(
                canvas(
                    {
                        let e = entity.clone();
                        move |b, _, cx| e.update(cx, |v, _| v.root_bounds = b)
                    },
                    move |bounds, _, window, _| {
                        let Some(f) = frame else { return };
                        let r = fit(bounds, f.aspect());
                        // Round only where the picture reaches the rounded
                        // corners of the frame.
                        let dx = f32::from(r.origin.x - bounds.origin.x);
                        let dy = f32::from(r.origin.y - bounds.origin.y);
                        let cr = px((radius - dx.max(dy)).max(0.0));
                        let corners = Corners { top_left: cr, top_right: cr, bottom_right: cr, bottom_left: cr };
                        #[cfg(target_os = "macos")]
                        window.paint_surface_rounded(r, corners, f.buffer.clone());
                        #[cfg(not(target_os = "macos"))]
                        window.paint_surface_frame(r, corners, f.surface.clone());
                    },
                )
                .absolute()
                .inset_0(),
            );

        // First frame fades up out of black.
        let fo = self.frame_o.get();
        if fo < 0.999 {
            root = root.child(div().absolute().inset_0().rounded(px(radius)).bg(theme::black(1.0 - fo)));
        }
        let dim = self.dim_o.get();
        if dim > 0.001 {
            root = root.child(div().absolute().inset_0().rounded(px(radius)).bg(theme::black(0.4 * dim)));
        }

        if !self.controls {
            return root.child(self.render_spinner()).into_any_element();
        }

        root = root
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_hover(cx.listener(|v, h: &bool, _, cx| {
                v.hovered = *h;
                if *h {
                    v.poke();
                } else {
                    v.reveal_until = 0.0;
                }
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|v, _: &MouseMoveEvent, _, cx| {
                v.poke();
                if !v.controls_shown {
                    cx.notify();
                }
            }))
            // Click toggles; double-click toggles full screen (and undoes
            // the first click's toggle, as every web player does).
            .child(
                div()
                    .id("video-surface")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|v, _: &MouseDownEvent, window, cx| {
                            window.focus(&v.focus, cx);
                            if v.speed_open {
                                v.speed_open = false;
                                cx.notify();
                            }
                        }),
                    )
                    .on_click(cx.listener(|v, ev: &ClickEvent, window, cx| {
                        if v.status.has_frame {
                            v.toggle(cx);
                            if ev.click_count() == 2 {
                                v.toggle(cx);
                                v.toggle_fullscreen(window, cx);
                            }
                        }
                    })),
            )
            .child(self.render_spinner());

        let co = self.controls_o.get();
        if co > 0.001 {
            root = root.child(self.render_controls(co, window, cx));
            if let Some(f) = self.hover_frac.filter(|_| self.bar_hover || self.scrub.is_some()) {
                root = root.child(self.render_preview(f, co));
            }
        }
        if self.speed_o.get() > 0.001 {
            root = root.child(self.render_speed_menu(window, cx));
        }
        // --media-frame-border: a hairline that keeps light video edges
        // from bleeding into the page.
        if !self.fullscreen {
            root = root.child(div().absolute().inset_0().rounded(px(radius)).border_1().border_color(theme::black(0.1)));
        }
        root.into_any_element()
    }

    fn render_spinner(&self) -> AnyElement {
        let o = self.spinner_o.get();
        if o < 0.001 {
            return div().into_any_element();
        }
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .opacity(o)
            .child(
                div()
                    .size(px(64.0))
                    .rounded_full()
                    .bg(theme::black(0.35))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Spinner::new(SpinnerSize::Lg).color(theme::white(0.95)).base(theme::white(0.18))),
            )
            .into_any_element()
    }

    fn render_controls(&mut self, co: f32, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let fs = self.fullscreen;
        let d = self.duration;
        let progress = if d > 0.0 { (self.position / d) as f32 } else { 0.0 }.clamp(0.0, 1.0);
        let buffered = if d > 0.0 { (self.status.buffered / d) as f32 } else { 0.0 }.clamp(0.0, 1.0);
        let knob = self.knob.get();
        let long = d >= 3600.0;
        let scrubbing = self.scrub.is_some();
        let entity = cx.entity();

        // ── Time slider (video.js: 4px track at 20% white, white fill, a
        // 12px white thumb that only shows under the pointer) ──
        let probe = entity.clone();
        let global = entity.clone();
        let slider = div()
            .id("video-bar")
            .relative()
            .flex_1()
            .min_w(px(72.0))
            .h(px(32.0))
            .mx(px(14.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(4.0))
                    .rounded_full()
                    .bg(theme::white(0.2))
                    .child(div().absolute().left_0().top_0().bottom_0().w(relative(buffered)).rounded_full().bg(theme::white(0.2)))
                    .child(div().absolute().left_0().top_0().bottom_0().w(relative(progress)).rounded_full().bg(theme::white(1.0))),
            )
            .child(
                canvas(
                    move |b, _, cx| probe.update(cx, |v, _| v.bar_bounds = b),
                    move |b, _, window, _| {
                        if knob > 0.01 {
                            let scale = if scrubbing { 0.9 } else { 0.8 + 0.2 * knob };
                            let r = 6.0 * scale;
                            let cxp = f32::from(b.origin.x) + f32::from(b.size.width) * progress;
                            let cyp = f32::from(b.origin.y) + f32::from(b.size.height) / 2.0;
                            let circle = |r: f32, dy: f32| Bounds {
                                origin: point(px(cxp - r), px(cyp - r + dy)),
                                size: gpui_kit::gpui::size(px(2.0 * r), px(2.0 * r)),
                            };
                            window.paint_quad(gpui_kit::gpui::fill(circle(r, 0.0), theme::white(knob)).corner_radii(px(r)));
                        }
                        // While dragging, follow the pointer anywhere.
                        if scrubbing {
                            let e = global.clone();
                            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    e.update(cx, |v, cx| v.scrub_to(ev.position.x, cx));
                                }
                            });
                            let e = global.clone();
                            window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                                    e.update(cx, |v, cx| v.end_scrub(cx));
                                }
                            });
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_hover(cx.listener(|v, h: &bool, _, cx| {
                v.bar_hover = *h;
                if !*h && v.scrub.is_none() {
                    v.hover_frac = None;
                }
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                if v.scrub.is_none() {
                    v.hover_frac = Some(v.frac_at(ev.position.x));
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, ev: &MouseDownEvent, window, cx| {
                    window.focus(&v.focus, cx);
                    v.begin_scrub(ev.position.x, cx);
                }),
            );

        // ── Buttons ──
        let pp = self.pp.get();
        let ended = self.status.ended;
        let play_icon = if ended { Icon::VpReplay } else { Icon::VpPlay };
        let play_pause = ctl_button("video-play", false, Some("Play / Pause (Space)"), {
            div()
                .relative()
                .size(px(20.0))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(1.0 - pp)
                        .child(play_icon.scaled(20.0, theme::white(1.0), 0.6 + 0.4 * (1.0 - pp))),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(pp)
                        .child(Icon::VpPause.scaled(20.0, theme::white(1.0), 0.6 + 0.4 * pp)),
                )
        })
        .on_click(cx.listener(|v, _: &ClickEvent, window, cx| {
            window.focus(&v.focus, cx);
            v.toggle(cx);
        }));

        let time = {
            let left = if self.show_remaining {
                format!("−{}", fmt_clock((d - self.position).max(0.0), long))
            } else {
                fmt_clock(self.position, long)
            };
            div()
                .id("video-time")
                .ml(px(8.0))
                .flex()
                .gap(px(4.0))
                .cursor_pointer()
                .font_features(tnum())
                .ts(14.0)
                .w500()
                .child(div().text_color(theme::white(1.0)).child(left))
                .child(div().text_color(theme::white(0.65)).child(format!("/ {}", fmt_clock(d, long))))
                .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                    v.show_remaining = !v.show_remaining;
                    cx.notify();
                }))
        };

        let settings_probe = entity.clone();
        let settings = ctl_button("video-settings", self.speed_open, (!self.speed_open).then_some("Settings"), {
            Icon::GearSixFill.el(20.0, theme::white(1.0))
        })
        .relative()
        .child(
            canvas(move |b, _, cx| settings_probe.update(cx, |v, _| v.speed_bounds = b), |_, _, _, _| {})
                .absolute()
                .inset_0(),
        )
        .on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            v.speed_open = !v.speed_open;
            cx.notify();
        }));

        let fs_btn = ctl_button("video-fullscreen", false, Some(if fs { "Exit full screen (F)" } else { "Full screen (F)" }), {
            (if fs { Icon::VpShrink } else { Icon::VpExpand }).el(20.0, theme::white(1.0))
        })
        .on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.toggle_fullscreen(window, cx)));

        let (pad_x, pad_b) = if fs { (24.0, 16.0) } else { (12.0, 8.0) };
        // GPUI clips children to rectangles, so the scrim rounds itself to
        // match the player's corners.
        let radius = if fs { 0.0 } else { self.radius };
        div()
            .id("video-controls")
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .rounded_b(px(radius))
            .opacity(co)
            .pt(px(if fs { 96.0 } else { 56.0 }))
            .px(px(pad_x))
            .pb(px(pad_b))
            .bg(gpui_kit::gpui::linear_gradient(
                180.0,
                gpui_kit::gpui::linear_color_stop(theme::black(0.0), 0.0),
                gpui_kit::gpui::linear_color_stop(theme::black(0.5), 1.0),
            ))
            // Clicks on the bar's empty space shouldn't fall through to
            // the play/pause surface.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(play_pause)
                    .child(time)
                    .child(slider)
                    .child(settings)
                    .child(div().w(px(4.0)))
                    .child(fs_btn),
            )
            .into_any_element()
    }

    /// The time under the pointer and a marker on the track.
    fn render_preview(&self, frac: f32, co: f32) -> AnyElement {
        let d = self.duration;
        if d <= 0.0 {
            return div().into_any_element();
        }
        let t = frac as f64 * d;
        let label = fmt_clock(t, d >= 3600.0);
        let rb = self.root_bounds;
        let bb = self.bar_bounds;
        let x = f32::from(bb.origin.x - rb.origin.x) + f32::from(bb.size.width) * frac;
        let track_y = f32::from(bb.origin.y - rb.origin.y) + f32::from(bb.size.height) / 2.0;
        let root_h = f32::from(rb.size.height);
        let root_w = f32::from(rb.size.width);

        // Pointer marker: a hairline through the track.
        let marker = div()
            .absolute()
            .left(px(x - 0.75))
            .top(px(track_y - 9.0))
            .w(px(1.5))
            .h(px(18.0))
            .rounded_full()
            .bg(theme::white(0.6))
            .opacity(co);

        let label_el = div()
            .font_features(tnum())
            .ts(14.0)
            .w600()
            .text_color(theme::white(1.0))
            .child(label);
        let w = 64.0;
        let body = div().w(px(w)).flex().justify_center().child(label_el);
        let left = (x - w / 2.0).clamp(8.0, (root_w - w - 8.0).max(8.0));
        let bottom = root_h - track_y + 18.0;
        let preview = div().absolute().left(px(left)).bottom(px(bottom)).opacity(co).child(body);
        div().absolute().inset_0().child(marker).child(preview).into_any_element()
    }

    /// Settings popover above the gear: playback speed and loop.
    ///
    /// Drawn in a window-level layer rather than inside the player, which
    /// clips to its own bounds: on the session page the player is shorter
    /// than the menu, so its top rows were cut off and unreachable.
    fn render_speed_menu(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let o = self.speed_o.get();
        let vp = window.viewport_size();
        let sb = self.speed_bounds;
        let right = f32::from(vp.width - (sb.origin.x + sb.size.width));
        // --media-popover-side-offset (12px), rising 8px into place; kept
        // inside the window when there isn't room above the gear.
        let max_bottom = (f32::from(vp.height) - SPEED_MENU_H - 8.0).max(8.0);
        let bottom = (f32::from(vp.height - sb.origin.y) + 12.0).min(max_bottom) - 8.0 * (1.0 - o);
        let current = self.rate;
        let row = |id: SharedString, label: String, checked: bool| {
            div()
                .id(id)
                .h(px(32.0))
                .px(px(10.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .hover(|s| s.bg(theme::white(0.1)))
                .font_features(tnum())
                .ts(13.0)
                .when(checked, |d| d.w600())
                .text_color(theme::white(if checked { 1.0 } else { 0.85 }))
                .child(label)
                .when(checked, |d| d.child(Icon::LkCheckLine.el(15.0, theme::white(1.0))))
        };
        let heading = |text: &'static str| {
            div().px(px(10.0)).pt(px(6.0)).pb(px(4.0)).ts(11.0).w600().text_color(theme::white(0.5)).child(text)
        };
        let mut menu = div()
            .id("video-settings-menu")
            .absolute()
            .right(px(right.max(8.0)))
            .bottom(px(bottom))
            .w(px(168.0))
            .opacity(o)
            .p(px(4.0))
            .rounded(px(12.0))
            .bg(theme::rgba8(30, 30, 33, 0.94))
            .border_1()
            .border_color(theme::white(0.08))
            .shadow_lg()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(heading("Speed"));
        for &r in player::RATES.iter().rev() {
            let selected = (r - current).abs() < 1e-6;
            menu = menu.child(row(SharedString::from(format!("video-rate-{r}")), fmt_rate(r), selected).on_click(
                cx.listener(move |v, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    v.set_rate(r, cx);
                    v.speed_open = false;
                    v.poke();
                }),
            ));
        }
        menu = menu
            .child(div().my(px(4.0)).mx(px(6.0)).h(px(1.0)).bg(theme::white(0.08)))
            .child(row("video-loop".into(), "Loop".into(), self.looping).on_click(cx.listener(|v, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                v.toggle_loop(cx);
            })));
        // Click-away catcher behind the menu, covering the whole window.
        let catcher = div().id("video-settings-catcher").absolute().inset_0().on_mouse_down(
            MouseButton::Left,
            cx.listener(|v, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                v.speed_open = false;
                cx.notify();
            }),
        );
        let layer = div()
            .relative()
            .w(vp.width)
            .h(vp.height)
            .when(self.speed_open, |d| d.child(catcher))
            .child(menu);
        // Above the full-screen layer (priority 100).
        deferred(anchored().position(point(px(0.0), px(0.0))).child(layer))
            .with_priority(200)
            .into_any_element()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn reports_duration() {
        let path = "/tmp/phoenix-media/test.webm";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let (tx, rx) = flume::unbounded::<()>();
        let b = crate::player::Backend::open(
            &format!("file://{path}"),
            std::sync::Arc::new(move || {
                let _ = tx.send(());
            }),
        )
        .expect("open");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = 0.0;
        while std::time::Instant::now() < deadline {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
            let s = b.status();
            assert!(s.error.is_none(), "error {:?}", s.error);
            if s.duration > 0.0 && s.has_frame {
                got = s.duration;
                break;
            }
        }
        assert!((got - 9.0).abs() < 0.5, "duration {got}");
        assert!(b.frame().is_some_and(|f| f.width > 0 && f.height > 0));
    }
}
