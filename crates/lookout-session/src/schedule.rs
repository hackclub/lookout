//! The capture schedule's rules, as pure arithmetic on wall-clock
//! milliseconds. The loop that owns the timer asks these questions; the
//! answers are what the server's credit grid rewards.

/// When the next capture should fire, from the server's `nextExpectedAt`
/// (`target_ms`, server wall-clock) given the corrected local `now_ms`, the
/// cadence, and the slot of the newest capture already taken.
///
/// `None` when the target is not news: a confirm delivered late (out of
/// the backlog) names the slot after ITS capture, which has been captured
/// since. Acting on that fired an extra, off-grid tick that reset the
/// credit streak. Only a target clear of the newest captured slot by half
/// an interval — the credit window — is taken.
///
/// A target already in the past means "fire now" (catch-up after sleep).
/// The delay is capped at twice the interval as a guard against a
/// malformed target; `clamped` says when that bound bit, which should
/// never happen once the clock offset is applied.
pub fn next_capture_delay_ms(
    target_ms: i64,
    now_ms: i64,
    interval_ms: u64,
    last_slot_ms: i64,
) -> Option<NextCapture> {
    if target_ms <= last_slot_ms + (interval_ms / 2) as i64 {
        return None;
    }
    let delay_ms = (target_ms - now_ms).max(0) as u64;
    let clamp_ms = interval_ms * 2;
    Some(NextCapture {
        delay_ms: delay_ms.min(clamp_ms),
        clamped: delay_ms > clamp_ms,
    })
}

/// A scheduled next capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NextCapture {
    pub delay_ms: u64,
    /// The server's target was further out than twice the interval and was
    /// capped.
    pub clamped: bool,
}

/// Which captured-but-undelivered intervals a backlog keeps. Oldest
/// first in; the oldest beyond the cap go, anything past the age limit
/// goes (the server would re-stamp it with its own clock, wrecking the
/// grid rather than helping), and another session's leftovers go quietly.
///
/// Returns, per item in order, whether to keep it, plus the number dropped
/// that belonged to this session (worth telling the user about).
pub fn backlog_keep(
    items: impl Iterator<Item = BacklogItem>,
    now_ms: i64,
    max_age_ms: u64,
    max_items: usize,
) -> (Vec<bool>, usize) {
    let items: Vec<BacklogItem> = items.collect();
    let mut pending = items.len();
    let mut keep = Vec::with_capacity(items.len());
    let mut dropped = 0;
    for item in &items {
        let too_old = now_ms - item.captured_ms > max_age_ms as i64;
        let over_cap = pending > max_items;
        if !item.ours || too_old || over_cap {
            pending -= 1;
            if item.ours {
                dropped += 1;
            }
            keep.push(false);
        } else {
            keep.push(true);
        }
    }
    (keep, dropped)
}

/// What the backlog policy needs to know about one queued interval.
#[derive(Clone, Copy, Debug)]
pub struct BacklogItem {
    pub captured_ms: i64,
    /// Captured for the session the uploader is serving.
    pub ours: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const I: u64 = 60_000;

    #[test]
    fn a_target_after_the_newest_slot_schedules_it() {
        let n = next_capture_delay_ms(120_000, 70_000, I, 60_000).unwrap();
        assert_eq!(n.delay_ms, 50_000);
        assert!(!n.clamped);
    }

    #[test]
    fn a_stale_target_from_a_late_confirm_is_ignored() {
        // The confirm for the slot at 60s says "next at 120s", but the
        // loop has already captured 120s and 180s.
        assert_eq!(next_capture_delay_ms(120_000, 185_000, I, 180_000), None);
        // Just past the newest slot but inside the window: still stale.
        assert_eq!(next_capture_delay_ms(200_000, 185_000, I, 180_000), None);
    }

    #[test]
    fn a_past_target_fires_now() {
        let n = next_capture_delay_ms(120_000, 130_000, I, 60_000).unwrap();
        assert_eq!(n.delay_ms, 0);
    }

    #[test]
    fn a_far_target_is_clamped_to_twice_the_interval() {
        let n = next_capture_delay_ms(1_000_000, 0, I, 0).unwrap();
        assert_eq!(n.delay_ms, 2 * I);
        assert!(n.clamped);
    }

    #[test]
    fn backlog_drops_old_foreign_and_over_cap_oldest_first() {
        let items = [
            BacklogItem { captured_ms: 0, ours: true },        // too old
            BacklogItem { captured_ms: 100_000, ours: false },  // foreign
            BacklogItem { captured_ms: 110_000, ours: true },   // over cap (oldest of ours)
            BacklogItem { captured_ms: 120_000, ours: true },
            BacklogItem { captured_ms: 130_000, ours: true },
            BacklogItem { captured_ms: 140_000, ours: true },
        ];
        let (keep, dropped) = backlog_keep(items.into_iter(), 150_000, 100_000, 3);
        assert_eq!(keep, vec![false, false, false, true, true, true]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn backlog_keeps_everything_within_limits() {
        let items = [
            BacklogItem { captured_ms: 10, ours: true },
            BacklogItem { captured_ms: 20, ours: true },
        ];
        let (keep, dropped) = backlog_keep(items.into_iter(), 100, 1_000, 5);
        assert_eq!(keep, vec![true, true]);
        assert_eq!(dropped, 0);
    }
}
