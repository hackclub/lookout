//! Handing the window frame back to GTK.
//!
//! On Linux the main and editor windows have no native titlebar — the
//! webview draws its own header bar (HeaderBar.tsx). Earlier revisions got
//! there with `set_decorations(false)` and then rebuilt everything the
//! frame used to provide by hand: a transparent margin for a CSS shadow, an
//! input shape so the shadow didn't eat clicks, webview-drawn resize strips,
//! and compositor state polling to square the corners when tiled. All of it
//! was an imitation of GTK's own client-side decorations, hardcoded to
//! Adwaita's look — which is exactly why it read wrong on any desktop that
//! wasn't GNOME.
//!
//! GTK will do the real thing if asked. Setting *any* titlebar widget on a
//! GtkWindow flips it into client-side decoration mode: GTK draws the
//! shadow, the outer border, the rounded corners, and the invisible resize
//! frame itself, styled by whatever GTK theme the session runs — Adwaita on
//! GNOME, Breeze-GTK on KDE, whatever the user picked elsewhere. It also
//! squares the corners and drops the shadow when the window is maximized or
//! tiled, keeps the shadow's input out of the way, and dims it on focus
//! loss. The titlebar widget here is a zero-height box, so the visible
//! titlebar remains the webview's header bar; window dragging keeps going
//! through `data-tauri-drag-region` as before.
//!
//! The one thing GTK cannot do is round the *webview's* corners — GTK3
//! doesn't clip children to the frame's radius — so the webview keeps a
//! transparent background and the frontend rounds its top corners in CSS
//! (see linuxChrome.ts), squaring them again while the window is
//! window-manager-sized. `window_manager_sized` below answers that query.

#[cfg(target_os = "linux")]
use tauri::Manager;

/// Give this window GTK's client-side frame while keeping the titlebar
/// area empty.
///
/// Must run on the GTK main thread, before the window is first realized —
/// GTK warns and forces an unrealize/realize cycle otherwise, which is why
/// the windows that want this are created hidden and only shown afterwards.
#[cfg(target_os = "linux")]
pub fn adopt_gtk_frame(window: &tauri::WebviewWindow) -> Result<(), String> {
    use gtk::prelude::{GtkWindowExt, WidgetExt};

    let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;

    // tao marks transparent windows app-paintable, which tells GTK to skip
    // its own drawing — including the decoration this function exists for.
    // Undo that; the RGBA visual tao picked stays, and matters: without an
    // alpha channel GTK falls back to `solid-csd`, a plain border instead
    // of a real shadow.
    gtk_window.set_app_paintable(false);

    // An empty box measures zero height, so the "titlebar" reserves no
    // room — but its mere presence is the switch that enables GTK's CSD
    // path. It must be shown: GTK only allocates visible titlebars, and an
    // unallocated one leaves GTK_STYLE_CLASS_TITLEBAR-dependent theming in
    // an odd half-state on some themes.
    let titlebar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    titlebar.set_size_request(0, 0);
    titlebar.show();
    gtk_window.set_titlebar(Some(&titlebar));
    Ok(())
}

/// Adopt the GTK frame for the window named `label`, then show it.
///
/// Windows that want the GTK frame are created hidden so the titlebar can
/// land before the first realize; this is the "and now reveal it" half. The
/// show is unconditional — a theming failure must not leave the user with a
/// window that exists but can't be seen.
#[tauri::command]
pub fn show_with_gtk_frame(app: tauri::AppHandle, label: String) -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, label);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        let window = app
            .get_webview_window(&label)
            .ok_or_else(|| format!("no window labelled {label}"))?;
        let target = window.clone();
        // GTK is main-thread only; the show is queued from the same closure
        // so it cannot overtake the titlebar change.
        window
            .run_on_main_thread(move || {
                if let Err(e) = adopt_gtk_frame(&target) {
                    eprintln!("[window-frame] window keeps its stock frame: {e}");
                }
                if let Err(e) = target.show() {
                    eprintln!("[window-frame] could not show {}: {e}", target.label());
                }
            })
            .map_err(|e| e.to_string())
    }
}

/// Whether the window manager is sizing this window rather than letting it
/// float — tiled, maximized or fullscreen.
///
/// GTK squares its own frame in those states; this exists so the webview
/// can square the top corners it rounds in CSS at the same moment. The
/// compositor is asked directly rather than inferred from geometry: GDK
/// carries the tiled and maximized bits straight from the protocol —
/// xdg_toplevel's tiled states on Wayland, EWMH on X11 — so this is right
/// for a quarter-tile, which no comparison against the work area can be,
/// and it needs no window position, which Wayland refuses to give a client
/// anyway.
#[tauri::command]
pub fn window_manager_sized(window: tauri::WebviewWindow) -> Result<bool, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        Ok(false)
    }

    #[cfg(target_os = "linux")]
    {
        use std::sync::mpsc;
        use std::time::Duration;

        let (tx, rx) = mpsc::channel();
        let target = window.clone();
        // GTK is main-thread only; hop across and bring the answer back.
        window
            .run_on_main_thread(move || {
                let _ = tx.send(is_window_manager_sized(&target).unwrap_or(false));
            })
            .map_err(|e| e.to_string())?;

        // Bounded so a wedged main thread can't hang the caller; the corners
        // simply stay as they were.
        rx.recv_timeout(Duration::from_millis(500))
            .map_err(|e| format!("timed out reading window state: {e}"))
    }
}

/// The granular *_TILED bits matter: a window snapped to one edge reports
/// only that edge, not the general TILED flag, on some compositors.
#[cfg(target_os = "linux")]
fn is_window_manager_sized(window: &tauri::WebviewWindow) -> Result<bool, String> {
    use gtk::gdk::WindowState;
    use gtk::prelude::WidgetExt;

    let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
    let gdk_window = gtk_window.window().ok_or("window not realized yet")?;
    Ok(gdk_window.state().intersects(
        WindowState::TILED
            | WindowState::LEFT_TILED
            | WindowState::RIGHT_TILED
            | WindowState::TOP_TILED
            | WindowState::BOTTOM_TILED
            | WindowState::MAXIMIZED
            | WindowState::FULLSCREEN,
    ))
}
