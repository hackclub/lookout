//! The recording as a state machine's data: its phases, what the shell
//! renders, and the pure transitions. A host drives it (talks to the
//! server, runs the capture loop) and never has to decide what the
//! controls should look like.

use serde::{Deserialize, Serialize};

use crate::clock::{ClockState, RecordingClock};
use crate::config::{SessionClipCapabilities, SessionConfig};
use crate::source::RecordingSource;

/// Capture bounds and JPEG quality for a recording. Mirror `MAX_WIDTH`,
/// `MAX_HEIGHT` and `JPEG_QUALITY` in @lookout/shared.
pub const MAX_CAPTURE_WIDTH: u32 = 1920;
pub const MAX_CAPTURE_HEIGHT: u32 = 1080;
pub const CAPTURE_JPEG_QUALITY: u8 = 85;

/// Server-credited seconds below which a session has no timelapse in it.
/// Mirrors `MIN_STOPPABLE_TRACKED_SECONDS` in @lookout/shared.
pub const MIN_STOPPABLE_TRACKED_SECONDS: i64 = 60;

/// No successful capture for this long while capturing means something is
/// wrong — the banner says so. Sleep is not a false positive: the loop's
/// first act on wake is a capture, which clears it.
pub const STALE_CAPTURE_SECS: u64 = 3 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecorderPhase {
    /// The session is being fetched.
    Loading,
    /// The session couldn't be fetched; `session_error` says why.
    LoadError,
    /// The capture loop is running.
    Recording,
    /// Pause clicked: flushing the partial minute, then pausing the session.
    Pausing,
    /// The session is paused (or the source is lost); no loop.
    Paused,
    /// Resume clicked: resuming the session, then starting the loop.
    Resuming,
    /// Stop clicked: the same flush-then-pause as `Pausing`, on the way to
    /// the naming prompt.
    StopPausing,
    /// Paused, waiting for a name / "edit & save" / "keep recording".
    StopPrompting,
    /// The final stop is in flight (rename, then stop with retries).
    StopFinishing,
    /// The session is in a terminal state — stopped, compiling, complete
    /// or failed. Nothing more to do here.
    Finished,
}

/// What the controls should look like: the pinned, pre-action reading
/// during a transition, so the pulsing dot and the buttons don't flash
/// through intermediate states on the way through a pause or a stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlMode {
    Recording,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceLossReason {
    /// The portal told us the cast ended — the user hit Stop in the
    /// system's sharing indicator.
    Revoked,
    /// The stream went quiet without the portal ever saying so, and the
    /// capture loop's watchdog gave up on it.
    Failed,
}

/// Why a screen source stopped producing frames.
///
/// A screencast can end without the recording ending, and only on
/// Linux/Wayland does that happen behind the app's back. Neither case is
/// recoverable in code — portal sessions are opened without a restore
/// token, so the only way back is a trip through the picker. Hence a
/// distinct state from a capture error: this one has an action attached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLoss {
    pub reason: SourceLossReason,
    /// What to put in front of the user.
    pub message: String,
    /// The raw capture error behind a `Failed` loss, for the details dialog.
    pub detail: Option<String>,
}

/// Everything a shell renders for an open recording.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderState {
    pub token: String,
    pub source: RecordingSource,
    pub phase: RecorderPhase,
    /// The server's status string, as last seen. `None` until loaded.
    pub session_status: Option<String>,
    pub session_name: String,
    /// Why the session couldn't be loaded (`LoadError`).
    pub session_error: Option<String>,
    /// Why the last pause/resume/stop failed. Cleared by the next action.
    pub action_error: Option<String>,
    pub control_mode: ControlMode,
    /// A loop is running, flushing, or about to start — the updater must
    /// not install (and exit the process) while this is true.
    pub capturing: bool,
    pub clock: ClockState,
    /// The clock at the moment of this snapshot; surfaces that tick re-derive
    /// from `clock` instead of reading this.
    pub display_seconds: i64,
    /// Ratcheted server-credited seconds.
    pub tracked_seconds: i64,
    /// Under one capture unit there is nothing for the compile to work with.
    /// Gates the stop prompt's copy, never the button.
    pub too_short_to_compile: bool,
    /// Capture units the server holds: the count at load plus every
    /// confirm since.
    pub capture_count: u32,
    /// The latest capture failure. The loop carries on; the banner says
    /// progress so far is safe.
    pub capture_error: Option<String>,
    pub source_lost: Option<SourceLoss>,
    /// No capture has landed for `STALE_CAPTURE_SECS` while capturing.
    pub stale: bool,
    pub last_capture_at_unix_ms: Option<i64>,
    /// Captured intervals the uploader still owes the server (the
    /// backlog). Filled in by the recorder; the state alone doesn't know.
    #[serde(default)]
    pub pending_uploads: u32,
}

