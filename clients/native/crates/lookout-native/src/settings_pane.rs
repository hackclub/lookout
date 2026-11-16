//! The bridge behind Lookout's System Settings pane (`swift/lookout-prefpane`).
//!
//! The pane runs `Lookout.app/Contents/MacOS/lookout --settings-pane '<json>'`
//! and reads one JSON reply from stdout. That process never starts the app:
//!
//! - If Lookout is running, it relays the request over the single-instance
//!   socket (per user, in `$TMPDIR`), and the app applies it like its own
//!   Settings page would: the store, the engine's filter list, a server
//!   switch's reload, an unlink's revoke. Only that app writes its state.
//! - If not, it applies the request to the files itself.
//!
//! Requests: `{"op": "state"}`, `{"op": "apps"}`, `{"op": "setFiltered",
//! "apps": [...]}`, `{"op": "setBeta", "on": true}`, `{"op": "setServer",
//! "url": "..."}`, `{"op": "unlink", "program": "..."}`. Every reply has
//! `ok`; a failure carries `error` (a sentence to show), success carries
//! `state` (see [`state_json`]) or `apps`.
//!
//! Unlike `lookout://` links, which any web page can open and which only
//! navigate, this changes settings: it's reachable only by processes of the
//! same user, who could edit these files anyway.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

use crate::store::DEFAULT_API_BASE;

pub const ARG: &str = "--settings-pane";

/// A pane request relayed to the running app, with where to send the reply.
pub struct Request {
    pub req: Value,
    pub reply: flume::Sender<Value>,
}

fn channel() -> &'static (flume::Sender<Request>, flume::Receiver<Request>) {
    static CH: OnceLock<(flume::Sender<Request>, flume::Receiver<Request>)> = OnceLock::new();
    CH.get_or_init(flume::unbounded)
}

/// For the single-instance listener: hand a request to the app.
pub fn relay(req: Value, reply: flume::Sender<Value>) {
    let _ = channel().0.send(Request { req, reply });
}

fn error(msg: impl Into<String>) -> Value {
    json!({ "ok": false, "error": msg.into() })
}

fn op(req: &Value) -> &str {
    req.get("op").and_then(Value::as_str).unwrap_or("")
}

