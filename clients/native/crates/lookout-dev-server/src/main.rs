//! `cargo run -p lookout-dev-server` → http://127.0.0.1:8787
//!
//! An in-memory stand-in for the Lookout API, so every screen of the native
//! client can be exercised offline: a handful of sessions in each status
//! with generated thumbnails, a program registry, an announcement and a
//! tip, and a recordable session that accepts uploads (each confirm credits
//! 60s). Run the app with `LOOKOUT_API_BASE=http://127.0.0.1:8787`.
//!
//! Seed tokens are printed at startup; `lookout://session/?token=<t>` deep
//! links (or the "Paste a lookout:// link" box) open them.
//!
//! `LOOKOUT_DEV_UPDATES=<dir>` also serves fake update releases at
//! `/updates/…` (see `lookout-update-tool fake-release`).

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tiny_http::{Header, Method, Response, Server};

#[derive(Clone)]
struct Session {
    token: String,
    name: String,
    status: String,
    tracked: i64,
    shots: i64,
    created_at: String,
    hue: u8,
    stopped_at: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (d, rem) = (secs / 86400, secs % 86400);
    // civil-from-days (Howard Hinnant)
    let z = d as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, (rem % 3600) / 60, rem % 60, ms % 1000)
}

/// Files under `LOOKOUT_DEV_UPDATES` at `/updates/…`, honouring `Range:
/// bytes=N-`. With `DEV_UPDATE_DROP=1` the first full download of each
/// large file stops halfway, to exercise the client's resume.
fn serve_update(rest: &[&str], range: Option<&str>) -> Response<Cursor<Vec<u8>>> {
    static DROPPED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let not_found = || Response::from_data(b"not found".to_vec()).with_status_code(404);
    let Ok(root) = std::env::var("LOOKOUT_DEV_UPDATES") else { return not_found() };
    if rest.iter().any(|p| p.is_empty() || *p == ".." || p.contains('\\')) {
        return not_found();
    }
    let path = rest.iter().fold(std::path::PathBuf::from(root), |p, s| p.join(s));
    let Ok(data) = std::fs::read(&path) else { return not_found() };
    let start = range.and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
    println!("  update {} ({} bytes){}", rest.join("/"), data.len(), start.map(|s| format!(" from {s}")).unwrap_or_default());
    match start {
        Some(s) if s >= data.len() => Response::from_data(Vec::new()).with_status_code(416),
        Some(s) => {
            let total = data.len();
            Response::from_data(data[s..].to_vec())
                .with_status_code(206)
                .with_header(Header::from_bytes("Content-Range", format!("bytes {s}-{}/{total}", total - 1)).unwrap())
        }
        None => {
            let key = rest.join("/");
            let mut body = data;
            if std::env::var("DEV_UPDATE_DROP").is_ok() && body.len() > 100_000 && !DROPPED.lock().unwrap().contains(&key) {
                DROPPED.lock().unwrap().push(key);
                body.truncate(body.len() / 2);
                println!("  (dropping after {} bytes)", body.len());
            }
            Response::from_data(body)
        }
    }
}

fn token(seed: u8) -> String {
    (0..32).map(|i| format!("{:02x}", (seed as u32 * 37 + i * 11) % 256)).collect()
}

fn thumb(hue: u8, w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let base = [
            (hue as f32 * 0.9 + fx * 80.0) as u8,
            (60.0 + fy * 120.0) as u8,
            (200.0 - fx * 100.0 + hue as f32 * 0.2) as u8,
        ];
        // a fake "window" in the middle
        let win = fx > 0.18 && fx < 0.82 && fy > 0.2 && fy < 0.85;
        *p = if win {
            let bar = fy < 0.27;
            if bar { image::Rgb([40, 40, 46]) } else { image::Rgb([24, 24, 28]) }
        } else {
            image::Rgb(base)
        };
        if win && fy > 0.33 && ((y / 6) % 3 == 0) && fx < 0.2 + (((y / 18) * 7 % 50) as f32 / 100.0) + 0.1 {
            *p = image::Rgb([120 + hue / 3, 200, 150]);
        }
    }
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

