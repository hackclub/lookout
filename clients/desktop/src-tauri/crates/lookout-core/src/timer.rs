//! The recording clock shown in the menu bar / tray. The ticker is
//! `lookout_recorder::ticker`; re-exported here for the shell.

pub use lookout_recorder::ticker::{
    format_tray_time, tray_display_seconds, TrayTickerHandle, MAX_TRAY_INTERPOLATION_SECS,
    TIMER_INTERPOLATION_SLACK_SECS,
};
pub use lookout_session::clock::derive_display_seconds;
