//! The recording session, as a state machine the shell only draws.
//!
//! A shell opens a recording with [`Core::recorder_open`], drives it with
//! `recorder_pause` / `recorder_resume` / `recorder_begin_stop` /
//! `recorder_cancel_stop` / `recorder_finish_stop`, closes it with
//! [`Core::recorder_close`], and in between renders [`RecorderState`] —
//! published as [`CoreEvent::RecorderState`] on every change and readable
//! any time with [`Core::recorder_state`]. The shell holds no recording
//! logic of its own: not the order of operations behind a pause, not which
//! tracked-seconds number to believe, not when the clock stops.
//!
//! Most of what is in here used to live in the desktop app's TypeScript,
//! where it accreted one fix at a time. Each of those fixes is kept, and
//! each is written down where it lives, because every one of them was a
//! bug someone hit:
//!
//! - **Pause flushes before it pauses.** The capture loop is stopped AND
//!   AWAITED before the pause request goes out: on cancel it uploads the
//!   partial minute as a `final` capture, and that confirm has to land
//!   while the session is still active (a paused session rejects it).
//!   Reverse the order and a pause at 03:15 resumes at 03:00.
//! - **The clock freezes at the click.** The pause/stop button holds the
//!   clock the instant it is pressed, before any network round trip, and
//!   the flush's credit lands underneath the held value. See
//!   [`crate::clock`].
//! - **One ratcheted tracked-seconds.** The session load, every confirm,
//!   every status sync and the stop response all feed one value that only
//!   moves forward. The menu bar used to be seeded from a hook-local zero
//!   and read "0m" next to a window saying 25:00.
//! - **A failed pause does not strand the recording.** If the pause POST
//!   fails after the loop was stopped, the loop is restarted — the session
//!   is still active server-side — instead of showing recording controls
//!   over a dead loop.
//! - **409 means "sync, don't fail".** Pause, resume and stop treat a 409
//!   as "the server already did this" and read the status back; a stop
//!   retries on 5xx with backoff.
//! - **Losing the source is not an error.** A Wayland screencast the user
//!   stopped from the system indicator, or a PipeWire stream that died,
//!   parks the controls as paused — the session is still active and
//!   keeps its time — and asks for a new source; nothing is retried on a
//!   dead node, including a tray Resume.
//! - **The server can end the session underneath us.** A terminal status
//!   discovered anywhere (sleep recovery, a 409 sync) finishes the
//!   recording so the shell navigates on.
//! - **Camera sessions run the same loop.** The frame comes from the shell
//!   (see [`crate::Frontend::camera_start`]); the cadence, sleep recovery,
//!   pause flush and clips are the loop's. Camera pauses used to lose the
//!   partial minute because the camera path had no flush at all.
//!
//! Actions are serialized on [`Core::recorder_action_lock`]: each is a
//! multi-step sequence with awaits in it, and the tray, the GNOME pill and
//! the window can all issue them. An action that finds the recording in a
//! phase it doesn't apply to is refused with [`RecorderError::InvalidPhase`]
//! rather than run against the wrong state.

// The recorder itself lives in `lookout-recorder` (orchestration) and
// `lookout-session` (the state); re-exported so `crate::recorder::*` is
// the whole recorder, as it always was.
pub use lookout_recorder::recorder::{set_test_timeouts, Recorder, RecorderTasks};
pub use lookout_session::config::SessionClipCapabilities;
pub use lookout_session::source::{PickedSource, RecordingSource};
pub use lookout_session::state::*;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::{Duration, Instant};
#[cfg(test)]
use crate::capture_loop::current_unix_ms;
#[cfg(test)]
use crate::{Core, CoreEvent, SessionConfig};

// ── Tests ────────────────────────────────────────────────────────────

