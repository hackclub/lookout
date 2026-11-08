//! `VideoPlayer` — the session's compiled timelapse, played natively.
//!
//! The Tauri app used a `<video>` element with the video.js v10 skin (mute,
//! volume and picture-in-picture hidden). GPUI has no video element, so on
//! Linux this decodes with GStreamer (`playbin` → BGRA `appsink`, which
//! the engine already links) and paints frames as images. Elsewhere it
//! shows the poster-less fallback with a link to open the file — a native
//! AVFoundation / Media Foundation backend can slot in behind the same API.
//!
//! The same player drives the editor's stage (`TimelapseEditor`), which
//! needs precise seeking while paused, so seeks are accurate and a paused
//! seek prerolls the new frame.

use std::sync::Arc;

use gpui_kit::gpui::{
    App, Bounds, Context, CursorStyle, EventEmitter, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Pixels, Render, RenderImage, StatefulInteractiveElement, Styled, Task, Window,
    canvas, div, px,
};

use crate::motion::{Anim, Curve, Ease};
use crate::theme::{self, theme};
use crate::ui::icon::Icon;
use crate::ui::paint;
use crate::ui::text::TextExt;

pub enum VideoEvent {
    /// Playback position changed (seconds).
    Time(f64),
    Playing(bool),
    Loaded { duration: f64 },
    Error(String),
}

pub struct VideoPlayer {
    pub url: String,
    frame: Option<(Arc<RenderImage>, f32)>,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub error: Option<String>,
    /// Draw our own controls (the session player). The editor draws its own.
    pub controls: bool,
    hovered: bool,
    controls_o: Anim,
    scrubbing: bool,
    bar_bounds: Bounds<Pixels>,
    backend: Option<backend::Backend>,
    _pump: Option<Task<()>>,
}

impl EventEmitter<VideoEvent> for VideoPlayer {}

impl VideoPlayer {
    pub fn new(url: String, cx: &mut Context<Self>) -> Self {
        let mut v = Self {
            url: url.clone(),
            frame: None,
            playing: false,
            position: 0.0,
            duration: 0.0,
            error: None,
            controls: true,
            hovered: false,
            controls_o: Anim::new(1.0),
            scrubbing: false,
            bar_bounds: Bounds::default(),
            backend: None,
            _pump: None,
        };
        match backend::Backend::open(&url) {
            Ok((b, rx)) => {
                v.backend = Some(b);
                v._pump = Some(cx.spawn(async move |this, cx| {
                    while let Ok(msg) = rx.recv_async().await {
                        let alive = this.update(cx, |v, cx| v.on_msg(msg, cx)).is_ok();
                        if !alive {
                            break;
                        }
                    }
                }));
            }
            Err(e) => v.error = Some(e),
        }
        v
    }

    fn on_msg(&mut self, msg: backend::Msg, cx: &mut Context<Self>) {
        match msg {
            backend::Msg::Frame(img, aspect) => {
                if let Some((old, _)) = self.frame.replace((img, aspect)) {
                    cx.drop_image(old, None);
                }
            }
            backend::Msg::Duration(d) => {
                self.duration = d;
                cx.emit(VideoEvent::Loaded { duration: d });
            }
            backend::Msg::Position(p) => {
                if !self.scrubbing {
                    self.position = p;
                    cx.emit(VideoEvent::Time(p));
                }
            }
            backend::Msg::Eos => {
                self.playing = false;
                cx.emit(VideoEvent::Playing(false));
            }
            backend::Msg::Error(e) => {
                log::warn!("[video] {e}");
                self.error = Some(e.clone());
                cx.emit(VideoEvent::Error(e));
            }
        }
        cx.notify();
    }

    pub fn play(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = &self.backend {
            if self.duration > 0.0 && self.position >= self.duration - 0.05 {
                b.seek(0.0);
            }
            b.play();
            self.playing = true;
            cx.emit(VideoEvent::Playing(true));
            cx.notify();
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = &self.backend {
            b.pause();
            self.playing = false;
            cx.emit(VideoEvent::Playing(false));
            cx.notify();
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.playing { self.pause(cx) } else { self.play(cx) }
    }

    pub fn seek(&mut self, t: f64, cx: &mut Context<Self>) {
        let t = t.clamp(0.0, self.duration.max(0.0));
        self.position = t;
        if let Some(b) = &self.backend {
            b.seek(t);
        }
        cx.emit(VideoEvent::Time(t));
        cx.notify();
    }

    pub fn frame(&self) -> Option<(Arc<RenderImage>, f32)> {
        self.frame.clone()
    }

    fn scrub_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let b = self.bar_bounds;
        let w = f32::from(b.size.width).max(1.0);
        let frac = ((f32::from(x) - f32::from(b.origin.x)) / w).clamp(0.0, 1.0) as f64;
        self.seek(frac * self.duration, cx);
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        if let Some(b) = self.backend.take() {
            b.stop();
        }
    }
}

fn fmt_clock(s: f64) -> String {
    let s = s.max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

impl Render for VideoPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        if let Some(err) = &self.error {
            // `.lookout-video-player__error`
            let url = self.url.clone();
            let detail = if cfg!(target_os = "linux") {
                "Your system can't decode this video (H.264). On Linux, install gst-plugins-bad / OpenH264 to enable playback."
                    .to_string()
            } else {
                err.clone()
            };
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.0))
                .p(px(24.0))
                .text_center()
                .bg(theme::rgba8(11, 13, 18, 0.92))
                .rounded(px(8.0))
                .child(div().ts(18.0).w600().text_color(theme::hex(0xf4f5f7)).child("Can't play this timelapse"))
                .child(div().max_w(px(480.0)).ts_lh(14.0, 1.5).text_color(theme::hex(0xc8ccd4)).child(detail))
                .child(
                    div()
                        .flex()
                        .ts(13.0)
                        .text_color(theme::hex(0x9aa0aa))
                        .child("Your timelapse saved successfully. You can ")
                        .child(
                            div()
                                .id("video-download")
                                .text_color(theme::hex(0x6ea8ff))
                                .underline()
                                .cursor(CursorStyle::PointingHand)
                                .child("download the file")
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                        .child(" and play it in another app."),
                )
                .into_any_element();
        }

