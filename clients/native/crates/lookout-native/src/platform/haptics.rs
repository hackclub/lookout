//! Trackpad haptics (macOS Force Touch trackpads): the light "alignment"
//! tap Final Cut and Keynote give when something snaps into place. Silent
//! elsewhere, and on mice.

/// One alignment tap, now.
pub fn alignment() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{
            NSHapticFeedbackManager, NSHapticFeedbackPattern, NSHapticFeedbackPerformanceTime, NSHapticFeedbackPerformer,
        };
        let performer = NSHapticFeedbackManager::defaultPerformer();
        performer.performFeedbackPattern_performanceTime(NSHapticFeedbackPattern::Alignment, NSHapticFeedbackPerformanceTime::Now);
    }
}

/// Taps once each time a drag comes to rest against something (a timeline
/// end, another cut's edge, the frame's edge), not again while it stays
/// there, and again after it leaves and comes back.
#[derive(Default, Debug)]
pub struct Detent {
    engaged: bool,
    fresh: bool,
}

impl Detent {
    /// Whether the dragged thing touches something now; returns whether it tapped.
    pub fn update(&mut self, touching: bool) -> bool {
        let tap = touching && !self.engaged && !std::mem::take(&mut self.fresh);
        self.engaged = touching;
        if tap {
            alignment();
        }
        tap
    }

    /// A new drag: whatever it starts against doesn't count as arriving.
    pub fn start(&mut self) {
        self.fresh = true;
    }
}

#[cfg(test)]
mod tests {
    use super::Detent;

    #[test]
    fn taps_on_arrival_only() {
        let mut d = Detent::default();
        d.start();
        assert!(!d.update(true), "already touching when the drag began");
        assert!(!d.update(false));
        assert!(d.update(true), "arrived");
        assert!(!d.update(true), "still there");
        assert!(!d.update(false));
        assert!(d.update(true), "arrived again");
    }
}