fn strings(v: Option<&Value>) -> Vec<String> {
    let mut out: Vec<String> = v
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// What the pane shows, from the persisted state (as JSON) plus the program
/// registry when the app has one.
pub fn state_json(persisted: &Value, api_base: &str, registry: &[crate::programs::Program], running: bool) -> Value {
    let filtered = strings(persisted.get("blacklistedApps"));
    let mut links: Vec<Value> = persisted
        .get("programLinks")
        .and_then(Value::as_object)
        .map(|m| {
            m.values()
                .map(|l| {
                    let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    let program = s("program");
                    let reg = registry.iter().find(|p| p.name == program);
                    let label = reg
                        .and_then(|p| p.display_name.clone())
                        .or_else(|| l.get("displayName").and_then(Value::as_str).map(String::from))
                        .unwrap_or_else(|| program.clone());
                    let icon = reg.and_then(|p| p.icon_url.clone()).or_else(|| l.get("iconUrl").and_then(Value::as_str).map(String::from));
                    let host = url::Url::parse(&s("pairUrl")).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
                    json!({
                        "program": program,
                        "label": label,
                        "iconUrl": icon,
                        "detail": format!("Linked {} · {host}", crate::fmt::numeric_date(&s("pairedAt"))),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    links.sort_by(|a, b| a["program"].as_str().cmp(&b["program"].as_str()));
    json!({
        "running": running,
        "version": crate::updater::installed_version(),
        "filtered": filtered,
        "links": links,
        "apiBase": api_base,
        "isDefault": api_base == DEFAULT_API_BASE,
        "beta": crate::updater::beta_enabled(),
        "betaSupported": crate::updater::supported(),
    })
}

fn apps_json(list: Vec<lookout_core::apps::AppEntry>) -> Value {
    let apps: Vec<Value> = list.into_iter().map(|a| json!({ "name": a.name, "path": a.path, "running": a.running })).collect();
    json!({ "ok": true, "apps": apps })
}

/// `normalizeServerUrl` + the error the Advanced page shows when a probe fails.
fn probe_error(message: &str) -> String {
    if message.to_lowercase().contains("timed out") {
        "Could not reach the server (timed out).".into()
    } else {
        format!("Could not reach the server: {message}")
    }
}

// ── The command-line side ───────────────────────────────────────────────

/// `lookout --settings-pane '<json>'`: answer on stdout and exit.
pub fn cli(arg: &str) -> ! {
    let req: Value = serde_json::from_str(arg).unwrap_or_else(|_| json!({}));
    let out = match forward(&req) {
        Some(reply) => reply,
        None => offline(&req),
    };
    println!("{out}");
    std::process::exit(0)
}

/// Relay to a running Lookout. `None` if there isn't one.
#[cfg(unix)]
fn forward(req: &Value) -> Option<Value> {
    use std::io::{BufRead, BufReader, Write};
    let mut s = std::os::unix::net::UnixStream::connect(crate::platform::single_instance::path()).ok()?;
    // A server probe takes up to 6s, an unlink's revoke up to 10s.
    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
    let sent = writeln!(s, "{}", json!({ "pane": req }));
    let mut line = String::new();
    let read = sent.ok().and_then(|_| BufReader::new(&s).read_line(&mut line).ok());
    Some(match read {
        Some(n) if n > 0 => serde_json::from_str(line.trim()).unwrap_or_else(|_| error("Lookout sent a reply this pane can't read.")),
        // An older Lookout that doesn't know the pane yet just hangs up.
        _ => error("Lookout didn't answer. Quit and reopen Lookout, then try again."),
    })
}

#[cfg(not(unix))]
fn forward(_: &Value) -> Option<Value> {
    None
}

/// The state file as JSON, keeping every key (also ones this build doesn't know).
fn read_file() -> Value {
    std::fs::read_to_string(crate::store::state_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn write_file(v: &Value) -> Result<(), String> {
    let raw = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    crate::store::write_atomic(&crate::store::state_path(), &raw, false).map_err(|e| format!("Couldn't save: {e}"))
}

fn file_api_base(v: &Value) -> String {
    v.get("apiBase").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_API_BASE).to_string()
}

fn offline_state() -> Value {
    let v = read_file();
    json!({ "ok": true, "state": state_json(&v, &file_api_base(&v), &[], false) })
}

/// Apply a request with Lookout closed.
fn offline(req: &Value) -> Value {
    match op(req) {
        "state" => offline_state(),
        "apps" => apps_json(lookout_core::apps::list_installed_apps_blocking()),
        "setFiltered" => {
            let mut v = read_file();
            v["blacklistedApps"] = json!(strings(req.get("apps")));
            match write_file(&v) {
                Ok(()) => offline_state(),
                Err(e) => error(e),
            }
        }
        "setBeta" => {
            crate::updater::set_beta_saved(req.get("on").and_then(Value::as_bool).unwrap_or(false));
            offline_state()
        }
        "setServer" => {
            let Some(target) = req.get("url").and_then(Value::as_str).and_then(crate::screens::settings::normalize_server_url) else {
                return error("Enter a valid HTTPS URL (e.g. https://lookout-stage.example.com).");
            };
            if target == file_api_base(&read_file()) {
                return offline_state();
            }
            if let Err(e) = block_on(probe(target.clone())) {
                return error(probe_error(&e));
            }
            let mut v = read_file();
            v["apiBase"] = if target == DEFAULT_API_BASE { Value::Null } else { json!(target) };
            v["galleryCache"] = json!({});
            match write_file(&v) {
                Ok(()) => offline_state(),
                Err(e) => error(e),
            }
        }
        "unlink" => {
            let program = req.get("program").and_then(Value::as_str).unwrap_or("").to_string();
            let v = read_file();
            let pair_url = v["programLinks"][&program]["pairUrl"].as_str().map(String::from);
            let key = crate::store::secrets::program_key(&program);
            // Revoke on the program, best effort, as the app does.
            if let (Some(url), Some(token)) = (pair_url, crate::store::secrets::get(&key)) {
                let _ = block_on(async move {
                    reqwest::Client::new().delete(url).bearer_auth(token).timeout(Duration::from_secs(10)).send().await.map(|_| ())
                });
            }
            let _ = crate::store::secrets::delete(&key);
            let mut v = read_file();
            if let Some(m) = v.get_mut("programLinks").and_then(Value::as_object_mut) {
                m.remove(&program);
            }
            match write_file(&v) {
                Ok(()) => offline_state(),
                Err(e) => error(e),
            }
        }
        other => error(format!("Unknown request {other:?}.")),
    }
}

fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime").block_on(fut)
}

/// `GET /api/programs`, as the Advanced page checks a server before saving it.
async fn probe(base: String) -> Result<(), String> {
    let res = reqwest::Client::new()
        .get(format!("{base}/api/programs"))
        .timeout(Duration::from_millis(6000))
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "timed out".to_string() } else { e.to_string() })?;
    if res.status().is_success() { Ok(()) } else { Err(format!("HTTP {}", res.status().as_u16())) }
}

// ── The running app's side ──────────────────────────────────────────────

/// Serve relayed pane requests on the main thread, for the app's lifetime.
pub fn serve(cx: &mut gpui_kit::gpui::App) {
    let rx = channel().1.clone();
    cx.spawn(async move |cx| {
        while let Ok(r) = rx.recv_async().await {
            cx.update(|cx| handle(r, cx));
        }
    })
    .detach();
}

fn online_state(cx: &gpui_kit::gpui::App) -> Value {
    let st = crate::app::store(cx);
    let v = serde_json::to_value(st).unwrap_or_else(|_| json!({}));
    json!({ "ok": true, "state": state_json(&v, &st.api_base(), &crate::programs::registry(cx), true) })
}

fn main_window(cx: &gpui_kit::gpui::App) -> Option<gpui_kit::gpui::WindowHandle<crate::windows::main_window::MainWindow>> {
    cx.try_global::<crate::windows::MainHandle>().map(|h| h.0)
}

fn handle(r: Request, cx: &mut gpui_kit::gpui::App) {
    use crate::app::{self, store, store_mut};
    let Request { req, reply } = r;
    match op(&req) {
        "state" => {
            let _ = reply.send(online_state(cx));
        }
        "apps" => {
            let fut = app::tokio(cx, async {
                tokio::task::spawn_blocking(lookout_core::apps::list_installed_apps_blocking).await.unwrap_or_default()
            });
            cx.spawn(async move |_| {
                let _ = reply.send(apps_json(fut.await));
            })
            .detach();
        }
        "setFiltered" => {
            let list = strings(req.get("apps"));
            let st = store_mut(cx);
            st.blacklisted_apps = list.clone();
            st.save();
            let _ = app::state(cx).core.set_blacklisted_apps(list);
            cx.refresh_windows();
            let _ = reply.send(online_state(cx));
        }
        "setBeta" => {
            crate::updater::set_beta(req.get("on").and_then(Value::as_bool).unwrap_or(false), cx);
            cx.refresh_windows();
            let _ = reply.send(online_state(cx));
        }
        "setServer" => {
            let Some(target) = req.get("url").and_then(Value::as_str).and_then(crate::screens::settings::normalize_server_url) else {
                let _ = reply.send(error("Enter a valid HTTPS URL (e.g. https://lookout-stage.example.com)."));
                return;
            };
            if target == store(cx).api_base() {
                let _ = reply.send(online_state(cx));
                return;
            }
            let busy = main_window(cx).and_then(|h| h.read(cx).ok().map(|mw| mw.in_timelapse())).unwrap_or(false);
            if busy {
                let _ = reply.send(error("Close the timelapse that's open in Lookout first."));
                return;
            }
            let core = app::state(cx).core.clone();
            let t2 = target.clone();
            let fut = app::tokio(cx, async move { core.programs(&t2, Some(Duration::from_millis(6000))).await });
            cx.spawn(async move |cx| {
                let res = fut.await;
                cx.update(|cx| match res {
                    Ok(_) => {
                        let st = store_mut(cx);
                        st.api_base = if target == DEFAULT_API_BASE { None } else { Some(target) };
                        st.gallery_cache.clear();
                        st.save();
                        if let Some(h) = main_window(cx) {
                            let _ = h.update(cx, |mw, _, cx| mw.reload_for_server(cx));
                        }
                        let _ = reply.send(online_state(cx));
                    }
                    Err(e) => {
                        let _ = reply.send(error(probe_error(&e.message)));
                    }
                });
            })
            .detach();
        }
        "unlink" => {
            let program = req.get("program").and_then(Value::as_str).unwrap_or("").to_string();
            let task = crate::programs::unlink(program, cx);
            cx.spawn(async move |cx| {
                task.await;
                cx.update(|cx| {
                    cx.refresh_windows();
                    let _ = reply.send(online_state(cx));
                });
            })
            .detach();
        }
        other => {
            let _ = reply.send(error(format!("Unknown request {other:?}.")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_from_persisted() {
        let v = json!({
            "blacklistedApps": ["Slack", "Messages", "Slack"],
            "programLinks": {
                "lapse": {"program": "lapse", "pairUrl": "https://lapse.hackclub.com/api/pair", "pairedAt": "2026-11-12T10:00:00Z", "displayName": "Lapse"},
                "aaa": {"program": "aaa", "pairUrl": "nonsense", "pairedAt": ""}
            },
            "someFutureKey": 1
        });
        let s = state_json(&v, DEFAULT_API_BASE, &[], false);
        assert_eq!(s["filtered"], json!(["Messages", "Slack"]));
        assert_eq!(s["isDefault"], json!(true));
        assert_eq!(s["links"][0]["label"], json!("aaa"));
        assert_eq!(s["links"][1]["label"], json!("Lapse"));
        assert_eq!(s["links"][1]["detail"], json!("Linked 11/12/2026 · lapse.hackclub.com"));
        assert_eq!(s["running"], json!(false));
    }

    #[test]
    fn unknown_ops_fail_cleanly() {
        let r = offline(&json!({"op": "launchMissiles"}));
        assert_eq!(r["ok"], json!(false));
        let r = offline(&json!({"op": "setServer", "url": "http://example.com"}));
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("HTTPS"));
    }
}