        let want = if self.hovered || !self.playing || self.scrubbing { 1.0 } else { 0.0 };
        if self.controls_o.target() != want {
            self.controls_o.set(want, Curve::tween(if want > 0.5 { 150.0 } else { 400.0 }, Ease::EASE_OUT));
        }
        if self.controls_o.tick() {
            window.request_animation_frame();
        }
        let frame = self.frame.clone();
        let co = self.controls_o.get();
        let progress = if self.duration > 0.0 { (self.position / self.duration) as f32 } else { 0.0 };
        let accent = t.accent;
        let entity = cx.entity();
        let bar_probe = cx.entity();
        let mut root = div()
            .id("video-player")
            .relative()
            .size_full()
            .bg(theme::black(1.0))
            .rounded(px(8.0))
            .overflow_hidden()
            .child(
                canvas(|_, _, _| (), move |bounds, _, window, _| {
                    if let Some((img, aspect)) = frame.clone() {
                        paint::paint_fit_image(bounds, img, aspect, false, 1.0, 0.0, window);
                    }
                })
                .absolute()
                .inset_0(),
            )
            .on_hover(cx.listener(|v, h: &bool, _, cx| {
                v.hovered = *h;
                cx.notify();
            }));
        if !self.controls {
            return root.into_any_element();
        }
        root = root.child(
            div()
                .id("video-surface")
                .absolute()
                .inset_0()
                .on_click(cx.listener(|v, _, _, cx| v.toggle(cx))),
        );
        if !self.playing {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .size(px(56.0))
                            .rounded_full()
                            .bg(theme::black(0.55))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::LkPlay.el(20.0, theme::hex(0xffffff))),
                    ),
            );
        }
        // Control bar
        root.child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .opacity(co)
                .pt(px(24.0))
                .px(px(10.0))
                .pb(px(8.0))
                .bg(gpui_kit::gpui::linear_gradient(
                    180.0,
                    gpui_kit::gpui::linear_color_stop(theme::black(0.0), 0.0),
                    gpui_kit::gpui::linear_color_stop(theme::black(0.55), 1.0),
                ))
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    div()
                        .id("video-play")
                        .size(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.0))
                        .hover(|s| s.bg(theme::white(0.15)))
                        .child(if self.playing { Icon::LkPauseBars } else { Icon::LkPlay }.el(14.0, theme::hex(0xffffff)))
                        .on_click(move |_, _, cx| entity.update(cx, |v, cx| v.toggle(cx))),
                )
                .child(
                    div()
                        .ts(12.0)
                        .text_color(theme::hex(0xffffff))
                        .child(format!("{} / {}", fmt_clock(self.position), fmt_clock(self.duration))),
                )
                .child(
                    div()
                        .id("video-bar")
                        .flex_1()
                        .h(px(16.0))
                        .flex()
                        .items_center()
                        .cursor(CursorStyle::PointingHand)
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .h(px(4.0))
                                .rounded_full()
                                .bg(theme::white(0.3))
                                .child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .bottom_0()
                                        .w(gpui_kit::gpui::relative(progress.clamp(0.0, 1.0)))
                                        .rounded_full()
                                        .bg(accent),
                                )
                                .child(
                                    canvas(move |b, _, cx| bar_probe.update(cx, |v, _| v.bar_bounds = b), |_, _, _, _| {})
                                        .absolute()
                                        .inset_0(),
                                ),
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(|v, ev: &MouseDownEvent, _, cx| {
                            v.scrubbing = true;
                            v.scrub_to(ev.position.x, cx);
                        }))
                        .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                            if v.scrubbing {
                                v.scrub_to(ev.position.x, cx);
                            }
                        }))
                        .on_mouse_up(MouseButton::Left, cx.listener(|v, _, _, cx| {
                            v.scrubbing = false;
                            cx.notify();
                        }))
                        .on_mouse_up_out(MouseButton::Left, cx.listener(|v, _, _, cx| {
                            v.scrubbing = false;
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use super::*;
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    pub enum Msg {
        Frame(Arc<RenderImage>, f32),
        Duration(f64),
        Position(f64),
        Eos,
        Error(String),
    }

    pub struct Backend {
        playbin: gst::Element,
        stop_tx: flume::Sender<()>,
    }

    fn sample_to_image(sample: &gst::Sample) -> Option<(Arc<RenderImage>, f32)> {
        let caps = sample.caps()?;
        let info = gstreamer_video::VideoInfo::from_caps(caps).ok()?;
        let buffer = sample.buffer()?;
        let map = buffer.map_readable().ok()?;
        let (w, h) = (info.width(), info.height());
        let stride = info.stride()[0] as usize;
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            let start = row * stride;
            data.extend_from_slice(&map.as_slice()[start..start + w as usize * 4]);
        }
        let img = image::RgbaImage::from_raw(w, h, data)?;
        // Already BGRA — what RenderImage wants.
        let render = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(img)]));
        Some((render, w as f32 / h.max(1) as f32))
    }

    impl Backend {
        pub fn open(url: &str) -> Result<(Self, flume::Receiver<Msg>), String> {
            gst::init().map_err(|e| e.to_string())?;
            let playbin = gst::ElementFactory::make("playbin").build().map_err(|e| format!("playbin: {e}"))?;
            playbin.set_property("uri", url);
            let sink = gst_app::AppSink::builder()
                .caps(&gst::Caps::builder("video/x-raw").field("format", "BGRA").build())
                .max_buffers(1)
                .drop(true)
                .build();
            playbin.set_property("video-sink", &sink);
            // No audio in timelapses; keep it muted like the web player.
            playbin.set_property("mute", true);
            let (tx, rx) = flume::unbounded::<Msg>();
            let tx_s = tx.clone();
            let tx_p = tx.clone();
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        if let Ok(sample) = s.pull_sample() {
                            if let Some((img, a)) = sample_to_image(&sample) {
                                let _ = tx_s.send(Msg::Frame(img, a));
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .new_preroll(move |s| {
                        if let Ok(sample) = s.pull_preroll() {
                            if let Some((img, a)) = sample_to_image(&sample) {
                                let _ = tx_p.send(Msg::Frame(img, a));
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
            playbin.set_state(gst::State::Paused).map_err(|e| format!("{e}"))?;
            let (stop_tx, stop_rx) = flume::bounded::<()>(1);
            let bus = playbin.bus().ok_or("no bus")?;
            let pb = playbin.downgrade();
            std::thread::Builder::new()
                .name("lookout-video".into())
                .spawn(move || {
                    let mut sent_duration = false;
                    loop {
                        if stop_rx.try_recv().is_ok() {
                            break;
                        }
                        while let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(30)) {
                            use gst::MessageView;
                            match msg.view() {
                                MessageView::Eos(_) => {
                                    let _ = tx.send(Msg::Eos);
                                }
                                MessageView::Error(e) => {
                                    let _ = tx.send(Msg::Error(format!("{} ({:?})", e.error(), e.debug())));
                                }
                                _ => {}
                            }
                        }
                        let Some(pb) = pb.upgrade() else { break };
                        if !sent_duration {
                            let mut q = gst::query::Duration::new(gst::Format::Time);
                            if pb.query(&mut q) {
                                if let gst::GenericFormattedValue::Time(Some(d)) = q.result() {
                                    sent_duration = true;
                                    log::debug!("[video] duration {:.2}s", d.nseconds() as f64 / 1e9);
                                    let _ = tx.send(Msg::Duration(d.nseconds() as f64 / 1e9));
                                }
                            }
                        }
                        if let Some(p) = pb.query_position::<gst::ClockTime>() {
                            let _ = tx.send(Msg::Position(p.nseconds() as f64 / 1e9));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(16));
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok((Self { playbin, stop_tx }, rx))
        }

        pub fn play(&self) {
            let _ = self.playbin.set_state(gst::State::Playing);
        }
        pub fn pause(&self) {
            let _ = self.playbin.set_state(gst::State::Paused);
        }
        pub fn seek(&self, t: f64) {
            let _ = self.playbin.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds((t.max(0.0) * 1e9) as u64),
            );
        }
        pub fn stop(&self) {
            let _ = self.stop_tx.try_send(());
            let _ = self.playbin.set_state(gst::State::Null);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod backend {
    pub use crate::platform::decoder::player::{Msg, Player as Backend};
}

#[allow(unused)]
fn _k(_: &App) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn reports_duration() {
        let path = "/tmp/phoenix-media/test.webm";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let (b, rx) = super::backend::Backend::open(&format!("file://{path}")).expect("open");
        let mut got = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok(m) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
                match m {
                    super::backend::Msg::Duration(d) => {
                        got = Some(d);
                        break;
                    }
                    super::backend::Msg::Error(e) => panic!("error {e}"),
                    _ => {}
                }
            }
        }
        b.stop();
        assert!(got.map(|d| (d - 9.0).abs() < 0.5).unwrap_or(false), "duration {got:?}");
    }
}
