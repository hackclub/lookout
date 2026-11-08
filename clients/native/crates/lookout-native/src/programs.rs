//! Programs — the registry (`GET /api/programs`) and device pairing for
//! "desktop instant start" (`programLink.ts`): an OAuth-device-grant-style
//! PKCE dance whose credential lives in the secret store and is only ever
//! presented to the origin the user consented to.

use std::time::Duration;

use base64::Engine;
use gpui_kit::gpui::{App, Entity, Global};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::app::{self, api_base, store, store_mut};
use crate::store::{PendingPairing, ProgramLink, secrets};
use crate::windows::main_window::MainWindow;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub new_session_url: String,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub pair_url: Option<String>,
    #[serde(default)]
    pub start_url: Option<String>,
}

impl Program {
    pub fn label(&self) -> String {
        self.display_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| self.name.clone())
    }
    pub fn linkable(&self) -> bool {
        acceptable_endpoint(self.pair_url.as_deref()) && acceptable_endpoint(self.start_url.as_deref())
    }
}

/// The last registry read, shared by the gallery's + menu, the add page and
/// settings.
#[derive(Default)]
pub struct Registry(pub Vec<Program>);
impl Global for Registry {}

pub fn registry(cx: &App) -> Vec<Program> {
    cx.try_global::<Registry>().map(|r| r.0.clone()).unwrap_or_default()
}

/// Fetch the registry (failure → empty list), store it, and return it.
pub async fn fetch(cx: &gpui_kit::gpui::AsyncApp, timeout: Option<Duration>) -> Result<Vec<Program>, String> {
    let (core, base) = cx.update(|cx| (app::state(cx).core.clone(), api_base(cx)));
    let v = app::tokio_async(cx, async move { core.programs(&base, timeout).await }).await.map_err(|e| e.message)?;
    let list: Vec<Program> = v.get("programs").and_then(|p| serde_json::from_value(p.clone()).ok()).unwrap_or_default();
    let list2 = list.clone();
    // Warm the native add menu's icon cache (macOS) the way the web app did.
    crate::platform::mac_menu::prefetch(&list.iter().filter_map(|p| p.icon_url.clone()).collect::<Vec<_>>());
    cx.update(|cx| cx.set_global(Registry(list2)));
    Ok(list)
}

pub fn acceptable_endpoint(raw: Option<&str>) -> bool {
    let Some(raw) = raw else { return false };
    let Ok(url) = url::Url::parse(raw) else { return false };
    match url.scheme() {
        "https" => true,
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1")),
        _ => false,
    }
}

fn origin_of(raw: &str) -> Option<String> {
    url::Url::parse(raw).ok().map(|u| u.origin().ascii_serialization())
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rng().fill_bytes(&mut v);
    v
}

pub fn device_label() -> String {
    let os = if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    };
    format!("Lookout Desktop ({os})")
}

const PENDING_TTL_MS: i64 = 10 * 60_000;

/// `beginPairing`: open the program's consent page with a PKCE challenge.
pub fn begin_pairing(p: &Program, cx: &mut App) -> Result<(), String> {
    if !p.linkable() {
        return Err("program does not support pairing".into());
    }
    let pair_url = p.pair_url.clone().unwrap();
    let verifier = b64url(&random(32));
    let state = b64url(&random(16));
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    store_mut(cx).pending_pairing = Some(PendingPairing {
        program: p.name.clone(),
        state: state.clone(),
        verifier,
        origin: origin_of(&pair_url).unwrap_or_default(),
        pair_url: pair_url.clone(),
        start_url: p.start_url.clone().unwrap_or_default(),
        created_at: chrono::Utc::now().timestamp_millis(),
        display_name: p.display_name.clone(),
        icon_url: p.icon_url.clone(),
    });
    store(cx).save();
    let mut url = url::Url::parse(&pair_url).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("challenge", &challenge)
        .append_pair("state", &state)
        .append_pair("device", &device_label());
    log::info!("[pair] opening consent page for {}", p.name);
    cx.open_url(url.as_str());
    Ok(())
}

/// What became of `openProgram`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// A session token is in hand and the app navigated to it.
    Started,
    /// The browser took over (new-session page or consent page).
    Browser,
    Failed,
}

