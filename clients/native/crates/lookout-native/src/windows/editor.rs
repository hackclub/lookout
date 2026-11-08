//! The editor window (`EditorWindow.tsx`): one per session, 940×660
//! (min 620×480), resizable. Closing it publishes the timelapse — with the
//! cuts made so far — after a confirmation, then hands focus back to the
//! main window.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyView, App, AppContext, Bounds, Context, Decorations, Entity, Global, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, Render, ResizeEdge, Size, Styled, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowOptions, div, point, px, size,
};

use crate::app::{self, api_base};
use crate::screens::editor::{ApplyResult, EditorEvent, TimelapseEditor};
use crate::theme::{Theme, is_linux, is_mac, is_windows, theme};
use crate::ui::header_bar::{HeaderBar, HeaderNav};
use crate::ui::overlay::{ConfirmDialog, Entrance, ModalSlot, ModalWidth, render_modals};
use crate::ui::text::family;
use crate::windows::main_window::{WINDOW_MARGIN, linux_frame, uses_header_bar};

#[derive(Default)]
pub struct EditorWindows(pub HashMap<String, WindowHandle<EditorWindow>>);
impl Global for EditorWindows {}

pub struct EditorWindow {
    /// A system material is behind the window (see platform::vibrancy).
    vibrant: bool,
    token: String,
    editor: Entity<TimelapseEditor>,
    name: Option<String>,
    removed: usize,
    dirty: bool,
    finished: bool,
    confirm: Option<Entity<ConfirmDialog>>,
    modals: Vec<ModalSlot>,
    active: crate::motion::Anim,
    _subs: Vec<Subscription>,
}

pub fn open(token: String, cx: &mut App) {
    if !cx.has_global::<EditorWindows>() {
        cx.set_global(EditorWindows::default());
    }
    if let Some(h) = cx.global::<EditorWindows>().0.get(&token).copied() {
        if h.update(cx, |_, window, _| window.activate_window()).is_ok() {
            crate::windows::main_window::editor_opened(token, cx);
            return;
        }
    }
    let pad = if is_linux() && crate::platform::desktop_appearance::client_frames() { 2.0 * WINDOW_MARGIN } else { 0.0 };
    let bounds = Bounds::centered(None, size(px(940.0 + pad), px(660.0 + pad)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Edit timelapse".into()),
            appears_transparent: is_mac(),
            traffic_light_position: is_mac().then(|| point(px(12.0), px(9.0))),
        }),
        window_decorations: is_linux().then(|| if crate::platform::desktop_appearance::client_frames() { WindowDecorations::Client } else { WindowDecorations::Server }),
        window_background: if is_linux() && crate::platform::desktop_appearance::client_frames() {
            WindowBackgroundAppearance::Transparent
        } else if is_linux() {
            WindowBackgroundAppearance::Opaque
        } else if is_mac() {
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::MicaBackdrop
        },
        is_resizable: true,
        window_min_size: Some(size(px(620.0 + pad), px(480.0 + pad))),
        app_id: Some(crate::store::APP_ID.into()),
        icon: crate::platform::window_icon(),
        ..Default::default()
    };
    let t2 = token.clone();
    if let Ok(handle) = cx.open_window(options, move |window, cx| cx.new(|cx| EditorWindow::new(t2, window, cx))) {
        cx.global_mut::<EditorWindows>().0.insert(token.clone(), handle);
        crate::windows::main_window::editor_opened(token, cx);
    }
}

/// Bring an open editor to the front (EditorOpenPlaceholder's click).
pub fn focus(token: &str, cx: &mut App) {
    if let Some(h) = cx.try_global::<EditorWindows>().and_then(|e| e.0.get(token).copied()) {
        h.update(cx, |_, window, _| window.activate_window()).ok();
    }
}

