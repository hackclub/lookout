//! The recording clock: the one source of truth for "what time does the
//! recording show", shared by every surface that renders it.
//!
//! The desktop app shows the recording clock in three places — the main
//! window, the menu-bar / tray title, and the tray popup — and for a long
//! time each of them ticked its own copy with the same rules, kept in step
//! by passing anchors around. Every "the menu bar shows a different time"
//! bug came from one copy drifting. This is the single copy: the recorder
//! owns one `RecordingClock`, the Rust ticker reads it, and the UI surfaces
//! are handed its [`ClockState`] and re-derive the display with the same
//! arithmetic (`deriveDisplaySeconds` in @lookout/react — keep the two in
//! step).
//!
//! The rules, each of which exists because its absence was a shipped bug:
//!
//! 1. `display = base + min(cap, now - anchor)` while running. The server
//!    credits time in whole minutes; the clock interpolates between credits
//!    so it looks alive, but never more than one capture interval plus a
//!    latency allowance ahead of the last credit (`MAX_INTERPOLATION_SECS`).
//!    Uncapped, a stalled capture let the clock run for an hour and then
//!    "lose" that hour at stop — the "tracked time cut in half" reports.
//! 2. While not running (paused, stopped, source lost) the display is the
//!    bare base: the interpolated remainder is dropped, not frozen. Freezing
//!    it left the menu bar up to a minute ahead for the whole pause.
//! 3. The base ratchets forward only, and the anchor is reset only when the
//!    base actually advances. An idempotent-retry confirm can answer with a
//!    stale, lower `trackedSeconds`; taking it moved the clock backwards.
//!    Re-anchoring on an equal reading restarted the interpolation window
//!    and silently discarded up to a minute of elapsed time.
//! 4. A credit landing never moves the display backward. Credits are earned
//!    on the 60s grid but arrive a confirm round trip later, during which
//!    the clock has interpolated a few seconds past the credited value;
//!    snapping to the credit made the clock visibly jump back
//!    (0:00→1:05→1:00) at the first credit of every session. The overshoot
//!    is folded into the anchor as carry, bounded by the latency slack so a
//!    long freeze followed by a small partial credit can't run the display
//!    most of a minute ahead of the truth.
//! 5. A hold freezes the clock's notion of "now" at the instant pause/stop
//!    was clicked, before any network round trip. The user expects the
//!    clock to stop right then — not to keep ticking through the final
//!    flush and the pause POST, and not to snap anywhere when the flush's
//!    partial-minute credit lands underneath the held value. Releasing the
//!    hold (a failed pause) lets the clock catch back up to the truth.
//!
//! Time is wall-clock milliseconds, passed in by the caller, so the rules
//! are deterministic under test and so that a JavaScript surface handed an
//! anchor can derive the same value with `Date.now()`.

use serde::{Deserialize, Serialize};

use crate::timing::CAPTURE_INTERVAL_SECS;

/// Extra interpolation headroom past one capture interval, absorbing the
/// confirm round trip a credit takes to reach the UI. Mirrors
/// `TIMER_INTERPOLATION_SLACK_S` in @lookout/shared — keep the two in step.
/// Without it, any two confirms arriving more than one interval apart
/// (multi-MB clip uploads jitter by seconds) froze every clock surface
/// exactly at a xx:00 boundary until the next confirm landed.
pub const TIMER_INTERPOLATION_SLACK_SECS: i64 = 15;

/// Max seconds the display may run ahead of the last server-credited base.
/// Must equal `MAX_INTERPOLATION_S` in @lookout/react's useSessionTimer —
/// one capture interval plus latency slack.
pub const MAX_INTERPOLATION_SECS: i64 =
    CAPTURE_INTERVAL_SECS as i64 + TIMER_INTERPOLATION_SLACK_SECS;

/// What a UI surface needs to tick the clock itself. Hand this over, never
/// an already-interpolated display value: a surface that extrapolates from
/// a value that already contains the interpolated remainder double-counts,
/// which is how the tray popup once drifted ahead of the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockState {
    /// Ratcheted, server-credited seconds the display is anchored to.
    pub base_seconds: i64,
    /// Wall-clock ms when `base_seconds` last advanced (carry-adjusted).
    pub anchor_at_unix_ms: i64,
    /// Whether the clock is interpolating. False shows the bare base.
    pub running: bool,
    /// Wall-clock ms the clock is frozen at while a pause/stop is in flight.
    pub hold_at_unix_ms: Option<i64>,
    /// The interpolation cap, so a surface never has to know the constant.
    pub max_interpolation_seconds: i64,
}

