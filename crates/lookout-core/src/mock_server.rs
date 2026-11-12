//! A fake Lookout server for recorder tests: the session lifecycle
//! endpoints, the upload-url → R2 PUT → confirm pipeline, and a request
//! log with bodies, so a test can assert what went over the wire and in
//! what order. Fault injection by `(method, path)`, one-shot, in order.
//!
//! Two crediting models. The default is a flat one (every confirm after
//! the seed credits an interval, a `final` credits a fixed partial) — enough
//! for the recorder's state-machine tests, which only need monotonic
//! numbers. `set_real_credit` switches on the real server's rules
//! (packages/server/src/lib/timing.ts): a seed anchors a grid, a capture
//! credits a full interval only inside ±half an interval of its slot, a
//! miss reseeds, a `final` credits the elapsed partial since the last
//! credited mark, resume wipes the grid, and `capturedAt` is validated
//! (monotonic, after the session start, inside the ±5min envelope — or
//! re-stamped with server time). That's what the credit simulations run
//! against.
//!
//! Besides one-shot faults, the link itself can be degraded: `set_latency`
//! delays every answer, `go_offline` drops or hangs every request for a
//! while.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::capture_loop::current_unix_ms;
use crate::upload::unix_ms_to_iso;

#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    /// Path without the query string.
    pub path: String,
    #[allow(dead_code)]
    pub query: String,
    pub body: String,
}

struct Failure {
    method: String,
    path: String,
    kind: FailureKind,
}

enum FailureKind {
    /// Answer with this status and body.
    Status(u16, String),
    /// Close the connection without answering: a network error (no status).
    Drop,
    /// Never answer; the client's own timeout has to fire.
    Hang,
    /// Answer normally, after this long.
    Delay(u64),
    /// Process the request, then close the connection without answering:
    /// the server did the work, the client never heard.
    DropAfter,
}

enum Planned {
    Respond(u16, String),
    Drop,
    Hang,
}

/// How the link fails while offline.
#[derive(Clone, Copy, Debug)]
pub enum OfflineKind {
    /// Connections are closed without an answer.
    Drop,
    /// Requests are never answered.
    Hang,
}

/// The real server's crediting rules, scaled to an interval.
#[derive(Clone, Copy, Debug)]
struct CreditRules {
    interval_ms: i64,
    /// Half an interval, like STREAK_WINDOW_MS.
    window_ms: i64,
}

#[derive(Clone, Debug)]
struct Shot {
    captured_ms: i64,
    confirmed: bool,
}

/// What one confirm credited, for the simulations' bookkeeping.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CreditEntry {
    pub captured_ms: i64,
    pub is_final: bool,
    pub credit_secs: i64,
    /// The capture missed its slot and reseeded the grid.
    pub reset: bool,
}

struct State {
    status: String,
    tracked_seconds: i64,
    screenshot_count: i64,
    /// What `confirm` answers for `trackedSeconds`, regardless of the
    /// running count — to simulate a stale idempotent-retry reading.
    confirm_tracked_override: Option<i64>,
    clips_enabled: bool,
    failures: VecDeque<Failure>,
    next_id: u64,
    // ── real credit model ──
    rules: Option<CreditRules>,
    anchor_ms: Option<i64>,
    streak_count: i64,
    latest_confirmed_ms: Option<i64>,
    started_at_ms: Option<i64>,
    shots: HashMap<u64, Shot>,
    credit_log: Vec<CreditEntry>,
    // ── link quality ──
    latency_ms: u64,
    offline_until: Option<(tokio::time::Instant, OfflineKind)>,
    /// The server's clock runs this far ahead of the machine's.
    skew_ms: i64,
}

pub struct MockServer {
    base: String,
    state: Mutex<State>,
    log: Mutex<Vec<Req>>,
}

/// Seconds a `final` (pause/stop flush) capture credits in this fake.
pub const FINAL_CREDIT_SECS: i64 = 20;