/// `openProgram`: instant start when linked, pairing when linkable,
/// otherwise the program's own new-session page in the browser.
pub fn open_program(p: Program, main: Entity<MainWindow>, cx: &mut App) -> gpui_kit::gpui::Task<OpenOutcome> {
    let linked = store(cx).program_links.contains_key(&p.name);
    if p.linkable() && linked {
        return start_via_link(p, main, cx);
    }
    if p.linkable() {
        let r = begin_pairing(&p, cx);
        return gpui_kit::gpui::Task::ready(if r.is_ok() { OpenOutcome::Browser } else { OpenOutcome::Failed });
    }
    if acceptable_endpoint(Some(&p.new_session_url)) {
        cx.open_url(&p.new_session_url);
        gpui_kit::gpui::Task::ready(OpenOutcome::Browser)
    } else {
        gpui_kit::gpui::Task::ready(OpenOutcome::Failed)
    }
}

enum StartFailure {
    Unauthorized,
    OriginChanged,
    Unavailable(String),
}

fn start_via_link(p: Program, main: Entity<MainWindow>, cx: &mut App) -> gpui_kit::gpui::Task<OpenOutcome> {
    let label = p.label();
    main.update(cx, |mw, cx| mw.set_starting(Some(label), cx));
    let link = store(cx).program_links.get(&p.name).cloned();
    let http = app::state(cx).http.clone();
    let base = api_base(cx);
    let program = p.clone();
    let fut = app::tokio(cx, async move {
        let link = link.ok_or(StartFailure::Unavailable("not linked".into()))?;
        let start_url = program.start_url.clone().unwrap_or_default();
        if !acceptable_endpoint(Some(&start_url)) || origin_of(&start_url).as_deref() != Some(link.origin.as_str()) {
            return Err(StartFailure::OriginChanged);
        }
        let token = secrets::get(&secrets::program_key(&program.name)).ok_or(StartFailure::Unauthorized)?;
        let res = http
            .post(&start_url)
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| StartFailure::Unavailable(format!("start request failed: {e}")))?;
        if res.status() == 401 || res.status() == 403 {
            return Err(StartFailure::Unauthorized);
        }
        if !res.status().is_success() {
            return Err(StartFailure::Unavailable(format!("start failed: HTTP {}", res.status())));
        }
        let data: serde_json::Value = res.json().await.unwrap_or_default();
        let session_token = data.get("sessionToken").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if !crate::deeplink::is_valid_token(&session_token) {
            return Err(StartFailure::Unavailable("start returned no valid sessionToken".into()));
        }
        // Verify the session belongs to this program and is recordable.
        let check = http
            .get(format!("{base}/api/sessions/{session_token}"))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| StartFailure::Unavailable(format!("session verification failed: {e}")))?;
        if !check.status().is_success() {
            return Err(StartFailure::Unavailable(format!("session verification failed: HTTP {}", check.status())));
        }
        let session: serde_json::Value = check.json().await.unwrap_or_default();
        if session.get("program").and_then(|p| p.as_str()) != Some(program.name.as_str()) {
            return Err(StartFailure::Unavailable("session belongs to another program".into()));
        }
        let status = session.get("status").and_then(|s| s.as_str()).unwrap_or("");
        if !matches!(status, "pending" | "active" | "paused") {
            return Err(StartFailure::Unavailable(format!("session is not recordable ({status})")));
        }
        Ok(session_token)
    });
    cx.spawn(async move |cx| {
        let res = fut.await;
        let outcome = match res {
            Ok(token) => {
                main.update(cx, |mw, cx| {
                    mw.set_starting(None, cx);
                    mw.handle_deep_links(vec![format!("lookout://session/?token={token}")], cx);
                });
                OpenOutcome::Started
            }
            Err(StartFailure::Unauthorized) | Err(StartFailure::OriginChanged) => {
                log::warn!("[pair] {} credential rejected or moved — re-pairing", p.name);
                let outcome = cx.update(|cx| {
                    unlink_local(&p.name, cx);
                    main.update(cx, |mw, cx| mw.set_starting(None, cx));
                    if begin_pairing(&p, cx).is_ok() { OpenOutcome::Browser } else { OpenOutcome::Failed }
                });
                outcome
            }
            Err(StartFailure::Unavailable(msg)) => {
                log::warn!("[pair] instant start for {} unavailable: {msg}", p.name);
                cx.update(|cx| {
                    main.update(cx, |mw, cx| mw.set_starting(None, cx));
                    if acceptable_endpoint(Some(&p.new_session_url)) {
                        cx.open_url(&p.new_session_url);
                        OpenOutcome::Browser
                    } else {
                        OpenOutcome::Failed
                    }
                })
            }
        };
        outcome
    })
}