/// The pure derivation every surface uses — the Rust twin of
/// `deriveDisplaySeconds` in @lookout/react. `now_ms` is wall-clock
/// milliseconds; a hold is applied by the caller via [`ClockState`].
pub fn derive_display_seconds(
    base_seconds: i64,
    anchor_at_unix_ms: i64,
    running: bool,
    now_ms: i64,
) -> i64 {
    if !running {
        return base_seconds;
    }
    let elapsed = (now_ms - anchor_at_unix_ms).div_euclid(1_000);
    base_seconds + elapsed.clamp(0, MAX_INTERPOLATION_SECS)
}

impl ClockState {
    /// What to show at `now_ms`, with the hold applied.
    pub fn display_seconds(&self, now_ms: i64) -> i64 {
        let clock_now = match self.hold_at_unix_ms {
            Some(hold) => now_ms.min(hold),
            None => now_ms,
        };
        derive_display_seconds(self.base_seconds, self.anchor_at_unix_ms, self.running, clock_now)
    }
}

/// The recorder's clock. See the module docs for the rules.
#[derive(Clone, Debug)]
pub struct RecordingClock {
    base_seconds: i64,
    anchor_at_unix_ms: i64,
    running: bool,
    hold_at_unix_ms: Option<i64>,
}

impl RecordingClock {
    /// A stopped clock showing `base_seconds`.
    pub fn new(base_seconds: i64, now_ms: i64) -> Self {
        Self {
            base_seconds: base_seconds.max(0),
            anchor_at_unix_ms: now_ms,
            running: false,
            hold_at_unix_ms: None,
        }
    }

    pub fn base_seconds(&self) -> i64 {
        self.base_seconds
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn state(&self) -> ClockState {
        ClockState {
            base_seconds: self.base_seconds,
            anchor_at_unix_ms: self.anchor_at_unix_ms,
            running: self.running,
            hold_at_unix_ms: self.hold_at_unix_ms,
            max_interpolation_seconds: MAX_INTERPOLATION_SECS,
        }
    }

    /// The clock's own "now": wall clock, stopped at the hold instant while
    /// a pause/stop is in flight.
    fn clock_now(&self, now_ms: i64) -> i64 {
        match self.hold_at_unix_ms {
            Some(hold) => now_ms.min(hold),
            None => now_ms,
        }
    }

    pub fn display_seconds(&self, now_ms: i64) -> i64 {
        self.state().display_seconds(now_ms)
    }

    /// A server-credited reading. Ratchets: a lower or equal value changes
    /// nothing (rule 3). On an advance, any display overshoot is carried
    /// into the anchor so the clock never moves backward (rule 4). Returns
    /// whether the base advanced.
    pub fn credit(&mut self, tracked_seconds: i64, now_ms: i64) -> bool {
        if tracked_seconds <= self.base_seconds {
            return false;
        }
        // The clock's frozen "now" while held, so a flush credit landing
        // mid-hold reconciles against the value the user is looking at and
        // the display stays put.
        let clock_now = self.clock_now(now_ms);
        let shown = derive_display_seconds(
            self.base_seconds,
            self.anchor_at_unix_ms,
            self.running,
            clock_now,
        );
        // Capped at the slack: carry exists to absorb confirm LATENCY, and
        // latency can't legitimately exceed the slack allowance. Without the
        // cap, a long freeze followed by a small partial credit would let
        // the display run most of a minute ahead of the credited truth.
        let carry = (shown - tracked_seconds).clamp(0, TIMER_INTERPOLATION_SLACK_SECS);
        self.base_seconds = tracked_seconds;
        self.anchor_at_unix_ms = clock_now - carry * 1_000;
        true
    }

    /// Start interpolating from the current base, from `now_ms`. Clears any
    /// hold: resuming is the one thing that always releases it.
    pub fn start(&mut self, now_ms: i64) {
        self.hold_at_unix_ms = None;
        self.anchor_at_unix_ms = now_ms;
        self.running = true;
    }

    /// Stop interpolating. The display drops to the bare base (rule 2).
    /// The anchor is deliberately left alone: a later `start` re-anchors,
    /// and nothing in between may bake paused time into the base.
    pub fn stop(&mut self) {
        self.running = false;
        self.hold_at_unix_ms = None;
    }

    /// Freeze the clock's "now" at the pause/stop click (rule 5).
    pub fn hold(&mut self, now_ms: i64) {
        if self.hold_at_unix_ms.is_none() {
            self.hold_at_unix_ms = Some(now_ms);
        }
    }

    /// A pause that failed: let the clock catch back up rather than lie
    /// frozen while the session is still recording.
    pub fn release_hold(&mut self) {
        self.hold_at_unix_ms = None;
    }
}

/// These pin the clock to the contract every rendering surface is written
/// against; they are ports of the `useSessionTimer` tests in @lookout/react,
/// so a change here that breaks one of them would break the web client's
/// clock too.
#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000;
    const MAX: i64 = MAX_INTERPOLATION_SECS;

