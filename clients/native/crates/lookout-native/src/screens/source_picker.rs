//! `SourcePicker.tsx` — pick the screens, windows, camera or (Wayland)
//! portal casts to record.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    AnyElement, App, AppContext, Bounds, Context, ElementId, Entity, EventEmitter, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render, RenderImage, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use lookout_core::sources::CaptureSourceList;
use lookout_core::{CaptureSource, PickedSource};

use crate::app::{self, store, store_mut};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::platform::camera::CameraDevice;
use crate::previews;
use crate::theme::{self, Theme, eid, theme};
use crate::ui::button::{BtnSize, Button, PressState, Variant, press_values};
use crate::ui::feedback::error_page;
use crate::ui::paint;
use crate::ui::primitives::edge_faded;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Screens,
    Windows,
    Cameras,
    Cast,
}

#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub enum Sel {
    Monitor(u32),
    Window(u32),
    Pipewire(u32),
    Camera(String),
}

impl Sel {
    fn kind(&self) -> u8 {
        match self {
            Sel::Monitor(_) => 0,
            Sel::Window(_) => 1,
            Sel::Pipewire(_) => 2,
            Sel::Camera(_) => 3,
        }
    }
    pub fn capture(&self) -> Option<CaptureSource> {
        match self {
            Sel::Monitor(id) => Some(CaptureSource::Monitor { id: *id }),
            Sel::Window(id) => Some(CaptureSource::Window { id: *id }),
            Sel::Pipewire(id) => Some(CaptureSource::PipeWire { id: *id }),
            Sel::Camera(_) => None,
        }
    }
    pub fn picked(&self) -> PickedSource {
        match self {
            Sel::Monitor(id) => PickedSource::Monitor { id: *id },
            Sel::Window(id) => PickedSource::Window { id: *id },
            Sel::Pipewire(id) => PickedSource::Pipewire { id: *id },
            Sel::Camera(id) => PickedSource::Camera { id: id.clone() },
        }
    }
    fn key(&self) -> String {
        match self {
            Sel::Monitor(id) => format!("monitor:{id}"),
            Sel::Window(id) => format!("window:{id}"),
            Sel::Pipewire(id) => format!("pipewire:{id}"),
            Sel::Camera(id) => format!("camera:{id}"),
        }
    }
}

pub enum PickerEvent {
    Submit(Vec<Sel>),
    /// A portal or detection failure worth the troubleshooting dialog.
    CaptureError { error: String, detecting: bool },
}

/// A preview tile animating in or out (`AnimatePresence mode="popLayout"`).
struct Tile {
    sel: Sel,
    opacity: Anim,
    scale: Anim,
    /// The tile's box in the preview row (motion `layout`, spring
    /// 420/34/0.75). Exiting tiles keep theirs (`popLayout`).
    rect: LinRect,
    /// `objectFit` at the time it last rendered (exiting tiles keep theirs).
    cover: bool,
    leaving: bool,
}

/// A box whose edges are linear in the container size: `x = ax·W + bx`,
/// `w = aw·W + bw`, `y = ay·H + by`, `h = ah·H + bh`. Animating the eight
/// coefficients is the same as animating the pixel rect for a fixed box,
/// and lets a tile's flex slot (fractions minus gaps) and the hover card's
/// fixed top-right rect share one representation for the handoff morph.
#[derive(Clone, Copy)]
struct LinRect([Anim; 8]);

const LAYOUT: Spring = Spring::new(420.0, 34.0).with_mass(0.75);

impl LinRect {
    fn at(c: [f32; 8]) -> Self {
        LinRect(c.map(Anim::new))
    }
    fn set(&mut self, c: [f32; 8]) {
        for (a, v) in self.0.iter_mut().zip(c) {
            a.set(v, Curve::Spring(LAYOUT));
        }
    }
    fn tick(&mut self) -> bool {
        let mut any = false;
        for a in &mut self.0 {
            any |= a.tick();
        }
        any
    }
    fn resolve(&self, row: Bounds<Pixels>) -> Bounds<Pixels> {
        let v = self.0.map(|a| a.get());
        let (w, h) = (f32::from(row.size.width), f32::from(row.size.height));
        Bounds {
            origin: gpui_kit::gpui::point(row.origin.x + px(v[0] * w + v[1]), row.origin.y + px(v[4] * h + v[5])),
            size: gpui_kit::gpui::size(px((v[2] * w + v[3]).max(0.0)), px((v[6] * h + v[7]).max(0.0))),
        }
    }
}

/// Slot `i` of `n` in the preview row (flex, `gap: 4px` when n > 1).
fn slot_rect(i: usize, n: usize) -> [f32; 8] {
    let (i, nf) = (i as f32, n.max(1) as f32);
    let g = if n > 1 { 4.0 } else { 0.0 };
    [i / nf, g * (i - i * (nf - 1.0) / nf), 1.0 / nf, -g * (nf - 1.0) / nf, 0.0, 0.0, 1.0, 0.0]
}

/// One window hover card (`key = hover-<id>`): enter opacity 0→1 + scale
/// 0.92→1, exit opacity only, all on spring 420/32/0.7. Hovering another
/// row exits this card and enters a new one, so they crossfade.
struct HoverCard {
    id: u32,
    opacity: Anim,
    scale: Anim,
    leaving: bool,
}