/// What came of a finished stop.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopOutcome {
    /// The session's status after the stop: usually `stopped`; `compiling`
    /// or `complete` if the server had already moved on.
    pub status: String,
    /// The stop asked for an edit hold, so the shell should open its editor.
    pub edit: bool,
}

/// The outcome of [`Core::session_finish`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishOutcome {
    pub status: String,
    pub tracked_seconds: Option<i64>,
    pub total_active_seconds: Option<i64>,
}

/// Something worth an OS notification: the user is most likely looking at
/// something else when it happens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderNotice {
    pub title: String,
    pub body: String,
}

/// Why a recorder action was refused or failed.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RecorderError {
    /// No recording is open.
    NoRecording,
    /// The action doesn't apply to the recording's current phase.
    InvalidPhase { phase: RecorderPhase },
    /// There is no source to resume onto; the shell should ask for one.
    SourceLost,
    /// The server refused or couldn't be reached.
    Api { status: Option<u16>, message: String },
    Other { message: String },
}

impl std::fmt::Display for RecorderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecorderError::NoRecording => f.write_str("no recording is open"),
            RecorderError::InvalidPhase { phase } => {
                write!(f, "action doesn't apply while {phase:?}")
            }
            RecorderError::SourceLost => f.write_str("the capture source is gone"),
            RecorderError::Api { message, .. } => f.write_str(message),
            RecorderError::Other { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for RecorderError {}

pub fn is_terminal(status: &str) -> bool {
    matches!(status, "stopped" | "compiling" | "complete" | "failed")
}

/// Active or pending: the states a capture loop may run in. A pending
/// session is activated by its first upload.
pub fn is_live(status: &str) -> bool {
    matches!(status, "active" | "pending")
}

// ── Internal state ───────────────────────────────────────────────────

/// The open recording. Lives in `Core::recorder`; mutated under that
/// mutex and never held across an await.
pub struct Recording {
    pub cfg: SessionConfig,
    pub source: RecordingSource,
    pub phase: RecorderPhase,
    pub session_status: Option<String>,
    pub session_name: String,
    pub session_error: Option<String>,
    pub action_error: Option<String>,
    pub clock: RecordingClock,
    /// `screenshotCount` from the session load.
    pub base_capture_count: u32,
    /// Confirms observed since.
    pub ticks: u32,
    pub capture_error: Option<String>,
    pub source_lost: Option<SourceLoss>,
    /// Unix ms of the last confirmed capture, for the stale check and the
    /// shell.
    pub last_capture_at: Option<i64>,
    pub clip_capabilities: Option<SessionClipCapabilities>,
    /// The generation of the loop this recording is listening to.
    pub loop_generation: u64,
    /// Whether the shell's camera has been asked to open and not yet
    /// released. Every `camera_start` is matched by exactly one
    /// `camera_stop`, whichever path ends the capturing.
    pub camera_open: bool,
    /// For the notice diff: whether an error was showing last time, and
    /// the status last published.
    pub notice_error_shown: bool,
    pub notice_status: Option<String>,
    /// For the housekeeping tick: the stale flag last published.
    pub published_stale: bool,
}


impl Recording {
    pub fn new(cfg: SessionConfig, source: RecordingSource, now_ms: i64) -> Self {
        Self {
            cfg,
            source,
            phase: RecorderPhase::Loading,
            session_status: None,
            session_name: String::new(),
            session_error: None,
            action_error: None,
            clock: RecordingClock::new(0, now_ms),
            base_capture_count: 0,
            ticks: 0,
            capture_error: None,
            source_lost: None,
            last_capture_at: None,
            clip_capabilities: None,
            loop_generation: 0,
            camera_open: false,
            notice_error_shown: false,
            notice_status: None,
            published_stale: false,
        }
    }

    pub fn status_is_live(&self) -> bool {
        self.session_status.as_deref().is_some_and(is_live)
    }

    /// Pin the controls to the pre-action state during a transition.
    pub fn control_mode(&self) -> ControlMode {
        // Checked first, ahead of every transition case: the session is
        // still `active` server-side, so without this the pulsing dot and
        // the menu-bar clock would both claim a recording that has no
        // source.
        if self.source_lost.is_some() {
            return ControlMode::Paused;
        }
        match self.phase {
            RecorderPhase::Recording => ControlMode::Recording,
            RecorderPhase::Paused | RecorderPhase::Resuming => ControlMode::Paused,
            // A pause or stop in flight still reads as whatever the
            // session was when it was clicked.
            RecorderPhase::Pausing
            | RecorderPhase::StopPausing
            | RecorderPhase::StopPrompting
            | RecorderPhase::StopFinishing => {
                if self.status_is_live() {
                    ControlMode::Recording
                } else {
                    ControlMode::Paused
                }
            }
            RecorderPhase::Loading | RecorderPhase::LoadError => ControlMode::Recording,
            RecorderPhase::Finished => ControlMode::Paused,
        }
    }

    pub fn capturing(&self) -> bool {
        matches!(
            self.phase,
            RecorderPhase::Recording
                | RecorderPhase::Pausing
                | RecorderPhase::StopPausing
                | RecorderPhase::Resuming
        )
    }

    pub fn stale(&self, now_ms: i64) -> bool {
        self.capturing()
            && self
                .last_capture_at
                .is_some_and(|at| (now_ms - at) / 1000 > STALE_CAPTURE_SECS as i64)
    }

    pub fn snapshot(&self, now_ms: i64) -> RecorderState {
        RecorderState {
            token: self.cfg.token.clone(),
            source: self.source.clone(),
            phase: self.phase,
            session_status: self.session_status.clone(),
            session_name: self.session_name.clone(),
            session_error: self.session_error.clone(),
            action_error: self.action_error.clone(),
            control_mode: self.control_mode(),
            capturing: self.capturing(),
            clock: self.clock.state(),
            display_seconds: self.clock.display_seconds(now_ms),
            tracked_seconds: self.clock.base_seconds(),
            too_short_to_compile: self.clock.base_seconds() < MIN_STOPPABLE_TRACKED_SECONDS,
            capture_count: self.base_capture_count + self.ticks,
            capture_error: self.capture_error.clone(),
            source_lost: self.source_lost.clone(),
            stale: self.stale(now_ms),
            last_capture_at_unix_ms: self.last_capture_at,
            pending_uploads: 0,
        }
    }

    /// Notices owed since the last publish. Mirrors `useSessionNotifications`:
    /// an error appearing (a capture failure or a lost source), a pause that
    /// interrupts active recording (not the load of an already-paused
    /// session), and a failed session.
    pub fn take_notices(&mut self) -> Vec<RecorderNotice> {
        let mut notices = Vec::new();
        let error = self
            .capture_error
            .clone()
            .or_else(|| self.source_lost.as_ref().map(|l| l.message.clone()));
        if let Some(error) = &error {
            if !self.notice_error_shown {
                let first_line = error.lines().next().unwrap_or("").to_string();
                notices.push(RecorderNotice {
                    title: "Lookout — recording error".into(),
                    body: format!("Your screen isn't being captured: {first_line}"),
                });
            }
        }
        self.notice_error_shown = error.is_some();

        let status = self.session_status.clone();
        if status != self.notice_status {
            let prev = self.notice_status.as_deref();
            match status.as_deref() {
                Some("paused") if prev.is_some_and(is_live) => notices.push(RecorderNotice {
                    title: "Lookout — recording paused".into(),
                    body: "Your session is paused and isn't capturing screenshots.".into(),
                }),
                Some("failed") => notices.push(RecorderNotice {
                    title: "Lookout — session failed".into(),
                    body: "Your recording session ended unexpectedly.".into(),
                }),
                _ => {}
            }
            self.notice_status = status;
        }
        notices
    }

    pub fn set_status(&mut self, status: &str) {
        self.session_status = Some(status.to_string());
    }

    /// Fold a server reading of tracked seconds into the clock.
    pub fn credit(&mut self, tracked_seconds: Option<i64>, now_ms: i64) {
        if let Some(ts) = tracked_seconds {
            self.clock.credit(ts, now_ms);
        }
    }

    /// The session reached a terminal state: nothing left to drive.
    pub fn finish(&mut self, status: &str) {
        self.set_status(status);
        self.clock.stop();
        self.phase = RecorderPhase::Finished;
    }

    pub fn lose_source(&mut self, loss: SourceLoss) {
        self.source_lost = Some(loss);
        self.clock.stop();
        if self.phase == RecorderPhase::Recording {
            self.phase = RecorderPhase::Paused;
        }
    }
}