fn unlink_local(program: &str, cx: &mut App) {
    let _ = secrets::delete(&secrets::program_key(program));
    store_mut(cx).program_links.remove(program);
    store(cx).save();
}

/// `unlinkProgram`: revoke on the program (best effort), forget locally.
pub fn unlink(program: String, cx: &mut App) -> gpui_kit::gpui::Task<()> {
    let link = store(cx).program_links.get(&program).cloned();
    let http = app::state(cx).http.clone();
    let key = secrets::program_key(&program);
    let fut = app::tokio(cx, async move {
        if let (Some(link), Some(token)) = (link, secrets::get(&key)) {
            let _ = http.delete(&link.pair_url).bearer_auth(token).timeout(Duration::from_secs(10)).send().await;
        }
    });
    cx.spawn(async move |cx| {
        fut.await;
        cx.update(|cx| unlink_local(&program, cx));
    })
}

/// `completePairing` + `openProgram(linked)`, for a `lookout://pair?...`
/// callback. Failures show "Linking failed — you can retry from the + menu."
pub fn complete_pairing_and_open(url: String, main: Entity<MainWindow>, cx: &mut App) {
    let Some((code, state)) = crate::deeplink::parse_pair_callback(&url) else { return };
    let pending = store(cx).pending_pairing.clone();
    let Some(pending) = pending.filter(|p| p.state == state) else {
        log::warn!("[pair] callback matched no pending pairing — ignoring");
        return;
    };
    if chrono::Utc::now().timestamp_millis() - pending.created_at > PENDING_TTL_MS {
        store_mut(cx).pending_pairing = None;
        store(cx).save();
        return;
    }
    store_mut(cx).pending_pairing = None;
    store(cx).save();
    let http = app::state(cx).http.clone();
    let p2 = pending.clone();
    let fut = app::tokio(cx, async move {
        let res = http
            .post(&p2.pair_url)
            .json(&serde_json::json!({ "code": code, "verifier": p2.verifier }))
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("pairing exchange failed: HTTP {}", res.status()));
        }
        let data: serde_json::Value = res.json().await.unwrap_or_default();
        let device = data.get("deviceToken").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if device.is_empty() || device.len() > 4096 {
            return Err("pairing exchange returned no deviceToken".into());
        }
        secrets::set(&secrets::program_key(&p2.program), &device)?;
        Ok(())
    });
    cx.spawn(async move |cx| {
        match fut.await {
            Ok(()) => {
                cx.update(|cx| {
                    store_mut(cx).program_links.insert(
                        pending.program.clone(),
                        ProgramLink {
                            program: pending.program.clone(),
                            origin: pending.origin.clone(),
                            pair_url: pending.pair_url.clone(),
                            start_url: pending.start_url.clone(),
                            paired_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                            device_label: device_label(),
                            display_name: pending.display_name.clone(),
                            icon_url: pending.icon_url.clone(),
                        },
                    );
                    store(cx).save();
                });
                log::info!("[pair] linked {}", pending.program);
                let mut programs = cx.update(|cx| registry(cx));
                if !programs.iter().any(|p| p.name == pending.program) {
                    programs = fetch(cx, None).await.unwrap_or_default();
                }
                if let Some(p) = programs.into_iter().find(|p| p.name == pending.program) {
                    let task = cx.update(|cx| open_program(p, main.clone(), cx));
                    let _ = task.await;
                }
            }
            Err(e) => {
                log::error!("[pair] pairing failed: {e}");
                cx.update(|cx| {
                    main.update(cx, |mw, cx| {
                        mw.alert("Lookout", "Linking failed — you can retry from the + menu.", cx);
                    })
                });
            }
        }
    })
    .detach();
}
