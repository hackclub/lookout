//! The menu-bar / tray item for a recording, and the popup it opens.
//!
//! The item's presence and its paused glyph follow the recorder's state
//! (`TauriFrontend::sync_tray`), and its clock text is written by the
//! core's once-a-second ticker (`TauriFrontend::set_tray_title`) — one
//! writer, so the title can't flicker between two surfaces' idea of the
//! second. The webview never touches the tray.
//!
//! Actions from the popup, the Linux menu and the GNOME pill all come
//! through [`tray_action`], which drives the recorder directly.

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::Mutex;
#[cfg(not(target_os = "macos"))]
use tauri::image::Image;
#[cfg(target_os = "linux")]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
#[cfg(not(target_os = "macos"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalPosition, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::AppState;

#[cfg(target_os = "linux")]
const MENU_TOGGLE: &str = "tray_toggle_pause";
#[cfg(target_os = "linux")]
const MENU_STOP: &str = "tray_stop";
#[cfg(target_os = "linux")]
const MENU_OPEN: &str = "tray_open";

/// Mirrors the pause state for the Linux menu, which has to decide between
/// emitting `pause` and `resume` without a round trip to the WebView.
#[cfg(target_os = "linux")]
static TRAY_PAUSED: AtomicBool = AtomicBool::new(false);

/// Keeps the Pause/Resume item reachable so its label can be flipped later;
/// `TrayIcon` exposes no menu getter. Replaced rather than added to, so a tray
/// rebuilt after the pill detaches doesn't leave a dead item behind.
#[cfg(target_os = "linux")]
static TRAY_TOGGLE: Mutex<Option<MenuItem<tauri::Wry>>> = Mutex::new(None);

/// Put the tray item up for a recording, showing `time_text`.
pub fn show(app: &AppHandle, time_text: &str, paused: bool) -> Result<(), String> {
    // macOS gets a native NSStatusItem rendered with SwiftUI so the time
    // digits animate with contentTransition(.numericText()) — see
    // swift/lookout-tray. Other platforms keep the tauri tray.
    #[cfg(target_os = "macos")]
    {
        crate::native_tray::show(app, time_text)?;
        return crate::native_tray::update(time_text, Some(paused));
    }

    #[cfg(target_os = "linux")]
    {
        TRAY_PAUSED.store(paused, Ordering::Relaxed);
        crate::gnome_indicator::publish(true, time_text, paused);
        // The GNOME extension draws the panel pill itself; a StatusNotifier
        // item next to it would be a second indicator for one recording.
        if crate::gnome_indicator::pill_attached() {
            return Ok(());
        }
    }

    #[cfg(not(target_os = "macos"))]
    show_tauri_tray(app, time_text, paused)
}

#[cfg(not(target_os = "macos"))]
fn show_tauri_tray(app: &AppHandle, time_text: &str, paused: bool) -> Result<(), String> {
    if app.tray_by_id("timelapse_tray").is_some() {
        return update(app, time_text, paused);
    }

    // Windows gets the app icon, so the tray entry matches the taskbar and
    // Start menu. Linux panels can't take a colourful one: Yaru is white in
    // light mode and black in dark, so the app icon there is the timelapse
    // glyph in the app's red, which reads against both.
    #[cfg(target_os = "linux")]
    let icon_bytes: &[u8] = include_bytes!("../icons/timelapse_linux.png");
    #[cfg(not(target_os = "linux"))]
    let icon_bytes: &[u8] = include_bytes!("../icons/32x32.png");

    let icon = Image::from_bytes(icon_bytes).map_err(|e| e.to_string())?;
    let (title, tooltip) = title_and_tooltip(time_text, paused);

    #[allow(unused_mut)]
    let mut builder = TrayIconBuilder::with_id("timelapse_tray")
        .title(&title)
        // Windows doesn't render tray titles — the tooltip is the only place
        // the recorded time is visible there.
        .tooltip(tooltip)
        .icon(icon)
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                rect,
                ..
            } = event
            {
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    toggle_tray_window(tray.app_handle(), rect);
                }
            }
        });

    // A StatusNotifierItem that exposes no D-Bus menu is not shown at all by
    // some hosts, GNOME's appindicator extension included: it gates every
    // item on `NEEDED_PROPERTIES = ['Id', 'Menu']` and treats a missing menu
    // path as not-ready, so the icon *and* its time label never appear. This
    // was the whole reason the Linux tray looked like it did nothing.
    //
    // The menu also carries the controls. `TrayIconEvent` is documented as
    // never emitted on Linux, so the click handler above is dead there and
    // the popup window it toggles is unreachable — the menu is the only way
    // to pause or stop from the panel.
    #[cfg(target_os = "linux")]
    {
        let toggle = MenuItem::with_id(
            app,
            MENU_TOGGLE,
            if paused {
                "Resume recording"
            } else {
                "Pause recording"
            },
            true,
            None::<&str>,
        )
        .map_err(|e| e.to_string())?;
        let stop = MenuItem::with_id(app, MENU_STOP, "Stop recording", true, None::<&str>)
            .map_err(|e| e.to_string())?;
        let open = MenuItem::with_id(app, MENU_OPEN, "Open Lookout", true, None::<&str>)
            .map_err(|e| e.to_string())?;
        let separator = PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?;
        let menu = Menu::with_items(app, &[&toggle, &separator, &stop, &open])
            .map_err(|e| e.to_string())?;

        if let Ok(mut slot) = TRAY_TOGGLE.lock() {
            *slot = Some(toggle.clone());
        }
        builder = builder.menu(&menu).on_menu_event(on_tray_menu_event);
    }

    builder.build(app).map_err(|e| e.to_string())?;

    Ok(())
}

