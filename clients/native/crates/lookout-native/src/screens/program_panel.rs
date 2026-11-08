//! `ProgramPanel.tsx` — after a timelapse is recorded, a program can ask for
//! a few details in an in-app sheet showing the program's own page.
//!
//! The Tauri app framed the page in a sandboxed iframe; here it is a native
//! webview (`platform::webview`) laid over the sheet's body and moved with it
//! every frame. The protocol is `programPanel.ts`'s: the page posts
//! `lookout:ready` / `lookout:resize {height}` / `lookout:done` /
//! `lookout:cancel` to `window.parent`, origin-checked. A panel that doesn't
//! load within 12s, or a webview that can't be created (no WebKitGTK, a
//! Wayland window), shows the "finish in your browser" fallback instead.
//!
//! Differences forced by a native view: it always draws above GPUI, so it is
//! hidden (not merely covered) until the panel is revealed and as soon as the
//! sheet starts to close; and a panel taller than the window scrolls inside
//! its own page rather than in the sheet.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    App, Bounds, Context, Element, ElementId, EventEmitter, GlobalElementId, Hitbox, HitboxBehavior,
    InteractiveElement, IntoElement, LayoutId, ParentElement, Pixels, Render, Rgba, StatefulInteractiveElement,
    Style, Styled, WeakEntity, Window, div, img, px, relative,
};

use crate::images::{self, Remote};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::platform::webview::{PanelWebView, WebEvent};
use crate::theme::theme;
use crate::ui::button::{BtnSize, Button};
use crate::ui::icon::Icon;
use crate::ui::sheet::Sheet;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

pub enum PanelEvent {
    /// Closed without finishing (X, cancel, fallback to the browser).
    Dismiss,
    /// The program said `lookout:done`.
    Done,
}

/// `programPanel.ts` constants.
pub const PANEL_MIN_HEIGHT: f32 = 220.0;
pub const PANEL_SANITY_MAX_HEIGHT: f32 = 6000.0;
const PANEL_HEIGHT_SLACK: f32 = 2.0;
const PANEL_LOAD_TIMEOUT: Duration = Duration::from_millis(12_000);
const PANEL_READY_GRACE: Duration = Duration::from_millis(4000);
/// Header (grabber + attribution row) and the bottom spacer, for capping the
/// body to the window.
const CHROME_HEIGHT: f32 = 54.0 + 10.0 + 24.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelMessage {
    Ready,
    Resize(f32),
    Done,
    Cancel,
}

/// `parsePanelMessage` minus the origin check (done where the message
/// enters, against the URL of the page that sent it).
pub fn parse_message(data: &serde_json::Value) -> Option<PanelMessage> {
    match data.get("type")?.as_str()? {
        "lookout:ready" => Some(PanelMessage::Ready),
        "lookout:done" => Some(PanelMessage::Done),
        "lookout:cancel" => Some(PanelMessage::Cancel),
        "lookout:resize" => {
            let h = data.get("height")?.as_f64()?;
            if !h.is_finite() || h <= 0.0 {
                return None;
            }
            Some(PanelMessage::Resize((h.round() as f32).clamp(PANEL_MIN_HEIGHT, PANEL_SANITY_MAX_HEIGHT)))
        }
        _ => None,
    }
}

/// The URL with `lookout_theme` set, so the page paints the right way round
/// from its first byte of CSS. Only the query changes, never the origin.
pub fn frame_src(url: &str, dark: bool) -> String {
    let Ok(mut u) = url::Url::parse(url) else { return url.to_string() };
    let kept: Vec<(String, String)> =
        u.query_pairs().filter(|(k, _)| k != "lookout_theme").map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    u.query_pairs_mut().clear().extend_pairs(kept).append_pair("lookout_theme", if dark { "dark" } else { "light" });
    u.to_string()
}

pub struct ProgramPanel {
    pub token: String,
    pub url: String,
    pub label: String,
    pub icon_url: Option<String>,
    /// The sheet hosting this panel; the webview hides once it is closing.
    pub sheet: Option<WeakEntity<Sheet>>,
    web: Option<Rc<PanelWebView>>,
    tried_web: bool,
    loaded: bool,
    ready: bool,
    grace_elapsed: bool,
    broken: bool,
    closing: bool,
    height: Anim,
    /// Loading cover opacity: 1 → 0 over 0.18s easeOut once revealed.
    cover: Anim,
}

impl EventEmitter<PanelEvent> for ProgramPanel {}

