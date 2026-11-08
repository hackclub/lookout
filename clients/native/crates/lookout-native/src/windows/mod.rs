pub mod editor;
pub mod main_window;
pub mod tray_window;

use gpui_kit::gpui::{App, Global, WindowHandle};

use crate::app::{self, RecorderEvent};
use crate::platform::tray::{self, TrayAction};
use main_window::MainWindow;

/// The main window, for focusing it from the tray and deep links.
pub struct MainHandle(pub WindowHandle<MainWindow>);
impl Global for MainHandle {}

/// `focus_main_window`: unminimise, show and focus.
pub fn focus_main(cx: &mut App) {
    if let Some(h) = cx.try_global::<MainHandle>().map(|h| h.0) {
        h.update(cx, |_, window, _| window.activate_window()).ok();
    }
}

/// Keep the tray in step with the recorder and act on its clicks
/// (`tray_action` semantics from the Tauri shell).
pub fn wire_tray(rt: tokio::runtime::Handle, cx: &mut App) {
    let (tx, rx) = flume::unbounded::<TrayAction>();
    tray::init(tx, rt, cx);
    let model = app::state(cx).recorder.clone();
    cx.subscribe(&model, |model, ev: &RecorderEvent, cx| {
        let m = model.read(cx);
        let visible = m.tray_visible();
        let paused = m.paused();
        let text = match ev {
            RecorderEvent::Tray => m.tray_text.clone(),
            _ => m.state.as_ref().map(|s| crate::fmt::tray_time(s.display_seconds)).unwrap_or_default(),
        };
        let text = if matches!(ev, RecorderEvent::State) && !visible { String::new() } else { text };
        tray::sync(visible, &text, paused, cx);
        tray_window::sync(cx);
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(action) = rx.recv_async().await {
            cx.update(|cx| handle_tray_action(action, cx));
        }
    })
    .detach();
}

pub fn handle_tray_action(action: TrayAction, cx: &mut App) {
    if !matches!(action, TrayAction::Toggle(_)) {
        tray_window::hide(cx);
    }
    let core = app::state(cx).core.clone();
    let rt = app::state(cx).rt.clone();
    match action {
        TrayAction::Pause => {
            rt.spawn(async move {
                let _ = core.recorder_pause().await;
            });
        }
        TrayAction::Resume => {
            let lost = app::state(cx).recorder.read(cx).state.as_ref().map(|s| s.source_lost.is_some()).unwrap_or(false);
            if lost {
                focus_main(cx);
            } else {
                rt.spawn(async move {
                    let _ = core.recorder_resume().await;
                });
            }
        }
        TrayAction::Stop => {
            focus_main(cx);
            rt.spawn(async move {
                let _ = core.recorder_begin_stop().await;
            });
        }
        TrayAction::Open => focus_main(cx),
        TrayAction::Toggle(rect) => tray_window::toggle(rect, cx),
    }
}
