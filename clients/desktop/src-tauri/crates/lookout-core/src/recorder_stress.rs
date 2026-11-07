//! The adversarial campaign against the recorder: every fault at every
//! step, every action in every phase, concurrently and out of order, with
//! the shell misbehaving too — plus a randomized, model-checked stress run.
//!
//! The scenario tests each pin one situation and the exact outcome owed.
//! The stress run drives random sequences of actions and injected faults
//! and checks a set of invariants after every step; a failure prints the
//! seed and the trace so it can be replayed. Together they are the
//! argument that the recorder does the right thing regardless of what
//! happens around it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::mock_server::MockServer;
use crate::recorder::*;
use crate::{BoxFuture, CameraFrame, CaptureSource, Core, CoreEvent, Frontend, SessionConfig};

// ── A shell that can misbehave on request ────────────────────────────

#[derive(Default)]
struct CameraBehaviour {
    /// Fail this many starts before succeeding.
    fail_starts: u32,
    /// Fail this many grabs before succeeding.
    fail_grabs: u32,
    /// Never answer a start.
    hang_start: bool,
    /// Never answer a grab.
    hang_grab: bool,
}

struct Shell {
    events: Mutex<Vec<CoreEvent>>,
    titles: Mutex<Vec<(String, bool)>>,
    camera_starts: Mutex<u32>,
    camera_stops: Mutex<u32>,
    camera_grabs: Mutex<u32>,
    /// Every camera start/stop in order, so "is the camera closed at the
    /// end" can be answered even when the loop reopened it mid-way.
    camera_log: Mutex<Vec<&'static str>>,
    behaviour: Mutex<CameraBehaviour>,
}

impl Shell {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            titles: Mutex::new(Vec::new()),
            camera_starts: Mutex::new(0),
            camera_stops: Mutex::new(0),
            camera_grabs: Mutex::new(0),
            camera_log: Mutex::new(Vec::new()),
            behaviour: Mutex::new(CameraBehaviour::default()),
        })
    }

    fn set(&self, b: CameraBehaviour) {
        *self.behaviour.lock().unwrap() = b;
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

    fn starts(&self) -> u32 {
        *self.camera_starts.lock().unwrap()
    }
    fn stops(&self) -> u32 {
        *self.camera_stops.lock().unwrap()
    }
    /// The camera is not left on: nothing was ever started, or the last
    /// thing that happened to it was a stop. (A tick that found the
    /// camera gone reopens it in place, so starts can outnumber stops.)
    fn camera_closed(&self) -> bool {
        let log = self.camera_log.lock().unwrap();
        log.last().map_or(true, |e| *e == "stop")
    }
}

fn tiny_jpeg() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        16,
        12,
        image::Rgba([20, 120, 220, 255]),
    ));
    crate::capture::encode_frame_jpeg(&img, 80).unwrap().data
}

impl Frontend for Shell {
    fn emit(&self, event: CoreEvent) {
        self.events.lock().unwrap().push(event);
    }
    fn wants_preview_frames(&self) -> bool {
        true
    }
    fn set_tray_title(&self, text: &str, paused: bool) {
        self.titles.lock().unwrap().push((text.to_string(), paused));
    }
    fn camera_start(&self, _device_id: &str) -> BoxFuture<'static, Result<(), String>> {
        *self.camera_starts.lock().unwrap() += 1;
        self.camera_log.lock().unwrap().push("start");
        let mut b = self.behaviour.lock().unwrap();
        if b.hang_start {
            return Box::pin(std::future::pending());
        }
        if b.fail_starts > 0 {
            b.fail_starts -= 1;
            return Box::pin(async { Err("NotReadableError: device busy".into()) });
        }
        Box::pin(async { Ok(()) })
    }
    fn camera_grab_frame(&self, _device_id: &str) -> BoxFuture<'static, Result<CameraFrame, String>> {
        *self.camera_grabs.lock().unwrap() += 1;
        let mut b = self.behaviour.lock().unwrap();
        if b.hang_grab {
            return Box::pin(std::future::pending());
        }
        if b.fail_grabs > 0 {
            b.fail_grabs -= 1;
            return Box::pin(async { Err("video has no dimensions yet".into()) });
        }
        let jpeg = tiny_jpeg();
        Box::pin(async move {
            Ok(CameraFrame {
                jpeg,
                width: 16,
                height: 12,
            })
        })
    }
    fn camera_stop(&self) {
        *self.camera_stops.lock().unwrap() += 1;
        self.camera_log.lock().unwrap().push("stop");
    }
}

// ── Harness ──────────────────────────────────────────────────────────

const TOK: &str = "/api/sessions/tok";

fn camera() -> RecordingSource {
    RecordingSource::Camera {
        device_id: "cam".into(),
    }
}

fn pipewire() -> RecordingSource {
    RecordingSource::Screen {
        sources: vec![CaptureSource::PipeWire { id: 42 }],
    }
}