impl MockServer {
    pub async fn start() -> Arc<Self> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = Arc::new(Self {
            base: format!("http://{addr}"),
            state: Mutex::new(State {
                status: "active".into(),
                tracked_seconds: 0,
                screenshot_count: 0,
                confirm_tracked_override: None,
                clips_enabled: false,
                failures: VecDeque::new(),
                next_id: 0,
                rules: None,
                anchor_ms: None,
                streak_count: 0,
                latest_confirmed_ms: None,
                started_at_ms: None,
                shots: HashMap::new(),
                credit_log: Vec::new(),
                latency_ms: 0,
                offline_until: None,
                skew_ms: 0,
            }),
            log: Mutex::new(Vec::new()),
        });
        let this = Arc::clone(&server);
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let this = Arc::clone(&this);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 8192];
                    let (head_end, mut n) = loop {
                        let k = match sock.read(&mut tmp).await {
                            Ok(k) => k,
                            Err(_) => return,
                        };
                        if k == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..k]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break (i + 4, buf.len());
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let mut lines = head.lines();
                    let request_line = lines.next().unwrap_or_default();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_string();
                    let target = parts.next().unwrap_or_default().to_string();
                    let mut content_length = 0usize;
                    for l in lines {
                        let lower = l.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("content-length:") {
                            content_length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    while n - head_end < content_length {
                        let k = match sock.read(&mut tmp).await {
                            Ok(k) => k,
                            Err(_) => return,
                        };
                        if k == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..k]);
                        n = buf.len();
                    }
                    let body_bytes = &buf[head_end..];
                    let (path, query) = match target.split_once('?') {
                        Some((p, q)) => (p.to_string(), q.to_string()),
                        None => (target.clone(), String::new()),
                    };
                    // R2 PUT bodies are binary; everything else is JSON.
                    let body = if path.starts_with("/r2/") {
                        format!("<{} bytes>", body_bytes.len())
                    } else {
                        String::from_utf8_lossy(body_bytes).to_string()
                    };
                    let req = Req {
                        method,
                        path,
                        query,
                        body,
                    };
                    let (delay_ms, planned) = this.plan(&req);
                    this.log.lock().unwrap().push(req);
                    if delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    let (status, resp_body) = match planned {
                        Planned::Respond(s, b) => (s, b),
                        Planned::Drop => {
                            let _ = sock.shutdown().await;
                            return;
                        }
                        Planned::Hang => {
                            // Hold the socket open for longer than any client
                            // timeout, then give up.
                            tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                            return;
                        }
                    };
                    let reason = match status {
                        200 => "OK",
                        400 => "Bad Request",
                        404 => "Not Found",
                        409 => "Conflict",
                        500 => "Internal Server Error",
                        502 => "Bad Gateway",
                        503 => "Service Unavailable",
                        _ => "Status",
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        server
    }

    pub fn base_url(&self) -> String {
        self.base.clone()
    }

    pub fn set_status(&self, status: &str) {
        self.state.lock().unwrap().status = status.to_string();
    }

    pub fn status(&self) -> String {
        self.state.lock().unwrap().status.clone()
    }

    pub fn tracked_seconds(&self) -> i64 {
        self.state.lock().unwrap().tracked_seconds
    }

    pub fn set_clips_enabled(&self, on: bool) {
        self.state.lock().unwrap().clips_enabled = on;
    }

    pub fn set_tracked_seconds(&self, secs: i64) {
        self.state.lock().unwrap().tracked_seconds = secs;
    }

    pub fn set_screenshot_count(&self, n: i64) {
        self.state.lock().unwrap().screenshot_count = n;
    }

    pub fn set_confirm_tracked_override(&self, v: Option<i64>) {
        self.state.lock().unwrap().confirm_tracked_override = v;
    }

    /// Credit like the real server, with intervals of `interval_ms`.
    pub fn set_real_credit(&self, interval_ms: u64) {
        let mut s = self.state.lock().unwrap();
        s.rules = Some(CreditRules {
            interval_ms: interval_ms as i64,
            window_ms: interval_ms as i64 / 2,
        });
    }

    /// Every answer is delayed by this much (on top of one-shot delays).
    pub fn set_latency(&self, ms: u64) {
        self.state.lock().unwrap().latency_ms = ms;
    }

    /// Every request for the next `dur` fails this way, and the server
    /// sees none of them.
    pub fn go_offline(&self, dur: std::time::Duration, kind: OfflineKind) {
        self.state.lock().unwrap().offline_until = Some((tokio::time::Instant::now() + dur, kind));
    }

    pub fn credit_log(&self) -> Vec<CreditEntry> {
        self.state.lock().unwrap().credit_log.clone()
    }

    /// Confirmed captures, in the real model.
    pub fn confirmed_count(&self) -> usize {
        self.state.lock().unwrap().shots.values().filter(|s| s.confirmed).count()
    }

    fn plan_failure(&self, method: &str, path: &str, kind: FailureKind) {
        self.state.lock().unwrap().failures.push_back(Failure {
            method: method.to_string(),
            path: path.to_string(),
            kind,
        });
    }

    /// Answer the next `method path` with this status and body instead.
    pub fn fail_next(&self, method: &str, path: &str, status: u16, body: &str) {
        self.plan_failure(method, path, FailureKind::Status(status, body.to_string()));
    }

    /// Drop the connection on the next `method path` — a network error.
    pub fn drop_next(&self, method: &str, path: &str) {
        self.plan_failure(method, path, FailureKind::Drop);
    }

    /// Never answer the next `method path`.
    pub fn hang_next(&self, method: &str, path: &str) {
        self.plan_failure(method, path, FailureKind::Hang);
    }

    /// Answer the next `method path` normally, but only after `ms`.
    pub fn delay_next(&self, method: &str, path: &str, ms: u64) {
        self.plan_failure(method, path, FailureKind::Delay(ms));
    }

    /// Process the next `method path`, then drop the connection: the
    /// client sees a network error for work that was done.
    pub fn drop_after_next(&self, method: &str, path: &str) {
        self.plan_failure(method, path, FailureKind::DropAfter);
    }

    /// Run the server's clock `ms` ahead of the machine's.
    pub fn set_clock_skew(&self, ms: i64) {
        self.state.lock().unwrap().skew_ms = ms;
    }

    pub fn requests(&self) -> Vec<Req> {
        self.log.lock().unwrap().clone()
    }

    /// Requests with exactly this method and path (query excluded).
    pub fn count(&self, method: &str, path: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.method == method && r.path == path)
            .count()
    }

    /// Decide the response (and any delay) for a request, consuming a
    /// planned fault if one matches.
    fn plan(&self, req: &Req) -> (u64, Planned) {
        let fault = {
            let mut s = self.state.lock().unwrap();
            s.failures
                .iter()
                .position(|f| f.method == req.method && f.path == req.path)
                .map(|i| s.failures.remove(i).unwrap().kind)
        };
        let (latency, offline) = {
            let mut s = self.state.lock().unwrap();
            let offline = match s.offline_until {
                Some((until, kind)) if tokio::time::Instant::now() < until => Some(kind),
                Some(_) => {
                    s.offline_until = None;
                    None
                }
                None => None,
            };
            (s.latency_ms, offline)
        };
        match offline {
            Some(OfflineKind::Drop) => return (0, Planned::Drop),
            Some(OfflineKind::Hang) => return (0, Planned::Hang),
            None => {}
        }
        match fault {
            Some(FailureKind::Status(status, body)) => (latency, Planned::Respond(status, body)),
            Some(FailureKind::Drop) => (0, Planned::Drop),
            Some(FailureKind::Hang) => (0, Planned::Hang),
            Some(FailureKind::Delay(ms)) => {
                let (status, body) = self.respond(req);
                (latency + ms, Planned::Respond(status, body))
            }
            Some(FailureKind::DropAfter) => {
                let _ = self.respond(req);
                (latency, Planned::Drop)
            }
            None => {
                let (status, body) = self.respond(req);
                (latency, Planned::Respond(status, body))
            }
        }
    }

    fn respond(&self, req: &Req) -> (u16, String) {
        let mut s = self.state.lock().unwrap();
        let now_ms = current_unix_ms() + s.skew_ms;
        let next_expected = unix_ms_to_iso(now_ms + 60_000);
        let server_time = unix_ms_to_iso(now_ms);
        let status = s.status.clone();
        let session = "/api/sessions/tok";
        let path = req.path.as_str();
        let method = req.method.as_str();

        if method == "GET" && path == session {
            return (
                200,
                serde_json::json!({
                    "status": status,
                    "name": "Test session",
                    "trackedSeconds": s.tracked_seconds,
                    "screenshotCount": s.screenshot_count,
                    "startedAt": null,
                    "createdAt": server_time,
                    "totalActiveSeconds": 0,
                    "clipsEnabled": s.clips_enabled,
                    "frameIntervalMs": 10000,
                })
                .to_string(),
            );
        }
        if method == "GET" && path == format!("{session}/status") {
            return (
                200,
                serde_json::json!({ "status": status, "trackedSeconds": s.tracked_seconds })
                    .to_string(),
            );
        }
        if method == "POST" && path == format!("{session}/pause") {
            return match status.as_str() {
                // The real server returns paused for a pending session
                // without changing it.
                "pending" => (200, r#"{"status":"paused","totalActiveSeconds":0}"#.into()),
                "paused" => (200, r#"{"status":"paused","totalActiveSeconds":0}"#.into()),
                "active" => {
                    s.status = "paused".into();
                    (200, r#"{"status":"paused","totalActiveSeconds":0}"#.into())
                }
                other => (409, format!(r#"{{"error":"Session is {other}, cannot pause"}}"#)),
            };
        }
        if method == "POST" && path == format!("{session}/resume") {
            return match status.as_str() {
                "paused" => {
                    s.status = "active".into();
                    // Credit-mode: clear the streak so the first
                    // post-resume capture seeds a fresh anchor.
                    s.anchor_ms = None;
                    s.streak_count = 0;
                    let next_expected = match s.rules {
                        Some(r) => unix_ms_to_iso(now_ms + r.interval_ms),
                        None => next_expected,
                    };
                    (
                        200,
                        serde_json::json!({
                            "status": "active",
                            "nextExpectedAt": next_expected,
                            "serverTime": server_time,
                        })
                        .to_string(),
                    )
                }
                other => (409, format!(r#"{{"error":"Session is {other}, cannot resume"}}"#)),
            };
        }
        if method == "POST" && path == format!("{session}/stop") {
            return match status.as_str() {
                "active" | "paused" | "pending" => {
                    s.status = "stopped".into();
                    (
                        200,
                        serde_json::json!({
                            "status": "stopped",
                            "trackedSeconds": s.tracked_seconds,
                            "totalActiveSeconds": 0,
                        })
                        .to_string(),
                    )
                }
                other => (409, format!(r#"{{"error":"Session is {other}, cannot stop"}}"#)),
            };
        }
        if method == "PATCH" && path == format!("{session}/name") {
            return (200, r#"{"ok":true}"#.into());
        }
        if method == "GET" && path == format!("{session}/upload-url") {
            if status != "active" && status != "pending" {
                return (409, format!(r#"{{"error":"Session is {status}, cannot upload"}}"#));
            }
            let activating = status == "pending";
            if activating {
                s.status = "active".into();
            }
            let mut next_expected = next_expected;
            let mut adopted = false;
            let mut captured_ms = now_ms;
            if let Some(rules) = s.rules {
                // The real server's trust envelope and monotonicity.
                let claimed = query_param(&req.query, "capturedAt")
                    .and_then(|v| crate::capture_loop::parse_iso_to_unix_ms(&v));
                let Some(claimed) = claimed else {
                    return (400, r#"{"error":"credit_mode_requires_captured_at"}"#.into());
                };
                const ENVELOPE_MS: i64 = 5 * 60 * 1000;
                captured_ms = claimed;
                if claimed > now_ms + ENVELOPE_MS || claimed < now_ms - ENVELOPE_MS {
                    captured_ms = now_ms;
                    adopted = true;
                }
                if activating || s.started_at_ms.is_none() {
                    s.started_at_ms = Some(captured_ms.min(now_ms));
                }
                let started = s.started_at_ms.unwrap();
                if captured_ms < started - 2_000 {
                    return (400, r#"{"error":"captured_at_before_session_start"}"#.into());
                }
                if let Some(latest) = s.latest_confirmed_ms {
                    if captured_ms <= latest {
                        return (400, r#"{"error":"captured_at_not_monotonic"}"#.into());
                    }
                }
                next_expected = match s.anchor_ms {
                    None => unix_ms_to_iso(captured_ms + rules.interval_ms),
                    Some(a) => unix_ms_to_iso(a + (s.streak_count + 1) * rules.interval_ms),
                };
            }
            s.next_id += 1;
            let id = s.next_id;
            s.shots.insert(
                id,
                Shot {
                    captured_ms,
                    confirmed: false,
                },
            );
            return (
                200,
                serde_json::json!({
                    "uploadUrl": format!("{}/r2/{id}", self.base),
                    "r2Key": format!("screenshots/tok/{id}.jpg"),
                    "screenshotId": format!("shot-{id}"),
                    "minuteBucket": 0,
                    "nextExpectedAt": next_expected,
                    "serverTime": server_time,
                    "format": "jpeg",
                    "capturedAtAdopted": adopted,
                })
                .to_string(),
            );
        }
        if method == "PUT" && path.starts_with("/r2/") {
            return (200, String::new());
        }
        if method == "POST" && path == format!("{session}/screenshots") {
            if status != "active" && status != "pending" {
                return (409, format!(r#"{{"error":"Session is {status}, cannot confirm"}}"#));
            }
            if status == "pending" {
                s.status = "active".into();
            }
            let is_final = req.body.contains("\"final\":true");
            let mut next_expected = next_expected;
            if let Some(rules) = s.rules {
                let id = body_screenshot_id(&req.body);
                let Some(shot) = id.and_then(|id| s.shots.get(&id).cloned()) else {
                    return (404, r#"{"error":"Screenshot not found"}"#.into());
                };
                if shot.confirmed {
                    // Idempotent: already confirmed.
                    let next = match s.anchor_ms {
                        Some(a) => unix_ms_to_iso(a + (s.streak_count + 1) * rules.interval_ms),
                        None => next_expected,
                    };
                    return (
                        200,
                        serde_json::json!({
                            "confirmed": true,
                            "trackedSeconds": s.tracked_seconds,
                            "nextExpectedAt": next,
                            "serverTime": server_time,
                        })
                        .to_string(),
                    );
                }
                let cap = shot.captured_ms;
                let credit_per_capture = rules.interval_ms / 1000;
                let (credit, new_anchor, new_count, reset) = match s.anchor_ms {
                    // Seed.
                    None => (0, cap, 0, false),
                    Some(anchor) => {
                        let expected = anchor + (s.streak_count + 1) * rules.interval_ms;
                        if is_final {
                            // Elapsed since the last credited mark, clamped.
                            let last_mark = anchor + s.streak_count * rules.interval_ms;
                            let elapsed_s = (cap - last_mark).div_euclid(1000);
                            (elapsed_s.clamp(0, credit_per_capture), cap, 0, false)
                        } else if (cap - expected).abs() <= rules.window_ms {
                            (credit_per_capture, anchor, s.streak_count + 1, false)
                        } else {
                            (0, cap, 0, true)
                        }
                    }
                };
                s.anchor_ms = Some(new_anchor);
                s.streak_count = new_count;
                s.tracked_seconds += credit;
                s.latest_confirmed_ms = Some(cap);
                s.shots.get_mut(&id.unwrap()).unwrap().confirmed = true;
                s.credit_log.push(CreditEntry {
                    captured_ms: cap,
                    is_final,
                    credit_secs: credit,
                    reset,
                });
                next_expected = unix_ms_to_iso(new_anchor + (new_count + 1) * rules.interval_ms);
            } else {
                s.screenshot_count += 1;
                if is_final {
                    s.tracked_seconds += FINAL_CREDIT_SECS;
                } else if s.screenshot_count > 1 {
                    s.tracked_seconds += 60;
                }
            }
            let tracked = s.confirm_tracked_override.unwrap_or(s.tracked_seconds);
            return (
                200,
                serde_json::json!({
                    "confirmed": true,
                    "trackedSeconds": tracked,
                    "nextExpectedAt": next_expected,
                    "serverTime": server_time,
                })
                .to_string(),
            );
        }
        (404, r#"{"error":"Not found"}"#.into())
    }
}

/// A query-string value, percent-decoded enough for ISO timestamps.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        if k != key {
            return None;
        }
        Some(
            v.replace("%3A", ":")
                .replace("%3a", ":")
                .replace("%2B", "+")
                .replace("%2E", "."),
        )
    })
}

/// The numeric id out of a confirm body's `"screenshotId":"shot-<n>"`.
fn body_screenshot_id(body: &str) -> Option<u64> {
    let i = body.find("\"screenshotId\":\"shot-")? + "\"screenshotId\":\"shot-".len();
    let rest = &body[i..];
    let end = rest.find('"')?;
    rest[..end].parse().ok()
}
