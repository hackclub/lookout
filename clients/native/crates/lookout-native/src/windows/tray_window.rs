//! The tray popup (`TrayApp.tsx`): a 300×50 borderless, always-on-top
//! window under the menu-bar item / tray icon with the live clock, the
//! capture count and Pause/Resume + Stop. macOS and Windows only — on Linux
//! the tray never reports clicks, so the menu is used instead.

use gpui_kit::gpui::prelude::FluentBuilder;
use gpui_kit::gpui::{
    App, AppContext, Bounds, Context, Global, IntoElement, ParentElement, Pixels, Render, Styled, Subscription,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point, px, size,
};
use lookout_core::ControlMode;

use crate::app::{self, RecorderModel};
use crate::platform::tray::TrayAction;
use crate::theme::{self, is_mac, theme};
use crate::ui::button::{BtnSize, Button, Variant};
use crate::ui::feedback::NumberFlow;
use crate::ui::icon::Icon;
use crate::ui::text::TextExt;

pub struct TrayHandle(pub Option<WindowHandle<TrayApp>>);
impl Global for TrayHandle {}

pub struct TrayApp {
    model: gpui_kit::gpui::Entity<RecorderModel>,
    tick_due: Option<std::time::Instant>,
    _tick: Option<gpui_kit::gpui::Task<()>>,
    _subs: Vec<Subscription>,
}

const W: f32 = 300.0;
const H: f32 = 50.0;

pub fn toggle(rect: Option<Bounds<Pixels>>, cx: &mut App) {
    if cx.try_global::<TrayHandle>().and_then(|h| h.0).is_some() {
        hide(cx);
        return;
    }
    // Centre under the icon; flip above when it would leave the screen
    // (a bottom taskbar on Windows); keep it on the display.
    let display = cx.primary_display();
    let screen = display.as_ref().map(|d| d.bounds()).unwrap_or(Bounds { origin: point(px(0.0), px(0.0)), size: size(px(1920.0), px(1080.0)) });
    let (mut x, mut y) = match rect {
        Some(r) => (
            f32::from(r.origin.x) + f32::from(r.size.width) / 2.0 - W / 2.0,
            f32::from(r.origin.y) + f32::from(r.size.height),
        ),
        None => (f32::from(screen.origin.x) + f32::from(screen.size.width) - W - 12.0, f32::from(screen.origin.y) + 30.0),
    };
    let (sx, sy, sw, sh) = (
        f32::from(screen.origin.x),
        f32::from(screen.origin.y),
        f32::from(screen.size.width),
        f32::from(screen.size.height),
    );
    if y + H > sy + sh {
        if let Some(r) = rect {
            y = f32::from(r.origin.y) - H;
        }
    }
    if x + W > sx + sw {
        x = sx + sw - W;
    }
    if x < sx {
        x = sx;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(x), px(y)), size: size(px(W), px(H)) })),
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: if is_mac() { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Transparent },
        ..Default::default()
    };
    let handle = cx.open_window(options, |window, cx| cx.new(|cx| TrayApp::new(window, cx))).ok();
    cx.set_global(TrayHandle(handle));
}

pub fn hide(cx: &mut App) {
    if let Some(h) = cx.try_global::<TrayHandle>().and_then(|h| h.0) {
        h.update(cx, |_, window, _| window.remove_window()).ok();
        cx.set_global(TrayHandle(None));
    }
}

/// Close the popup when the recording ends.
pub fn sync(cx: &mut App) {
    if !app::state(cx).recorder.read(cx).tray_visible() {
        hide(cx);
    }
}

impl TrayApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = app::state(cx).recorder.clone();
        let subs = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            // Hide on blur, like the Tauri popup.
            cx.observe_window_activation(window, |_, window, cx| {
                if !window.is_window_active() {
                    cx.defer(hide);
                }
            }),
        ];
        Self { model, tick_due: None, _tick: None, _subs: subs }
    }
}

impl Render for TrayApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let state = self.model.read(cx).state.clone();
        let recording = state.as_ref().map(|s| s.control_mode == ControlMode::Recording).unwrap_or(true);
        let count = state.as_ref().map(|s| s.capture_count).unwrap_or(0);
        let secs = state.as_ref().map(|s| s.clock.display_seconds(chrono::Utc::now().timestamp_millis())).unwrap_or(0);
        // Repaint once a second for the clock (the dot doesn't pulse; see
        // RecorderView), not every display frame.
        if let Some(clock) = state.as_ref().map(|s| &s.clock).filter(|c| c.running && c.hold_at_unix_ms.is_none()) {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let next = 1000 - (now_ms - clock.anchor_at_unix_ms).rem_euclid(1000) as u64;
            let due = std::time::Instant::now() + std::time::Duration::from_millis(next.max(16));
            if !self.tick_due.is_some_and(|d| d <= due) {
                self.tick_due = Some(due);
                self._tick = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(next.max(16))).await;
                    this.update(cx, |v, cx| {
                        v.tick_due = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
        }
        let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
        let colon = || div().mx(px(1.0)).relative().top(px(-1.0)).child(":");
        // Keyed by position, the way React reconciles TrayTimer's two
        // branches: at 59:59 → 1:00:00 the first NumberFlow rolls 59 → 1,
        // the second 59 → 00 and the seconds one mounts fresh.
        let mut timer = div().flex().items_center();
        if h > 0 {
            timer = timer
                .child(NumberFlow::new("tray-0", h))
                .child(colon())
                .child(NumberFlow::new("tray-1", m).min_digits(2))
                .child(colon())
                .child(NumberFlow::new("tray-2", s).min_digits(2));
        } else {
            timer = timer
                .child(NumberFlow::new("tray-0", m).min_digits(2))
                .child(colon())
                .child(NumberFlow::new("tray-1", s).min_digits(2));
        }
        let windows_look = !is_mac();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .py(px(8.0))
            .font_family(crate::ui::text::family())
            .when(windows_look, |d| d.bg(theme::rgba8(26, 26, 26, 0.93)))
            .when(!windows_look, |d| d.rounded(px(10.0)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .when(recording, |d| {
                                d.child(div().size(px(8.0)).rounded_full().bg(theme::status::danger()))
                            })
                            .when(!recording, |d| d.child(Icon::LkPauseGlyph.el(12.0, t.text_tertiary)))
                            .child(div().ts(13.0).w700().text_color(t.text_primary).child(timer)),
                    )
                    .child(div().ts(12.0).text_color(t.text_secondary).child(format!("{count} {}", if count == 1 { "shot" } else { "shots" }))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(if recording {
                        Button::new("tray-pause", "Pause")
                            .variant(Variant::Warning)
                            .size(BtnSize::Sm)
                            .on_click(|_, _, cx| super::handle_tray_action(TrayAction::Pause, cx))
                    } else {
                        Button::new("tray-resume", "Resume")
                            .variant(Variant::Success)
                            .size(BtnSize::Sm)
                            .on_click(|_, _, cx| super::handle_tray_action(TrayAction::Resume, cx))
                    })
                    .child(
                        Button::new("tray-stop", "Stop")
                            .variant(Variant::Danger)
                            .size(BtnSize::Sm)
                            .on_click(|_, _, cx| super::handle_tray_action(TrayAction::Stop, cx)),
                    ),
            )
    }
}
