//! The recording session as pure logic: no I/O, no runtime, no platform.
//!
//! What every Lookout client shares regardless of how it captures frames
//! or speaks HTTP: the recording's phases and the controls they imply, the
//! one clock every surface ticks from, the clock-offset estimate that keeps
//! a wrong system clock from costing credit, the capture schedule rules,
//! and the timing constants the pipeline runs on. `lookout-core` drives
//! this on the desktop; a browser or a device host drives the same crate.
//!
//! Nothing here blocks, sleeps or allocates beyond the state it holds.

#![forbid(unsafe_code)]

pub mod clock;
pub mod clock_offset;
pub mod config;
pub mod log;
pub mod schedule;
pub mod source;
pub mod state;
pub mod time;
pub mod timing;

pub use clock::{ClockState, RecordingClock, MAX_INTERPOLATION_SECS, TIMER_INTERPOLATION_SLACK_SECS};
pub use clock_offset::ClockOffset;
pub use config::{SessionClipCapabilities, SessionConfig};
pub use source::{CaptureSource, PickedSource, RecordingSource};
pub use state::*;
pub use timing::{Timing, CAPTURE_INTERVAL_SECS};