impl ProgramPanel {
    pub fn new(token: String, url: String, label: String, icon_url: Option<String>, cx: &mut Context<Self>) -> Self {
        cx.observe(&images::cache(cx), |_, _, cx| cx.notify()).detach();
        // Both timed from mount, as in the original.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PANEL_READY_GRACE).await;
            this.update(cx, |p, cx| {
                p.grace_elapsed = true;
                p.reveal();
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PANEL_LOAD_TIMEOUT).await;
            this.update(cx, |p, cx| {
                if !p.loaded && !p.broken {
                    log::warn!("[panel] {} panel did not load in time", p.label);
                    p.set_broken(cx);
                }
            })
            .ok();
        })
        .detach();
        let broken = url::Url::parse(&url).map(|u| !u.origin().is_tuple()).unwrap_or(true);
        Self {
            token,
            url,
            label,
            icon_url,
            sheet: None,
            web: None,
            tried_web: false,
            loaded: false,
            ready: false,
            grace_elapsed: false,
            broken,
            closing: false,
            height: Anim::new(PANEL_MIN_HEIGHT),
            cover: Anim::new(1.0),
        }
    }

    fn revealed(&self) -> bool {
        self.ready || self.grace_elapsed
    }

    fn reveal(&mut self) {
        if self.revealed() && self.cover.target() != 0.0 {
            self.cover.set(0.0, Curve::tween(180.0, Ease::EASE_OUT));
        }
    }

    fn set_broken(&mut self, cx: &mut Context<Self>) {
        self.broken = true;
        // Drop the native view: the fallback is GPUI content in its place.
        self.web = None;
        cx.notify();
    }

    fn close(&mut self, ev: PanelEvent, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        cx.emit(ev);
        cx.notify();
    }

    fn on_web(&mut self, ev: WebEvent, cx: &mut Context<Self>) {
        match ev {
            WebEvent::Loaded => self.loaded = true,
            WebEvent::Failed => {
                if !self.broken {
                    self.set_broken(cx);
                }
            }
            WebEvent::OpenExternal(u) => cx.open_url(&u),
            WebEvent::Message(v) => match parse_message(&v) {
                Some(PanelMessage::Ready) => {
                    self.ready = true;
                    self.reveal();
                }
                Some(PanelMessage::Resize(h)) => self.height.set(h, Curve::Spring(Spring::OVERLAY)),
                Some(PanelMessage::Done) => self.close(PanelEvent::Done, cx),
                Some(PanelMessage::Cancel) => self.close(PanelEvent::Dismiss, cx),
                None => {}
            },
        }
        cx.notify();
    }

    fn ensure_web(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tried_web || self.broken {
            return;
        }
        self.tried_web = true;
        let t = theme(cx).clone();
        let bg = Rgba::from(t.bg_panel);
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let src = frame_src(&self.url, t.dark);
        let pending = match PanelWebView::prepare(&src, t.dark, (to8(bg.r), to8(bg.g), to8(bg.b)), window, cx) {
            Ok(p) => p,
            Err(e) => return self.web_unavailable(e),
        };
        // Build outside this render: WebView2 pumps messages while it starts,
        // which runs queued GPUI tasks, and they'd find the App borrowed
        // (see `PendingWebView`). Between `update`s in a task it isn't.
        cx.spawn_in(window, async move |this, cx| {
            let built = pending.build();
            let rx = this
                .update_in(cx, |p, window, cx| match built {
                    Ok((web, rx)) if !p.broken => {
                        web.place(None, window);
                        p.web = Some(Rc::new(web));
                        cx.notify();
                        Some(rx)
                    }
                    Ok(_) => None,
                    Err(e) => {
                        p.web_unavailable(e);
                        cx.notify();
                        None
                    }
                })
                .ok()
                .flatten();
            let Some(rx) = rx else { return };
            while let Ok(ev) = rx.recv_async().await {
                if this.update(cx, |p, cx| p.on_web(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn web_unavailable(&mut self, e: anyhow::Error) {
        log::warn!("[panel] can't embed {}: {e:#}", self.label);
        self.broken = true;
    }
}

/// `isPanelUrlAcceptable`: https (or http on localhost), never our own host.
pub fn acceptable(url: &str, api_base: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    let host = u.host_str().unwrap_or("");
    if host.ends_with(".localhost") || host == "tauri.localhost" {
        return false;
    }
    if url::Url::parse(api_base).ok().and_then(|b| b.host_str().map(String::from)).as_deref() == Some(host) {
        return false;
    }
    match u.scheme() {
        "https" => true,
        "http" => matches!(host, "localhost" | "127.0.0.1"),
        _ => false,
    }
}

impl Render for ProgramPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        self.ensure_web(window, cx);
        if crate::keep_animating!(window, self.height, self.cover) {}
        let sheet_open = self.sheet.as_ref().and_then(|s| s.upgrade()).map(|s| s.read(cx).is_open).unwrap_or(true);
        let show_web = !self.broken && !self.closing && sheet_open && self.revealed();

        let origin = self.url.trim_start_matches("https://").trim_start_matches("http://").split('/').next().unwrap_or("").to_string();
        let icon = self.icon_url.as_deref().and_then(|u| match images::get(u, cx) {
            Remote::Ready(i, _) => Some(img(i).size(px(18.0)).rounded(px(5.0))),
            _ => None,
        });
        let me = cx.entity();
        let me2 = cx.entity();
        let url = self.url.clone();
        let max_h = (f32::from(window.viewport_size().height) - CHROME_HEIGHT).max(PANEL_MIN_HEIGHT);
        let body_h = if self.broken { PANEL_MIN_HEIGHT } else { (self.height.get() + PANEL_HEIGHT_SLACK).min(max_h) };
        let cover = self.cover.get();

        let body = if self.broken {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.0))
                .p(px(20.0))
                .text_center()
                .child(Icon::WarningCircleFill.el(26.0, t.text_tertiary))
                .child(
                    div()
                        .ts_lh(12.0, 1.5)
                        .text_color(t.text_secondary)
                        .child(format!("{} couldn't load here. You can finish up in your browser instead.", self.label)),
                )
                .child(Button::new("panel-browser", "Open in browser").size(BtnSize::Sm).on_click(move |_, _, cx| {
                    cx.open_url(&url);
                    me2.update(cx, |p, cx| p.close(PanelEvent::Dismiss, cx));
                }))
        } else {
            div()
                .size_full()
                .relative()
                .child(WebSlot { web: self.web.clone(), show: show_web })
                .when(cover > 0.001, |d| {
                    d.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(t.bg_panel)
                            .opacity(cover)
                            .child(Spinner::new(SpinnerSize::Md)),
                    )
                })
        };

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(12.0))
                    .py(px(8.0))
                    .flex_none()
                    .child(div().w(px(36.0)).h(px(4.0)).rounded(px(2.0)).bg(t.border_hover).mx_auto())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .mt(px(8.0))
                            .children(icon)
                            .child(div().ts(13.0).w600().text_color(t.text_primary).flex_none().child(self.label.clone()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .ts(11.0)
                                    .text_color(t.text_quaternary)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_right()
                                    .child(origin),
                            )
                            .child(
                                div()
                                    .id("panel-close")
                                    .size(px(22.0))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(t.bg_selected)
                                    .cursor_pointer()
                                    .child(Icon::XBold.el(12.0, t.text_secondary))
                                    .on_click(move |_, _, cx| me.update(cx, |p, cx| p.close(PanelEvent::Dismiss, cx))),
                            ),
                    ),
            )
            .child(div().h(px(body_h)).flex_none().overflow_hidden().child(body))
    }
}

