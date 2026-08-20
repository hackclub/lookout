//! Letting clicks fall through the window's shadow.
//!
//! The Linux window reserves a transparent frame around itself so it has
//! somewhere to draw its outer border and shadow (see `WINDOW_MARGIN` in
//! linuxChrome.ts). That frame is invisible but it is still part of the
//! window, so without further help a click anywhere in it — on what looks
//! like empty desktop beside the app — lands on Lookout and focuses it.
//!
//! GTK's answer is an input shape: a region telling the compositor which
//! part of the window actually accepts pointer input. Everything outside it
//! is passed through to whatever is behind.
//!
//! The region deliberately does NOT stop at the visible window. A band of
//! the frame nearest the content stays interactive, because that band is
//! the invisible border you grab to resize (WindowResizeHandles.tsx draws
//! its strips there). Excluding the whole frame would make the window
//! click-through-able *and* impossible to resize.
//!
//!     ┌─────────────────────────────┐
//!     │  passes through             │  ← outer frame: shadow only
//!     │  ┌───────────────────────┐  │
//!     │  │ resize band           │  │  ← still accepts input
//!     │  │  ┌─────────────────┐  │  │
//!     │  │  │ visible window  │  │  │
//!
//! Whether the frame should exist at all is the compositor's call, read
//! from GDK rather than guessed from geometry — see `sync_window_frame`.
//! It may also not exist because Lookout never drew one: a shell extension
//! that rounds and shades every window takes the job over entirely, and the
//! frontend then asks for an inset of 0, which clears the shape.
//!
//! The frontend drives *when* to re-check, since it already listens for
//! resizes, and mirrors the answer into its CSS.

/// Reconcile the window's frame with how the compositor is treating it, and
/// report back whether that frame should exist at all.
///
/// A window the window manager is sizing — tiled, maximized, fullscreen —
/// must not keep a transparent frame. It has no shadow to cast (its edges
/// are against other windows or the screen) and the frame would show as a
/// band of desktop between it and its neighbours. That is the difference
/// between a shadow and a gap, and under a tiling WM every window would
/// have one.
///
/// The compositor is asked directly rather than inferred from geometry.
/// GDK carries the tiled and maximized bits straight from the protocol —
/// xdg_toplevel's tiled states on Wayland, EWMH on X11 — so this is right
/// for a quarter-tile, which no comparison against the work area can be,
/// and it needs no window position, which Wayland refuses to give a client
/// anyway.
///
/// Returns true when the frame is collapsed, so the caller can match its
/// CSS (square corners, no shadow) to the shape just applied.
#[tauri::command]
pub fn sync_window_frame(window: tauri::WebviewWindow, inset: i32) -> Result<bool, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, inset);
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
                let managed = is_window_manager_sized(&target).unwrap_or(false);
                // A managed window keeps no frame, so nothing to pass through.
                let effective = if managed { 0 } else { inset.max(0) };
                if let Err(e) = apply_input_shape(&target, effective) {
                    eprintln!("[window-shape] could not set input shape: {e}");
                }
                let _ = tx.send(managed);
            })
            .map_err(|e| e.to_string())?;

        // Bounded so a wedged main thread can't hang the caller; the frame
        // simply stays as it was.
        rx.recv_timeout(Duration::from_millis(500))
            .map_err(|e| format!("timed out reading window state: {e}"))
    }
}

/// Whether the window manager is sizing this window rather than letting it
/// float. The granular *_TILED bits matter: a window snapped to one edge
/// reports only that edge, not the general TILED flag, on some compositors.
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

/// Applies the window's input shape, inset by `inset` logical pixels on
/// every side. An inset of 0 clears the shape, making the whole window
/// interactive again — which is what a window-manager-sized window wants.
///
/// Failures are reported but never fatal: the cost of getting this wrong is
/// a window that catches clicks on its own shadow, which is exactly where we
/// started and is not worth taking the app down over.
#[cfg(target_os = "linux")]
fn apply_input_shape(window: &tauri::WebviewWindow, inset: i32) -> Result<(), String> {
    use gtk::cairo::{RectangleInt, Region};
    use gtk::prelude::WidgetExt;

    let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;

    if inset == 0 {
        // Passing None unsets the shape entirely, so the window takes input
        // across its whole area again.
        gtk_window.input_shape_combine_region(None);
        return Ok(());
    }

    // Widget coordinates, which is the space this region is interpreted in,
    // and the same logical pixels the CSS margin is expressed in.
    let width = gtk_window.allocated_width();
    let height = gtk_window.allocated_height();

    // Too small to inset — a zero or negative region would make the window
    // entirely click-through, which is unrecoverable without a keyboard.
    if width <= inset * 2 || height <= inset * 2 {
        gtk_window.input_shape_combine_region(None);
        return Ok(());
    }

    let interactive = RectangleInt::new(inset, inset, width - inset * 2, height - inset * 2);
    gtk_window.input_shape_combine_region(Some(&Region::create_rectangle(&interactive)));
    Ok(())
}