    fn running(base: i64) -> RecordingClock {
        let mut c = RecordingClock::new(base, T0);
        c.start(T0);
        c
    }

    #[test]
    fn cap_matches_the_js_side() {
        // MAX_INTERPOLATION_S in useSessionTimer.ts is
        // SCREENSHOT_INTERVAL_MS / 1000 + TIMER_INTERPOLATION_SLACK_S = 60 + 15.
        assert_eq!(MAX, 75);
    }

    #[test]
    fn initializes_to_the_server_value() {
        let c = RecordingClock::new(0, T0);
        assert_eq!(c.display_seconds(T0), 0);
        let c = RecordingClock::new(100, T0);
        assert_eq!(c.display_seconds(T0 + 30_000), 100);
    }

    #[test]
    fn ticks_forward_at_wall_clock_rate_while_running() {
        let c = running(0);
        assert_eq!(c.display_seconds(T0 + 30_000), 30);
        assert_eq!(c.display_seconds(T0 + 30_999), 30);
    }

    #[test]
    fn re_anchors_when_a_credit_lands() {
        let mut c = running(0);
        assert!(c.credit(60, T0 + 30_000));
        assert_eq!(c.display_seconds(T0 + 30_000), 60);
    }

    #[test]
    fn caps_interpolation_at_one_interval_plus_slack() {
        let c = running(0);
        assert_eq!(c.display_seconds(T0 + 180_000), MAX);
    }

    #[test]
    fn does_not_freeze_at_the_minute_while_a_late_confirm_is_in_flight() {
        // A cap of exactly 60 froze the display at every minute boundary for
        // the length of the confirm latency — read as the recorder hanging.
        let c = running(60);
        assert_eq!(c.display_seconds(T0 + 65_000), 125);
    }

    #[test]
    fn stays_frozen_indefinitely_when_captures_stall() {
        let c = running(120);
        assert_eq!(c.display_seconds(T0 + 600_000), 120 + MAX);
    }

    #[test]
    fn a_late_credit_lands_without_the_clock_moving_backward() {
        // 3s of confirm latency: the display ticked to 123; the credit for
        // 120 lands and the overshoot is carried, never a jump back to 120.
        let mut c = running(60);
        let now = T0 + 63_000;
        let shown_before = c.display_seconds(now);
        assert_eq!(shown_before, 123);
        c.credit(120, now);
        assert_eq!(c.display_seconds(now), shown_before);
        assert_eq!(c.display_seconds(now + 1_000), 124);
    }

    #[test]
    fn after_a_genuine_stall_a_full_credit_continues_from_the_frozen_value() {
        let mut c = running(60);
        let now = T0 + 120_000;
        assert_eq!(c.display_seconds(now), 60 + MAX);
        c.credit(120, now);
        assert_eq!(c.display_seconds(now), 60 + MAX);
    }

    #[test]
    fn carry_is_capped_at_the_slack_after_a_stall_and_a_small_credit() {
        let mut c = running(60);
        let now = T0 + 600_000;
        c.credit(70, now);
        assert_eq!(c.display_seconds(now), 70 + (MAX - 60));
    }

    #[test]
    fn interpolation_resumes_carrying_the_frozen_overshoot() {
        let mut c = running(60);
        let now = T0 + 120_000;
        c.credit(120, now);
        assert_eq!(c.display_seconds(now + 30_000), 135 + 30);
    }

    #[test]
    fn back_to_back_credits_catch_up() {
        let mut c = running(0);
        let now = T0 + 200_000;
        c.credit(60, now);
        c.credit(120, now);
        c.credit(180, now);
        assert_eq!(c.display_seconds(now), 180);
    }

    #[test]
    fn hold_freezes_the_clock_at_the_click_through_the_flush() {
        let mut c = running(0);
        let click = T0 + 23_000;
        assert_eq!(c.display_seconds(click), 23);
        c.hold(click);
        assert_eq!(c.display_seconds(click + 5_000), 23);
        assert_eq!(c.display_seconds(click + 60_000), 23);
    }

    #[test]
    fn the_flush_credit_lands_under_the_held_value_without_moving_it() {
        let mut c = running(0);
        let click = T0 + 23_000;
        c.hold(click);
        let held = c.display_seconds(click + 4_000);
        assert_eq!(held, 23);
        // The flush credits the exact elapsed 23s; the display stays put...
        c.credit(23, click + 4_000);
        assert_eq!(c.display_seconds(click + 4_000), held);
        // ...and the paused snap lands on the same number: pause at 0:23
        // SHOWS 0:23.
        c.stop();
        assert_eq!(c.display_seconds(click + 10_000), 23);
    }