/// A ProgramPanel page speaking the `programPanel.ts` protocol.
const PANEL_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
body{margin:0;font:13px system-ui,sans-serif;background:transparent;color:#222}
body.dark{color:#eee}
main{padding:16px 20px;display:flex;flex-direction:column;gap:12px}
input{font:inherit;padding:8px 10px;border-radius:8px;border:1px solid #8886;background:transparent;color:inherit}
.row{display:flex;gap:8px}button{font:inherit;padding:8px 14px;border-radius:8px;border:0;cursor:pointer}
#done{background:#ec3750;color:#fff}#more{display:none;flex-direction:column;gap:8px}
</style></head><body><main>
<div><b>What did you build?</b><div style="opacity:.6">Theme: <span id="t"></span>. Embedded: <span id="e"></span></div></div>
<input placeholder="Project name" autofocus>
<label><input type="checkbox" id="x"> Show more fields</label>
<div id="more"><input placeholder="Repo URL"><input placeholder="Demo URL"><textarea rows="4"></textarea></div>
<div class="row"><button id="done">Done</button><button id="cancel">Cancel</button>
<a href="https://hackclub.com" target="_blank" style="align-self:center;color:inherit">Hack Club (new window)</a>
<a href="https://example.com" style="align-self:center;color:inherit">Navigate away</a></div>
</main><script>
var theme=new URLSearchParams(location.search).get("lookout_theme");
document.getElementById("t").textContent=theme;document.body.className=theme||"";
document.getElementById("e").textContent=String(window.parent!==window);
var post=function(m){window.parent.postMessage(m,"*")};
var size=function(){post({type:"lookout:resize",height:document.querySelector("main").getBoundingClientRect().height})};
new ResizeObserver(size).observe(document.querySelector("main"));
document.getElementById("x").onchange=function(e){document.getElementById("more").style.display=e.target.checked?"flex":"none"};
document.getElementById("done").onclick=function(){post({type:"lookout:done"})};
document.getElementById("cancel").onclick=function(){post({type:"lookout:cancel"})};
setTimeout(function(){size();post({type:"lookout:ready"})},300);
</script></body></html>"#;

fn main() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8787".into());
    let base = format!("http://127.0.0.1:{port}");
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let seed = [
        ("Building a ray tracer in Rust", "complete", 3 * 3600 + 25 * 60, 205, 10),
        ("Portfolio site redesign", "complete", 47 * 60, 47, 120),
        ("Compiling: game jam", "compiling", 2 * 3600, 120, 40),
        ("Pixel art sprites", "paused", 25 * 60, 25, 200),
        ("Fresh session", "pending", 0, 0, 160),
        ("Old failed attempt", "failed", 15 * 60, 15, 80),
    ];
    for (i, (name, status, tracked, shots, hue)) in seed.iter().enumerate() {
        let t = token(i as u8 + 1);
        sessions.insert(
            t.clone(),
            Session {
                token: t,
                name: name.to_string(),
                status: status.to_string(),
                tracked: *tracked,
                shots: *shots,
                created_at: format!("2026-11-0{}T1{}:30:00.000Z", 7 - i.min(5), i),
                hue: *hue,
                stopped_at: if *status == "compiling" { Some(now_ms()) } else { None },
            },
        );
    }
    println!("Lookout dev server on {base}");
    let mut list: Vec<_> = sessions.values().cloned().collect();
    list.sort_by(|a, b| a.created_at.cmp(&b.created_at).reverse());
    for s in &list {
        println!("  {:10} {}  lookout://session/?token={}", s.status, s.name, s.token);
    }
    let state = Arc::new(Mutex::new(sessions));
    let server = Server::http(format!("127.0.0.1:{port}")).expect("bind");
    // One thread per request: a slow or stalled request (a client holding a
    // keep-alive connection, a big video) mustn't hold up the rest. Seen on
    // the macOS CI runner, where parallel thumbnail fetches queued behind
    // each other until they timed out.
    for req in server.incoming_requests() {
        let state = state.clone();
        let port = port.clone();
        let base = base.clone();
        std::thread::spawn(move || {
            let mut req = req;
            let url = req.url().to_string();
            let path = url.split('?').next().unwrap_or("").to_string();
            let method = req.method().clone();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
            let json_resp = |v: Value, code: u16| {
                Response::from_string(v.to_string())
                    .with_status_code(code)
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
                    .with_header(Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap())
            };
            let mut st = state.lock().unwrap();
            // Compiling sessions finish after 20s.
            for s in st.values_mut() {
                if s.status == "compiling" || s.status == "stopped" {
                    if let Some(at) = s.stopped_at {
                        if now_ms() - at > 20_000 {
                            s.status = "complete".into();
                        } else if now_ms() - at > 3_000 {
                            s.status = "compiling".into();
                        }
                    }
                }
            }
            let summary = |s: &Session| {
                json!({
                    "token": s.token, "name": s.name, "status": s.status, "trackedSeconds": s.tracked,
                    "screenshotCount": s.shots, "startedAt": s.created_at, "createdAt": s.created_at,
                    "totalActiveSeconds": s.tracked,
                    "thumbnailUrl": if s.shots > 0 { Value::String(format!("{base}/media/thumb/{}.jpg", s.token)) } else { Value::Null },
                    "videoUrl": Value::Null, "metadata": {}
                })
            };
            let resp = match (method.clone(), parts.as_slice()) {
                (Method::Get, ["media", "thumb", file]) => {
                    let tok = file.trim_end_matches(".jpg");
                    let hue = st.get(tok).map(|s| s.hue).unwrap_or(100);
                    let _ = req.respond(Response::from_data(thumb(hue, 640, 360)).with_header(Header::from_bytes("Content-Type", "image/jpeg").unwrap()));
                    return;
                }
                // Fake update releases (`lookout-update-tool fake-release`).
                (Method::Get, ["updates", rest @ ..]) => {
                    let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
                    let _ = req.respond(serve_update(rest, range.as_deref()));
                    return;
                }
                (Method::Put, ["r2", ..]) => json_resp(json!({}), 200),
                (Method::Get, ["panel", _]) => {
                    let _ = req.respond(Response::from_string(PANEL_HTML).with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap()));
                    return;
                }
                (Method::Get, ["media", "video", _]) => {
                    let path = std::env::var("LOOKOUT_DEV_VIDEO").unwrap_or_else(|_| "/tmp/phoenix-media/test.webm".into());
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            let ct = if path.ends_with(".webm") { "video/webm" } else { "video/mp4" };
                            // Range requests, as R2 serves them (players stream and seek with these).
                            let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
                            let total = bytes.len();
                            let span = range.as_deref().and_then(|r| r.strip_prefix("bytes=")).and_then(|r| {
                                let (a, b) = r.split_once('-')?;
                                let start: usize = a.parse().ok()?;
                                let end: usize = if b.is_empty() { total - 1 } else { b.parse::<usize>().ok()?.min(total - 1) };
                                (start <= end).then_some((start, end))
                            });
                            let resp = match span {
                                Some((a, b)) => Response::from_data(bytes[a..=b].to_vec())
                                    .with_status_code(206)
                                    .with_header(Header::from_bytes("Content-Range", format!("bytes {a}-{b}/{total}")).unwrap()),
                                None => Response::from_data(bytes),
                            };
                            let _ = req.respond(
                                resp.with_header(Header::from_bytes("Content-Type", ct).unwrap())
                                    .with_header(Header::from_bytes("Accept-Ranges", "bytes").unwrap())
                                    // A real Content-Length (tiny-http would chunk it), as R2 sends.
                                    .with_chunked_threshold(usize::MAX),
                            );
                        }
                        Err(_) => { let _ = req.respond(json_resp(json!({ "error": "no video file" }), 404)); }
                    }
                    return;
                }
                (Method::Post, ["api", "sessions", "batch"]) => {
                    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    let toks: Vec<String> = v["tokens"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
                    let out: Vec<Value> = toks.iter().filter_map(|t| st.get(t)).map(summary).collect();
                    json_resp(json!({ "sessions": out }), 200)
                }
                (Method::Get, ["api", "programs"]) => json_resp(json!({ "programs": [
                    { "name": "fallout", "displayName": "Fallout", "newSessionUrl": "https://fallout.hackclub.com/new", "iconUrl": format!("{base}/media/thumb/icon.jpg"),
                      // Linkable, so a linked Fallout starts sessions instantly (see /dev-program/start).
                      "pairUrl": format!("http://localhost:{port}/dev-program/pair"), "startUrl": format!("http://localhost:{port}/dev-program/start") },
                    { "name": "summer", "displayName": "Summer of Making", "newSessionUrl": "https://summer.hackclub.com/new" }
                ]}), 200),
                // Instant start for a linked program: hands back the pending seed session.
                (Method::Post, ["dev-program", "start"]) => match st.values().find(|s| s.status == "pending") {
                    Some(s) => json_resp(json!({ "sessionToken": s.token }), 200),
                    None => json_resp(json!({ "error": "no pending session" }), 409),
                },
                (Method::Get, ["api", "announcement"]) => json_resp(if std::env::var("DEV_ANNOUNCE").is_ok() { json!({ "announcement": { "level": "info", "message": "Lookout servers will be down for maintenance Saturday 2am–4am UTC.", "url": "https://hackclub.com" } }) } else { json!({ "announcement": null }) }, 200),
                (Method::Get, ["api", "tip"]) => json_resp(if std::env::var("DEV_TIP").is_ok() { json!({ "tip": { "id": "tip-1", "badge": "NEW!", "title": "Sample tip title that runs about this long", "body": "Sample body copy, roughly the length a real tip runs to, so the sheet is the height it will actually be in front of a user.", "imageUrl": format!("{base}/media/thumb/tip.jpg"), "program": null, "trigger": "any", "requireUnlinked": false } }) } else { json!({ "tip": null }) }, 200),
                (_, ["api", "sessions", tok, rest @ ..]) => {
                    let tok = tok.to_string();
                    let Some(s) = st.get_mut(&tok) else {
                        let _ = req.respond(json_resp(json!({ "error": "Session not found" }), 404));
                        return;
                    };
                    match (method, rest) {
                        (Method::Get, []) => {
                            let mut v = summary(s);
                            v["clipsEnabled"] = json!(false);
                            v["program"] = json!("fallout");
                            v["cuts"] = json!([]);
                            json_resp(v, 200)
                        }
                        (Method::Get, ["status"]) => {
                            let held = s.name.starts_with("Portfolio") && std::env::var("DEV_EDIT").is_ok() && !s.status.eq("published");
                            if held {
                                json_resp(json!({ "status": "stopped", "trackedSeconds": s.tracked, "editable": true, "editHoldUntil": iso(now_ms() + 3_600_000) }), 200)
                            } else {
                                let mut v = json!({ "status": if s.status == "published" { "complete" } else { s.status.as_str() }, "trackedSeconds": s.tracked });
                                // DEV_PANEL=1: finished sessions ask for details in a ProgramPanel.
                                // Served on `localhost`, not 127.0.0.1, so it isn't "our own host".
                                if std::env::var("DEV_PANEL").is_ok() && matches!(s.status.as_str(), "complete" | "stopped" | "compiling") {
                                    // DEV_PANEL=dead points at a closed port, for the fallback.
                                    let p = if std::env::var("DEV_PANEL").as_deref() == Ok("dead") { "1" } else { port.as_str() };
                                    v["panelUrl"] = json!(format!("http://localhost:{p}/panel/{}", s.token));
                                }
                                json_resp(v, 200)
                            }
                        }
                        (Method::Post, ["pause"]) => { s.status = "paused".into(); json_resp(json!({ "status": "paused", "trackedSeconds": s.tracked, "totalActiveSeconds": s.tracked }), 200) }
                        (Method::Post, ["resume"]) => { s.status = "active".into(); json_resp(json!({ "status": "active" }), 200) }
                        (Method::Post, ["stop"]) => { s.status = "stopped".into(); s.stopped_at = Some(now_ms()); json_resp(json!({ "status": "stopped", "trackedSeconds": s.tracked, "totalActiveSeconds": s.tracked }), 200) }
                        (Method::Post, ["name"]) | (Method::Patch, ["name"]) | (Method::Put, ["name"]) => {
                            let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                            if let Some(n) = v["name"].as_str() { s.name = n.to_string(); }
                            json_resp(json!({ "name": s.name }), 200)
                        }
                        (Method::Get, ["upload-url"]) => {
                            if s.status == "pending" || s.status == "paused" { s.status = "active".into(); }
                            json_resp(json!({ "uploadUrl": format!("{base}/r2/{}", now_ms()), "r2Key": "k", "screenshotId": format!("{}", now_ms()), "minuteBucket": s.shots, "nextExpectedAt": iso(now_ms() + 60_000), "serverTime": iso(now_ms()), "trackingMode": "credit" }), 200)
                        }
                        (Method::Post, ["screenshots"]) => {
                            s.shots += 1; s.tracked += 60; s.status = "active".into();
                            json_resp(json!({ "confirmed": true, "trackedSeconds": s.tracked, "nextExpectedAt": iso(now_ms() + 60_000), "serverTime": iso(now_ms()) }), 200)
                        }
                        (Method::Get, ["video"]) => json_resp(json!({ "videoUrl": if std::env::var("LOOKOUT_DEV_VIDEO_HTTP").is_ok() {
                            // Stream over HTTP like a presigned R2 URL.
                            format!("{base}/media/video/timelapse.mp4")
                        } else {
                            format!("file://{}", std::env::var("LOOKOUT_DEV_VIDEO").unwrap_or_else(|_| "/tmp/phoenix-media/test.webm".into()))
                        } }), 200),
                        (Method::Get, ["units"]) => {
                            if s.name.starts_with("Portfolio") && std::env::var("DEV_EDIT").is_ok() {
                                let n: u64 = std::env::var("DEV_EDIT_UNITS").ok().and_then(|v| v.parse().ok()).unwrap_or(9);
                                let base_ms = now_ms() - (n + 38) * 60_000;
                                let units: Vec<Value> = (0..n).map(|i| {
                                    let gap = if i >= n * 2 / 3 { 20 * 60_000 } else { 0 };
                                    json!({ "capturedAt": iso(base_ms + i * 60_000 + gap), "screenshotId": format!("s{i}") })
                                }).collect();
                                let mut v = json!({ "units": units, "cuts": [], "editable": true, "originalVideoUrl": format!("file://{}", std::env::var("LOOKOUT_DEV_VIDEO").unwrap_or_else(|_| "/tmp/phoenix-media/test.webm".into())), "recompilesRemaining": 5, "expectedUnits": n });
                                // DEV_MASKS: act like a server with the masking API (PR #198).
                                if std::env::var("DEV_MASKS").is_ok() {
                                    v["masks"] = json!([]);
                                }
                                json_resp(v, 200)
                            } else {
                                json_resp(json!({ "units": [], "cuts": [], "editable": false, "editableReason": "published", "originalVideoUrl": null, "recompilesRemaining": 0 }), 200)
                            }
                        }
                        (Method::Post, ["editing"]) => json_resp(json!({ "held": s.name.starts_with("Portfolio") }), 200),
                        (Method::Post, ["cuts"]) | (Method::Put, ["cuts"]) => { println!("  cuts body: {body}"); json_resp(json!({ "cuts": [], "unitsTotal": 9, "unitsCut": 0, "trackedSeconds": s.tracked, "uncutTrackedSeconds": s.tracked }), 200) }
                        (Method::Post, ["compile"]) => { s.status = "published".into(); json_resp(json!({ "status": "complete", "instant": true, "recompilesRemaining": 4, "redirectUrl": null }), 200) }
                        _ => json_resp(json!({ "error": "not implemented in dev server" }), 404),
                    }
                }
                _ => json_resp(json!({ "error": "not found" }), 404),
            };
            drop(st);
            let _ = req.respond(resp);
        });
    }
}
