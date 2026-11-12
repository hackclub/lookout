//! Credit simulations: the recorder and capture loop against the real
//! server's crediting rules (see `mock_server::set_real_credit`), scaled
//! down so an hour of abuse runs in seconds. Each test measures what the
//! server credited against what the user actually recorded, and pins the
//! bound on the difference — the number a "paused and resumed all hour and
//! lost minutes" report is really about.
//!
//! The link is degraded the ways real links are: slow on every request,
//! dead for a while (connections refused, or never answered), dead for
//! longer than the uploader keeps captures, dead exactly when the user
//! pauses.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::mock_server::{MockServer, OfflineKind};
use crate::recorder::*;
use crate::tuning::Timing;
use crate::{BoxFuture, CameraFrame, Core, CoreEvent, Frontend, SessionConfig};

/// One scaled interval. Two real seconds stand in for a minute.
const INTERVAL_MS: u64 = 2_000;

/// A shell whose camera takes a while to produce its first frame — the
/// resume-to-first-frame gap a real webcam (or a screencast negotiation)
/// has, which used to be uncredited on every resume.
struct SlowCameraShell {
    start_delay: Duration,
    events: Mutex<Vec<CoreEvent>>,
}

impl SlowCameraShell {
    fn new(start_delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            start_delay: Duration::from_millis(start_delay_ms),
            events: Mutex::new(Vec::new()),
        })
    }

    fn tick_errors(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                CoreEvent::TickError(e) => Some(e.message.clone()),
                _ => None,
            })
            .collect()
    }
}

fn tiny_jpeg() -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        16,
        12,
        image::Rgba([200, 90, 20, 255]),
    ));
    crate::capture::encode_frame_jpeg(&img, 80).unwrap().data
}

impl Frontend for SlowCameraShell {
    fn emit(&self, event: CoreEvent) {
        self.events.lock().unwrap().push(event);
    }
    fn wants_preview_frames(&self) -> bool {
        false
    }
    fn set_tray_title(&self, _text: &str, _paused: bool) {}
    fn camera_start(&self, _device_id: &str) -> BoxFuture<'static, Result<(), String>> {
        let delay = self.start_delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(())
        })
    }
    fn camera_grab_frame(&self, _device_id: &str) -> BoxFuture<'static, Result<CameraFrame, String>> {
        let jpeg = tiny_jpeg();
        Box::pin(async move {
            Ok(CameraFrame {
                jpeg,
                width: 16,
                height: 12,
            })
        })
    }
    fn camera_stop(&self) {}
}

fn camera() -> RecordingSource {
    RecordingSource::Camera {
        device_id: "cam".into(),
    }
}

async fn setup(
    status: &str,
    camera_delay_ms: u64,
) -> (Arc<MockServer>, Arc<SlowCameraShell>, Arc<Core>, SessionConfig) {
    let server = MockServer::start().await;
    server.set_status(status);
    server.set_real_credit(INTERVAL_MS);
    let shell = SlowCameraShell::new(camera_delay_ms);
    let core = Core::new(shell.clone() as Arc<dyn Frontend>, "Sim", "0.0.0");
    core.set_timing(Timing::scaled(INTERVAL_MS));
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

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// The seconds the server credited against the seconds recorded, with the
/// bound every simulation holds the loss to.
fn report(label: &str, recorded: Duration, server: &MockServer) -> f64 {
    let tracked = server.tracked_seconds() as f64;
    let loss = secs(recorded) - tracked;
    let resets = server.credit_log().iter().filter(|e| e.reset).count();
    eprintln!(
        "[sim] {label}: recorded {:.2}s, credited {tracked}s, loss {loss:+.2}s, grid resets {resets}",
        secs(recorded)
    );
    loss
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x % n
    }
}

// ── Pause/resume cycling ─────────────────────────────────────────────

