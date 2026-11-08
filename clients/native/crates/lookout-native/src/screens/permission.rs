//! The macOS setup page: one row per privacy permission, each with its own
//! "Allow" and macOS's own icon for it (after Codex Computer Use's). Screen
//! Recording and Accessibility (which window and file you're in) are
//! required to continue, though "Skip for now" gets past the page until the
//! next launch; Camera is optional. "Allow" opens System Settings with the
//! permission guide, whose card flies out of the row itself
//! (`platform::permission_guide`), so each row reports where it is on screen.
//! Everywhere else permission is assumed (the page never shows).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::gpui::{
    AnyElement, Bounds, Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
    Styled, Task, Window, canvas, div, img, prelude::FluentBuilder, px,
};

use crate::platform::permission_guide::{self, Phase, Source};
use crate::platform::permissions::{self, Kind};
use crate::theme::{Theme, black, fade, sh, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::icon::Icon;
use crate::ui::spinner::{Spinner, SpinnerSize};
use crate::ui::text::TextExt;

pub enum PermissionEvent {
    /// Past the page. `cache: false` ("Skip for now") shows it again next launch.
    Done { cache: bool },
}

const KINDS: [Kind; 3] = [Kind::Screen, Kind::Accessibility, Kind::Camera];

type Image = std::sync::Arc<gpui_kit::gpui::RenderImage>;

/// Row icons are drawn at this size (points).
const ROW_ICON: f32 = 48.0;

thread_local! {
    /// Decoded once (at 2× for HiDPI): a fresh `RenderImage` per frame would
    /// leak an atlas tile each time.
    static APP_ICON: Option<Image> = crate::images::decode_fit(crate::assets::APP_ICON_PNG, 176, 176).ok();
    static SYSTEM_ICONS: Vec<Option<Image>> = KINDS
        .iter()
        .map(|k| {
            let px = (ROW_ICON * 2.0) as u32;
            crate::platform::system_icons::png(*k, px).and_then(|b| crate::images::decode_fit(&b, px, px).ok())
        })
        .collect();
}

#[derive(Clone, Copy, PartialEq)]
struct RowState {
    granted: bool,
    phase: Phase,
}

pub struct PermissionScreen {
    rows: [RowState; 3],
    /// Each row's bounds as last painted (window coordinates).
    bounds: [Rc<Cell<Option<Bounds<Pixels>>>>; 3],
    _poll: Task<()>,
}

impl EventEmitter<PermissionEvent> for PermissionScreen {}

impl PermissionScreen {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let rows = KINDS.map(read);
        if rows.iter().all(|r| r.granted) {
            let me = cx.entity();
            cx.defer(move |cx| me.update(cx, |_, cx| cx.emit(PermissionEvent::Done { cache: true })));
        }
        // Grants land in System Settings (or the camera prompt) and the
        // guide moves on its own, so keep the rows in step.
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(60)).await;
                let alive = this
                    .update(cx, |s, cx| {
                        let rows = KINDS.map(read);
                        if rows != s.rows {
                            s.rows = rows;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        });
        Self { rows, bounds: Default::default(), _poll: poll }
    }

    fn allow(&mut self, i: usize, window: &Window, cx: &mut Context<Self>) {
        let _ = window;
        let source = self.bounds[i].get().map(|b| Source {
            x: f64::from(f32::from(b.origin.x)),
            y: f64::from(f32::from(b.origin.y)),
            w: f64::from(f32::from(b.size.width)),
            h: f64::from(f32::from(b.size.height)),
        });
        permissions::request(KINDS[i], source, cx);
        self.rows[i] = read(KINDS[i]);
        cx.notify();
    }
}

fn read(kind: Kind) -> RowState {
    RowState { granted: permissions::check(kind), phase: permission_guide::phase(kind) }
}

/// Title, description, whether it's optional, fallback glyph.
fn copy(kind: Kind) -> (&'static str, &'static str, bool, Icon) {
    match kind {
        Kind::Screen => ("Screen Recording", "Takes the screenshots your timelapse is made of", false, Icon::LkMonitor),
        Kind::Accessibility => ("Accessibility", "Notes which window and file you're working in", false, Icon::LkPencil),
        Kind::Camera => ("Camera", "Adds photos of you working to your timelapse", true, Icon::LkCamera),
    }
}

impl Render for PermissionScreen {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        // Every permission that isn't optional, granted.
        let required_ok = KINDS.iter().zip(&self.rows).all(|(k, r)| r.granted || copy(*k).2);
        // The first row still to do gets the filled button.
        let next = self.rows.iter().position(|r| !r.granted);

        let rows: Vec<AnyElement> = (0..KINDS.len()).map(|i| self.row(i, next == Some(i), &t, cx)).collect();

