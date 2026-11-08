//! Client clock-offset estimation — the Rust mirror of `ClockOffset` in
//! `@lookout/shared`. Keep the two in step: the desktop loop and the web SDK
//! each stamp captures and schedule ticks off their own clock, so any
//! difference in the arithmetic shows up as the two clients crediting
//! differently on the same machine.
//!
//! Every timestamp the credit system reads comes from the client's own clock,
//! measured against a ±30s streak window inside a ±5min trust envelope. The
//! server adopts its own time for captures whose stamps it can't trust, so a
//! wrong clock never costs the recording. This closes the other half: the
//! client learns how far off it is from the server's own timestamps and
//! corrects both its stamps and its schedule, so a skewed clock costs nothing
//! at all.
//!
//! The schedule half is the one that bit in production: `nextExpectedAt` is
//! server wall-clock, and subtracting the local clock from it baked the skew
//! into every tick delay. Any skew past the ±30s window zeroed the credit;
//! past 60s the defensive 2x-interval clamp turned the loop into a plausible-
//! looking half-rate recording with no error anywhere.

/// Smallest offset worth correcting for. Below this, the "correction" would
/// be indistinguishable from network jitter and would only add noise.
pub const CLOCK_OFFSET_DEADBAND_MS: i64 = 2_000;

/// A running estimate of `serverNow - clientNow`, in milliseconds.
///
/// Not a time sync protocol: we only need to be well inside a 30-second
/// window, and one sample per minute arrives for free on every upload.
pub struct ClockOffset {
    offset_ms: f64,
    samples: u64,
}

impl ClockOffset {
    pub const fn new() -> Self {
        Self {
            offset_ms: 0.0,
            samples: 0,
        }
    }

    /// Fold in one observation.
    ///
    /// `server_ms` is the server's clock when it handled the request (already
    /// parsed — the caller owns ISO parsing), and `request_sent_at_ms` /
    /// `response_received_at_ms` bracket it on the local clock. The server's
    /// timestamp was taken somewhere inside that window, so the local instant
    /// that best corresponds to it is the midpoint — which removes most of
    /// the round trip from the estimate rather than charging all of it to the
    /// offset. Same reasoning as NTP, minus the rigour we don't need.
    pub fn observe(
        &mut self,
        server_ms: i64,
        request_sent_at_ms: i64,
        response_received_at_ms: i64,
    ) {
        // A negative interval means the local clock moved under us
        // mid-request (NTP correction, sleep/wake). Treat the sample as
        // untrustworthy rather than folding a jump into the estimate.
        if response_received_at_ms < request_sent_at_ms {
            return;
        }

        let local_midpoint = request_sent_at_ms as f64
            + (response_received_at_ms - request_sent_at_ms) as f64 / 2.0;
        let sample = server_ms as f64 - local_midpoint;

        // First sample is adopted outright — a badly wrong clock should be
        // corrected on the very next capture, not eased into over many
        // minutes. Later samples are smoothed, so ordinary jitter doesn't
        // wobble the stamps we send.
        self.offset_ms = if self.samples == 0 {
            sample
        } else {
            self.offset_ms * 0.75 + sample * 0.25
        };
        self.samples += 1;
    }

    /// Current estimate of how far the local clock is behind the server's.
    pub fn offset_ms(&self) -> i64 {
        if self.samples == 0 {
            0
        } else {
            self.offset_ms.round() as i64
        }
    }

    /// True once an offset large enough to matter has been observed.
    pub fn is_significant(&self) -> bool {
        self.offset_ms().abs() >= CLOCK_OFFSET_DEADBAND_MS
    }

    /// Correct a local timestamp into server time.
    ///
    /// A no-op inside the deadband, so a healthy client's timestamps are
    /// passed through untouched and nothing about its behaviour changes.
    pub fn correct(&self, local_ms: i64) -> i64 {
        if self.is_significant() {
            local_ms + self.offset_ms()
        } else {
            local_ms
        }
    }
}

#[cfg(test)]
mod tests {
    // Mirrors clockOffset.test.ts in @lookout/shared, case for case.
    use super::*;

    const LOCAL: i64 = 1_700_000_000_000;

    #[test]
    fn no_op_before_it_has_seen_anything() {
        let c = ClockOffset::new();
        assert_eq!(c.offset_ms(), 0);
        assert!(!c.is_significant());
        assert_eq!(c.correct(1_000), 1_000);
    }

    #[test]
    fn leaves_a_healthy_clock_untouched() {
        // Well inside the deadband: correcting here would add noise, not
        // accuracy.
        let mut c = ClockOffset::new();
        c.observe(LOCAL + 300, LOCAL, LOCAL + 100);
        assert!(!c.is_significant());
        assert_eq!(c.correct(LOCAL), LOCAL);
    }

    #[test]
    fn corrects_a_slow_clock_on_the_first_sample() {
        // First sample is adopted outright — a badly wrong clock must be
        // fixed on the very next capture, not eased into over ten minutes
        // of lost credit.
        let mut c = ClockOffset::new();
        let skew = 7 * 60_000;
        c.observe(LOCAL + skew, LOCAL, LOCAL + 40);
        assert!(c.is_significant());
        assert!((c.correct(LOCAL) - (LOCAL + skew)).abs() < 100);
    }

    #[test]
    fn corrects_a_fast_clock() {
        let mut c = ClockOffset::new();
        let skew = -9 * 60_000;
        c.observe(LOCAL + skew, LOCAL, LOCAL + 40);
        assert!((c.correct(LOCAL) - (LOCAL + skew)).abs() < 100);
    }

    #[test]
    fn charges_the_round_trip_to_latency_not_offset() {
        // A perfectly synced clock observed over a slow request must not
        // read as skewed by half the RTT.
        let mut c = ClockOffset::new();
        let rtt = 4_000;
        c.observe(LOCAL + rtt / 2, LOCAL, LOCAL + rtt);
        assert!(c.offset_ms().abs() < CLOCK_OFFSET_DEADBAND_MS);
        assert!(!c.is_significant());
    }

    #[test]
    fn smooths_later_samples() {
        let mut c = ClockOffset::new();
        let skew = 60_000;
        c.observe(LOCAL + skew, LOCAL, LOCAL + 20);
        let after_first = c.offset_ms();
        // One wild outlier must not move the estimate far.
        c.observe(LOCAL + skew + 30_000, LOCAL, LOCAL + 20);
        assert!(c.offset_ms() > after_first);
        assert!(c.offset_ms() < after_first + 30_000);
    }

    #[test]
    fn ignores_a_sample_whose_local_window_ran_backwards() {
        let mut c = ClockOffset::new();
        c.observe(LOCAL + 60_000, LOCAL, LOCAL - 5_000);
        assert_eq!(c.offset_ms(), 0);
    }

    #[test]
    fn brings_a_skewed_capture_inside_the_credit_window() {
        // End to end: a device 6 minutes fast is outside the ±5min envelope.
        // After one observation its stamps land within a second of server
        // time — comfortably inside the ±30s streak window.
        let mut c = ClockOffset::new();
        let server_ms = LOCAL;
        let device_ms = server_ms + 6 * 60_000;
        c.observe(server_ms, device_ms, device_ms + 50);
        assert!((c.correct(device_ms) - server_ms).abs() < 1_000);
    }
}