/// Where the native webview goes: it takes this element's bounds each frame
/// (so it rides the sheet's slide and drag), or hides when `show` is off.
struct WebSlot {
    web: Option<Rc<PanelWebView>>,
    show: bool,
}

impl IntoElement for WebSlot {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebSlot {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let web = self.web.as_ref()?;
        // Whole bounds, not the clip: the platform clips the child view to
        // the window, and resizing it to the visible part would reflow the
        // page on every frame of the slide.
        let on_screen = !bounds.intersect(&window.content_mask().bounds).is_empty();
        let show = self.show && on_screen;
        web.place(show.then_some(bounds), window);
        // GPUI content under the native view must not react to the pointer.
        show.then(|| window.insert_hitbox(bounds, HitboxBehavior::BlockMouse))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        _: &mut Window,
        _: &mut App,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_protocol() {
        assert_eq!(parse_message(&json!({"type": "lookout:ready"})), Some(PanelMessage::Ready));
        assert_eq!(parse_message(&json!({"type": "lookout:done"})), Some(PanelMessage::Done));
        assert_eq!(parse_message(&json!({"type": "lookout:cancel"})), Some(PanelMessage::Cancel));
        assert_eq!(parse_message(&json!({"type": "lookout:resize", "height": 10})), Some(PanelMessage::Resize(220.0)));
        assert_eq!(parse_message(&json!({"type": "lookout:resize", "height": 480.6})), Some(PanelMessage::Resize(481.0)));
        assert_eq!(parse_message(&json!({"type": "lookout:resize", "height": 1e9})), Some(PanelMessage::Resize(6000.0)));
        assert_eq!(parse_message(&json!({"type": "lookout:resize", "height": -1})), None);
        assert_eq!(parse_message(&json!({"type": "lookout:resize", "height": "300"})), None);
        assert_eq!(parse_message(&json!({"type": "other"})), None);
        assert_eq!(parse_message(&json!("lookout:ready")), None);
    }

    #[test]
    fn theme_param_keeps_origin_and_query() {
        assert_eq!(frame_src("https://p.example/x?a=1&lookout_theme=light", true), "https://p.example/x?a=1&lookout_theme=dark");
        assert_eq!(frame_src("https://p.example/x", false), "https://p.example/x?lookout_theme=light");
    }
}