        let me1 = cx.entity();
        let me2 = cx.entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(460.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .children(APP_ICON.with(|i| i.clone()).map(|i| {
                        // The icon art is a macOS squircle with its own margin.
                        div()
                            .size(px(88.0))
                            .mb(px(14.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(img(i).size(px(88.0)))
                    }))
                    .child(div().ts(24.0).w700().text_color(t.text_primary).mb(px(8.0)).child("Set up Lookout"))
                    .child(
                        div()
                            .ts_lh(13.0, 1.55)
                            .text_color(t.text_secondary)
                            .text_center()
                            .mb(px(24.0))
                            .child("Lookout needs to see your screen to record your work. Everything is captured on your Mac, and only periodic snapshots are uploaded."),
                    )
                    .child(div().w_full().flex().flex_col().gap(px(12.0)).children(rows))
                    .child(
                        div()
                            .w_full()
                            .mt(px(24.0))
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(
                                Button::new("setup-continue", "Continue")
                                    .size(BtnSize::Lg)
                                    .full_width()
                                    .disabled(!required_ok)
                                    .on_click(move |_, _, cx| {
                                        permission_guide::close();
                                        me1.update(cx, |_, cx| cx.emit(PermissionEvent::Done { cache: true }))
                                    }),
                            )
                            .when(!required_ok, |d| {
                                d.child(
                                    Button::new("setup-skip", "Skip for now")
                                        .variant(Variant::Ghost)
                                        .size(BtnSize::Lg)
                                        .full_width()
                                        .on_click(move |_, _, cx| {
                                            permission_guide::close();
                                            me2.update(cx, |_, cx| cx.emit(PermissionEvent::Done { cache: false }))
                                        }),
                                )
                            }),
                    ),
            )
    }
}

impl PermissionScreen {
    fn row(&self, i: usize, is_next: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let kind = KINDS[i];
        let state = self.rows[i];
        let (title, body, optional, glyph) = copy(kind);
        // The card has left for System Settings: the row stays behind as
        // its outline (the flight covers the swap).
        let away = state.phase == Phase::InSettings && !state.granted;
        let probe = self.bounds[i].clone();

        let icon: AnyElement = match SYSTEM_ICONS.with(|v| v[i].clone()) {
            Some(image) => img(image).size(px(ROW_ICON)).flex_none().into_any_element(),
            None => div()
                .size(px(ROW_ICON))
                .flex_none()
                .rounded(px(12.0))
                .bg(t.bg_sunken)
                .flex()
                .items_center()
                .justify_center()
                .child(glyph.el(22.0, t.text_secondary))
                .into_any_element(),
        };

        let trailing: AnyElement = if state.granted {
            div()
                .h(px(30.0))
                .px(px(12.0))
                .rounded_full()
                .bg(fade(t.accent, 0.14))
                .flex()
                .items_center()
                .gap(px(5.0))
                .child(Icon::LkCheckLine.el(14.0, t.accent))
                .child(div().ts(13.0).w600().text_color(t.accent).child("Allowed"))
                .into_any_element()
        } else if state.phase == Phase::Opening {
            div().w(px(72.0)).flex().justify_center().child(Spinner::new(SpinnerSize::Sm)).into_any_element()
        } else {
            let me = cx.entity();
            Button::new(("setup-allow", i), "Allow")
                .variant(if is_next { Variant::Primary } else { Variant::Secondary })
                .size(BtnSize::Md)
                .radius(999.0)
                .on_click(move |_, window, cx| me.update(cx, |s, cx| s.allow(i, window, cx)))
                .into_any_element()
        };

        div()
            .id(("setup-row", i))
            .relative()
            .w_full()
            .rounded(px(20.0))
            .border_1()
            .map(|d| {
                if away {
                    d.border_dashed().border_color(t.border_hover)
                } else {
                    d.border_color(t.border_default).bg(t.bg_surface).shadow(vec![
                        sh(0.0, 1.0, 2.0, 0.0, black(0.10)),
                        sh(0.0, 10.0, 24.0, -10.0, black(0.35)),
                    ])
                }
            })
            .child(canvas(move |b, _, _| probe.set(Some(b)), |_, _, _, _| {}).absolute().inset_0())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.0))
                    .py(px(14.0))
                    .pl(px(14.0))
                    .pr(px(16.0))
                    .when(away, |d| d.opacity(0.0))
                    .child(icon)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(div().ts(15.0).w600().text_color(t.text_primary).child(title))
                                    .when(optional, |d| d.child(div().ts(11.0).w500().text_color(t.text_tertiary).child("Optional"))),
                            )
                            .child(div().ts_lh(12.5, 1.4).text_color(t.text_secondary).child(body)),
                    )
                    .child(div().flex_none().child(trailing)),
            )
            .when(away, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .ts(12.0)
                        .text_color(t.text_tertiary)
                        .child("Finish in System Settings"),
                )
            })
            .into_any_element()
    }
}
