//! CI smoke test: `LOOKOUT_SMOKE_SECS=5 lookout`.
//!
//! Boots the real app, keeps the main window asking for frames, and after the
//! given time prints how many frames were actually painted and exits: status
//! 0 if the window kept animating, 1 if it didn't. That catches a window that
//! paints once and freezes (the macOS 26 display-link bug,
//! `platform/macos_occlusion.rs`), a GPU/driver that can't present, and a
//! crash during startup, on real macOS/Windows/Linux runners.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::gpui::App;

static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Seconds to run for, when smoke mode is on.
pub fn duration() -> Option<Duration> {
    std::env::var("LOOKOUT_SMOKE_SECS").ok()?.parse::<f64>().ok().filter(|s| *s > 0.0).map(Duration::from_secs_f64)
}

pub fn enabled() -> bool {
    duration().is_some()
}

/// Called from the main window's render while smoke mode is on.
pub fn frame() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// Schedule the verdict.
pub fn start(cx: &mut App) {
    let Some(secs) = duration() else { return };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(secs).await;
        let frames = FRAMES.load(Ordering::Relaxed);
        // A window that animates presents far more than this; a frozen one
        // stops at one or two frames.
        let min = (secs.as_secs_f64() * 10.0).max(20.0) as u64;
        let ok = frames >= min;
        println!("LOOKOUT_SMOKE frames={frames} seconds={:.1} min={min} result={}", secs.as_secs_f64(), if ok { "pass" } else { "FAIL" });
        let _ = cx.update(|cx| cx.quit());
        std::process::exit(if ok { 0 } else { 1 });
    })
    .detach();
}

/// `LOOKOUT_SMOKE_TOUR=<dir>`: step through the screens worth checking on a
/// real OS. At each step the app writes `<dir>/<n>.ready` (with the step's
/// name) and waits up to 20s for `<dir>/<n>.done`, which the CI script
/// creates after screenshotting the desktop. Used with the dev server
/// (`DEV_PANEL=1` for the program panel step).
pub const TOUR: &[&str] = &["gallery", "settings", "tray", "panel", "add-menu", "confirm"];

pub fn start_tour(cx: &mut App) {
    let Some(dir) = std::env::var_os("LOOKOUT_SMOKE_TOUR").map(std::path::PathBuf::from) else { return };
    let _ = std::fs::create_dir_all(&dir);
    cx.spawn(async move |cx| {
        let pause = |ms: u64| cx.background_executor().timer(Duration::from_millis(ms));
        pause(4000).await;
        for (i, step) in TOUR.iter().enumerate() {
            let _ = cx.update(|cx| {
                if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
                    let _ = h.update(cx, |mw, window, cx| mw.smoke_step(step, window, cx));
                }
            });
            // The program panel's page needs time to load in its webview.
            pause(if *step == "panel" { 6000 } else { 2500 }).await;
            let _ = std::fs::write(dir.join(format!("{i}.ready")), step);
            for _ in 0..100 {
                if dir.join(format!("{i}.done")).exists() {
                    break;
                }
                pause(200).await;
            }
        }
        let _ = std::fs::write(dir.join("finished"), "");
    })
    .detach();
}