/// The report: pause and resume all hour, lose minutes. The grid reseeds
/// on every resume, and the seed used to be stamped when the camera's
/// first frame arrived — a second or more after the resume, every time.
/// Now the seed carries the resume instant, so a cycle loses at most the
/// flush's rounding-down to whole seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_resume_cycling_loses_at_most_the_rounding() {
    // A slow camera: 1.2s to the first frame, so the old gap would show
    // as >1s lost per cycle, well clear of the ≤1s rounding.
    let (server, shell, core, cfg) = setup("paused", 1_200).await;
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.phase == RecorderPhase::Paused).await;

    let cycles = 10;
    let mut rng = Rng(0xC0FFEE);
    let mut recorded = Duration::ZERO;
    for i in 0..cycles {
        let pressed_resume = Instant::now();
        core.recorder_resume().await.unwrap();
        // Record for 1.2–4.5s: short cycles that straddle the interval
        // boundary both ways.
        let active = Duration::from_millis(1_200 + rng.below(3_300));
        tokio::time::sleep(active).await;
        let pressed_pause = Instant::now();
        core.recorder_pause().await.unwrap();
        recorded += pressed_pause - pressed_resume;
        let loss = report(&format!("cycle {i}"), recorded, &server);
        // Per cycle: the flush floors to whole seconds (<1s), nothing else.
        assert!(
            loss <= (i + 1) as f64 * 1.0 + 0.3,
            "cycle {i}: lost {loss:.2}s over {} cycles — more than rounding",
            i + 1
        );
        assert!(loss >= -0.5, "cycle {i}: credited more than recorded ({loss:+.2}s)");
        tokio::time::sleep(Duration::from_millis(300 + rng.below(600))).await;
    }
    assert!(
        shell.tick_errors().is_empty(),
        "no upload should have failed: {:?}",
        shell.tick_errors()
    );
    assert_eq!(server.status(), "paused");
    core.recorder_close().await;
}

/// The seed capture is stamped with the resume instant, not with the
/// first frame's arrival.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_seed_is_stamped_at_the_resume_instant() {
    let (server, _shell, core, cfg) = setup("paused", 1_200).await;
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.phase == RecorderPhase::Paused).await;
    let before = crate::capture_loop::current_unix_ms();
    core.recorder_resume().await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 1).await;
    let reqs = server.requests();
    let resume_at = reqs.iter().position(|r| r.path.ends_with("/resume")).unwrap();
    let presign = reqs[resume_at..]
        .iter()
        .find(|r| r.path.ends_with("/upload-url"))
        .expect("a seed presign after the resume");
    let stamped = presign
        .query
        .split('&')
        .find_map(|kv| kv.strip_prefix("capturedAt="))
        .map(|v| v.replace("%3A", ":"))
        .and_then(|v| crate::capture_loop::parse_iso_to_unix_ms(&v))
        .expect("capturedAt on the seed");
    let gap_ms = stamped - before;
    assert!(
        (0..600).contains(&gap_ms),
        "seed stamped {gap_ms}ms after the resume press; the camera took 1200ms"
    );
    core.recorder_close().await;
}

// ── Slow links ───────────────────────────────────────────────────────

/// Every request slow, every credit kept: captures are stamped when
/// taken, and the next tick is scheduled from the slot, not from when the
/// upload finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_latency_keeps_every_credit() {
    let (server, shell, core, cfg) = setup("active", 100).await;
    // 100ms per request — three seconds per request at full scale —
    // three requests per upload, every interval.
    server.set_latency(100);
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.phase == RecorderPhase::Recording).await;
    tokio::time::sleep(Duration::from_millis(6 * INTERVAL_MS + 700)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("latency", pressed_pause - pressed_record, &server);
    assert!(
        server.credit_log().iter().all(|e| !e.reset),
        "a capture missed its slot: {:#?}",
        server.credit_log()
    );
    assert!(loss.abs() <= 1.3, "lost {loss:+.2}s under latency");
    assert!(shell.tick_errors().is_empty(), "{:?}", shell.tick_errors());
    core.recorder_close().await;
}

/// The link dies (connections refused) for two and a half intervals.
/// Captures keep being taken on the grid, wait in the backlog, and all
/// land when the link is back — nothing lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drop_outage_inside_the_envelope_loses_nothing() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 2).await;
    server.go_offline(Duration::from_millis(INTERVAL_MS * 5 / 2), OfflineKind::Drop);
    tokio::time::sleep(Duration::from_millis(5 * INTERVAL_MS + 400)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("drop outage", pressed_pause - pressed_record, &server);
    assert!(
        server.credit_log().iter().all(|e| !e.reset),
        "a capture missed its slot: {:#?}",
        server.credit_log()
    );
    assert!(loss.abs() <= 1.3, "lost {loss:+.2}s across a drop outage");
    core.recorder_close().await;
}

/// The link hangs (requests never answered) for two and a half intervals.
/// Hanging used to be the worst case: every attempt ate its full timeout,
/// the tick waited on the upload, and the next capture landed far off its
/// slot. Now the capture cadence is independent of the uploader.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hang_outage_inside_the_envelope_loses_nothing() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 2).await;
    server.go_offline(Duration::from_millis(INTERVAL_MS * 5 / 2), OfflineKind::Hang);
    tokio::time::sleep(Duration::from_millis(6 * INTERVAL_MS + 400)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("hang outage", pressed_pause - pressed_record, &server);
    assert!(
        server.credit_log().iter().all(|e| !e.reset),
        "a capture missed its slot: {:#?}",
        server.credit_log()
    );
    assert!(loss.abs() <= 1.3, "lost {loss:+.2}s across a hang outage");
    core.recorder_close().await;
}

