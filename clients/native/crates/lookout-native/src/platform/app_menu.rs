//! The macOS menu bar (`lib.rs:1188-1293` in the Tauri shell): Lookout
//! (About, Hide, Hide Others, Show All, Quit), File → Start Timelapse (⌘N), Edit,
//! Window, Help. Other platforms have no app menu.

use gpui_kit::gpui::{App, KeyBinding, Menu, MenuItem, OsAction, actions};

actions!(
    lookout_menu,
    [About, Quit, Hide, HideOthers, ShowAll, StartTimelapse, Minimize, Zoom, CloseWindow, FalloutDocs, HowToTimelapse, GitHubRepo]
);

pub fn init(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-n", StartTimelapse, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
    // Key and menu actions are dispatched *inside* the active window's
    // update, while GPUI has that window checked out — `handle.update` on it
    // then fails with "window not found". Anything that touches a window runs
    // deferred, once the dispatch has returned the window.
    cx.on_action(|_: &Quit, cx| {
        cx.defer(|cx| {
            if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
                let close = h.update(cx, |_, _, cx| crate::windows::main_window::main_should_close(cx)).unwrap_or(true);
                if !close {
                    return;
                }
            }
            cx.quit();
        })
    });
    cx.on_action(|_: &About, cx| {
        crate::windows::focus_main(cx);
        if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
            h.update(cx, |mw, _, cx| mw.navigate(crate::windows::main_window::Route::Info, cx)).ok();
        }
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &StartTimelapse, cx| {
        cx.defer(|cx| {
            crate::windows::focus_main(cx);
            if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
                h.update(cx, |mw, _, cx| mw.navigate(crate::windows::main_window::Route::Add, cx)).ok();
            }
        })
    });
    cx.on_action(|_: &Minimize, cx| {
        cx.defer(|cx| {
            if let Some(w) = cx.active_window() {
                w.update(cx, |_, window, _| window.minimize_window()).ok();
            }
        })
    });
    cx.on_action(|_: &Zoom, cx| {
        cx.defer(|cx| {
            if let Some(w) = cx.active_window() {
                w.update(cx, |_, window, _| window.zoom_window()).ok();
            }
        })
    });
    cx.on_action(|_: &CloseWindow, cx| {
        cx.defer(|cx| {
            if let Some(w) = cx.active_window() {
                w.update(cx, |_, window, _| window.remove_window()).ok();
            }
        })
    });
    cx.on_action(|_: &FalloutDocs, cx| cx.open_url("https://fallout.hackclub.com/docs"));
    cx.on_action(|_: &HowToTimelapse, cx| cx.open_url("https://fallout.hackclub.com/docs/project-resources/how-to-timelapse"));
    cx.on_action(|_: &GitHubRepo, cx| cx.open_url("https://github.com/hackclub/lookout/"));

    // Edit-menu actions are the text field's own (bound in `ui::input`).
    use crate::ui::input::{Copy, Cut, Paste, SelectAll};
    cx.set_menus(vec![
        Menu::new("Lookout").items([
            MenuItem::action("About Lookout", About),
            MenuItem::separator(),
            MenuItem::action("Hide Lookout", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Lookout", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Start Timelapse", StartTimelapse)]),
        Menu::new("Edit").items([
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Help").items([
            MenuItem::action("Fallout Docs", FalloutDocs),
            MenuItem::action("How to Timelapse?", HowToTimelapse),
            MenuItem::action("GitHub Repo", GitHubRepo),
        ]),
    ]);
}
