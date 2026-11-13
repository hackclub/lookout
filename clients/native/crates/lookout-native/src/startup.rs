//! How long a launch takes, phase by phase, from `main` to the main
//! window's first painted frame. Logged as one line and attached to every
//! Sentry event of the run (context `startup`, tag `startup.first_frame`),
//! so slow opens on testers' machines show where the time went.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static MARKS: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());
static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// First thing in `main`.
pub fn begin() {
    START.get_or_init(Instant::now);
}

fn elapsed_ms() -> u64 {
    START.get().map_or(0, |s| s.elapsed().as_millis() as u64)
}

/// The end of a launch phase named `name`.
pub fn mark(name: &'static str) {
    if DONE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Ok(mut m) = MARKS.lock() {
        m.push((name, elapsed_ms()));
    }
}

/// The main window painted its first frame: report the launch once.
pub fn first_frame() {
    if DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let total = elapsed_ms();
    let marks = MARKS.lock().map(|m| m.clone()).unwrap_or_default();
    let mut prev = 0;
    let mut phases = Vec::with_capacity(marks.len());
    for (name, at) in &marks {
        phases.push((*name, at.saturating_sub(prev)));
        prev = *at;
    }
    phases.push(("first frame", total.saturating_sub(prev)));
    let summary: Vec<String> = phases.iter().map(|(n, ms)| format!("{n} {ms}ms")).collect();
    log::info!("[startup] first frame after {total}ms ({})", summary.join(", "));

    let bucket = match total {
        0..500 => "<0.5s",
        500..1000 => "0.5-1s",
        1000..2000 => "1-2s",
        2000..5000 => "2-5s",
        _ => ">5s",
    };
    sentry::configure_scope(|scope| {
        let mut ctx = std::collections::BTreeMap::new();
        ctx.insert("first_frame_ms".to_string(), total.into());
        for (name, ms) in &phases {
            ctx.insert(format!("{name}_ms"), (*ms).into());
        }
        scope.set_context("startup", sentry::protocol::Context::Other(ctx));
        scope.set_tag("startup.first_frame", bucket);
    });
}

/// This launch isn't timed (the first-launch onboarding opens first, so
/// the main window's first frame says nothing about launch speed).
pub fn skip() {
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
}