/// An outage longer than the uploader keeps captures: the oldest are
/// dropped (the server would re-stamp them anyway), the user is told, and
/// crediting resumes on the grid — the loss is the outage, not the rest of
/// the recording.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outage_past_the_backlog_age_drops_the_oldest_and_recovers() {
    let (server, shell, core, cfg) = setup("active", 100).await;
    let timing = core.timing();
    let max_age = Duration::from_millis(timing.backlog_max_age_ms);
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 2).await;
    let outage = max_age + Duration::from_millis(2 * INTERVAL_MS);
    server.go_offline(outage, OfflineKind::Drop);
    tokio::time::sleep(outage + Duration::from_millis(4 * INTERVAL_MS + 500)).await;
    let log = server.credit_log();
    let full = (INTERVAL_MS / 1000) as i64;
    let tail: Vec<_> = log.iter().rev().take(2).collect();
    assert!(
        tail.len() == 2 && tail.iter().all(|e| e.credit_secs == full && !e.reset),
        "crediting did not recover after the outage: {log:#?}"
    );
    assert!(
        shell.tick_errors().iter().any(|m| m.contains("could not be uploaded")),
        "the user was not told about the lost minutes: {:?}",
        shell.tick_errors()
    );
    // What was lost is bounded by the outage: at most the dropped slots
    // plus the one reseed.
    let intervals_recorded = log.len() as i64; // confirmed captures incl. seed
    let resets = log.iter().filter(|e| e.reset).count() as i64;
    assert!(resets <= 1, "more than one reseed: {log:#?}");
    assert!(intervals_recorded >= 6, "too few captures landed: {log:#?}");
    core.recorder_close().await;
}

/// Pausing while the link is dead: the flush can't land within its budget
/// and neither can the pause, so the recorder stays recording with the
/// error showing, the captures stay in the Core's backlog across the loop
/// restart, and the next pause — link back — delivers everything. The
/// restarted loop reseeds the grid, so one partial interval is the most
/// a failed pause can cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_during_an_outage_fails_loudly_and_the_next_one_credits_everything() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 2).await;
    server.go_offline(Duration::from_millis(3 * INTERVAL_MS), OfflineKind::Drop);
    tokio::time::sleep(Duration::from_millis(INTERVAL_MS + 300)).await;
    let attempted = Instant::now();
    let err = core.recorder_pause().await.expect_err("the pause cannot reach the server");
    eprintln!("[sim] pause offline: {err:?} after {:.2}s", secs(attempted.elapsed()));
    let state = core.recorder_state().unwrap();
    assert_eq!(state.phase, RecorderPhase::Recording, "still recording: {state:#?}");
    let flush_budget = core.timing().flush_budget();
    assert!(
        attempted.elapsed() < flush_budget + Duration::from_millis(1_500),
        "the offline pause took {:.2}s to fail",
        secs(attempted.elapsed())
    );
    assert!(state.action_error.is_some(), "the failure is shown");
    assert!(core.has_capture_loop(), "the loop was put back");
    assert_eq!(server.status(), "active");
    // The link returns; keep recording a little, then pause for real.
    tokio::time::sleep(Duration::from_millis(3 * INTERVAL_MS)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("pause offline, then online", pressed_pause - pressed_record, &server);
    // The failed pause restarted the loop, which reseeds the grid once
    // (its flush could not land): one interval at most, plus rounding.
    assert!(
        loss <= (INTERVAL_MS / 1000) as f64 + 1.3,
        "lost {loss:+.2}s across an offline pause: {:#?}",
        server.credit_log()
    );
    let resets = server.credit_log().iter().filter(|e| e.reset).count();
    assert!(resets <= 1, "more than the one reseed: {:#?}", server.credit_log());
    assert_eq!(server.status(), "paused");
    core.recorder_close().await;
}