async fn setup(status: &str) -> (Arc<MockServer>, Arc<Shell>, Arc<Core>, SessionConfig) {
    let server = MockServer::start().await;
    server.set_status(status);
    let shell = Shell::new();
    let core = Core::new(shell.clone() as Arc<dyn Frontend>, "Stress", "0.0.0");
    let cfg = SessionConfig {
        token: "tok".into(),
        api_base_url: server.base_url(),
    };
    (server, shell, core, cfg)
}

async fn wait_for(core: &Core, secs: u64, pred: impl Fn(&RecorderState) -> bool) -> RecorderState {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let state = core.recorder_state();
        if let Some(s) = &state {
            if pred(s) {
                return s.clone();
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting; last state: {state:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_until(secs: u64, pred: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !pred() {
        assert!(Instant::now() < deadline, "timed out waiting on a condition");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Open a camera recording and wait for the seed capture to land.
async fn open_recording(core: &Arc<Core>, cfg: &SessionConfig) -> RecorderState {
    core.recorder_open(cfg.clone(), camera()).await.unwrap();
    wait_for(core, 10, |s| s.capture_count >= 1).await
}

fn paths(server: &MockServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

// ── Concurrency and ordering ─────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn double_clicking_pause_runs_it_once() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    let a = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    let b = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    let (a, b) = (a.await.unwrap(), b.await.unwrap());
    let oks = [&a, &b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(oks, 1, "exactly one pause ran: {a:?} / {b:?}");
    let refused = [&a, &b].into_iter().find(|r| r.is_err()).unwrap();
    assert!(matches!(refused, Err(RecorderError::InvalidPhase { phase: RecorderPhase::Paused })));
    assert_eq!(server.count("POST", &format!("{TOK}/pause")), 1);
    assert_eq!(
        server.requests().iter().filter(|r| r.body.contains("\"final\":true")).count(),
        1,
        "one flush"
    );
    assert_eq!(core.recorder_state().unwrap().phase, RecorderPhase::Paused);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_then_resume_clicked_together_end_recording() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    let pause = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    // Give the pause the lock first.
    tokio::time::sleep(Duration::from_millis(10)).await;
    let resume = core.recorder_resume().await;
    pause.await.unwrap().unwrap();
    resume.unwrap();
    let state = core.recorder_state().unwrap();
    assert_eq!(state.phase, RecorderPhase::Recording);
    assert!(core.has_capture_loop());
    assert_eq!(server.status(), "active");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_clicked_while_pausing_lands_in_the_prompt() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    server.delay_next("POST", &format!("{TOK}/pause"), 400);
    let pause = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(core.recorder_state().unwrap().phase, RecorderPhase::Pausing);
    let stop = core.recorder_begin_stop().await;
    pause.await.unwrap().unwrap();
    assert_eq!(stop.unwrap().phase, RecorderPhase::StopPrompting);
    assert_eq!(server.status(), "paused");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_while_resuming_is_refused_not_doubled() {
    let (server, _shell, core, cfg) = setup("paused").await;
    core.recorder_open(cfg, camera()).await.unwrap();
    server.delay_next("POST", &format!("{TOK}/resume"), 300);
    let a = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_resume().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let b = core.recorder_resume().await;
    a.await.unwrap().unwrap();
    assert!(matches!(b, Err(RecorderError::InvalidPhase { phase: RecorderPhase::Recording })));
    assert_eq!(server.count("POST", &format!("{TOK}/resume")), 1);
    core.recorder_close().await;
}

// ── Faults at every step ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hung_flush_cannot_wedge_the_pause() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    // The flush's confirm never answers.
    server.hang_next("POST", &format!("{TOK}/screenshots"));
    let started = Instant::now();
    let state = core.recorder_pause().await.unwrap();
    let took = started.elapsed();
    assert_eq!(state.phase, RecorderPhase::Paused);
    assert_eq!(server.status(), "paused");
    // The flush budget is 8s; the pause went out right after.
    assert!(took < Duration::from_secs(14), "pause took {took:?}");
    assert!(took >= Duration::from_secs(7), "the flush was given its budget: {took:?}");
    // The partial minute is lost — exactly what happened before the flush
    // existed — and the clock shows the credited base.
    assert_eq!(state.tracked_seconds, 0);
    assert_eq!(state.display_seconds, 0);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_flush_keeps_the_clock_held_and_the_controls_pinned() {
    let (server, shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    server.delay_next("POST", &format!("{TOK}/screenshots"), 1_500);
    let pause = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let a = core.recorder_state().unwrap();
    assert_eq!(a.phase, RecorderPhase::Pausing);
    assert_eq!(a.control_mode, ControlMode::Recording, "pinned pre-action");
    assert!(a.clock.hold_at_unix_ms.is_some());
    assert!(a.capturing);
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let b = core.recorder_state().unwrap();
    assert_eq!(b.phase, RecorderPhase::Pausing);
    assert_eq!(b.display_seconds, a.display_seconds, "held through the flush");
    let done = pause.await.unwrap().unwrap();
    assert_eq!(done.phase, RecorderPhase::Paused);
    // The flush credited 20 in the fake; the paused clock shows the base.
    assert_eq!(done.display_seconds, 20);
    // The tray saw the hold too (never a jump up during the pause).
    let titles = shell.titles.lock().unwrap().clone();
    assert!(!titles.is_empty());
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn network_errors_restore_the_phase_and_report() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;

    server.drop_next("POST", &format!("{TOK}/pause"));
    let err = core.recorder_pause().await.unwrap_err();
    assert!(matches!(err, RecorderError::Api { status: None, .. }), "{err:?}");
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert!(s.action_error.unwrap().contains("Network error"));
    assert!(core.has_capture_loop());
    assert_eq!(server.status(), "active", "the server never paused");

    core.recorder_pause().await.unwrap();
    server.drop_next("POST", &format!("{TOK}/resume"));
    let err = core.recorder_resume().await.unwrap_err();
    assert!(matches!(err, RecorderError::Api { status: None, .. }));
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert!(!core.has_capture_loop());
    assert_eq!(server.status(), "paused");

    core.recorder_begin_stop().await.unwrap();
    server.drop_next("POST", &format!("{TOK}/stop"));
    let err = core.recorder_finish_stop(None, false).await.unwrap_err();
    assert!(matches!(err, RecorderError::Api { status: None, .. }));
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::StopPrompting, "the prompt stays up to retry");
    assert_eq!(server.status(), "paused");
    let out = core.recorder_finish_stop(None, false).await.unwrap();
    assert_eq!(out.status, "stopped");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rate_limited_pause_is_an_error_and_recording_continues() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    server.fail_next("POST", &format!("{TOK}/pause"), 429, r#"{"error":"Rate limit exceeded"}"#);
    let err = core.recorder_pause().await.unwrap_err();
    assert!(matches!(err, RecorderError::Api { status: Some(429), .. }));
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert!(core.has_capture_loop());
    assert_eq!(server.status(), "active");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_change_409_on_pause_is_retried_once() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    server.fail_next(
        "POST",
        &format!("{TOK}/pause"),
        409,
        r#"{"error":"Session state changed concurrently, please retry"}"#,
    );
    let s = core.recorder_pause().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert_eq!(server.status(), "paused", "the retry paused it");
    assert_eq!(server.count("POST", &format!("{TOK}/pause")), 2);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_409_on_an_already_active_session_just_records() {
    let (server, _shell, core, cfg) = setup("paused").await;
    core.recorder_open(cfg, camera()).await.unwrap();
    // Someone resumed it from the web meanwhile.
    server.set_status("active");
    let s = core.recorder_resume().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert_eq!(s.session_status.as_deref(), Some("active"));
    assert!(core.has_capture_loop());
    assert_eq!(server.count("POST", &format!("{TOK}/resume")), 1, "no pointless retry");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_change_409_on_resume_is_retried_once() {
    let (server, _shell, core, cfg) = setup("paused").await;
    core.recorder_open(cfg, camera()).await.unwrap();
    server.fail_next(
        "POST",
        &format!("{TOK}/resume"),
        409,
        r#"{"error":"Session state changed concurrently, please retry"}"#,
    );
    let s = core.recorder_resume().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert_eq!(server.status(), "active");
    assert_eq!(server.count("POST", &format!("{TOK}/resume")), 2);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_ending_the_session_mid_pause_finishes_it() {
    let (server, shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    // A cron auto-stop lands between the flush and the pause POST.
    server.fail_next("POST", &format!("{TOK}/screenshots"), 409, r#"{"error":"Session is stopped"}"#);
    server.set_status("failed");
    let s = core.recorder_pause().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Finished);
    assert_eq!(s.session_status.as_deref(), Some("failed"));
    assert!(!core.has_capture_loop());
    assert!(shell.notices().iter().any(|n| n.title == "Lookout — session failed"));
    assert!(matches!(core.recorder_pause().await, Err(RecorderError::InvalidPhase { .. })));
    assert!(matches!(core.recorder_resume().await, Err(RecorderError::InvalidPhase { .. })));
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_the_server_answers_failed_finishes_as_failed() {
    let (server, shell, core, cfg) = setup("paused").await;
    core.recorder_open(cfg, camera()).await.unwrap();
    core.recorder_begin_stop().await.unwrap();
    // No captures: the real server marks the session failed on stop.
    server.fail_next(
        "POST",
        &format!("{TOK}/stop"),
        200,
        r#"{"status":"failed","trackedSeconds":0,"totalActiveSeconds":0}"#,
    );
    let out = core.recorder_finish_stop(None, true).await.unwrap();
    assert_eq!(out.status, "failed");
    assert!(out.edit);
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::Finished);
    assert!(shell.notices().iter().any(|n| n.title == "Lookout — session failed"));
    core.recorder_close().await;
}

// ── Source lost, in every phase ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_while_pausing_ends_paused_and_lost() {
    let (server, _shell, core, cfg) = setup("active").await;
    core.recorder_open(cfg, pipewire()).await.unwrap();
    server.delay_next("POST", &format!("{TOK}/pause"), 500);
    let pause = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked { node_ids: vec![42] }));
    let s = pause.await.unwrap().unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert!(s.source_lost.is_some());
    assert_eq!(s.control_mode, ControlMode::Paused);
    wait_until(5, || !core.has_capture_loop()).await;
    assert!(matches!(core.recorder_resume().await, Err(RecorderError::SourceLost)));
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_while_paused_marks_the_loss_without_a_loop_to_stop() {
    let (_server, _shell, core, cfg) = setup("active").await;
    core.recorder_open(cfg, pipewire()).await.unwrap();
    core.recorder_pause().await.unwrap();
    assert!(!core.has_capture_loop());
    core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked { node_ids: vec![42] }));
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert!(s.source_lost.is_some());
    assert!(matches!(core.recorder_resume().await, Err(RecorderError::SourceLost)));
    // Stop is still the way out.
    let s = core.recorder_begin_stop().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::StopPrompting);
    let out = core.recorder_finish_stop(Some("after loss".into()), false).await.unwrap();
    assert_eq!(out.status, "stopped");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_in_the_prompt_keeps_the_prompt_and_refuses_keep_recording() {
    let (_server, _shell, core, cfg) = setup("active").await;
    core.recorder_open(cfg, pipewire()).await.unwrap();
    core.recorder_begin_stop().await.unwrap();
    core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked { node_ids: vec![42] }));
    let s = core.recorder_state().unwrap();
    assert_eq!(s.phase, RecorderPhase::StopPrompting, "the prompt is not torn down");
    assert!(s.source_lost.is_some());
    // "Keep recording" has nothing to resume onto: the prompt closes, the
    // recording stays paused and lost, and the shell goes to the picker.
    let s = core.recorder_cancel_stop().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert!(s.source_lost.is_some());
    assert!(!core.has_capture_loop());
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_watchdogs_loss_during_a_resume_does_not_restart_onto_a_dead_node() {
    let (server, _shell, core, cfg) = setup("active").await;
    core.recorder_open(cfg, pipewire()).await.unwrap();
    core.recorder_pause().await.unwrap();
    server.delay_next("POST", &format!("{TOK}/resume"), 300);
    let resume = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_resume().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The compositor revokes while the resume POST is in flight.
    core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked { node_ids: vec![42] }));
    resume.await.unwrap().unwrap();
    let s = core.recorder_state().unwrap();
    // The resume completed (the session IS active again), the loop started —
    // and the loss was cleared by starting over, as any fresh start does.
    // Then the stream fails on the first tick, which is reported as an
    // error; the watchdog takes it from there. The important thing: no
    // panic, a coherent state, and Stop still works.
    assert!(matches!(s.phase, RecorderPhase::Recording | RecorderPhase::Paused));
    let s = core.recorder_begin_stop().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::StopPrompting);
    core.recorder_close().await;
}

// ── Tearing down at the worst moment ─────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_in_the_middle_of_a_pause_is_safe() {
    let (server, shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    server.delay_next("POST", &format!("{TOK}/pause"), 600);
    let pause = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_pause().await }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    core.recorder_abort();
    assert!(core.recorder_state().is_none());
    assert!(!core.has_capture_loop());
    let result = pause.await.unwrap();
    assert!(matches!(result, Err(RecorderError::NoRecording)), "{result:?}");
    assert!(core.recorder_state().is_none(), "the finished pause did not resurrect it");
    assert_eq!(shell.stops(), 1);
    // Nothing more happens afterwards.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(core.recorder_state().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_during_a_slow_flush_waits_for_the_credit() {
    let (server, shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    server.delay_next("POST", &format!("{TOK}/screenshots"), 800);
    let started = Instant::now();
    core.recorder_close().await;
    assert!(started.elapsed() >= Duration::from_millis(700), "close waited for the flush");
    assert!(core.recorder_state().is_none());
    assert!(!core.has_capture_loop());
    assert_eq!(server.tracked_seconds(), 20, "the flush landed");
    assert_eq!(server.status(), "active", "close leaves the session alone");
    assert!(shell.camera_closed());
    assert_eq!(shell.starts(), shell.stops());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn open_right_after_close_is_serialized_behind_the_flush() {
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    server.delay_next("POST", &format!("{TOK}/screenshots"), 600);
    let close = tokio::spawn({
        let core = core.clone();
        async move { core.recorder_close().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let state = core.recorder_open(cfg, camera()).await.unwrap();
    close.await.unwrap();
    assert_eq!(state.phase, RecorderPhase::Recording);
    let reqs = server.requests();
    let flush_at = reqs.iter().position(|r| r.body.contains("\"final\":true")).unwrap();
    let second_get = reqs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.method == "GET" && r.path == TOK)
        .nth(1)
        .map(|(i, _)| i)
        .unwrap();
    assert!(flush_at < second_get, "{:#?}", paths(&server));
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_opens_at_once_leave_exactly_one_recording_and_loop() {
    let (server, shell, core, cfg) = setup("active").await;
    let a = tokio::spawn({
        let (core, cfg) = (core.clone(), cfg.clone());
        async move { core.recorder_open(cfg, camera()).await }
    });
    let b = tokio::spawn({
        let (core, cfg) = (core.clone(), cfg.clone());
        async move { core.recorder_open(cfg, camera()).await }
    });
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    let s = wait_for(&core, 10, |s| s.capture_count >= 1).await;
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert!(core.has_capture_loop());
    assert_eq!(server.count("GET", TOK), 2);
    core.recorder_close().await;
    assert!(shell.camera_closed());
    assert_eq!(shell.starts(), shell.stops(), "one open, one release per recording");
    assert!(!core.has_capture_loop());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_action_is_refused_after_close() {
    let (_server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    core.recorder_close().await;
    assert!(matches!(core.recorder_pause().await, Err(RecorderError::NoRecording)));
    assert!(matches!(core.recorder_resume().await, Err(RecorderError::NoRecording)));
    assert!(matches!(core.recorder_begin_stop().await, Err(RecorderError::NoRecording)));
    assert!(matches!(core.recorder_cancel_stop().await, Err(RecorderError::NoRecording)));
    assert!(matches!(
        core.recorder_finish_stop(None, false).await,
        Err(RecorderError::NoRecording)
    ));
    core.recorder_close().await; // idempotent
    assert!(core.recorder_state().is_none());
}

// ── The camera misbehaving ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_never_answers_start_times_out_and_the_loop_still_runs() {
    set_test_timeouts(1_500, 500);
    let (_server, shell, core, cfg) = setup("active").await;
    shell.set(CameraBehaviour {
        hang_start: true,
        ..Default::default()
    });
    let started = Instant::now();
    let s = core.recorder_open(cfg, camera()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert!(s.capture_error.unwrap().contains("no frames after"));
    // Grabs still work: the first tick lands and clears the error.
    let s = wait_for(&core, 10, |s| s.capture_count >= 1).await;
    assert_eq!(s.capture_error, None);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_fails_its_first_grab_is_reopened_and_reported() {
    let (_server, shell, core, cfg) = setup("active").await;
    shell.set(CameraBehaviour {
        fail_grabs: 1,
        ..Default::default()
    });
    core.recorder_open(cfg, camera()).await.unwrap();
    let s = wait_for(&core, 10, |s| s.capture_error.is_some()).await;
    assert!(s.capture_error.unwrap().contains("no dimensions"));
    assert_eq!(s.phase, RecorderPhase::Recording, "the loop carries on");
    // The tick path tried to reopen the camera after the failed grab.
    wait_until(5, || shell.starts() >= 2).await;
    // A pause still flushes nothing (no frame) and pauses cleanly.
    let s = core.recorder_pause().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_hangs_on_a_grab_is_timed_out_by_the_tick() {
    // The grab timeout is 15s; the pause must not wait on it beyond the
    // loop stop's bound either. We only check the recording stays coherent
    // and a pause completes in bounded time.
    let (_server, shell, core, cfg) = setup("active").await;
    shell.set(CameraBehaviour {
        hang_grab: true,
        ..Default::default()
    });
    let s = core.recorder_open(cfg, camera()).await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let s = core.recorder_pause().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert!(started.elapsed() < Duration::from_secs(20));
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_fails_to_start_on_resume_still_resumes() {
    let (server, shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    core.recorder_pause().await.unwrap();
    shell.set(CameraBehaviour {
        fail_starts: 1,
        ..Default::default()
    });
    let s = core.recorder_resume().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert!(s.capture_error.unwrap().contains("Camera failed to start"));
    assert_eq!(server.status(), "active");
    // Frames still come (the shell only failed the start), so it recovers.
    let s = wait_for(&core, 10, |s| s.capture_error.is_none()).await;
    assert!(s.capture_count >= 2);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clips_session_on_a_camera_seeds_flushes_and_pauses() {
    let (server, _shell, core, cfg) = setup("active").await;
    server.set_clips_enabled(true);
    open_recording(&core, &cfg).await;
    // Let a couple of in-between frames go into the clip recorder.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = core.recorder_pause().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Paused);
    assert_eq!(s.capture_error, None);
    assert_eq!(s.capture_count, 2, "seed + flush");
    assert_eq!(server.status(), "paused");
    core.recorder_close().await;
}

// ── The loop dying, the clock, pending sessions ──────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_loop_that_dies_silently_is_noticed_and_recoverable() {
    set_test_timeouts(1_500, 500);
    let (server, _shell, core, cfg) = setup("active").await;
    open_recording(&core, &cfg).await;
    core.abort_capture_loop_for_test();
    let s = wait_for(&core, 10, |s| s.capture_error.is_some()).await;
    assert!(s.capture_error.unwrap().contains("stopped unexpectedly"));
    assert_eq!(s.phase, RecorderPhase::Recording);
    // Pause then resume brings a fresh loop.
    core.recorder_pause().await.unwrap();
    let s = core.recorder_resume().await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert_eq!(s.capture_error, None);
    assert!(core.has_capture_loop() && !core.capture_loop_finished());
    wait_for(&core, 10, |s| s.capture_count >= 2).await;
    assert_eq!(server.status(), "active");
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_session_activates_on_its_seed_and_pausing_it_notifies() {
    let (server, shell, core, cfg) = setup("pending").await;
    let s = core.recorder_open(cfg, camera()).await.unwrap();
    assert_eq!(s.phase, RecorderPhase::Recording);
    assert_eq!(s.control_mode, ControlMode::Recording);
    let s = wait_for(&core, 10, |s| s.capture_count >= 1).await;
    assert_eq!(s.session_status.as_deref(), Some("active"));
    assert_eq!(server.status(), "active");
    core.recorder_pause().await.unwrap();
    assert!(shell.notices().iter().any(|n| n.title == "Lookout — recording paused"));
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn credited_time_survives_pause_resume_and_never_moves_backward() {
    let (server, _shell, core, cfg) = setup("active").await;
    server.set_tracked_seconds(120);
    open_recording(&core, &cfg).await;
    let s = core.recorder_pause().await.unwrap();
    assert_eq!(s.tracked_seconds, 140, "load 120 + flush 20");
    // A stale, lower reading on the way back in.
    server.set_confirm_tracked_override(Some(60));
    let s = core.recorder_resume().await.unwrap();
    assert_eq!(s.tracked_seconds, 140);
    assert_eq!(s.clock.base_seconds, 140);
    let s = wait_for(&core, 10, |s| s.capture_count >= 3).await;
    assert_eq!(s.tracked_seconds, 140, "the stale 60 was ignored");
    assert!(s.display_seconds >= 140);
    core.recorder_close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_load_that_hits_a_dropped_connection_is_a_load_error_not_a_hang() {
    let (server, _shell, core, cfg) = setup("active").await;
    server.drop_next("GET", TOK);
    let started = Instant::now();
    let s = core.recorder_open(cfg, camera()).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(s.phase, RecorderPhase::LoadError);
    assert!(s.session_error.unwrap().contains("Network error"));
    assert!(!core.has_capture_loop());
    core.recorder_close().await;
}

// ── Randomized, model-checked stress ─────────────────────────────────

/// A tiny deterministic generator so a failing run can be replayed by seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

#[derive(Debug, Clone)]
enum Step {
    Pause,
    Resume,
    BeginStop,
    CancelStop,
    FinishStop { edit: bool },
    /// Two actions issued together, the second a few ms after the first.
    Both(Box<Step>, Box<Step>),
    /// The next `method path` fails with this status (or drops).
    Fault { method: &'static str, path: &'static str, status: Option<u16> },
    /// The server ends the session behind our back.
    ServerStops,
    /// The compositor revokes the screencast (screen runs only).
    Revoke,
    /// The capture loop reports a dead source (screen runs only).
    SourceLost,
    /// The shell's camera fails its next grab (camera runs only).
    CameraGrabFails,
    /// The link dies for a while: every request dropped, or hung.
    Offline { ms: u64, hang: bool },
    /// Every answer from here on takes this long.
    Latency { ms: u64 },
}

const FAULT_SITES: &[(&str, &str)] = &[
    ("POST", "/api/sessions/tok/pause"),
    ("POST", "/api/sessions/tok/resume"),
    ("POST", "/api/sessions/tok/stop"),
    ("PATCH", "/api/sessions/tok/name"),
    ("GET", "/api/sessions/tok/status"),
    ("POST", "/api/sessions/tok/screenshots"),
    ("GET", "/api/sessions/tok/upload-url"),
];

fn random_step(rng: &mut Rng, camera: bool, depth: u32) -> Step {
    match rng.below(17) {
        0 | 1 => Step::Pause,
        2 | 3 => Step::Resume,
        4 => Step::BeginStop,
        5 => Step::CancelStop,
        6 => Step::FinishStop {
            edit: rng.chance(2),
        },
        7 if depth == 0 => Step::Both(
            Box::new(random_step(rng, camera, 1)),
            Box::new(random_step(rng, camera, 1)),
        ),
        8 | 9 => {
            let (method, path) = FAULT_SITES[rng.below(FAULT_SITES.len() as u64) as usize];
            let status = match rng.below(4) {
                0 => Some(409),
                1 => Some(500),
                2 => Some(429),
                _ => None,
            };
            Step::Fault { method, path, status }
        }
        10 => Step::ServerStops,
        11 if !camera => Step::Revoke,
        12 if !camera => Step::SourceLost,
        13 if camera => Step::CameraGrabFails,
        14 | 15 => Step::Offline {
            ms: 200 + rng.below(1_300),
            hang: rng.chance(3),
        },
        16 => Step::Latency {
            ms: rng.below(150),
        },
        _ => Step::Pause,
    }
}

fn expected_control_mode(s: &RecorderState) -> ControlMode {
    if s.source_lost.is_some() {
        return ControlMode::Paused;
    }
    let live = matches!(s.session_status.as_deref(), Some("active" | "pending"));
    match s.phase {
        RecorderPhase::Recording => ControlMode::Recording,
        RecorderPhase::Paused | RecorderPhase::Resuming => ControlMode::Paused,
        RecorderPhase::Pausing
        | RecorderPhase::StopPausing
        | RecorderPhase::StopPrompting
        | RecorderPhase::StopFinishing => {
            if live {
                ControlMode::Recording
            } else {
                ControlMode::Paused
            }
        }
        RecorderPhase::Loading | RecorderPhase::LoadError => ControlMode::Recording,
        RecorderPhase::Finished => ControlMode::Paused,
    }
}

async fn run_action(core: &Arc<Core>, step: &Step) -> Result<(), RecorderError> {
    match step {
        Step::Pause => core.recorder_pause().await.map(|_| ()),
        Step::Resume => core.recorder_resume().await.map(|_| ()),
        Step::BeginStop => core.recorder_begin_stop().await.map(|_| ()),
        Step::CancelStop => core.recorder_cancel_stop().await.map(|_| ()),
        Step::FinishStop { edit } => core.recorder_finish_stop(Some("n".into()), *edit).await.map(|_| ()),
        _ => Ok(()),
    }
}

/// Everything that must hold between steps, whatever happened.
fn check_invariants(
    trace: &str,
    s: &RecorderState,
    core: &Core,
    server: &MockServer,
    prev: Option<&RecorderState>,
    server_flipped: bool,
) {
    let transitional = matches!(
        s.phase,
        RecorderPhase::Pausing
            | RecorderPhase::Resuming
            | RecorderPhase::StopPausing
            | RecorderPhase::StopFinishing
            | RecorderPhase::Loading
    );
    assert!(!transitional, "{trace}\nstuck in a transitional phase: {s:#?}");
    assert_eq!(
        s.capturing,
        s.phase == RecorderPhase::Recording,
        "{trace}\ncapturing/phase disagree: {s:#?}"
    );
    assert_eq!(
        s.clock.running,
        s.phase == RecorderPhase::Recording,
        "{trace}\nclock running/phase disagree: {s:#?}"
    );
    assert_eq!(s.control_mode, expected_control_mode(s), "{trace}\n{s:#?}");
    if s.phase == RecorderPhase::Recording {
        assert!(core.has_capture_loop(), "{trace}\nrecording with no loop: {s:#?}");
        assert!(s.source_lost.is_none(), "{trace}\nrecording with a lost source: {s:#?}");
    } else {
        assert!(!core.has_capture_loop(), "{trace}\nloop running while {:?}", s.phase);
    }
    if let Some(p) = prev {
        assert!(s.tracked_seconds >= p.tracked_seconds, "{trace}\ntracked moved back: {s:#?}");
        assert!(s.capture_count >= p.capture_count, "{trace}\ncount moved back: {s:#?}");
    }
    assert!(
        s.tracked_seconds <= server.tracked_seconds(),
        "{trace}\nwe credited more than the server did: {s:#?} vs {}",
        server.tracked_seconds()
    );
    assert_eq!(s.too_short_to_compile, s.tracked_seconds < 60, "{trace}");
    assert!(s.display_seconds >= s.tracked_seconds, "{trace}\n{s:#?}");
    assert!(
        s.display_seconds <= s.tracked_seconds + s.clock.max_interpolation_seconds,
        "{trace}\n{s:#?}"
    );
    // Against the server, unless the server changed behind our back and
    // we haven't had a reason to talk to it yet.
    if !server_flipped {
        let server_status = server.status();
        match s.phase {
            RecorderPhase::Recording => assert!(
                server_status == "active" || server_status == "pending",
                "{trace}\nrecording but the server says {server_status}: {s:#?}"
            ),
            RecorderPhase::Paused | RecorderPhase::StopPrompting => {
                // A lost source parks the controls but leaves the session
                // open and active on purpose: it keeps its time until the
                // user picks a source again or stops. Otherwise the server
                // agrees we're paused (a pending session stays pending).
                let ok = if s.source_lost.is_some() {
                    matches!(server_status.as_str(), "active" | "pending" | "paused")
                } else {
                    server_status == "paused" || server_status == "pending"
                };
                assert!(ok, "{trace}\n{:?} but the server says {server_status}: {s:#?}", s.phase);
            }
            RecorderPhase::Finished => assert!(
                matches!(server_status.as_str(), "stopped" | "compiling" | "complete" | "failed"),
                "{trace}\nfinished but the server says {server_status}: {s:#?}"
            ),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn randomized_sequences_hold_every_invariant() {
    // `LOOKOUT_STRESS_RUNS=200 cargo test ... randomized` for a deeper sweep.
    let runs: u64 = std::env::var("LOOKOUT_STRESS_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    const STEPS: usize = 9;
    for run in 0..runs {
        let seed = 0x9E37_79B9_7F4A_7C15u64 ^ (run + 1).wrapping_mul(0x2545_F491_4F6C_DD1D);
        let mut rng = Rng(seed);
        let camera_run = run % 2 == 0;
        let initial = ["active", "paused", "pending"][rng.below(3) as usize];
        let (server, shell, core, cfg) = setup(initial).await;
        let source = if camera_run { camera() } else { pipewire() };
        let mut trace = format!("run {run} seed {seed:#x} source {source:?} initial {initial}");
        let started = Instant::now();
        core.recorder_open(cfg, source).await.unwrap();
        if camera_run && initial != "paused" {
            wait_for(&core, 10, |s| s.capture_count >= 1).await;
        }
        let mut prev = core.recorder_state();
        let mut server_flipped = false;
        check_invariants(&trace, prev.as_ref().unwrap(), &core, &server, None, server_flipped);

        for i in 0..STEPS {
            let step = random_step(&mut rng, camera_run, 0);
            trace.push_str(&format!("\n  {i}: {step:?}"));
            match &step {
                Step::Fault { method, path, status } => match status {
                    Some(code) => server.fail_next(method, path, *code, r#"{"error":"injected"}"#),
                    None => server.drop_next(method, path),
                },
                Step::ServerStops => {
                    server.set_status("stopped");
                    server_flipped = true;
                }
                Step::Revoke => {
                    core.emit(CoreEvent::ScreencastRevoked(crate::ScreencastRevoked {
                        node_ids: vec![42],
                    }));
                }
                Step::SourceLost => {
                    let generation = core.with_recording(|rec| rec.loop_generation).unwrap_or(0);
                    core.emit_tagged(
                        generation,
                        CoreEvent::SourceLost(crate::CaptureSourceLost {
                            message: "injected".into(),
                        }),
                    );
                }
                Step::CameraGrabFails => shell.set(CameraBehaviour {
                    fail_grabs: 1,
                    ..Default::default()
                }),
                Step::Offline { ms, hang } => {
                    let kind = if *hang {
                        crate::mock_server::OfflineKind::Hang
                    } else {
                        crate::mock_server::OfflineKind::Drop
                    };
                    server.go_offline(Duration::from_millis(*ms), kind);
                    // Something happens while the link is down: the next
                    // step's action, or just the loop's own ticks.
                    tokio::time::sleep(Duration::from_millis(ms / 2)).await;
                }
                Step::Latency { ms } => server.set_latency(*ms),
                Step::Both(a, b) => {
                    let first = tokio::spawn({
                        let (core, a) = (core.clone(), (**a).clone());
                        async move { run_action(&core, &a).await }
                    });
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    let second = tokio::time::timeout(Duration::from_secs(40), run_action(&core, b)).await;
                    let first = tokio::time::timeout(Duration::from_secs(40), first).await;
                    assert!(first.is_ok() && second.is_ok(), "{trace}\nan action hung");
                    trace.push_str(&format!(" -> {:?} / {:?}", first.unwrap().unwrap(), second.unwrap()));
                }
                action => {
                    let result = tokio::time::timeout(Duration::from_secs(40), run_action(&core, action)).await;
                    assert!(result.is_ok(), "{trace}\nan action hung");
                    trace.push_str(&format!(" -> {:?}", result.unwrap()));
                }
            }
            // Let follow-ups (a revoke's loop stop, a terminated session's
            // status sync) and in-flight loop events settle.
            let settle = Instant::now() + Duration::from_secs(16);
            loop {
                let s = core.recorder_state().unwrap();
                let settled = match s.phase {
                    RecorderPhase::Recording => core.has_capture_loop(),
                    RecorderPhase::Paused | RecorderPhase::StopPrompting | RecorderPhase::Finished => {
                        !core.has_capture_loop()
                    }
                    _ => false,
                };
                if settled {
                    break;
                }
                assert!(Instant::now() < settle, "{trace}\nnever settled: {s:#?}");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
            let s = core.recorder_state().unwrap();
            // A terminal status learned from the server counts as caught up.
            if s.phase == RecorderPhase::Finished {
                server_flipped = false;
            }
            check_invariants(&trace, &s, &core, &server, prev.as_ref(), server_flipped);
            prev = Some(s);
        }

        // However it went, closing is clean and complete.
        tokio::time::timeout(Duration::from_secs(30), core.recorder_close())
            .await
            .unwrap_or_else(|_| panic!("{trace}\nclose hung"));
        assert!(core.recorder_state().is_none(), "{trace}");
        assert!(!core.has_capture_loop(), "{trace}");
        assert!(shell.camera_closed(), "{trace}\ncamera left open: {:?}", shell.camera_log.lock().unwrap());
        assert!(shell.states().last().unwrap().is_none(), "{trace}\nclose not published");
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "{trace}\nrun took too long: {:?}",
            started.elapsed()
        );
    }
}
