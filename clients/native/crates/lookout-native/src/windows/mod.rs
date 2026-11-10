pub mod editor;
pub mod main_window;
pub mod onboarding;
pub mod onboarding_demo;
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

/// macOS: the app was launched again (Dock, Spotlight, Raycast) while no
/// window is showing. Closing the main window doesn't quit there, so its
/// handle may be dead: open a fresh one in that case.
pub fn reopen(cx: &mut App) {
    let Some(h) = cx.try_global::<MainHandle>().map(|h| h.0) else {
        // Still onboarding (the app proper hasn't started).
        onboarding::bring_forward(cx);
        return;
    };
    if h.update(cx, |_, window, _| window.activate_window()).is_err() {
        log::info!("[app] reopened with the main window closed: opening a new one");
        let main = main_window::open(cx);
        cx.set_global(MainHandle(main));
    }
    cx.activate(true);
}

/// System Settings' "Quit & Reopen" after a privacy permission is turned on:
/// remember where the user was on the way out and put it back on the
/// relaunch (see `platform::relaunch`). `deep_link_launch` skips the restore
/// (the link decides where to go).
pub fn wire_relaunch_restore(deep_link_launch: bool, cx: &mut App) {
    use crate::platform::relaunch;

    let snap = relaunch::take();
    if let (Some(snap), false) = (snap, deep_link_launch) {
        log::info!("[relaunch] reopened by {:?}: restoring {}", snap.sender, snap.route);
        if let Some(h) = cx.try_global::<MainHandle>().map(|h| h.0) {
            h.update(cx, |mw, _, cx| mw.restore_snapshot(&snap, cx)).ok();
        }
        for token in snap.editors {
            editor::open(token, cx);
        }
    }

    cx.on_app_quit(|cx| {
        // Runs synchronously inside `applicationWillTerminate:`, while the
        // quit Apple Event is still the current one.
        save_for_relaunch(cx);
        async {}
    })
    .detach();
}

fn save_for_relaunch(cx: &mut App) {
    use crate::platform::{permissions, relaunch};

    let sender = relaunch::current_quit_sender();
    if let Some(s) = &sender {
        log::info!("[relaunch] quit requested by pid {} ({:?})", s.pid, s.bundle_id);
    }
    if !sender.as_ref().is_some_and(|s| s.is_permission_relaunch(permissions::opened_privacy_pane())) {
        relaunch::clear();
        return;
    }
    let Some(h) = cx.try_global::<MainHandle>().map(|h| h.0) else { return };
    if let Ok(mut snap) = h.read_with(cx, |mw, cx| mw.relaunch_snapshot(cx)) {
        snap.sender = sender.and_then(|s| s.bundle_id);
        relaunch::save(snap);
    }
    // This quit skips `main_should_close`, so pause a running capture here
    // (as Cmd-Q would) before the process goes away.
    let st = app::state(cx);
    if st.recorder.read(cx).state.as_ref().is_some_and(|s| s.capturing) {
        log::info!("[relaunch] pausing the capture before the reopen");
        let core = st.core.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        st.rt.spawn(async move {
            let _ = core.recorder_pause().await;
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
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