const HOVER_SPRING: Spring = Spring::new(420.0, 32.0).with_mass(0.7);

pub struct SourcePicker {
    pub sources: Option<CaptureSourceList>,
    cameras: Vec<CameraDevice>,
    error: Option<String>,
    tab: Tab,
    selected: Vec<Sel>,
    hovered_window: Option<u32>,
    hover_cards: Vec<HoverCard>,
    is_wayland: bool,
    scroll: ScrollHandle,
    tiles: Vec<Tile>,
    empty_text: Anim,
    tab_bounds: HashMap<u8, Bounds<Pixels>>,
    indicator_x: Anim,
    indicator_w: Anim,
    indicator_ready: bool,
    hint: Anim,
    hint_y: Anim,
    hint_shown: bool,
    pub submit_label: SharedString,
    shift_down: bool,
    /// Live, mirrored preview of the selected camera (getUserMedia in the web app).
    camera_preview: Option<(String, Option<(Arc<RenderImage>, f32)>)>,
    camera_task: Option<gpui_kit::gpui::Task<()>>,
}

impl EventEmitter<PickerEvent> for SourcePicker {}

impl SourcePicker {
    pub fn new(submit_label: SharedString, cx: &mut Context<Self>) -> Self {
        cx.observe(&previews::entity(cx), |_, _, cx| cx.notify()).detach();
        let mut p = Self {
            sources: None,
            cameras: Vec::new(),
            error: None,
            tab: Tab::Screens,
            selected: Vec::new(),
            hovered_window: None,
            hover_cards: Vec::new(),
            is_wayland: std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false),
            scroll: ScrollHandle::new(),
            tiles: Vec::new(),
            empty_text: Anim::new(1.0),
            tab_bounds: HashMap::new(),
            indicator_x: Anim::new(0.0),
            indicator_w: Anim::new(0.0),
            indicator_ready: false,
            hint: Anim::new(0.0),
            hint_y: Anim::new(5.0),
            hint_shown: false,
            submit_label,
            shift_down: false,
            camera_preview: None,
            camera_task: None,
        };
        p.refresh(cx);
        p
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        if self.is_wayland {
            self.sources = Some(CaptureSourceList { monitors: vec![], windows: vec![] });
            if self.cameras.is_empty() {
                self.tab = Tab::Cast;
            }
        } else {
            self.sources = None;
            let fut = app::tokio(cx, async {
                tokio::task::spawn_blocking(lookout_core::sources::list_capture_sources)
                    .await
                    .map_err(|e| e.to_string())?
            });
            cx.spawn(async move |this, cx| {
                let res = fut.await;
                this.update(cx, |this, cx| {
                    match res {
                        Ok(list) => {
                            // Pre-select the remembered monitors if they all still exist,
                            // else the primary (or first).
                            if this.selected.is_empty() {
                                let remembered = store(cx).last_monitor_selection.clone();
                                let all_there = !remembered.is_empty()
                                    && remembered.iter().all(|id| list.monitors.iter().any(|m| m.id == *id));
                                if all_there {
                                    this.set_selected(remembered.into_iter().map(Sel::Monitor).collect());
                                } else if let Some(m) =
                                    list.monitors.iter().find(|m| m.is_primary).or_else(|| list.monitors.first())
                                {
                                    this.set_selected(vec![Sel::Monitor(m.id)]);
                                }
                            }
                            this.sources = Some(list);
                        }
                        Err(e) => {
                            log::error!("[sources] failed to list sources: {e}");
                            this.error = Some(e.clone());
                            cx.emit(PickerEvent::CaptureError { error: e, detecting: true });
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let cams = app::tokio(cx, async {
            tokio::task::spawn_blocking(crate::platform::camera::list_devices).await.unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let cams = cams.await;
            this.update(cx, |this, cx| {
                this.cameras = cams;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The hover card's image box for window `id`: `right: 12, top: 20`,
    /// 240 wide with the window's aspect (maxHeight 180), inside a 1px border.
    fn hover_card_size(&self, id: u32) -> (f32, f32) {
        let info = self.sources.as_ref().and_then(|s| s.windows.iter().find(|w| w.id == id));
        let aspect = info.map(|w| (w.width as f32 / w.height.max(1) as f32).clamp(0.5, 3.0)).unwrap_or(16.0 / 9.0);
        let w: f32 = 240.0;
        let h = (w / aspect).min(180.0);
        ((h * aspect.min(w / h.max(1.0))).min(240.0), h)
    }

    fn set_selected(&mut self, next: Vec<Sel>) {
        // Tiles: new non-camera selections enter, removed ones leave. With
        // `popLayout` a leaving tile is popped out of the row at its last
        // box; the ones that stay spring (`layout`) to their new slots and
        // new ones appear in theirs.
        for tile in &mut self.tiles {
            if !next.contains(&tile.sel) && !tile.leaving {
                tile.leaving = true;
                tile.opacity.set(0.0, Curve::tween(220.0, Ease::EASE_OUT));
                tile.scale.set(0.98, Curve::Spring(Spring::new(360.0, 32.0).with_mass(0.7)));
            }
        }
        let present: Vec<Sel> = next.iter().filter(|s| !matches!(s, Sel::Camera(_))).cloned().collect();
        let n = present.len();
        for (i, sel) in present.iter().enumerate() {
            let target = slot_rect(i, n);
            if let Some(t) = self.tiles.iter_mut().find(|t| &t.sel == sel) {
                if t.leaving {
                    // Re-entering while exiting: back from where it is.
                    t.leaving = false;
                    t.opacity.set(1.0, Curve::tween(220.0, Ease::EASE_OUT));
                    t.scale.set(1.0, Curve::Spring(Spring::new(360.0, 32.0).with_mass(0.7)));
                }
                t.rect.set(target);
                t.cover = n > 1;
            } else {
                let from_scale = if matches!(sel, Sel::Window(_)) { 1.08 } else { 1.02 };
                // Shared `layoutId hover-to-main-preview-<id>`: picking the
                // window whose hover card is showing flies the card's image
                // into its tile on the tile's layout spring.
                let handoff = match sel {
                    Sel::Window(id) if self.hovered_window == Some(*id) => self
                        .hover_cards
                        .iter()
                        .any(|c| c.id == *id && !c.leaving && c.opacity.get() > 0.01)
                        .then(|| self.hover_card_size(*id)),
                    _ => None,
                };
                let rect = match handoff {
                    Some((cw, ch)) => {
                        let mut r = LinRect::at([1.0, -(12.0 + cw) + 1.0, 0.0, cw - 2.0, 0.0, 21.0, 0.0, ch - 2.0]);
                        r.set(target);
                        r
                    }
                    None => LinRect::at(target),
                };
                self.tiles.push(Tile {
                    sel: sel.clone(),
                    opacity: Anim::from_to(0.0, 1.0, Curve::tween(220.0, Ease::EASE_OUT)),
                    scale: Anim::from_to(from_scale, 1.0, Curve::Spring(Spring::new(360.0, 32.0).with_mass(0.7))),
                    rect,
                    cover: n > 1,
                    leaving: false,
                });
            }
        }
        let camera = next.iter().find_map(|s| if let Sel::Camera(id) = s { Some(id.clone()) } else { None });
        if camera.as_deref() != self.camera_preview.as_ref().map(|(id, _)| id.as_str()) {
            self.camera_preview = camera.map(|id| (id, None));
            self.camera_task = None;
        }
        let empty = next.iter().all(|s| matches!(s, Sel::Camera(_)));
        self.empty_text.set(if empty { 1.0 } else { 0.0 }, Curve::tween(300.0, Ease::MOTION_DEFAULT));
        self.selected = next;
    }

    fn select(&mut self, sel: Sel, shift: bool, cx: &mut Context<Self>) {
        if matches!(sel, Sel::Camera(_)) || !shift {
            self.set_selected(vec![sel]);
        } else {
            let mut next = self.selected.clone();
            if next.first().map(|f| f.kind() != sel.kind()).unwrap_or(false) {
                next = vec![sel];
            } else if let Some(i) = next.iter().position(|s| *s == sel) {
                next.remove(i);
            } else {
                next.push(sel);
            }
            self.set_selected(next);
        }
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let monitors: Vec<u32> = self.selected.iter().filter_map(|s| if let Sel::Monitor(id) = s { Some(*id) } else { None }).collect();
        if !monitors.is_empty() {
            store_mut(cx).last_monitor_selection = monitors;
            store(cx).save();
        }
        cx.emit(PickerEvent::Submit(self.selected.clone()));
    }

    fn cast(&mut self, add: bool, cx: &mut Context<Self>) {
        let core = app::state(cx).core.clone();
        let fut = app::tokio(cx, async move {
            if add { core.add_screencast().await } else { core.request_screencast().await }
        });
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(streams) if !streams.is_empty() => {
                        let new: Vec<Sel> = streams.iter().map(|s| Sel::Pipewire(s.node_id)).collect();
                        if add {
                            let mut next = this.selected.clone();
                            for s in new {
                                if !next.contains(&s) {
                                    next.push(s);
                                }
                            }
                            this.set_selected(next);
                        } else {
                            this.set_selected(new);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let cancelled = regex_lite_cancel(&e);
                        if cancelled {
                            log::info!("[sources] screencast dismissed by user: {e}");
                        } else {
                            log::error!("[sources] screencast failed: {e}");
                            cx.emit(PickerEvent::CaptureError { error: e, detecting: false });
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn tabs(&self) -> Vec<(Tab, String)> {
        let src = self.sources.as_ref();
        let n_mon = src.map(|s| s.monitors.len()).unwrap_or(0);
        let n_win = src.map(|s| s.windows.len()).unwrap_or(0);
        let mut tabs = Vec::new();
        if self.is_wayland {
            tabs.push((Tab::Cast, "Cast ".to_string()));
        } else {
            tabs.push((Tab::Screens, format!("Screens ({n_mon})")));
            if n_win > 0 {
                tabs.push((Tab::Windows, format!("Windows ({n_win})")));
            }
        }
        if !self.cameras.is_empty() {
            tabs.push((Tab::Cameras, format!("Cameras ({})", self.cameras.len())));
        }
        tabs
    }
}

/// `isCancelledCaptureError`: `/cancell?ed|response error: cancelled|user dismissed/i`.
fn regex_lite_cancel(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("canceled") || l.contains("cancelled") || l.contains("user dismissed")
}

fn tab_index(tab: Tab) -> u8 {
    match tab {
        Tab::Screens => 0,
        Tab::Windows => 1,
        Tab::Cameras => 2,
        Tab::Cast => 3,
    }
}

impl Render for SourcePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();

        // Error state.
        if let Some(err) = self.error.clone() {
            let e = cx.entity();
            let e2 = cx.entity();
            let err_copy = err.clone();
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .min_h(px(200.0))
                .p(px(24.0))
                .child(error_page(
                    "Failed to detect displays",
                    err.clone(),
                    Some((
                        "Retry".into(),
                        std::rc::Rc::new(move |_: &gpui_kit::gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                            e.update(cx, |p, cx| {
                                previews::entity(cx).update(cx, |pv, cx| pv.clear(cx));
                                p.refresh(cx)
                            })
                        }),
                    )),
                    Some(std::rc::Rc::new(move |_: &gpui_kit::gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                        cx.write_to_clipboard(gpui_kit::gpui::ClipboardItem::new_string(err_copy.clone()));
                    })),
                    &t,
                ))
                .child(div().mt(px(12.0)).child(
                    Button::new("troubleshoot", "Troubleshoot").variant(Variant::Ghost).size(BtnSize::Sm).on_click(move |_, _, cx| {
                        let err = err.clone();
                        e2.update(cx, |_, cx| cx.emit(PickerEvent::CaptureError { error: err, detecting: true }))
                    }),
                ))
                .into_any_element();
        }

        // Loading.
        let Some(sources) = self.sources.as_ref() else {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .min_h(px(200.0))
                .p(px(24.0))
                .gap(px(12.0))
                .child(Spinner::new(SpinnerSize::Md))
                .child(div().ts(14.0).text_color(t.text_secondary).text_center().child("Detecting displays..."))
                .into_any_element();
        };
        let monitors_len = sources.monitors.len();
        let windows_len = sources.windows.len();

        // Advance animations.
        let mut moving = false;
        for tile in &mut self.tiles {
            moving |= tile.opacity.tick() | tile.scale.tick() | tile.rect.tick();
        }
        self.tiles.retain(|t| !(t.leaving && !t.opacity.is_animating() && !t.scale.is_animating()));
        for c in &mut self.hover_cards {
            moving |= c.opacity.tick() | c.scale.tick();
        }
        self.hover_cards.retain(|c| !(c.leaving && !c.opacity.is_animating()));
        moving |= self.empty_text.tick() | self.indicator_x.tick() | self.indicator_w.tick();
        moving |= self.hint.tick() | self.hint_y.tick();
        if moving {
            window.request_animation_frame();
        }

        // Tab indicator follows the active tab (layoutId spring 350/35).
        let active_i = tab_index(self.tab);
        if let Some(b) = self.tab_bounds.get(&active_i).copied() {
            let (x, w) = (f32::from(b.origin.x), f32::from(b.size.width));
            if !self.indicator_ready {
                self.indicator_ready = true;
                self.indicator_x.jump(x);
                self.indicator_w.jump(w);
            } else if (self.indicator_x.target() - x).abs() > 0.5 || (self.indicator_w.target() - w).abs() > 0.5 {
                self.indicator_x.set(x, Curve::Spring(Spring::new(350.0, 35.0)));
                self.indicator_w.set(w, Curve::Spring(Spring::new(350.0, 35.0)));
                window.request_animation_frame();
            }
        }

        // Shift hint pill: screens with >1 monitor or windows with >1 window.
        let want_hint = (self.tab == Tab::Screens && monitors_len > 1) || (self.tab == Tab::Windows && windows_len > 1);
        if want_hint != self.hint_shown {
            self.hint_shown = want_hint;
            let sp = Curve::Spring(Spring::new(900.0, 30.0));
            if want_hint {
                // Re-shown mid-exit: AnimatePresence re-enters from where it is.
                if self.hint.get() <= 0.001 && !self.hint.is_animating() {
                    self.hint_y.jump(5.0);
                }
                self.hint_y.set(0.0, sp);
                self.hint.set(1.0, sp);
            } else {
                self.hint.set(0.0, sp);
                self.hint_y.set(10.0, sp);
            }
            window.request_animation_frame();
        }

        // Hover card target (windows tab, hovered row not selected).
        let hover_target = (self.tab == Tab::Windows)
            .then_some(self.hovered_window)
            .flatten()
            .filter(|id| !self.selected.contains(&Sel::Window(*id)));
        let current = self.hover_cards.iter().find(|c| !c.leaving).map(|c| c.id);
        if current != hover_target {
            for c in self.hover_cards.iter_mut().filter(|c| !c.leaving && Some(c.id) != hover_target) {
                c.leaving = true;
                c.opacity.set(0.0, Curve::Spring(HOVER_SPRING));
            }
            if let Some(id) = hover_target {
                if let Some(c) = self.hover_cards.iter_mut().find(|c| c.id == id) {
                    c.leaving = false;
                    c.opacity.set(1.0, Curve::Spring(HOVER_SPRING));
                    c.scale.set(1.0, Curve::Spring(HOVER_SPRING));
                } else {
                    self.hover_cards.push(HoverCard {
                        id,
                        opacity: Anim::from_to(0.0, 1.0, Curve::Spring(HOVER_SPRING)),
                        scale: Anim::from_to(0.92, 1.0, Curve::Spring(HOVER_SPRING)),
                        leaving: false,
                    });
                }
            }
            window.request_animation_frame();
        }

        let has_tabs = windows_len > 0 || !self.cameras.is_empty() || self.is_wayland;
        let camera_selected = self.selected.iter().any(|s| matches!(s, Sel::Camera(_)));

        // ── Preview box ───────────────────────────────────────────────
        let preview_box = self.preview_box(&t, camera_selected, window, cx);

        // ── Tabs ──────────────────────────────────────────────────────
        let tabs_el = has_tabs.then(|| self.tabs_bar(&t, cx));

        // ── List ──────────────────────────────────────────────────────
        let list = self.list(&t, cx);
        let offset = -f32::from(self.scroll.offset().y);
        let max = f32::from(self.scroll.max_offset().y);
        let (show_top, show_bottom) = (offset > 0.5, offset + 0.5 < max);

        let hint_o = self.hint.get().clamp(0.0, 1.0);
        let hint = (hint_o > 0.001).then(|| {
            let kbd = |label: &'static str| {
                div()
                    .border_1()
                    .border_color(theme::white(0.3))
                    .bg(theme::white(0.1))
                    .rounded(px(4.0))
                    .px(px(4.0))
                    .py(px(2.0))
                    .child(label)
            };
            div()
                .absolute()
                .bottom(px(-self.hint_y.get()))
                .right(px(16.0))
                .opacity(hint_o)
                .bg(theme::black(0.7))
                .text_color(theme::hex(0xffffff))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .ts(11.0)
                .flex()
                .items_center()
                .shadow(vec![theme::sh(0.0, 4.0, 12.0, 0.0, theme::black(0.15))])
                .child(div().mr(px(4.0)).child(kbd("shift")))
                .child("+")
                .child(div().ml(px(4.0)).mr(px(6.0)).child(kbd("click")))
                .child("to select multiple")
        });

        let entity = cx.entity();
        div()
            .size_full()
            .max_w(px(480.0))
            .mx_auto()
            .p(px(16.0))
            .flex()
            .flex_col()
            .on_modifiers_changed(cx.listener(|this, ev: &gpui_kit::gpui::ModifiersChangedEvent, _, _| {
                this.shift_down = ev.modifiers.shift;
            }))
            .child(div().flex_none().child(preview_box).children(tabs_el))
            .child(
                div()
                    .flex_1()
                    .relative()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    // CSS mask-image-style edge fades (12px), as SourcePicker.tsx.
                    .child(edge_faded(
                        div()
                            .id("source-list")
                            .flex()
                            .flex_col()
                            .gap(px(4.0))
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                            .pb(px(4.0))
                            .children(list),
                        if show_top { 12.0 } else { 0.0 },
                        if show_bottom { 12.0 } else { 0.0 },
                    ))
                    .children(hint),
            )
            .child(div().flex_none().when(!self.selected.is_empty(), |d| {
                d.child(
                    div().mt(px(16.0)).child(
                        Button::new("picker-submit", self.submit_label.clone())
                            .size(BtnSize::Lg)
                            .full_width()
                            .on_click(move |_, _, cx| entity.update(cx, |p, cx| p.submit(cx))),
                    ),
                )
            }))
            .into_any_element()
    }
}

impl SourcePicker {
    /// Start streaming the selected camera into the preview box (~10 fps).
    fn ensure_camera_preview(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = self.camera_preview.clone() else { return };
        if self.camera_task.is_some() {
            return;
        }
        let rt = app::state(cx).rt.clone();
        self.camera_task = Some(cx.spawn(async move |this, cx| {
            let cam = Arc::new(crate::platform::camera::Camera::new());
            let c2 = cam.clone();
            let id2 = id.clone();
            let started = rt.spawn(async move { c2.start(&id2).await }).await.map_err(|e| e.to_string()).and_then(|r| r);
            if let Err(e) = started {
                log::warn!("[camera] preview failed: {e}");
                return;
            }
            loop {
                let c3 = cam.clone();
                let id3 = id.clone();
                let frame = rt.spawn(async move { c3.grab(&id3).await }).await.ok().and_then(|r| r.ok());
                if let Some(f) = frame {
                    if let Ok(img) = image::load_from_memory(&f.jpeg) {
                        // `transform: scaleX(-1)` — a mirror, like a selfie view.
                        let img = image::imageops::flip_horizontal(&img.into_rgba8());
                        let aspect = f.width as f32 / f.height.max(1) as f32;
                        let render = crate::images::rgba_to_render_image(img);
                        let alive = this.update(cx, |p, cx| {
                            if let Some((_, slot)) = &mut p.camera_preview {
                                if let Some((old, _)) = slot.replace((render, aspect)) {
                                    cx.drop_image(old, None);
                                }
                            }
                            cx.notify();
                        });
                        if alive.is_err() {
                            break;
                        }
                    }
                }
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let still = this.update(cx, |p, _| p.camera_preview.as_ref().map(|(i, _)| *i == id).unwrap_or(false)).unwrap_or(false);
                if !still {
                    break;
                }
            }
            cam.stop();
        }));
    }

    fn preview_box(&mut self, t: &Theme, camera_selected: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if camera_selected {
            self.ensure_camera_preview(cx);
            if let Some((_, Some((img, aspect)))) = self.camera_preview.clone() {
                let mut b = div().relative().rounded(px(10.0)).overflow_hidden().bg(t.bg_sunken).border_1().border_color(t.border_default).mb(px(16.0));
                b.style().aspect_ratio = Some(2.0);
                return b
                    .child(canvas(|_, _, _| (), move |bounds, _, window, _| paint::paint_fit_image(bounds, img.clone(), aspect, false, 1.0, 0.0, window)).absolute().inset_0())
                    .into_any_element();
            }
        }
        let mut row = div().absolute().inset_0().overflow_hidden();
        let mut any_tile = false;
        for tile in &self.tiles {
            let Some(src) = tile.sel.capture() else { continue };
            any_tile = true;
            let img = previews::get(&src, Duration::from_millis(1000), true, cx);
            let (o, s) = (tile.opacity.get(), tile.scale.get());
            let (cover, rect) = (tile.cover, tile.rect);
            // Drawn into the animated box with object-fit re-applied each
            // frame (motion distorts the img with a scale transform during
            // a layout animation; GPUI has no transforms, so the box
            // interpolates and the image stays undistorted).
            let el: AnyElement = match img {
                Some((img, aspect)) => canvas(|_, _, _| (), move |bounds, _, window, _| {
                    paint::paint_fit_image(rect.resolve(bounds), img.clone(), aspect, cover, s, 0.0, window);
                })
                .absolute()
                .inset_0()
                .into_any_element(),
                None => {
                    let v = rect.0.map(|a| a.get());
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(gpui_kit::gpui::relative(v[0].clamp(0.0, 1.0)))
                        .w(gpui_kit::gpui::relative(v[2].clamp(0.0, 1.0)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Spinner::new(SpinnerSize::Sm))
                        .into_any_element()
                }
            };
            row = row.child(div().absolute().inset_0().opacity(o).child(el));
        }
        let empty_o = self.empty_text.get();
        let empty = (empty_o > 0.001 || !any_tile).then(|| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .opacity(empty_o)
                .child(
                    div().ts(13.0).text_color(t.text_quaternary).text_center().child(if camera_selected {
                        "Starting camera preview..."
                    } else {
                        "Select a source below"
                    }),
                )
        });

        // Window hover cards (top-right). Enter scales from 0.92 with the
        // opacity; exit fades only, keeping its scale.
        let hover_cards: Vec<AnyElement> = self
            .hover_cards
            .iter()
            .map(|card| {
                let (hc, sc) = (card.opacity.get(), card.scale.get());
                let id = card.id;
                let img = previews::get(&CaptureSource::Window { id }, Duration::from_millis(200), false, cx);
                let (w, h) = self.hover_card_size(id);
                let scale = sc.clamp(0.0, 1.2);
                let (surface, border) = (t.bg_surface, t.border_default);
                let tertiary = t.text_tertiary;
                let missing = img.is_none();
                div()
                    .absolute()
                    .right(px(12.0))
                    .top(px(20.0))
                    .w(px(w))
                    .h(px(h))
                    .opacity(hc.clamp(0.0, 1.0))
                    .child(
                        canvas(|_, _, _| (), move |bounds, _, window, _| {
                            let b = paint::scale_bounds(bounds, scale);
                            window.paint_drop_shadows(b, gpui_kit::gpui::Corners::all(px(8.0 * scale)), &[theme::sh(0.0, 10.0, 30.0, 0.0, theme::black(0.22))]);
                            paint::paint_rounded_bg(bounds, 8.0, scale, surface, border, window);
                            if let Some((img, a)) = img.clone() {
                                paint::paint_fit_image(paint::inset(b, 1.0), img, a, false, 1.0, 7.0, window);
                            }
                        })
                        .absolute()
                        .inset_0(),
                    )
                    .when(missing, |d| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .ts(11.0)
                                .text_color(tertiary)
                                .child("Loading preview..."),
                        )
                    })
                    .into_any_element()
            })
            .collect();

        let mut b = div()
            .relative()
            .rounded(px(10.0))
            .overflow_hidden()
            .bg(t.bg_sunken)
            .border_1()
            .border_color(t.border_default)
            .mb(px(16.0));
        b.style().aspect_ratio = Some(2.0);
        let _ = window;
        b.child(row).children(empty).children(hover_cards).into_any_element()
    }

    fn tabs_bar(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let tabs = self.tabs();
        let ix = self.indicator_x.get();
        let iw = self.indicator_w.get();
        let indicator_color = t.border_default;
        let bar_origin = self.tab_bounds.get(&255).map(|b| f32::from(b.origin.x)).unwrap_or(0.0);
        let probe = cx.entity();
        let mut bar = div()
            .relative()
            .flex()
            .gap(px(4.0))
            .mb(px(12.0))
            .bg(t.bg_surface)
            .rounded(px(8.0))
            .p(px(4.0))
            .child(
                canvas(move |b, _, cx| probe.update(cx, |p, _| { p.tab_bounds.insert(255, b); }), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            );
        if self.indicator_ready && iw > 0.0 {
            bar = bar.child(
                div()
                    .absolute()
                    .top(px(4.0))
                    .bottom(px(4.0))
                    .left(px(ix - bar_origin))
                    .w(px(iw))
                    .rounded(px(6.0))
                    .bg(indicator_color),
            );
        }
        for (tab, label) in tabs {
            let active = tab == self.tab;
            let i = tab_index(tab);
            let e = cx.entity();
            let probe = cx.entity();
            bar = bar.child(
                div()
                    .id(("tab", i as usize))
                    .relative()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(8.0))
                    .py(px(6.0))
                    .ts(12.0)
                    .when(active, |d| d.w600())
                    .when(!active, |d| d.w500())
                    .text_color(if active { t.text_primary } else { t.text_secondary })
                    .rounded(px(6.0))
                    .child(
                        canvas(move |b, _, cx| probe.update(cx, |p, _| { p.tab_bounds.insert(i, b); }), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    )
                    .child(div().relative().child(label))
                    .on_click(move |_, _, cx| {
                        e.update(cx, |p, cx| {
                            p.tab = tab;
                            p.hovered_window = None;
                            cx.notify();
                        })
                    }),
            );
        }
        bar.into_any_element()
    }

    fn list(&mut self, t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.sources.is_none() {
            return vec![];
        }
        let mut out: Vec<AnyElement> = Vec::new();
        if self.tab == Tab::Cast {
            out.push(self.cast_tab(t, cx));
            return out;
        }
        let sources = self.sources.as_ref().unwrap();
        let show_screens = self.tab == Tab::Screens || (!self.is_wayland && sources.windows.is_empty() && self.cameras.is_empty());
        match self.tab {
            Tab::Cast => {}
            Tab::Windows => {
                for w in &sources.windows {
                    let sel = Sel::Window(w.id);
                    let title: SharedString = if w.app_name.is_empty() { w.title.clone().into() } else { w.app_name.clone().into() };
                    let sub = format!(
                        "{}{}x{}",
                        if !w.title.is_empty() && !w.app_name.is_empty() { format!("{} — ", w.title) } else { String::new() },
                        w.width,
                        w.height
                    );
                    let badge = w.is_minimized.then(|| {
                        div()
                            .ts(10.0)
                            .w500()
                            .text_color(t.text_secondary)
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(6.0))
                            .child("Minimized")
                            .into_any_element()
                    });
                    let id = w.id;
                    let row = source_row(&sel, title, Some(sub.into()), badge, self.selected.contains(&sel), t, cx)
                        .on_hover(cx.listener(move |p, hovered: &bool, _, cx| {
                            if *hovered {
                                p.hovered_window = Some(id);
                            } else if p.hovered_window == Some(id) {
                                p.hovered_window = None;
                            }
                            cx.notify();
                        }));
                    out.push(div().when(w.is_minimized, |d| d.opacity(0.5)).child(row).into_any_element());
                }
            }
            Tab::Cameras => {
                for (i, c) in self.cameras.iter().enumerate() {
                    let sel = Sel::Camera(c.id.clone());
                    let label: SharedString =
                        if c.label.is_empty() { format!("Camera {}", i + 1).into() } else { c.label.clone().into() };
                    out.push(source_row(&sel, label, None, None, self.selected.contains(&sel), t, cx).into_any_element());
                }
            }
            Tab::Screens => {}
        }
        if show_screens && self.tab != Tab::Cast {
            for m in &sources.monitors {
                let sel = Sel::Monitor(m.id);
                let sub = format!(
                    "{}x{}{}",
                    m.width,
                    m.height,
                    if m.scale_factor > 1.0 { format!(" @ {}x", trim_float(m.scale_factor)) } else { String::new() }
                );
                let badge = m.is_primary.then(|| {
                    div()
                        .ts(10.0)
                        .w600()
                        .text_color(t.badge_primary_text)
                        .bg(t.badge_primary_bg)
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(6.0))
                        .child("Primary")
                        .into_any_element()
                });
                out.push(source_row(&sel, m.name.clone().into(), Some(sub.into()), badge, self.selected.contains(&sel), t, cx).into_any_element());
            }
        }
        out
    }

    fn cast_tab(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pipewire: Vec<u32> = self.selected.iter().filter_map(|s| if let Sel::Pipewire(id) = s { Some(*id) } else { None }).collect();
        let e1 = cx.entity();
        let e2 = cx.entity();
        let e3 = cx.entity();
        let mut col = div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap(px(12.0))
            .p(px(20.0))
            .child(
                div()
                    .text_center()
                    .ts(13.0)
                    .text_color(t.text_secondary)
                    .child("Wayland requires explicit permission to capture screens or windows."),
            );
        if !pipewire.is_empty() {
            let mut rows = div().w_full().flex().flex_col().gap(px(4.0)).my(px(8.0));
            for (i, _) in pipewire.iter().enumerate() {
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .p(px(12.0))
                        .bg(t.bg_selected)
                        .border_1()
                        .border_color(t.border_selected)
                        .rounded(px(8.0))
                        .w_full()
                        .child(radio(true, t))
                        .child(div().ts(13.0).w600().text_color(t.text_primary).child(format!("Cast Stream {}", i + 1))),
                );
            }
            col = col.child(rows).child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .w_full()
                    .child(
                        Button::new("cast-new", "Select new sets of windows")
                            .variant(Variant::Secondary)
                            .size(BtnSize::Md)
                            .flex_1()
                            .on_click(move |_, _, cx| e1.update(cx, |p, cx| p.cast(false, cx))),
                    )
                    .child(
                        Button::new("cast-add", "+")
                            .variant(Variant::Secondary)
                            .size(BtnSize::Md)
                            .fixed(40.0, 34.0)
                            .padding(0.0, 0.0)
                            .on_click(move |_, _, cx| e2.update(cx, |p, cx| p.cast(true, cx))),
                    ),
            );
        } else {
            col = col.child(
                Button::new("cast-first", "+ Add new Screen/Window")
                    .variant(Variant::Secondary)
                    .size(BtnSize::Md)
                    .on_click(move |_, _, cx| e3.update(cx, |p, cx| p.cast(false, cx))),
            );
        }
        col.into_any_element()
    }
}

fn trim_float(f: f32) -> String {
    let s = format!("{f:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn radio(selected: bool, t: &Theme) -> impl IntoElement {
    div()
        .size(px(18.0))
        .flex_none()
        .rounded_full()
        .border_2()
        .border_color(if selected { t.icon_selected } else { t.text_quaternary })
        .flex()
        .items_center()
        .justify_center()
        .when(selected, |d| d.child(div().size(px(8.0)).rounded_full().bg(t.icon_selected)))
}

/// One selectable row: background layer springs to 0.98 on press.
fn source_row(
    sel: &Sel,
    title: SharedString,
    subtitle: Option<SharedString>,
    badge: Option<AnyElement>,
    selected: bool,
    t: &Theme,
    cx: &mut Context<SourcePicker>,
) -> gpui_kit::gpui::Stateful<gpui_kit::gpui::Div> {
    let key = sel.key();
    let st: Entity<PressState> = cx
        .global::<RowStates>()
        .get(&key)
        .unwrap_or_else(|| {
            let e = cx.new(|_| PressState::new());
            cx.global_mut::<RowStates>().0.insert(key.clone(), e.clone());
            e
        });
    let moving = st.update(cx, |s, _| s.tick());
    if moving {
        cx.notify();
    }
    let scale = 1.0 - st.read(cx).press.get();
    let (fill, border) = if selected { (t.bg_selected, t.border_selected) } else { (t.bg_surface, t.border_default) };
    let (s1, s2, s3) = (st.clone(), st.clone(), st);
    let sel_click = sel.clone();
    div()
        .id(eid("source-row", &key))
        .relative()
        .flex()
        .items_center()
        .gap(px(12.0))
        .p(px(12.0))
        .rounded(px(8.0))
        .w_full()
        .child(
            canvas(|_, _, _| (), move |bounds, _, window, _| paint::paint_rounded_bg(bounds, 8.0, scale, fill, border, window))
                .absolute()
                .inset_0(),
        )
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .gap(px(12.0))
                .w_full()
                .child(radio(selected, t))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .ts(13.0)
                                .w600()
                                .text_color(t.text_primary)
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                                .children(badge),
                        )
                        .children(subtitle.map(|s| {
                            div().ts(11.0).text_color(t.text_tertiary).overflow_hidden().whitespace_nowrap().text_ellipsis().child(s)
                        })),
                ),
        )
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            s1.update(cx, |s, cx| {
                s.press.set(0.02, Curve::Spring(Spring::PRESS));
                cx.notify();
            })
        })
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            s2.update(cx, |s, cx| {
                s.press.set(0.0, Curve::Spring(Spring::PRESS));
                cx.notify();
            })
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
            s3.update(cx, |s, cx| {
                s.press.set(0.0, Curve::Spring(Spring::PRESS));
                cx.notify();
            })
        })
        .on_click(cx.listener(move |p, ev: &gpui_kit::gpui::ClickEvent, _, cx| {
            let shift = ev.modifiers().shift || p.shift_down;
            p.select(sel_click.clone(), shift, cx)
        }))
}

/// Press states for picker rows, keyed by source (rows are rebuilt each
/// render; their spring has to survive that).
#[derive(Default)]
pub struct RowStates(pub HashMap<String, Entity<PressState>>);
impl gpui_kit::gpui::Global for RowStates {}

impl RowStates {
    fn get(&self, k: &str) -> Option<Entity<PressState>> {
        self.0.get(k).cloned()
    }
}

#[allow(unused)]
fn _unused(_: Arc<RenderImage>, _: Hsla, _: ElementId) {
    let _ = press_values;
}
