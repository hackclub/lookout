//! The recording clock shown in the menu bar / tray. Ticks on the host's
//! runtime, once a second, so a throttled UI can't stall it; the host only
//! gets told what text to display (see [`crate::Host::set_tray_title`]).
//!
//! The ticker owns no clock of its own. It reads the recorder's
//! `RecordingClock` — the same one the main window and the tray popup are
//! handed a snapshot of — so the menu bar cannot disagree with the window:
//! there is only one anchor, one ratchet and one hold.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

pub use lookout_session::clock::{
    derive_display_seconds, MAX_INTERPOLATION_SECS as MAX_TRAY_INTERPOLATION_SECS,
    TIMER_INTERPOLATION_SLACK_SECS,
};
use lookout_session::state::ControlMode;

use crate::host::TaskHandle;
use crate::recorder::Recorder;

/// Handle for the tray title ticker task.
pub struct TrayTickerHandle {
    cancel_tx: watch::Sender<bool>,
    task: TaskHandle,
}

/// The Rust mirror of `deriveDisplaySeconds` in @lookout/react, in the
/// shape the old tray ticker used. Kept so the rules it pins stay pinned.
pub fn tray_display_seconds(base_seconds: i64, elapsed_secs: i64, running: bool) -> i64 {
    derive_display_seconds(base_seconds, 0, running, elapsed_secs * 1_000)
}

/// Format seconds into a clock-style tray title:
/// >0h: "{h}:{mm:02}:{ss:02}", else: "{mm:02}:{ss:02}"
pub fn format_tray_time(total_seconds: i64) -> String {
    let total = total_seconds.max(0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// The tray title ticker task — updates the menu bar text every second
/// from the recorder's clock.
async fn tray_ticker_task(recorder: Arc<Recorder>, mut cancel_rx: watch::Receiver<bool>) {
    // Most 1s ticks would rewrite the exact same title. Cache the last
    // (text, paused) pair and skip redundant native tray updates; a pause
    // or resume changes the pair, so the glyph flips on the next tick.
    let mut last: Option<(String, bool)> = None;

    loop {
        let sleep = recorder.host.runtime().sleep(Duration::from_secs(1));
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut sleep => {}
            _ = cancel_rx.changed() => {
                lookout_session::log_line!("[tray-timer] cancelled");
                break;
            }
        }

        let Some(state) = recorder.recorder_state() else {
            continue;
        };
        let next = (
            format_tray_time(state.display_seconds),
            state.control_mode == ControlMode::Paused,
        );
        if last.as_ref() != Some(&next) {
            recorder.host.set_tray_title(&next.0, next.1);
            last = Some(next);
        }
    }
}

impl Recorder {
    /// Start the tray ticker (if not already running). Lives as long as a
    /// recording is open; the recorder starts and stops it.
    pub(crate) fn start_tray_ticker(self: &Arc<Self>) {
        let mut guard = self.tray_ticker.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            return;
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let recorder = Arc::clone(self);
        let task = TaskHandle::spawn(self.host.runtime(), async move {
            tray_ticker_task(recorder, cancel_rx).await;
        });
        lookout_session::log_line!("[tray-timer] started");
        *guard = Some(TrayTickerHandle { cancel_tx, task });
    }

    /// Stop the tray ticker.
    pub(crate) fn stop_tray_ticker(&self) {
        let mut guard = self.tray_ticker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            lookout_session::log_line!("[tray-timer] stopping");
            let _ = handle.cancel_tx.send(true);
            handle.task.abort();
        }
    }
}

/// The menu-bar clock must agree with the main window's clock. Both now
/// read the same `RecordingClock`; these pin the derivation it is built on.
#[cfg(test)]
mod tray_timer_tests {
    use super::{format_tray_time, tray_display_seconds, MAX_TRAY_INTERPOLATION_SECS};

    #[test]
    fn cap_matches_the_js_side() {
        // MAX_INTERPOLATION_S in useSessionTimer.ts is
        // SCREENSHOT_INTERVAL_MS / 1000 + TIMER_INTERPOLATION_SLACK_S
        // = 60 + 15.
        assert_eq!(MAX_TRAY_INTERPOLATION_SECS, 75);
    }

    #[test]
    fn interpolates_at_wall_clock_rate() {
        assert_eq!(tray_display_seconds(120, 0, true), 120);
        assert_eq!(tray_display_seconds(120, 30, true), 150);
    }

    #[test]
    fn interpolation_is_capped_at_one_interval_plus_slack() {
        assert_eq!(
            tray_display_seconds(120, 90, true),
            120 + MAX_TRAY_INTERPOLATION_SECS
        );
        assert_eq!(
            tray_display_seconds(120, 600, true),
            120 + MAX_TRAY_INTERPOLATION_SECS
        );
    }

    #[test]
    fn pause_drops_the_interpolated_remainder() {
        assert_eq!(tray_display_seconds(120, 45, false), 120);
        assert_eq!(
            format_tray_time(tray_display_seconds(299, 59, false)),
            "04:59"
        );
    }

    #[test]
    fn formats_hours_when_present() {
        assert_eq!(format_tray_time(0), "00:00");
        assert_eq!(format_tray_time(3661), "1:01:01");
        assert_eq!(format_tray_time(-5), "00:00");
    }
}