/// Linux tray-menu clicks. Routed through the same `tray_action` the popup
/// window uses on the other platforms, so the handling is shared.
#[cfg(target_os = "linux")]
fn on_tray_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id: &str = event.id().as_ref();
    let action = match id {
        MENU_TOGGLE => {
            if TRAY_PAUSED.load(Ordering::Relaxed) {
                "resume"
            } else {
                "pause"
            }
        }
        MENU_STOP => "stop",
        MENU_OPEN => {
            focus_main_window(app);
            return;
        }
        _ => return,
    };
    let _ = tray_action(action.to_string(), app.clone());
}

fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub(crate) fn toggle_tray_window(app: &AppHandle, rect: tauri::Rect) {
    if let Some(window) = app.get_webview_window("tray") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let _ = position_and_show_window(&window, rect);
            let _ = window.emit("tray-opened", ());
        }
    } else {
        // Create it
        // (mut is for the macOS-only call below; off macOS nothing reassigns it)
        #[allow(unused_mut)]
        let mut builder = WebviewWindowBuilder::new(app, "tray", WebviewUrl::App("#/tray".into()))
            .title("Tray")
            .inner_size(300.0, 50.0)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .transparent(true)
            .skip_taskbar(true)
            .visible(false);

        #[cfg(target_os = "macos")]
        {
            builder = builder.visible_on_all_workspaces(true);
        }

        if let Ok(window) = builder.build() {
            // Hide on blur
            let w = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Focused(false) = event {
                    // Slight workaround: when clicking the tray icon, focus is lost immediately
                    // from the old window state before it's properly evaluated.
                    // By checking visibility, we make it reliable.
                    if w.is_visible().unwrap_or(false) {
                        let _ = w.hide();
                    }
                }
            });

            let _ = position_and_show_window(&window, rect);
            let _ = window.emit("tray-opened", ());
        }
    }
}

fn position_and_show_window(
    window: &tauri::WebviewWindow,
    tray_rect: tauri::Rect,
) -> Result<(), tauri::Error> {
    // Attempt to position it horizontally centered below the tray icon
    let monitor = window
        .current_monitor()?
        .unwrap_or_else(|| window.primary_monitor().unwrap().unwrap());
    let scale_factor = monitor.scale_factor();
    let window_size = window.outer_size()?;

    let tray_logical_pos = tray_rect.position.to_logical::<f64>(scale_factor);
    let tray_logical_size = tray_rect.size.to_logical::<f64>(scale_factor);

    let window_logical_size = window_size.to_logical::<f64>(scale_factor);
    let monitor_pos = monitor.position().to_logical::<f64>(scale_factor);
    let monitor_size = monitor.size().to_logical::<f64>(scale_factor);

    let mut x =
        tray_logical_pos.x + (tray_logical_size.width / 2.0) - (window_logical_size.width / 2.0);
    let mut y = tray_logical_pos.y + tray_logical_size.height;

    // Check if the window overflows the bottom of the screen (e.g., Windows taskbar at bottom)
    if y + window_logical_size.height > monitor_pos.y + monitor_size.height {
        // Position it ABOVE the tray icon instead
        y = tray_logical_pos.y - window_logical_size.height;
    }

    // Check right bounds
    if x + window_logical_size.width > monitor_pos.x + monitor_size.width {
        x = monitor_pos.x + monitor_size.width - window_logical_size.width;
    }

    // Check left bounds
    if x < monitor_pos.x {
        x = monitor_pos.x;
    }

    window.set_position(LogicalPosition::new(x, y))?;

    #[cfg(target_os = "macos")]
    {
        window.show()?;
        window.set_focus()?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On Windows, showing and focusing simultaneously can cause immediate blur
        window.show()?;
        window.set_focus()?;
    }
    Ok(())
}