impl EditorWindow {
    fn new(token: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| TimelapseEditor::new(token.clone(), cx));
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&editor, window, |this, _, ev: &EditorEvent, window, cx| match ev {
            EditorEvent::Applied(result) => {
                this.finished = true;
                this.done(result.clone(), window, cx);
            }
            EditorEvent::CutsChanged { removed, dirty } => {
                this.removed = *removed;
                this.dirty = *dirty;
            }
            EditorEvent::SessionName(name) => {
                window.set_window_title(&format!("Edit — {name}"));
                this.name = Some(name.clone());
                cx.notify();
            }
        }));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            this.active.set_css(if window.is_window_active() { 1.0 } else { 0.0 }, 160.0, crate::motion::Ease::EASE_OUT);
            cx.notify();
        }));
        subs.push(cx.observe_window_appearance(window, |_, _, cx| cx.notify()));
        let me = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            me.update(cx, |this, cx| this.request_close(window, cx)).unwrap_or(true)
        });
        Self {
            vibrant: crate::platform::vibrancy::apply(window),
            token,
            editor,
            name: None,
            removed: 0,
            dirty: false,
            finished: false,
            confirm: None,
            modals: Vec::new(),
            active: crate::motion::Anim::new(1.0),
            _subs: subs,
        }
    }

    /// `onCloseRequested`: confirm, then publish with the cuts so far.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.finished {
            return true;
        }
        let message = if self.dirty && self.removed > 0 {
            format!(
                "Closing publishes this timelapse with {} cut{} applied. This can't be undone.",
                self.removed,
                if self.removed == 1 { "" } else { "s" }
            )
        } else {
            "Closing publishes this timelapse as recorded. This can't be undone.".to_string()
        };
        if crate::ui::overlay::native_dialogs() {
            let me = cx.entity();
            crate::ui::overlay::native_dialog(
                window.window_handle(),
                gpui_kit::gpui::PromptLevel::Warning,
                "Finish timelapse?".into(),
                message,
                "Publish".into(),
                Some("Cancel".into()),
                move |window, cx| me.update(cx, |this, cx| this.finish_and_close(window, cx)),
                cx,
            );
            return false;
        }
        let me = cx.entity();
        let me2 = cx.entity();
        let dialog = cx.new(|cx| {
            let mut d = ConfirmDialog::new(
                "Finish timelapse?",
                message,
                "Publish",
                cx,
                move |window, cx| me.update(cx, |this, cx| this.finish_and_close(window, cx)),
                move |_, cx| me2.update(cx, |this, cx| {
                    this.confirm = None;
                    cx.notify();
                }),
            );
            d.destructive = true;
            d
        });
        self.confirm = Some(dialog);
        cx.notify();
        false
    }

    fn finish_and_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm = None;
        let cuts = self.editor.read(cx).current_cuts();
        let core = app::state(cx).core.clone();
        let cfg = lookout_core::SessionConfig { token: self.token.clone(), api_base_url: api_base(cx) };
        let fut = app::tokio(cx, async move {
            let body = serde_json::to_value(&cuts).unwrap_or_default();
            if let Err(e) = core.session_set_cuts(&cfg, body).await {
                log::warn!("[editor] setCuts on close failed: {}", e.message);
            }
            core.session_apply_cuts(&cfg).await.ok()
        });
        cx.spawn_in(window, async move |this, cx| {
            let v = fut.await;
            let s = |k: &str| v.as_ref().and_then(|v| v.get(k)).and_then(|x| x.as_str()).map(String::from);
            let result = ApplyResult {
                status: s("status"),
                redirect_url: s("redirectUrl"),
                panel_url: s("panelUrl"),
                panel_resolved: v.as_ref().and_then(|v| v.get("panelResolved")).and_then(|b| b.as_bool()).unwrap_or(false),
            };
            this.update_in(cx, |this, window, cx| {
                this.finished = true;
                this.done(result, window, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Emit `lookout-edited` to the main window, focus it, close this one.
    fn done(&mut self, result: ApplyResult, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.token.clone();
        cx.defer(move |cx| crate::windows::main_window::on_edited(token, result, cx));
        if let Some(e) = cx.try_global::<EditorWindows>() {
            let _ = e;
        }
        cx.global_mut::<EditorWindows>().0.remove(&self.token);
        crate::windows::focus_main(cx);
        window.remove_window();
    }
}

fn resize_edge(pos: Point<Pixels>, win: Size<Pixels>, margin: f32) -> Option<ResizeEdge> {
    let band = 8.0;
    let (x, y) = (f32::from(pos.x), f32::from(pos.y));
    let (w, h) = (f32::from(win.width), f32::from(win.height));
    let inner_l = margin;
    let inner_t = margin;
    let inner_r = w - margin;
    let inner_b = h - margin;
    let near = |v: f32, edge: f32| (v - edge).abs() <= band;
    let left = near(x, inner_l) || x < inner_l;
    let right = near(x, inner_r) || x > inner_r;
    let top = near(y, inner_t) || y < inner_t;
    let bottom = near(y, inner_b) || y > inner_b;
    match (top, bottom, left, right) {
        (true, _, true, _) => Some(ResizeEdge::TopLeft),
        (true, _, _, true) => Some(ResizeEdge::TopRight),
        (_, true, true, _) => Some(ResizeEdge::BottomLeft),
        (_, true, _, true) => Some(ResizeEdge::BottomRight),
        (true, _, _, _) => Some(ResizeEdge::Top),
        (_, true, _, _) => Some(ResizeEdge::Bottom),
        (_, _, true, _) => Some(ResizeEdge::Left),
        (_, _, _, true) => Some(ResizeEdge::Right),
        _ => None,
    }
}

impl Render for EditorWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance()).with_accent(crate::platform::accent());
        if cx.global::<Theme>().dark != t.dark {
            cx.set_global(t.clone());
        }
        let t = theme(cx).clone();
        let csd = uses_header_bar(window);
        if self.active.tick() {
            window.request_animation_frame();
        }
        // Confirm modal
        let want = self.confirm.clone().map(|c| c.entity_id());
        for m in &mut self.modals {
            if Some(m.id) != want {
                m.leave();
            }
        }
        if let Some(c) = &self.confirm {
            if !self.modals.iter().any(|m| m.id == c.entity_id() && !m.leaving) {
                let view: AnyView = c.clone().into();
                let me = cx.entity();
                self.modals.push(ModalSlot::new(
                    view,
                    Entrance::Overlay,
                    ModalWidth::Max(420.0),
                    Some(Rc::new(move |_: &mut Window, cx: &mut App| me.update(cx, |this, cx| {
                        this.confirm = None;
                        cx.notify();
                    }))),
                ));
            }
        }
        let mut moving = false;
        for m in &mut self.modals {
            moving |= m.tick();
        }
        self.modals.retain(|m| !m.finished());
        if moving {
            window.request_animation_frame();
        }

        let pad_x = 16.0;
        let mut body = div().size_full().flex().flex_col().relative().font_family(family()).text_color(t.text_primary);
        if csd {
            let me = cx.entity();
            body = body.child(
                HeaderBar::new(
                    HeaderNav {
                        title: self.name.clone().unwrap_or_else(|| "Edit timelapse".into()).into(),
                        subtitle: self.name.is_some().then(|| "Editing timelapse".into()),
                        on_back: None,
                    },
                    crate::platform::desktop_appearance::desktop_appearance().controls_on_right,
                    0.55 + 0.45 * self.active.get(),
                )
                .maximizable(true)
                .on_close(move |window, cx| {
                    let close = me.update(cx, |this, cx| this.request_close(window, cx));
                    if close {
                        window.remove_window();
                    }
                }),
            );
        } else if is_mac() {
            body = body.child(
                div()
                    .h(px(30.0))
                    .flex_none()
                    .window_control_area(gpui_kit::gpui::WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        if ev.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    }),
            );
        }
        body = body.child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .px(px(pad_x))
                .pb(px(16.0))
                .when(is_windows(), |d| d.pt(px(16.0)))
                .child(self.editor.clone()),
        );
        let win_w = f32::from(window.bounds().size.width) - if csd { 2.0 * WINDOW_MARGIN } else { 0.0 };
        body = body.children(render_modals(&self.modals, &t, win_w));
        let active = self.active.get();
        if csd {
            let tiled = matches!(window.window_decorations(), Decorations::Client { tiling } if tiling.top || tiling.bottom || tiling.left || tiling.right);
            let margin = if tiled { 0.0 } else { WINDOW_MARGIN };
            div()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    if let Some(edge) = resize_edge(ev.position, size, margin) {
                        window.start_window_resize(edge);
                    }
                })
                .child(linux_frame(body.into_any_element(), &t, active, window))
                .into_any_element()
        } else {
            div().size_full().when(!self.vibrant, |d| d.bg(t.bg_body)).child(body).into_any_element()
        }
    }
}
