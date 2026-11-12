//! The timing the capture pipeline runs on. Production values are the
//! defaults; a simulation test scales a `Core`'s copy down so an hour of
//! pause/resume cycling, outages and slow links runs in seconds of real
//! time, without touching any other test in the process.

use std::time::Duration;

/// Cadence of credited captures: one per server interval.
pub const CAPTURE_INTERVAL_SECS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Timing {
    /// The capture cadence.
    pub capture_interval_ms: u64,
    /// Backoff before retry `i` (0-based) of one upload step; past the
    /// end, the last entry repeats.
    pub retry_delays_ms: [u64; 3],
    /// Attempts per upload step before it gives up, the first included
    /// (the SDK's `retry.maxRetries`). At least one is always made.
    pub max_attempts: u32,
    /// Per-request timeout for one attempt of an upload step or a status
    /// check.
    pub step_timeout_ms: u64,
    /// Deadline for a session action the user is waiting on — pause,
    /// resume, stop, rename, the session load. While a pause's POST is in
    /// flight the loop is already stopped, so every second of this is a
    /// second not recorded if the link is hung: keep it short.
    pub api_timeout_ms: u64,
    /// How old a captured-but-unconfirmed interval may get before the
    /// uploader stops trying to deliver it. The server stamps anything
    /// older than five minutes with its own clock (which would wreck the
    /// credit grid rather than help), so give up safely short of that.
    pub backlog_max_age_ms: u64,
    /// Everything the pause/stop flush may take, backlog included: the
    /// pause button must never hang on the network, and
    /// `stop_capture_loop`'s backstop sits just outside this.
    pub flush_budget_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            capture_interval_ms: CAPTURE_INTERVAL_SECS * 1000,
            retry_delays_ms: [2_000, 4_000, 8_000],
            max_attempts: 3,
            step_timeout_ms: 30_000,
            api_timeout_ms: 15_000,
            backlog_max_age_ms: 4 * 60 * 1000,
            flush_budget_ms: 12_000,
        }
    }
}

impl Timing {
    pub fn step_timeout(&self) -> Duration {
        Duration::from_millis(self.step_timeout_ms)
    }

    pub fn api_timeout(&self) -> Duration {
        Duration::from_millis(self.api_timeout_ms)
    }

    pub fn flush_budget(&self) -> Duration {
        Duration::from_millis(self.flush_budget_ms)
    }

    pub fn retry_delay(&self, attempt: usize) -> Duration {
        Duration::from_millis(self.retry_delays_ms[attempt.min(self.retry_delays_ms.len() - 1)])
    }

    /// Everything scaled so that one interval is `interval_ms` — for
    /// simulations that run an hour of recording in seconds.
    pub fn scaled(interval_ms: u64) -> Self {
        let f = |ms: u64| (ms * interval_ms / (CAPTURE_INTERVAL_SECS * 1000)).max(1);
        let d = Self::default();
        Self {
            capture_interval_ms: interval_ms,
            retry_delays_ms: [f(d.retry_delays_ms[0]), f(d.retry_delays_ms[1]), f(d.retry_delays_ms[2])],
            max_attempts: d.max_attempts,
            step_timeout_ms: f(d.step_timeout_ms),
            api_timeout_ms: f(d.api_timeout_ms),
            backlog_max_age_ms: f(d.backlog_max_age_ms),
            flush_budget_ms: f(d.flush_budget_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Timing JSON from before `maxAttempts` existed (a host's saved
    /// override, or the SDK's own tests) still loads, with the old fixed
    /// three attempts.
    #[test]
    fn json_without_max_attempts_keeps_three() {
        let t: Timing = serde_json::from_str(r#"{"captureIntervalMs":2000,"retryDelaysMs":[60,120,240]}"#).unwrap();
        assert_eq!(t.max_attempts, 3);
        assert_eq!(t.capture_interval_ms, 2000);
        assert_eq!(t.retry_delays_ms, [60, 120, 240]);
        assert_eq!(t.step_timeout_ms, Timing::default().step_timeout_ms);
    }
}
