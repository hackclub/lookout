//! The uploader: delivers captured intervals strictly in order (the server
//! requires `capturedAt` to be monotonic, and the credit grid is measured
//! from confirmed captures), out of the recorder's backlog — a bounded
//! queue of what hasn't been delivered yet, so an outage shorter than the
//! server's trust envelope loses nothing: every capture carries the time
//! it was taken, and the server credits by that, not by arrival.

use std::sync::Arc;

use lookout_protocol::api;
use lookout_protocol::upload::{upload_and_confirm, UploadError, UploadErrorKind, UploadPayload};
use lookout_session::config::SessionConfig;
use lookout_session::schedule::{backlog_keep, BacklogItem};

use crate::events::{CaptureSessionTerminated, CaptureTickError, CaptureTickResult, CoreEvent};
use crate::recorder::Recorder;

/// Captured minutes the uploader keeps while the link is down. Older ones
/// are dropped first; anything past `Timing::backlog_max_age_ms` goes
/// regardless, because the server would re-stamp it with its own clock.
pub const BACKLOG_MAX_JOBS: usize = 5;

/// One captured interval waiting to be delivered. Lives in the recorder's
/// backlog until it is — so it survives the loop that took it: a pause
/// that failed on a dead link restarts the loop, a lost source gets a new
/// loop, and either way the next uploader picks the queue up.
pub struct UploadJob {
    /// The session this was captured for. A new session's uploader drops
    /// what an old one left behind.
    pub token: String,
    /// This interval's clip, tried first. On any failure the JPEG takes
    /// its place for good — a clip is only ever worth one minute of
    /// timelapse, the credit is what must not be lost.
    pub clip: Option<UploadPayload>,
    pub jpeg: UploadPayload,
    /// ISO `capturedAt`; `None` only with credit mode off.
    pub captured_at: Option<String>,
    /// Corrected unix ms of the capture, for the backlog age limit.
    pub captured_ms: i64,
    /// The pause/stop flush: the loop waits (bounded) on this. A flush
    /// never outlives its loop.
    pub done: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

impl UploadJob {
    pub fn is_final(&self) -> bool {
        self.done.is_some()
    }
}

/// What the uploader reports back to the loop after a job.
pub(crate) struct UploadOutcome {
    /// The server's target for the next capture, after a confirm.
    pub next_expected_at: Option<String>,
    /// The session is over server-side (SessionTerminated was emitted):
    /// stop capturing.
    pub terminated: bool,
}

/// What a status check found.
pub(crate) enum SessionCheck {
    /// Keep going (the session is live, was resumed, or couldn't be
    /// reached — best effort).
    Continue,
    /// The session is over server-side; `SessionTerminated` was emitted.
    Terminated,
}

/// Check the session with the server and handle sleep/pause recovery: a
/// session the server paused on its own (inactivity across a sleep or an
/// outage) is resumed, a finished one ends the loop.
pub(crate) async fn check_session(
    recorder: &Recorder,
    generation: u64,
    config: &SessionConfig,
) -> SessionCheck {
    let status_timeout = recorder.timing().step_timeout();
    let transport = recorder.host.transport();
    match api::session_status_typed(transport, config, status_timeout).await {
        Ok(data) => {
            lookout_session::log_line!("[capture-loop] session status: {}", data.status);
            if let Some(ts) = data.tracked_seconds {
                recorder.emit_tagged(generation, CoreEvent::TrackedSeconds(ts));
            }
            if data.status == "paused" {
                let _ = api::session_resume(transport, config, status_timeout).await;
                lookout_session::log_line!("[capture-loop] session resumed after sleep");
            } else if data.status != "active" && data.status != "pending" {
                lookout_session::log_line!(
                    "[capture-loop] session is {}, stopping capture loop",
                    data.status
                );
                recorder.emit_tagged(
                    generation,
                    CoreEvent::SessionTerminated(CaptureSessionTerminated {
                        status: data.status,
                    }),
                );
                return SessionCheck::Terminated;
            }
        }
        Err(e) => {
            lookout_session::log_line!("[capture-loop] status check failed: {e}");
        }
    }
    SessionCheck::Continue
}

/// Deliver one interval: the clip if there is one, else (or on any clip
/// failure) the JPEG.
async fn run_upload_job(
    recorder: &Recorder,
    config: &SessionConfig,
    clip: Option<UploadPayload>,
    jpeg: &UploadPayload,
    captured_at: Option<&str>,
) -> Result<lookout_protocol::upload::CaptureUploadResult, UploadError> {
    if let Some(clip) = clip {
        match upload_and_confirm(recorder, &clip, captured_at, config).await {
            Ok(r) => return Ok(r),
            Err(e) => lookout_session::log_line!("[uploader] clip upload failed ({e}) — retrying the interval as JPEG"),
        }
    }
    upload_and_confirm(recorder, jpeg, captured_at, config).await
}

/// Drop what can no longer be delivered (the policy is
/// [`lookout_session::schedule::backlog_keep`]), plus any flush this
/// uploader did not receive itself — its loop is gone. A flush of our own
/// is never dropped here; the loop's budget decides its fate.
fn trim_backlog(recorder: &Recorder, generation: u64, token: &str, own_flushes: &[u64]) {
    let now = recorder.corrected_now_ms();
    let max_age = recorder.timing().backlog_max_age_ms;
    let mut queue = recorder.upload_backlog.lock().unwrap();
    let (keep, dropped) = backlog_keep(
        queue.iter().filter(|j| !j.is_final()).map(|j| BacklogItem {
            captured_ms: j.captured_ms,
            ours: j.token == token,
        }),
        now,
        max_age,
        BACKLOG_MAX_JOBS,
    );
    let mut keep = keep.into_iter();
    let mut kept = std::collections::VecDeque::with_capacity(queue.len());
    while let Some(job) = queue.pop_front() {
        if job.is_final() {
            // Flushes are keyed by their captured_ms (a loop sends one).
            if own_flushes.contains(&(job.captured_ms as u64)) {
                kept.push_back(job);
            } else if let Some(done) = job.done {
                let _ = done.send(Err("flush orphaned".into()));
            }
            continue;
        }
        if keep.next().unwrap_or(false) {
            kept.push_back(job);
        }
    }
    *queue = kept;
    drop(queue);
    if dropped > 0 {
        let msg = format!(
            "{dropped} captured minute{} could not be uploaded and {} lost",
            if dropped == 1 { "" } else { "s" },
            if dropped == 1 { "was" } else { "were" }
        );
        lookout_session::log_line!("[uploader] {msg}");
        recorder.emit_tagged(generation, CoreEvent::TickError(CaptureTickError { message: msg }));
    }
}

/// The uploader task.
///
/// Per job: the pipeline's own three attempts per step, then a short
/// backoff and another round, until the job is delivered, refused for
/// good (a 4xx), or too old to be worth it. A flush (pause/stop) gets one
/// round: the loop's budget is what bounds a pause. After any failure the
/// session is checked, so a server-side pause is resumed and a finished
/// session ends the loop rather than the uploader retrying into a wall.
///
/// Jobs arrive over `jobs`; the queue itself is the recorder's, so a job
/// the uploader was killed with (its loop ended) is still there for the
/// next.
pub(crate) async fn uploader_task(
    recorder: Arc<Recorder>,
    generation: u64,
    mut jobs: tokio::sync::mpsc::UnboundedReceiver<UploadJob>,
    outcomes: tokio::sync::mpsc::UnboundedSender<UploadOutcome>,
) {
    let mut failed_rounds: usize = 0;
    // The flushes this uploader was handed, by captured_ms — any other
    // flush in the queue was a dead loop's.
    let mut own_flushes: Vec<u64> = Vec::new();
    let push = |recorder: &Recorder, own_flushes: &mut Vec<u64>, job: UploadJob| {
        if job.is_final() {
            own_flushes.push(job.captured_ms as u64);
        }
        recorder.upload_backlog.lock().unwrap().push_back(job);
    };
    loop {
        let empty = recorder.upload_backlog.lock().unwrap().is_empty();
        if empty {
            match jobs.recv().await {
                Some(job) => push(&recorder, &mut own_flushes, job),
                None => return,
            }
        }
        while let Ok(job) = jobs.try_recv() {
            push(&recorder, &mut own_flushes, job);
        }
        let config = recorder.config();
        let Some(config) = config else {
            recorder.emit_tagged(
                generation,
                CoreEvent::TickError(CaptureTickError {
                    message: "Not configured".into(),
                }),
            );
            let job = recorder.upload_backlog.lock().unwrap().pop_front();
            if let Some(done) = job.and_then(|j| j.done) {
                let _ = done.send(Err("not configured".into()));
            }
            continue;
        };
        trim_backlog(&recorder, generation, &config.token, &own_flushes);
        // Peek the head: it stays queued while in flight, so a killed
        // uploader loses nothing. The clip is taken — on any failure the
        // interval continues as its JPEG.
        let head = {
            let mut queue = recorder.upload_backlog.lock().unwrap();
            queue.front_mut().map(|job| {
                (
                    job.is_final(),
                    job.clip.take(),
                    job.jpeg.clone(),
                    job.captured_at.clone(),
                )
            })
        };
        let Some((is_final, clip, jpeg, captured_at)) = head else {
            continue;
        };
        // Deliver — unless a flush arrives meanwhile: the pause is waiting
        // on this queue, and an attempt stuck in a socket the link left
        // behind (a request hung during an outage times out long after
        // the link is back) must not run out the pause's budget. The
        // attempt is dropped and the head is retried at once, fresh.
        let upload = run_upload_job(&recorder, &config, clip, &jpeg, captured_at.as_deref());
        tokio::pin!(upload);
        let result = loop {
            tokio::select! {
                r = &mut upload => break Some(r),
                incoming = jobs.recv() => match incoming {
                    Some(job) => {
                        let cut_short = job.is_final();
                        push(&recorder, &mut own_flushes, job);
                        if cut_short {
                            break None;
                        }
                    }
                    None => return,
                },
            }
        };
        let Some(result) = result else {
            lookout_session::log_line!("[uploader] a flush is waiting — restarting the in-flight upload");
            continue;
        };
        match result {
            Ok(result) => {
                failed_rounds = 0;
                let (job, pending_behind) = {
                    let mut queue = recorder.upload_backlog.lock().unwrap();
                    let job = queue.pop_front().expect("head job");
                    (job, queue.len())
                };
                if pending_behind > 0 || is_final {
                    lookout_session::log_line!(
                        "[uploader] delivered {} (tracked {}s, {} still queued)",
                        if is_final { "the flush" } else { "a backlogged interval" },
                        result.tracked_seconds,
                        pending_behind
                    );
                }
                let next_expected_at = result.next_expected_at.clone();
                recorder.emit_tagged(generation, CoreEvent::TickResult(CaptureTickResult::from(result)));
                if let Some(done) = job.done {
                    let _ = done.send(Ok(()));
                }
                let _ = outcomes.send(UploadOutcome {
                    next_expected_at: Some(next_expected_at),
                    terminated: false,
                });
            }
            Err(e) => {
                lookout_session::log_line!("[uploader] upload failed: {e}");
                recorder.emit_tagged(
                    generation,
                    CoreEvent::TickError(CaptureTickError {
                        message: e.message.clone(),
                    }),
                );
                let give_up = match e.kind {
                    // The server refused this capture for good.
                    UploadErrorKind::Rejected(_) => true,
                    UploadErrorKind::SessionState | UploadErrorKind::Transient => {
                        if let SessionCheck::Terminated =
                            check_session(&recorder, generation, &config).await
                        {
                            let drained: Vec<UploadJob> =
                                recorder.upload_backlog.lock().unwrap().drain(..).collect();
                            for job in drained {
                                if let Some(done) = job.done {
                                    let _ = done.send(Err("session ended".into()));
                                }
                            }
                            let _ = outcomes.send(UploadOutcome {
                                next_expected_at: None,
                                terminated: true,
                            });
                            return;
                        }
                        // A flush gets its one round; the loop's budget is
                        // what bounds the pause, not our retries.
                        is_final
                    }
                };
                if give_up {
                    failed_rounds = 0;
                    let job = recorder.upload_backlog.lock().unwrap().pop_front();
                    if let Some(done) = job.and_then(|j| j.done) {
                        let _ = done.send(Err(e.message));
                    }
                    continue;
                }
                // Back off before the next round — but a flush arriving
                // mid-wait cuts it short: the pause is waiting on us.
                let delay = recorder.timing().retry_delay(failed_rounds);
                failed_rounds += 1;
                let backoff = recorder.host.runtime().sleep(delay);
                tokio::pin!(backoff);
                loop {
                    tokio::select! {
                        _ = &mut backoff => break,
                        incoming = jobs.recv() => match incoming {
                            Some(job) => {
                                let cut_short = job.is_final();
                                push(&recorder, &mut own_flushes, job);
                                if cut_short {
                                    break;
                                }
                            }
                            None => return,
                        },
                    }
                }
            }
        }
    }
}