/// The title and tooltip for a time and pause state, off macOS: a pause
/// glyph in the menu bar while paused, and a tooltip that says which.
#[cfg(not(target_os = "macos"))]
fn title_and_tooltip(time_text: &str, paused: bool) -> (String, String) {
    if paused {
        (
            format!("⏸ {time_text}"),
            format!("Lookout — paused at {time_text}"),
        )
    } else {
        (
            time_text.to_string(),
            format!("Lookout — {time_text} recorded"),
        )
    }
}

/// Write the tray's time text and pause state. The core's ticker calls
/// this once a second (when something changed), and the state sync calls
/// it the instant the pause state flips.
pub fn update(app: &AppHandle, time_text: &str, paused: bool) -> Result<(), String> {
    // The Swift side renders its own pause glyph and tooltip.
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        return crate::native_tray::update(time_text, Some(paused));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let (title, tooltip) = title_and_tooltip(time_text, paused);

        #[cfg(target_os = "linux")]
        {
            TRAY_PAUSED.store(paused, Ordering::Relaxed);
            // Linux has no tray click, so pause/resume lives in the menu —
            // one item whose label follows the state.
            if let Ok(slot) = TRAY_TOGGLE.lock() {
                if let Some(toggle) = slot.as_ref() {
                    let _ = toggle.set_text(if paused {
                        "Resume recording"
                    } else {
                        "Pause recording"
                    });
                }
            }
            crate::gnome_indicator::publish_tick(time_text, paused);
        }

        if let Some(tray) = app.tray_by_id("timelapse_tray") {
            let _ = tray.set_title(Some(title));
            let _ = tray.set_tooltip(Some(tooltip));
        }
        Ok(())
    }
}

/// Take the tray item down: the recording is over or the window is gone.
pub fn hide(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    crate::native_tray::hide();
    #[cfg(target_os = "linux")]
    crate::gnome_indicator::publish(false, "", false);
    #[cfg(not(target_os = "macos"))]
    app.remove_tray_by_id("timelapse_tray");
    if let Some(w) = app.get_webview_window("tray") {
        let _ = w.close();
    }
}

/// An action from the tray popup, the Linux menu or the GNOME pill, driven
/// straight into the recorder. `ready` is the popup asking for the current
/// state.
#[tauri::command]
pub fn tray_action(action: String, app: AppHandle) -> Result<(), String> {
    // Only hide the popup for real actions, not for its own state request.
    if action != "ready" {
        if let Some(w) = app.get_webview_window("tray") {
            let _ = w.hide();
        }
    }

    let core = std::sync::Arc::clone(&app.state::<AppState>().core);
    match action.as_str() {
        "ready" => {
            let _ = app.emit("recorder-state", core.recorder_state());
        }
        "pause" => {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = core.recorder_pause().await {
                    eprintln!("[tray] pause refused: {e}");
                }
            });
        }
        "resume" => {
            // With the source gone there is nothing to resume onto — and the
            // menu bar shows a paused session, so Resume is exactly what
            // someone would reach for. Surface the window instead: the
            // prompt asking for a new source is in it, behind whatever
            // they're working in.
            let lost = core
                .recorder_state()
                .is_some_and(|s| s.source_lost.is_some());
            if lost {
                eprintln!("[tray] resume ignored — no capture source, asking for one");
                focus_main_window(&app);
            } else {
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = core.recorder_resume().await {
                        eprintln!("[tray] resume refused: {e}");
                    }
                });
            }
        }
        "stop" => {
            // The naming prompt is in the main window.
            focus_main_window(&app);
            tauri::async_runtime::spawn(async move {
                if let Err(e) = core.recorder_begin_stop().await {
                    eprintln!("[tray] stop refused: {e}");
                }
            });
        }
        other => return Err(format!("unknown tray action {other:?}")),
    }
    Ok(())
}