/// Pausing right after an outage: the backlog goes first, then the flush,
/// and the pause POST only after both — so the partial minute is measured
/// from the right mark and nothing is refused as out of order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_backlog_lands_before_the_flush_and_the_pause() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 2).await;
    let outage = Duration::from_millis(2 * INTERVAL_MS + 200);
    server.go_offline(outage, OfflineKind::Drop);
    tokio::time::sleep(outage + Duration::from_millis(150)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("backlog then flush", pressed_pause - pressed_record, &server);
    assert!(loss.abs() <= 1.3, "lost {loss:+.2}s");
    let reqs: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/screenshots") || r.path.ends_with("/pause"))
        .collect();
    let finals: Vec<usize> = reqs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.body.contains("\"final\":true"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(finals.len(), 1, "one flush: {reqs:#?}");
    assert_eq!(finals[0], reqs.len() - 2, "the flush is the last confirm, right before the pause");
    assert!(
        server.credit_log().iter().all(|e| !e.reset),
        "nothing was refused or reseeded: {:#?}",
        server.credit_log()
    );
    core.recorder_close().await;
}

/// A single capture the server refuses for good (a 4xx) is dropped
/// without stalling the queue behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_capture_is_dropped_and_the_next_one_lands() {
    let (server, shell, core, cfg) = setup("active", 100).await;
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 1).await;
    server.fail_next("GET", "/api/sessions/tok/upload-url", 400, r#"{"error":"captured_at_not_monotonic"}"#);
    wait_for(&core, 10, |s| s.capture_count >= 3).await;
    assert!(
        shell.tick_errors().iter().any(|m| m.contains("captured_at_not_monotonic")),
        "{:?}",
        shell.tick_errors()
    );
    // Exactly one presign per surviving capture plus the refused one: no
    // retry storm on a 400.
    let presigns = server.count("GET", "/api/sessions/tok/upload-url");
    assert_eq!(presigns, server.confirmed_count() + 1, "retried a 400");
    core.recorder_close().await;
}

/// The link hung and came back, but the uploader is still inside a
/// request the hang swallowed (it times out long after the link is up).
/// A pause now must not wait for that: the flush preempts the stuck
/// attempt, the backlog and the flush land fresh, the pause succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_right_after_a_hang_does_not_wait_for_the_stuck_request() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    let timing = core.timing();
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 2).await;
    // Hang for most of an interval: the next tick's upload starts inside
    // it and is stuck for a step timeout, past the hang's end.
    let hang = Duration::from_millis(INTERVAL_MS * 3 / 4);
    server.go_offline(hang, OfflineKind::Hang);
    tokio::time::sleep(hang + Duration::from_millis(100)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.expect("the pause lands fresh");
    let took = pressed_pause.elapsed();
    assert!(
        took < timing.flush_budget() + Duration::from_millis(300),
        "the pause took {:.2}s, waited for the stuck request",
        secs(took)
    );
    let loss = report("pause after a hang", pressed_pause - pressed_record, &server);
    assert!(loss.abs() <= 1.3, "lost {loss:+.2}s: {:#?}", server.credit_log());
    assert_eq!(server.status(), "paused");
    core.recorder_close().await;
}

// ── Odd servers and clocks ───────────────────────────────────────────

/// The server did the work but the client never heard: a confirm whose
/// answer was lost is retried, the server answers idempotently, and the
/// interval is credited exactly once with the grid intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_confirm_answer_credits_exactly_once() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 1).await;
    server.drop_after_next("POST", "/api/sessions/tok/screenshots");
    tokio::time::sleep(Duration::from_millis(4 * INTERVAL_MS + 300)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("lost confirm answer", pressed_pause - pressed_record, &server);
    assert!(loss.abs() <= 1.3, "lost {loss:+.2}s: {:#?}", server.credit_log());
    assert!(
        server.credit_log().iter().all(|e| !e.reset),
        "the grid was reset: {:#?}",
        server.credit_log()
    );
    // The retried confirm hit the idempotent path: one credit entry per
    // capture, not two for the one whose answer was lost.
    assert_eq!(server.credit_log().len(), server.confirmed_count());
    core.recorder_close().await;
}

/// A machine whose clock is a minute and a half off. The seed is stamped
/// before the offset is known, so the first interval after it misses the
/// grid once; every later capture is corrected and credits. One interval
/// per session is the cost, not every capture (which is what happened
/// before the clock offset existed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skewed_clock_costs_at_most_one_interval() {
    let (server, _shell, core, cfg) = setup("active", 100).await;
    server.set_clock_skew(90_000);
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 1).await;
    tokio::time::sleep(Duration::from_millis(6 * INTERVAL_MS + 300)).await;
    let pressed_pause = Instant::now();
    core.recorder_pause().await.unwrap();
    let loss = report("skewed clock", pressed_pause - pressed_record, &server);
    let resets = server.credit_log().iter().filter(|e| e.reset).count();
    assert!(resets <= 1, "{:#?}", server.credit_log());
    assert!(
        loss <= (INTERVAL_MS / 1000) as f64 + 1.3,
        "lost {loss:+.2}s on a skewed clock: {:#?}",
        server.credit_log()
    );
    // After the correction, captures land on the grid again.
    let tail: Vec<_> = server.credit_log().into_iter().rev().skip(1).take(2).collect();
    assert!(
        tail.iter().all(|e| e.credit_secs == (INTERVAL_MS / 1000) as i64),
        "not back on the grid: {:#?}",
        server.credit_log()
    );
    core.recorder_close().await;
}