/// End-to-end, against a fake Lookout server: every path the recorder can
/// take, with the request log asserting the ORDER of what went over the
/// wire — that is where the bugs were.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_server::{MockServer, Req};
    use crate::{BoxFuture, CameraFrame, CaptureSource, Frontend};
    use std::sync::Mutex;

    /// A shell that records everything and answers camera requests with a
    /// tiny real JPEG.
    struct TestShell {
        events: Mutex<Vec<CoreEvent>>,
        titles: Mutex<Vec<(String, bool)>>,
        camera_starts: Mutex<Vec<String>>,
        camera_stops: Mutex<u32>,
        camera_fail_start: Mutex<bool>,
    }

    impl TestShell {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
                titles: Mutex::new(Vec::new()),
                camera_starts: Mutex::new(Vec::new()),
                camera_stops: Mutex::new(0),
                camera_fail_start: Mutex::new(false),
            })
        }

        fn states(&self) -> Vec<Option<RecorderState>> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    CoreEvent::RecorderState(s) => Some(s.clone()),
                    _ => None,
                })
                .collect()
        }

        fn notices(&self) -> Vec<RecorderNotice> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    CoreEvent::RecorderNotice(n) => Some(n.clone()),
                    _ => None,
                })
                .collect()
        }

        fn previews(&self) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, CoreEvent::PreviewFrame(_)))
                .count()
        }
    }

    fn tiny_jpeg() -> Vec<u8> {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            6,
            image::Rgba([200, 30, 30, 255]),
        ));
        crate::capture::encode_frame_jpeg(&img, 80).unwrap().data
    }

    impl Frontend for TestShell {
        fn emit(&self, event: CoreEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn wants_preview_frames(&self) -> bool {
            true
        }
        fn set_tray_title(&self, text: &str, paused: bool) {
            self.titles.lock().unwrap().push((text.to_string(), paused));
        }
        fn camera_start(&self, device_id: &str) -> BoxFuture<'static, Result<(), String>> {
            self.camera_starts.lock().unwrap().push(device_id.to_string());
            let fail = *self.camera_fail_start.lock().unwrap();
            Box::pin(async move {
                if fail {
                    Err("NotAllowedError".into())
                } else {
                    Ok(())
                }
            })
        }
        fn camera_grab_frame(&self, _device_id: &str) -> BoxFuture<'static, Result<CameraFrame, String>> {
            let jpeg = tiny_jpeg();
            Box::pin(async move {
                Ok(CameraFrame {
                    jpeg,
                    width: 8,
                    height: 6,
                })
            })
        }
        fn camera_stop(&self) {
            *self.camera_stops.lock().unwrap() += 1;
        }
    }

    fn camera() -> RecordingSource {
        RecordingSource::Camera {
            device_id: "cam-1".into(),
        }
    }

    fn pipewire(id: u32) -> RecordingSource {
        RecordingSource::Screen {
            sources: vec![CaptureSource::PipeWire { id }],
        }
    }

    async fn setup(status: &str) -> (Arc<MockServer>, Arc<TestShell>, Arc<Core>, SessionConfig) {
        let server = MockServer::start().await;
        server.set_status(status);
        let shell = TestShell::new();
        let core = Core::new(shell.clone() as Arc<dyn Frontend>, "Test", "0.0.0");
        let cfg = SessionConfig {
            token: "tok".into(),
            api_base_url: server.base_url(),
        };
        (server, shell, core, cfg)
    }

    /// Poll until `pred` holds on the state, or fail after `secs`.
    async fn wait_for(core: &Core, secs: u64, pred: impl Fn(&RecorderState) -> bool) -> RecorderState {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let state = core.recorder_state();
            if let Some(s) = &state {
                if pred(s) {
                    return s.clone();
                }
            }
            if Instant::now() > deadline {
                panic!("timed out waiting; last state: {state:#?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn paths(server: &MockServer) -> Vec<String> {
        server
            .requests()
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect()
    }

    // ── open ─────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_on_an_active_session_starts_capturing_and_the_seed_tick_uploads() {
        let (server, shell, core, cfg) = setup("active").await;
        server.set_tracked_seconds(120);
        server.set_screenshot_count(2);

        let state = core.recorder_open(cfg, camera()).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);
        assert_eq!(state.control_mode, ControlMode::Recording);
        assert!(state.capturing);
        assert!(state.clock.running);
        // The session load seeds the clock — not a hook-local zero.
        assert_eq!(state.tracked_seconds, 120);
        assert!(!state.too_short_to_compile);

        // The camera was opened before the loop asked for a frame.
        assert_eq!(shell.camera_starts.lock().unwrap().as_slice(), &["cam-1".to_string()]);

        // The seed tick lands: count = server count + confirms.
        let state = wait_for(&core, 10, |s| s.capture_count == 3).await;
        assert_eq!(state.capture_error, None);
        assert!(state.last_capture_at_unix_ms.is_some());
        assert!(shell.previews() >= 1, "the tick's frame is the preview");

        let log = paths(&server);
        assert_eq!(log[0], "GET /api/sessions/tok");
        assert!(log.iter().any(|p| p.starts_with("GET /api/sessions/tok/upload-url")));
        assert!(log.iter().any(|p| p == "POST /api/sessions/tok/screenshots"));

        // Loading was published first, then the recording state.
        let states = shell.states();
        assert_eq!(states[0].as_ref().unwrap().phase, RecorderPhase::Loading);
        core.recorder_close().await;
        assert!(shell.states().last().unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_on_a_paused_session_waits_for_resume() {
        let (server, shell, core, cfg) = setup("paused").await;
        server.set_tracked_seconds(300);
        let state = core.recorder_open(cfg, camera()).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Paused);
        assert_eq!(state.control_mode, ControlMode::Paused);
        assert!(!state.clock.running);
        assert_eq!(state.display_seconds, 300);
        assert!(!core.has_capture_loop());
        assert!(shell.camera_starts.lock().unwrap().is_empty());
        // Opening an already-paused session is not a "recording paused".
        assert!(shell.notices().is_empty());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_on_a_finished_session_is_finished() {
        let (server, _shell, core, cfg) = setup("complete").await;
        server.set_tracked_seconds(600);
        let state = core.recorder_open(cfg, camera()).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Finished);
        assert_eq!(state.session_status.as_deref(), Some("complete"));
        assert!(!core.has_capture_loop());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_reports_a_load_failure_as_state() {
        let (server, _shell, core, cfg) = setup("active").await;
        server.fail_next("GET", "/api/sessions/tok", 500, r#"{"error":"boom"}"#);
        let state = core.recorder_open(cfg, camera()).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::LoadError);
        assert!(state.session_error.unwrap().contains("HTTP 500"));
        assert!(!core.has_capture_loop());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_camera_that_fails_to_start_is_an_error_the_loop_keeps_honest() {
        let (_server, shell, core, cfg) = setup("active").await;
        *shell.camera_fail_start.lock().unwrap() = true;
        let state = core.recorder_open(cfg, camera()).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);
        assert!(state.capture_error.unwrap().contains("Camera failed to start"));
        core.recorder_close().await;
    }

    // ── pause / resume ───────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_flushes_the_partial_minute_before_pausing_the_session() {
        let (server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;

        let state = core.recorder_pause().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Paused);
        assert_eq!(state.control_mode, ControlMode::Paused);
        assert_eq!(state.session_status.as_deref(), Some("paused"));
        assert!(!state.clock.running);
        assert!(state.clock.hold_at_unix_ms.is_none());
        assert!(!state.capturing);
        assert!(!core.has_capture_loop());
        assert_eq!(*shell.camera_stops.lock().unwrap(), 1);

        // Wire order: seed upload, then the FINAL flush confirm, then pause.
        let reqs = server.requests();
        let confirms: Vec<&Req> = reqs
            .iter()
            .filter(|r| r.path == "/api/sessions/tok/screenshots")
            .collect();
        assert_eq!(confirms.len(), 2, "seed + flush: {:#?}", paths(&server));
        assert!(!confirms[0].body.contains("\"final\""));
        assert!(confirms[1].body.contains("\"final\":true"));
        let flush_at = reqs
            .iter()
            .position(|r| r.body.contains("\"final\":true"))
            .unwrap();
        let pause_at = reqs
            .iter()
            .position(|r| r.path == "/api/sessions/tok/pause")
            .unwrap();
        assert!(flush_at < pause_at, "flush must land before the pause POST");

        // The flush credited: the partial minute is kept (the mock credits
        // 20s for a final capture) and shown.
        assert_eq!(state.tracked_seconds, 20);
        assert_eq!(state.display_seconds, 20);
        assert_eq!(state.capture_count, 2);

        // The user was told.
        assert!(shell
            .notices()
            .iter()
            .any(|n| n.title == "Lookout — recording paused"));

        // The in-flight phase was published, with the clock held.
        let pausing = shell
            .states()
            .into_iter()
            .flatten()
            .find(|s| s.phase == RecorderPhase::Pausing)
            .expect("Pausing was published");
        assert!(pausing.clock.hold_at_unix_ms.is_some());
        assert_eq!(pausing.control_mode, ControlMode::Recording, "pinned pre-action");
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_pause_restarts_the_loop_instead_of_stranding_the_recording() {
        let (server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        let starts_before = shell.camera_starts.lock().unwrap().len();

        server.fail_next("POST", "/api/sessions/tok/pause", 500, r#"{"error":"db down"}"#);
        let err = core.recorder_pause().await.unwrap_err();
        assert!(matches!(err, RecorderError::Api { status: Some(500), .. }));

        let state = core.recorder_state().unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);
        assert!(state.clock.running);
        assert!(state.clock.hold_at_unix_ms.is_none(), "hold released");
        assert!(state.action_error.unwrap().contains("HTTP 500"));
        assert!(core.has_capture_loop());
        // The camera was reopened for the restarted loop.
        assert_eq!(shell.camera_starts.lock().unwrap().len(), starts_before + 1);
        // ...and the restarted loop uploads again.
        wait_for(&core, 10, |s| s.capture_count >= 3).await;
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_409_syncs_the_status_instead_of_failing() {
        let (server, _shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        // The server ended the session meanwhile.
        server.fail_next("POST", "/api/sessions/tok/pause", 409, r#"{"error":"Session is stopped"}"#);
        server.set_status("stopped");
        server.set_tracked_seconds(180);
        let state = core.recorder_pause().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Finished);
        assert_eq!(state.session_status.as_deref(), Some("stopped"));
        assert_eq!(state.tracked_seconds, 180);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resume_resumes_the_session_then_restarts_the_loop() {
        let (server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        core.recorder_pause().await.unwrap();
        let uploads_before = paths(&server)
            .iter()
            .filter(|p| p.contains("upload-url"))
            .count();

        let state = core.recorder_resume().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);
        assert_eq!(state.control_mode, ControlMode::Recording);
        assert!(state.clock.running);
        assert_eq!(state.session_status.as_deref(), Some("active"));
        assert!(core.has_capture_loop());
        assert_eq!(shell.camera_starts.lock().unwrap().len(), 2);

        // Resume POST came before the new loop's first upload-url.
        let deadline = Instant::now() + Duration::from_secs(10);
        while paths(&server).iter().filter(|p| p.contains("upload-url")).count() <= uploads_before {
            assert!(Instant::now() < deadline, "no upload after resume: {:#?}", paths(&server));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let reqs = server.requests();
        let resume_at = reqs.iter().position(|r| r.path == "/api/sessions/tok/resume").unwrap();
        let later_upload = reqs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.path.starts_with("/api/sessions/tok/upload-url"))
            .nth(uploads_before)
            .map(|(i, _)| i)
            .unwrap();
        assert!(resume_at < later_upload);
        // The resumed phase was published with the pinned paused look.
        let resuming = shell
            .states()
            .into_iter()
            .flatten()
            .find(|s| s.phase == RecorderPhase::Resuming)
            .unwrap();
        assert_eq!(resuming.control_mode, ControlMode::Paused);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resume_409_on_a_finished_session_finishes() {
        let (server, _shell, core, cfg) = setup("paused").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        server.fail_next("POST", "/api/sessions/tok/resume", 409, r#"{"error":"Session is stopped"}"#);
        server.set_status("compiling");
        let state = core.recorder_resume().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Finished);
        assert_eq!(state.session_status.as_deref(), Some("compiling"));
        assert!(!core.has_capture_loop());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_resume_stays_paused_with_the_error() {
        let (server, _shell, core, cfg) = setup("paused").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        server.fail_next("POST", "/api/sessions/tok/resume", 503, "");
        let err = core.recorder_resume().await.unwrap_err();
        assert!(matches!(err, RecorderError::Api { status: Some(503), .. }));
        let state = core.recorder_state().unwrap();
        assert_eq!(state.phase, RecorderPhase::Paused);
        assert!(state.action_error.is_some());
        assert!(!core.has_capture_loop());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn actions_are_refused_in_the_wrong_phase() {
        let (_server, _shell, core, cfg) = setup("paused").await;
        assert!(matches!(core.recorder_pause().await, Err(RecorderError::NoRecording)));
        core.recorder_open(cfg, camera()).await.unwrap();
        assert!(matches!(
            core.recorder_pause().await,
            Err(RecorderError::InvalidPhase { phase: RecorderPhase::Paused })
        ));
        assert!(matches!(
            core.recorder_cancel_stop().await,
            Err(RecorderError::InvalidPhase { .. })
        ));
        assert!(matches!(
            core.recorder_finish_stop(None, false).await,
            Err(RecorderError::InvalidPhase { .. })
        ));
        core.recorder_close().await;
    }

    // ── stop ─────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_pauses_into_the_prompt_and_finishes_with_rename_then_stop() {
        let (server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;

        let state = core.recorder_begin_stop().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::StopPrompting);
        assert_eq!(state.control_mode, ControlMode::Paused);
        assert_eq!(state.session_status.as_deref(), Some("paused"));
        assert!(state.too_short_to_compile, "20s credited: nothing to compile yet");
        assert!(!core.has_capture_loop());
        // Flush before pause, same as Pause.
        let reqs = server.requests();
        let flush_at = reqs.iter().position(|r| r.body.contains("\"final\":true")).unwrap();
        let pause_at = reqs.iter().position(|r| r.path == "/api/sessions/tok/pause").unwrap();
        assert!(flush_at < pause_at);

        let outcome = core
            .recorder_finish_stop(Some("  My cut  ".into()), true)
            .await
            .unwrap();
        assert_eq!(outcome.status, "stopped");
        assert!(outcome.edit);
        let state = core.recorder_state().unwrap();
        assert_eq!(state.phase, RecorderPhase::Finished);
        assert_eq!(state.session_status.as_deref(), Some("stopped"));
        assert_eq!(state.control_mode, ControlMode::Paused);

        let log = paths(&server);
        let rename_at = log.iter().position(|p| p == "PATCH /api/sessions/tok/name").unwrap();
        let stop_at = log.iter().position(|p| p == "POST /api/sessions/tok/stop").unwrap();
        assert!(rename_at < stop_at);
        let rename = server
            .requests()
            .into_iter()
            .find(|r| r.path == "/api/sessions/tok/name")
            .unwrap();
        assert_eq!(rename.body, r#"{"name":"My cut"}"#);
        let stop = server
            .requests()
            .into_iter()
            .find(|r| r.path == "/api/sessions/tok/stop")
            .unwrap();
        assert_eq!(stop.body, r#"{"edit":true}"#);
        // The prompt phases were published.
        let phases: Vec<RecorderPhase> = shell.states().into_iter().flatten().map(|s| s.phase).collect();
        assert!(phases.contains(&RecorderPhase::StopPausing));
        assert!(phases.contains(&RecorderPhase::StopFinishing));
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn keep_recording_from_the_prompt_resumes() {
        let (_server, _shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        core.recorder_begin_stop().await.unwrap();
        let state = core.recorder_cancel_stop().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);
        assert!(state.clock.running);
        assert!(core.has_capture_loop());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stop_from_a_paused_session_is_idempotent() {
        let (server, _shell, core, cfg) = setup("paused").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        let state = core.recorder_begin_stop().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::StopPrompting);
        assert_eq!(paths(&server), vec!["GET /api/sessions/tok", "POST /api/sessions/tok/pause"]);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn finish_stop_retries_on_5xx_and_leaves_the_prompt_open_on_failure() {
        let (server, _shell, core, cfg) = setup("paused").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        core.recorder_begin_stop().await.unwrap();

        // One 502, then success: the first backoff is a real second.
        server.fail_next("POST", "/api/sessions/tok/stop", 502, "");
        let outcome = core.recorder_finish_stop(None, false).await.unwrap();
        assert_eq!(outcome.status, "stopped");
        let stops = paths(&server).iter().filter(|p| p.ends_with("/stop")).count();
        assert_eq!(stops, 2);

        // A client error is not retried; the prompt stays up with the error.
        let (server, _shell, core, cfg) = setup("paused").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        core.recorder_begin_stop().await.unwrap();
        server.fail_next("POST", "/api/sessions/tok/stop", 400, r#"{"error":"bad"}"#);
        let err = core.recorder_finish_stop(None, false).await.unwrap_err();
        assert!(matches!(err, RecorderError::Api { status: Some(400), .. }));
        let state = core.recorder_state().unwrap();
        assert_eq!(state.phase, RecorderPhase::StopPrompting);
        assert!(state.action_error.is_some());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn finish_stop_409_counts_as_stopped_when_the_server_already_did() {
        let (server, _shell, core, cfg) = setup("paused").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        core.recorder_begin_stop().await.unwrap();
        server.fail_next("POST", "/api/sessions/tok/stop", 409, r#"{"error":"Session is compiling"}"#);
        server.set_status("compiling");
        server.set_tracked_seconds(240);
        let outcome = core.recorder_finish_stop(None, false).await.unwrap();
        assert_eq!(outcome.status, "compiling");
        let state = core.recorder_state().unwrap();
        assert_eq!(state.phase, RecorderPhase::Finished);
        assert_eq!(state.tracked_seconds, 240);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn session_finish_works_without_an_open_recording() {
        let (server, _shell, core, cfg) = setup("active").await;
        let outcome = core.session_finish(&cfg, Some("Picker stop"), false).await.unwrap();
        assert_eq!(outcome.status, "stopped");
        assert_eq!(
            paths(&server),
            vec!["PATCH /api/sessions/tok/name", "POST /api/sessions/tok/stop"]
        );
    }

    // ── the server and the source ────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_session_the_server_ended_finishes_the_recording() {
        let (server, _shell, core, cfg) = setup("active").await;
        // The seed tick's upload-url is refused: the loop runs sleep
        // recovery, finds the session stopped, and reports it terminated.
        server.set_status("stopped");
        server.set_tracked_seconds(900);
        core.recorder_open(cfg, camera()).await.unwrap();
        let state = wait_for(&core, 10, |s| s.phase == RecorderPhase::Finished).await;
        assert_eq!(state.session_status.as_deref(), Some("stopped"));
        assert!(!state.capturing);
        assert!(!state.clock.running);
        // The follow-up status sync refreshed the credited seconds.
        wait_for(&core, 10, |s| s.tracked_seconds == 900).await;
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_revoked_screencast_parks_the_controls_and_refuses_resume() {
        let (_server, shell, core, cfg) = setup("active").await;
        // Opening the session GET is enough; the loop's captures fail (no
        // PipeWire fd in a test) but that is a tick error, not a loss.
        let state = core.recorder_open(cfg, pipewire(7)).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);

        // A revoke for someone else's node is ignored...
        core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked { node_ids: vec![99] }));
        assert!(core.recorder_state().unwrap().source_lost.is_none());

        // ...ours parks the controls as paused: the session is still active
        // server-side, so the pulsing dot would otherwise claim a recording
        // that has no source.
        core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked { node_ids: vec![7] }));
        let state = wait_for(&core, 10, |s| s.source_lost.is_some()).await;
        assert_eq!(state.phase, RecorderPhase::Paused);
        assert_eq!(state.control_mode, ControlMode::Paused);
        assert!(!state.clock.running);
        assert_eq!(state.source_lost.as_ref().unwrap().reason, SourceLossReason::Revoked);
        assert_eq!(state.session_status.as_deref(), Some("active"));
        // The loop is stopped (orderly, so a held frame would flush).
        wait_for(&core, 10, |s| !s.capturing).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while core.has_capture_loop() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!core.has_capture_loop());

        // Nothing to resume onto.
        assert!(matches!(core.recorder_resume().await, Err(RecorderError::SourceLost)));
        // The user was told, once.
        let errors = shell
            .notices()
            .iter()
            .filter(|n| n.title == "Lookout — recording error")
            .count();
        assert_eq!(errors, 1);

        // Stop still works from here: the prompt opens over a paused session.
        let state = core.recorder_begin_stop().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::StopPrompting);
        // "Keep recording" with no source closes the prompt and stays paused
        // — the shell sends the user to the picker.
        let state = core.recorder_cancel_stop().await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Paused);
        assert!(state.source_lost.is_some());
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_source_the_watchdog_gave_up_on_is_a_loss_with_detail() {
        let (_server, _shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, pipewire(7)).await.unwrap();
        let generation = core.with_recording(|rec| rec.loop_generation).unwrap();
        core.emit_tagged(
            generation,
            CoreEvent::SourceLost(crate::CaptureSourceLost {
                message: "pipewiresrc timed out".into(),
            }),
        );
        let state = core.recorder_state().unwrap();
        let loss = state.source_lost.unwrap();
        assert_eq!(loss.reason, SourceLossReason::Failed);
        assert_eq!(loss.detail.as_deref(), Some("pipewiresrc timed out"));
        assert_eq!(state.phase, RecorderPhase::Paused);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn events_from_a_replaced_loop_are_ignored() {
        let (_server, _shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        let generation = core.with_recording(|rec| rec.loop_generation).unwrap();
        core.emit_tagged(
            generation + 100,
            CoreEvent::TickError(crate::CaptureTickError {
                message: "from a ghost".into(),
            }),
        );
        assert_eq!(core.recorder_state().unwrap().capture_error, None);
        core.emit_tagged(
            generation,
            CoreEvent::TickError(crate::CaptureTickError {
                message: "from the live loop".into(),
            }),
        );
        assert_eq!(
            core.recorder_state().unwrap().capture_error.as_deref(),
            Some("from the live loop")
        );
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tracked_seconds_ratchets_across_every_source() {
        let (server, _shell, core, cfg) = setup("active").await;
        server.set_tracked_seconds(120);
        // The mock's seed confirm answers with its running count — stale
        // relative to the 120 the session load reported.
        server.set_confirm_tracked_override(Some(60));
        core.recorder_open(cfg, camera()).await.unwrap();
        let state = wait_for(&core, 10, |s| s.capture_count == 1).await;
        assert_eq!(state.tracked_seconds, 120, "a lower confirm never moves it back");
        let generation = core.with_recording(|rec| rec.loop_generation).unwrap();
        core.emit_tagged(generation, CoreEvent::TrackedSeconds(60));
        assert_eq!(core.recorder_state().unwrap().tracked_seconds, 120);
        core.emit_tagged(generation, CoreEvent::TrackedSeconds(180));
        assert_eq!(core.recorder_state().unwrap().tracked_seconds, 180);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_capture_error_is_shown_until_the_next_capture_and_notified_once() {
        let (_server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        let generation = core.with_recording(|rec| rec.loop_generation).unwrap();
        for _ in 0..2 {
            core.emit_tagged(
                generation,
                CoreEvent::TickError(crate::CaptureTickError {
                    message: "r2-upload failed\nmore detail".into(),
                }),
            );
        }
        let state = core.recorder_state().unwrap();
        assert_eq!(state.capture_error.as_deref(), Some("r2-upload failed\nmore detail"));
        assert_eq!(state.phase, RecorderPhase::Recording, "the loop carries on");
        let errors: Vec<RecorderNotice> = shell
            .notices()
            .into_iter()
            .filter(|n| n.title == "Lookout — recording error")
            .collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].body, "Your screen isn't being captured: r2-upload failed");
        // A confirm clears it.
        core.emit_tagged(
            generation,
            CoreEvent::TickResult(crate::CaptureTickResult {
                confirmed: true,
                tracked_seconds: 60,
                next_expected_at: String::new(),
                screenshot_id: String::new(),
                preview_base64: String::new(),
                preview_width: 0,
                preview_height: 0,
            }),
        );
        assert_eq!(core.recorder_state().unwrap().capture_error, None);
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_tray_ticker_writes_the_clock_and_the_paused_glyph() {
        let (_server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        // The ticker runs on real seconds; give it a couple.
        tokio::time::sleep(Duration::from_millis(2_300)).await;
        core.recorder_pause().await.unwrap();
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        let titles = shell.titles.lock().unwrap().clone();
        assert!(titles.iter().any(|(_, paused)| !paused), "ticked while recording: {titles:?}");
        assert!(titles.iter().any(|(_, paused)| *paused), "wrote the paused title: {titles:?}");
        assert!(titles.iter().all(|(t, _)| t.len() >= 5));
        core.recorder_close().await;
        // Nothing ticks once closed.
        let n = shell.titles.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        assert_eq!(shell.titles.lock().unwrap().len(), n);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn opening_a_new_recording_closes_the_old_one_first() {
        let (server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg.clone(), camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        let state = core.recorder_open(cfg, camera()).await.unwrap();
        assert_eq!(state.phase, RecorderPhase::Recording);
        // The old loop was stopped orderly (its flush landed) before the
        // new one's seed upload.
        wait_for(&core, 10, |s| s.capture_count >= 1).await;
        let reqs = server.requests();
        let flush_at = reqs.iter().position(|r| r.body.contains("\"final\":true")).unwrap();
        let second_get = reqs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.method == "GET" && r.path == "/api/sessions/tok")
            .nth(1)
            .map(|(i, _)| i)
            .unwrap();
        assert!(flush_at < second_get);
        assert!(shell.states().iter().any(|s| s.is_none()), "the close was published");
        core.recorder_close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn abort_tears_down_without_touching_the_session() {
        let (server, shell, core, cfg) = setup("active").await;
        core.recorder_open(cfg, camera()).await.unwrap();
        wait_for(&core, 10, |s| s.capture_count == 1).await;
        core.recorder_abort();
        assert!(core.recorder_state().is_none());
        assert!(!core.has_capture_loop());
        assert_eq!(*shell.camera_stops.lock().unwrap(), 1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!paths(&server).iter().any(|p| p.ends_with("/pause")));
    }

    #[test]
    fn picked_sources_combine_screens_and_isolate_cameras() {
        let screen = RecordingSource::from_picked(vec![
            PickedSource::Monitor { id: 1 },
            PickedSource::Window { id: 2 },
            PickedSource::Pipewire { id: 3 },
        ])
        .unwrap();
        assert_eq!(
            screen,
            RecordingSource::Screen {
                sources: vec![
                    CaptureSource::Monitor { id: 1 },
                    CaptureSource::Window { id: 2 },
                    CaptureSource::PipeWire { id: 3 },
                ]
            }
        );
        assert_eq!(
            RecordingSource::from_picked(vec![PickedSource::Camera { id: "c".into() }]).unwrap(),
            RecordingSource::Camera {
                device_id: "c".into()
            }
        );
        assert!(RecordingSource::from_picked(vec![]).is_err());
        assert!(RecordingSource::from_picked(vec![
            PickedSource::Camera { id: "c".into() },
            PickedSource::Monitor { id: 1 },
        ])
        .is_err());
        // The TypeScript shape, byte for byte.
        let picked: Vec<PickedSource> =
            serde_json::from_str(r#"[{"type":"monitor","id":4},{"type":"camera","id":"abc"}]"#)
                .unwrap();
        assert_eq!(picked[1], PickedSource::Camera { id: "abc".into() });
    }

    #[test]
    fn control_mode_pins_the_pre_action_state() {
        let now = current_unix_ms();
        let cfg = SessionConfig {
            token: "t".into(),
            api_base_url: "http://x".into(),
        };
        let mut rec = Recording::new(cfg, camera(), now);
        rec.set_status("active");
        for (phase, mode) in [
            (RecorderPhase::Recording, ControlMode::Recording),
            (RecorderPhase::Pausing, ControlMode::Recording),
            (RecorderPhase::StopPausing, ControlMode::Recording),
            (RecorderPhase::Paused, ControlMode::Paused),
            (RecorderPhase::Resuming, ControlMode::Paused),
        ] {
            rec.phase = phase;
            assert_eq!(rec.control_mode(), mode, "{phase:?}");
        }
        rec.set_status("paused");
        for phase in [
            RecorderPhase::StopPausing,
            RecorderPhase::StopPrompting,
            RecorderPhase::StopFinishing,
        ] {
            rec.phase = phase;
            assert_eq!(rec.control_mode(), ControlMode::Paused, "{phase:?}");
        }
        // A lost source reads paused in every phase.
        rec.set_status("active");
        rec.phase = RecorderPhase::Recording;
        rec.source_lost = Some(SourceLoss {
            reason: SourceLossReason::Revoked,
            message: String::new(),
            detail: None,
        });
        assert_eq!(rec.control_mode(), ControlMode::Paused);
    }

    #[test]
    fn stale_needs_capturing_and_a_three_minute_gap() {
        let now_ms = current_unix_ms();
        let cfg = SessionConfig {
            token: "t".into(),
            api_base_url: "http://x".into(),
        };
        let mut rec = Recording::new(cfg, camera(), now_ms);
        rec.phase = RecorderPhase::Recording;
        assert!(!rec.stale(now_ms), "no capture yet is not stale");
        rec.last_capture_at = Some(now_ms);
        let secs = |s: u64| now_ms + (s as i64) * 1000;
        assert!(!rec.stale(secs(STALE_CAPTURE_SECS)));
        assert!(rec.stale(secs(STALE_CAPTURE_SECS + 1)));
        rec.phase = RecorderPhase::Paused;
        assert!(!rec.stale(secs(600)), "paused is expected to be quiet");
    }
}