    #[test]
    fn a_failed_pause_releases_the_hold_and_the_clock_catches_up() {
        let mut c = running(0);
        c.hold(T0 + 20_000);
        assert_eq!(c.display_seconds(T0 + 30_000), 20);
        c.release_hold();
        assert_eq!(c.display_seconds(T0 + 31_000), 31);
    }

    #[test]
    fn hold_is_first_click_wins() {
        // Stop clicked right after Pause: the clock stays at the first click.
        let mut c = running(0);
        c.hold(T0 + 10_000);
        c.hold(T0 + 12_000);
        assert_eq!(c.display_seconds(T0 + 20_000), 10);
    }

    #[test]
    fn stop_snaps_the_display_to_the_base() {
        let mut c = running(60);
        assert!(c.display_seconds(T0 + 45_000) > 100);
        c.stop();
        assert_eq!(c.display_seconds(T0 + 45_000), 60);
    }

    #[test]
    fn max_drop_on_pause_is_bounded_by_the_cap() {
        let mut c = running(0);
        let before = c.display_seconds(T0 + 600_000);
        c.stop();
        assert!(before - c.display_seconds(T0 + 600_000) <= MAX);
    }

    #[test]
    fn display_stays_frozen_at_the_base_while_paused() {
        let c = RecordingClock::new(60, T0);
        assert_eq!(c.display_seconds(T0 + 120_000), 60);
    }

    #[test]
    fn resume_interpolates_from_the_current_base() {
        let mut c = RecordingClock::new(120, T0);
        assert_eq!(c.display_seconds(T0), 120);
        c.start(T0);
        assert_eq!(c.display_seconds(T0 + 30_000), 150);
    }

    #[test]
    fn resume_does_not_double_count_the_pause() {
        let mut c = running(60);
        assert_eq!(c.display_seconds(T0 + 30_000), 90);
        c.stop();
        let resume_at = T0 + 330_000; // 5 minutes paused
        c.start(resume_at);
        assert_eq!(c.display_seconds(resume_at + 1_000), 61);
    }

    #[test]
    fn a_lower_reading_never_moves_the_display_backward() {
        let mut c = running(120);
        assert!(!c.credit(60, T0 + 1_000));
        assert_eq!(c.display_seconds(T0 + 1_000), 121);
        assert!(c.credit(180, T0 + 1_000));
        assert_eq!(c.display_seconds(T0 + 1_000), 180);
    }

    #[test]
    fn an_equal_reading_does_not_restart_the_interpolation_window() {
        // A repeated reading must not reset the anchor, or the clock silently
        // loses up to a minute of elapsed time the other surfaces kept.
        let mut c = running(120);
        assert!(!c.credit(120, T0 + 40_000));
        assert_eq!(c.display_seconds(T0 + 40_000), 160);
    }

    #[test]
    fn late_credit_after_a_cap_carries_bounded_overshoot() {
        let mut c = running(60);
        let now = T0 + 90_000;
        assert_eq!(c.display_seconds(now), 60 + MAX);
        c.credit(120, now);
        assert_eq!(c.display_seconds(now), 60 + MAX);
    }

    #[test]
    fn network_loss_then_catch_up_continues_from_the_frozen_value() {
        let mut c = running(60);
        let now = T0 + 300_000;
        assert_eq!(c.display_seconds(now), 60 + MAX);
        c.credit(120, now);
        assert_eq!(c.display_seconds(now), 120 + (MAX - 60));
    }

    #[test]
    fn a_credit_while_paused_sets_the_base_with_no_carry() {
        // Nothing is interpolating, so there is no overshoot to carry.
        let mut c = RecordingClock::new(60, T0);
        c.credit(120, T0 + 50_000);
        assert_eq!(c.display_seconds(T0 + 50_000), 120);
        c.start(T0 + 50_000);
        assert_eq!(c.display_seconds(T0 + 51_000), 121);
    }

    #[test]
    fn state_round_trips_through_the_shared_derivation() {
        // The UI surfaces derive from ClockState with the same arithmetic; a
        // snapshot taken at any instant must agree with the clock itself.
        let mut c = running(60);
        c.credit(120, T0 + 63_000);
        c.hold(T0 + 70_000);
        for dt in [0, 5_000, 65_000, 100_000] {
            let now = T0 + dt;
            assert_eq!(c.state().display_seconds(now), c.display_seconds(now));
        }
    }
}