// ── An hour of everything at once ────────────────────────────────────

/// Random pause/resume cycling on a flaky link: short outages (dropped
/// or hung), bursts of latency, pauses that land mid-outage. The loss is
/// accounted for exactly: a second of rounding per completed pause, and
/// per pause that failed on a dead link one interval (the restarted loop
/// reseeds) plus the api timeout if the POST hung (the loop is stopped
/// while a pause is in flight), and nothing else. `LOOKOUT_SIM_STEPS` for
/// a longer run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn random_cycling_on_a_flaky_link_loses_only_what_is_accounted_for() {
    let steps: u64 = std::env::var("LOOKOUT_SIM_STEPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(14);
    let (server, _shell, core, cfg) = setup("active", 300).await;
    let mut rng = Rng(0xDEAD_BEEF ^ steps);
    let timing = core.timing();
    let pressed_record = Instant::now();
    core.recorder_open(cfg, camera()).await.unwrap();
    wait_for(&core, 5, |s| s.capture_count >= 1).await;
    let mut recording_since = Some(pressed_record);
    let mut recorded = Duration::ZERO;
    let mut pauses_done = 0u32;
    let mut pauses_failed = 0u32;
    let mut outages = 0u32;
    for i in 0..steps {
        match rng.below(6) {
            0 | 1 => {
                // An outage, never longer than the backlog keeps captures.
                let ms = 300 + rng.below(timing.backlog_max_age_ms / 2);
                let kind = if rng.below(3) == 0 { OfflineKind::Hang } else { OfflineKind::Drop };
                server.go_offline(Duration::from_millis(ms), kind);
                outages += 1;
                tokio::time::sleep(Duration::from_millis(rng.below(ms))).await;
            }
            2 => server.set_latency(rng.below(120)),
            _ => {
                let state = core.recorder_state().unwrap();
                match state.phase {
                    RecorderPhase::Recording => {
                        let pressed = Instant::now();
                        match core.recorder_pause().await {
                            Ok(_) => {
                                recorded += pressed - recording_since.take().unwrap();
                                pauses_done += 1;
                            }
                            Err(e) => {
                                eprintln!("[sim] step {i}: pause failed: {e:?}");
                                pauses_failed += 1;
                                // Still recording; the loop was put back.
                                assert!(core.has_capture_loop(), "loop not restored");
                            }
                        }
                    }
                    RecorderPhase::Paused => {
                        let pressed = Instant::now();
                        match core.recorder_resume().await {
                            Ok(_) => recording_since = Some(pressed),
                            Err(e) => eprintln!("[sim] step {i}: resume failed: {e:?}"),
                        }
                    }
                    other => panic!("unexpected phase {other:?}"),
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(400 + rng.below(2_200))).await;
    }
    // Finish on a pause, with the link up.
    server.set_latency(0);
    tokio::time::sleep(Duration::from_millis(timing.backlog_max_age_ms / 2)).await;
    if core.recorder_state().unwrap().phase == RecorderPhase::Recording {
        let pressed = Instant::now();
        core.recorder_pause().await.unwrap();
        recorded += pressed - recording_since.take().unwrap();
        pauses_done += 1;
    }
    let loss = report(
        &format!("flaky cycling ({steps} steps, {pauses_done} pauses, {pauses_failed} failed, {outages} outages)"),
        recorded,
        &server,
    );
    // A failed pause costs the reseed (one interval) and, if the POST hung
    // rather than failed fast, the api timeout the loop sat stopped for.
    let interval_s = (INTERVAL_MS / 1000) as f64;
    let failed_pause_s = interval_s + timing.api_timeout_ms as f64 / 1000.0;
    let allowed = pauses_done as f64 * 1.0 + pauses_failed as f64 * failed_pause_s + 0.5;
    assert!(
        loss <= allowed,
        "lost {loss:+.2}s, more than the {allowed:.1}s accounted for: {:#?}",
        server.credit_log()
    );
    assert!(loss >= -0.5, "credited more than recorded ({loss:+.2}s)");
    let resets = server.credit_log().iter().filter(|e| e.reset).count() as u32;
    assert!(
        resets <= pauses_failed,
        "{resets} grid resets for {pauses_failed} failed pauses: {:#?}",
        server.credit_log()
    );
    core.recorder_close().await;
}
