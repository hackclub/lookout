//! Auto-update ("update v2") for macOS, Windows and Linux AppImages.
//!
//! - Checks a minisign-signed `update.json` on GitHub Releases at launch and
//!   every 30 minutes (Stable: `releases/latest/download/`, Beta: the rolling
//!   `channel-beta` release). The signature is checked before parsing;
//!   older manifests are refused unless this version is blocked.
//! - Downloads in the background: a `zstd --patch-from` delta when this
//!   install has the right base, else the full artifact (resumable). Every
//!   result is checked against the manifest's SHA-256; a bad patch falls back
//!   to the full file.
//! - Installs without asking: on quit, or at the next launch before any
//!   window opens. "Restart to Complete Update" in the pill is the fast path.
//!   Blocked / below-minimum versions install as soon as nothing is being
//!   captured. Never during a capture.
//! - Rollback: the previous version is kept; the new one is healthy once the
//!   main window has been up for 20 s. Two launches without getting there
//!   and the next launch restores the previous version.
//!
//! The pure parts (manifest, rollout, patches, downloads, state) live in the
//! `lookout-update` crate; this module is the GPUI glue and `platform` the
//! installers. Linux deb/rpm/pacman installs never run any of this: they
//! update through their package manager (`services.rs`).
//!
//! Debug builds read overrides for local testing (`lookout-update-tool
//! fake-release`): `LOOKOUT_UPDATE_URL`, `LOOKOUT_UPDATE_BETA_URL`,
//! `LOOKOUT_UPDATE_PUBKEY`, `LOOKOUT_UPDATE_DIR`, `LOOKOUT_UPDATE_AS_VERSION`.
//! `LOOKOUT_UPDATE_DEMO=1` plays a fake download → ready in the pill.

pub mod platform;

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::gpui::{App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, Task, WeakEntity};
use lookout_update::manifest::{Channel, Decision, Install, Manifest};
use lookout_update::state::{self, Dirs, LaunchAction, Staged, State};
use lookout_update::{download, pipeline, sig, version};
use parking_lot::Mutex;

use crate::app;

pub const RELEASES_URL: &str = "https://github.com/hackclub/lookout/releases/latest";
const STABLE_BASE: &str = "https://github.com/hackclub/lookout/releases/latest/download/";
const BETA_BASE: &str = "https://github.com/hackclub/lookout/releases/download/channel-beta/";

const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const HELD_RETRY: Duration = Duration::from_secs(30);
/// How long the main window must be up before a new version counts as healthy.
const HEALTHY_AFTER: Duration = Duration::from_secs(20);

// ── configuration ───────────────────────────────────────────────────────

fn debug_env(name: &str) -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The version this install reports (debug builds may pretend).
pub fn installed_version() -> String {
    debug_env("LOOKOUT_UPDATE_AS_VERSION").unwrap_or_else(|| app::VERSION.to_string())
}

/// Whether this build updates itself: macOS, Windows, and Linux only when
/// running as an AppImage.
pub fn supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows")) || platform::appimage_path().is_some()
}

fn platform_key() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "native-darwin-aarch64"
    } else if cfg!(target_os = "macos") {
        "native-darwin-x86_64"
    } else if cfg!(target_os = "windows") {
        "native-windows-x86_64"
    } else {
        "native-linux-x86_64"
    }
}

fn dirs() -> Dirs {
    if let Some(d) = debug_env("LOOKOUT_UPDATE_DIR") {
        return Dirs::new(d);
    }
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    Dirs::new(base.join(crate::store::APP_ID).join("updates"))
}

fn manifest_base(channel: Channel) -> String {
    let (env, default) = match channel {
        Channel::Stable => ("LOOKOUT_UPDATE_URL", STABLE_BASE),
        Channel::Beta => ("LOOKOUT_UPDATE_BETA_URL", BETA_BASE),
    };
    let b = debug_env(env).unwrap_or_else(|| default.to_string());
    if b.ends_with('/') { b } else { format!("{b}/") }
}

fn pubkey() -> Result<sig::PublicKey, String> {
    sig::decode_pubkey(&debug_env("LOOKOUT_UPDATE_PUBKEY").unwrap_or_else(|| sig::RELEASE_PUBKEY.to_string()))
}

// ── state file ──────────────────────────────────────────────────────────

static STATE_LOCK: Mutex<()> = Mutex::new(());

/// Read-modify-write of `updates/state.json`.
fn with_state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let _g = STATE_LOCK.lock();
    let d = dirs();
    let mut st = State::load(&d);
    let before = st.clone();
    let out = f(&mut st);
    if st != before {
        if let Err(e) = st.save(&d) {
            log::error!("[updater] couldn't save state: {e}");
        }
    }
    out
}

pub fn beta_enabled() -> bool {
    with_state(|s| s.beta)
}

fn channel() -> Channel {
    if beta_enabled() { Channel::Beta } else { Channel::Stable }
}

/// The Settings toggle. Leaving Beta drops a staged beta build, so opting
/// out never installs one; the installed beta stays until Stable passes it.
pub fn set_beta(on: bool, cx: &mut App) {
    with_state(|s| {
        s.beta = on;
        if !on && s.staged.as_ref().is_some_and(|st| !version::parse(&st.version).pre.is_empty()) {
            s.staged = None;
        }
    });
    if let Some(u) = updater(cx) {
        u.update(cx, |u, cx| {
            if !on && matches!(u.phase, UpdatePhase::Ready { .. }) && with_state(|s| s.staged.is_none()) {
                u.phase = UpdatePhase::Idle;
                cx.notify();
            }
            u.recheck(cx);
        });
    }
}

/// Notes to show once, after an update landed: `(version, notes)`.
pub fn take_whats_new() -> Option<(String, String)> {
    if !supported() {
        return None;
    }
    let running = installed_version();
    with_state(|s| match &s.whats_new {
        Some(w) if version::eq(&w.version, &running) => s.whats_new.take().map(|w| (w.version, w.notes)),
        _ => None,
    })
}

/// The "What's new" sheet reuses the tip sheet (badge, title, body).
pub fn whats_new_tip(version: &str, notes: &str) -> crate::services::Tip {
    let mut body: String = notes.trim().chars().take(600).collect();
    if notes.trim().chars().count() > 600 {
        body.push('…');
    }
    crate::services::Tip {
        id: format!("whats-new-{version}"),
        badge: Some("What's new".into()),
        title: format!("Lookout is now on v{version}"),
        body,
        image_url: None,
        program: None,
        trigger: "any".into(),
        require_unlinked: false,
    }
}

// ── launch (before any window) ──────────────────────────────────────────

/// Called from `main` once this is the primary instance, before the UI:
/// rolls back a version that never became healthy, or installs a staged
/// update and relaunches. Returns normally when there's nothing to do.
pub fn boot(deep_links: &[String]) {
    if !supported() {
        return;
    }
    let running = installed_version();
    let d = dirs();
    let action = with_state(|s| state::on_launch(s, &running));
    match action {
        LaunchAction::Rollback(trial) => {
            log::warn!("[updater] v{} never became healthy — restoring v{}", trial.version, trial.from);
            match platform::rollback(&d, &trial) {
                Ok(()) => {
                    with_state(|s| s.record_rollback(&trial));
                    log::logger().flush();
                    std::process::exit(0);
                }
                Err(e) => {
                    log::error!("[updater] rollback failed: {e}");
                    // Don't try again every launch; stay on this version.
                    with_state(|s| s.trial = None);
                }
            }
        }
        // A launch carrying links would lose them across a relaunch: the
        // update waits for quit instead.
        LaunchAction::Apply(staged) if deep_links.is_empty() => {
            log::info!("[updater] installing staged v{} before startup", staged.version);
            match apply_staged(&staged, true, false) {
                Ok(()) => {
                    log::logger().flush();
                    std::process::exit(0);
                }
                Err(e) => log::warn!("[updater] staged install failed: {e}"),
            }
        }
        _ => {}
    }
    // Staged files nobody refers to any more.
    if with_state(|s| s.staged.is_none()) {
        let _ = std::fs::remove_dir_all(d.staged());
    }
}

/// Installs the staged update and records the trial. Failures count
/// toward [`state::MAX_APPLY_FAILURES`] (automatic installs only).
fn apply_staged(staged: &Staged, relaunch: bool, interactive: bool) -> Result<(), String> {
    let d = dirs();
    let running = installed_version();
    match platform::install(&d, staged, relaunch, interactive) {
        Ok(prev) => {
            with_state(|s| s.record_install(staged, &running, prev));
            log::info!("[updater] v{} installed", staged.version);
            Ok(())
        }
        Err(e) => {
            if !interactive {
                with_state(|s| {
                    if let Some(st) = s.staged.as_mut().filter(|st| st.version == staged.version) {
                        st.failures += 1;
                    }
                });
            }
            Err(e)
        }
    }
}

// ── the entity ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Downloading { version: String, progress: u8 },
    Ready { version: String },
    Installing { version: String },
}

impl UpdatePhase {
    pub fn version(&self) -> Option<&str> {
        match self {
            UpdatePhase::Idle => None,
            UpdatePhase::Downloading { version, .. } | UpdatePhase::Ready { version } | UpdatePhase::Installing { version } => Some(version),
        }
    }
}

/// The update failed to install; the main window shows the dialog.
#[derive(Clone, Debug)]
pub enum UpdaterEvent {
    Stuck { version: String, detail: Option<String> },
}

pub struct Updater {
    pub phase: UpdatePhase,
    busy: bool,
    demo: bool,
    _loop: Option<Task<()>>,
    held: Option<Task<()>>,
    demo_task: Option<Task<()>>,
}

impl EventEmitter<UpdaterEvent> for Updater {}

pub struct UpdaterHandle(pub Entity<Updater>);
impl Global for UpdaterHandle {}

pub fn updater(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<UpdaterHandle>().map(|h| h.0.clone())
}

/// Create the updater (once). Its work starts on the next turn of the
/// executor, so a caller can subscribe to its events first.
pub fn init(cx: &mut App) -> Entity<Updater> {
    if let Some(u) = updater(cx) {
        return u;
    }
    let u = cx.new(|cx| {
        let mut u = Updater { phase: UpdatePhase::Idle, busy: false, demo: false, _loop: None, held: None, demo_task: None };
        if debug_env("LOOKOUT_UPDATE_DEMO").is_some_and(|v| v != "0") {
            u.start_demo(cx);
        } else if supported() {
            u._loop = Some(cx.spawn(async move |this, cx| run(this, cx).await));
        }
        u
    });
    if supported() {
        // Install a staged update on the way out (never mid-capture: quitting
        // pauses the capture first, see `main_should_close`).
        cx.on_app_quit(|cx| {
            if !capture_active(cx) {
                if let Some(s) = with_state(|s| s.staged.clone()).filter(|s| s.failures < state::MAX_APPLY_FAILURES) {
                    if version::cmp(&s.version, &installed_version()).is_gt() || s.forced {
                        log::info!("[updater] installing v{} on quit", s.version);
                        if let Err(e) = apply_staged(&s, false, false) {
                            log::warn!("[updater] install on quit failed: {e}");
                        }
                    }
                }
            }
            async {}
        })
        .detach();
    }
    cx.set_global(UpdaterHandle(u.clone()));
    u
}

fn capture_active(cx: &App) -> bool {
    app::state(cx).recorder.read(cx).state.as_ref().map(|s| s.capturing).unwrap_or(false)
}

async fn run(this: WeakEntity<Updater>, cx: &mut AsyncApp) {
    let running = installed_version();
    // On probation: healthy once the main window has been up a while.
    if with_state(|s| s.trial.as_ref().is_some_and(|t| version::eq(&t.version, &running))) {
        let r = running.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(HEALTHY_AFTER).await;
            if with_state(|s| s.mark_healthy(&r)) {
                log::info!("[updater] v{r} is healthy");
            }
        })
        .detach();
    }
    // Something downloaded earlier and still waiting.
    let staged = with_state(|s| s.staged.clone());
    if let Some(s) = staged.filter(|s| version::cmp(&s.version, &running).is_gt() || s.forced) {
        this.update(cx, |u, cx| {
            u.phase = UpdatePhase::Ready { version: s.version.clone() };
            cx.notify();
            if s.failures >= state::MAX_APPLY_FAILURES && !s.notified {
                with_state(|st| {
                    if let Some(x) = st.staged.as_mut() {
                        x.notified = true;
                    }
                });
                cx.emit(UpdaterEvent::Stuck { version: s.version.clone(), detail: None });
            }
        })
        .ok();
    }
    loop {
        check(this.clone(), cx).await;
        cx.background_executor().timer(CHECK_INTERVAL).await;
        if this.upgrade().is_none() {
            break;
        }
    }
}

enum Msg {
    Found(String),
    Progress(u8),
    Done(Result<Option<(Staged, bool)>, String>),
}

async fn check(this: WeakEntity<Updater>, cx: &mut AsyncApp) {
    let go = this.update(cx, |u, _| {
        if u.busy || u.demo || matches!(u.phase, UpdatePhase::Installing { .. }) {
            return false;
        }
        u.busy = true;
        true
    });
    if !matches!(go, Ok(true)) {
        return;
    }
    let http = cx.update(|cx| app::state(cx).http.clone());
    let (tx, rx) = flume::unbounded::<Msg>();
    let job = app::tokio_async(cx, async move {
        let r = fetch_and_stage(http, tx.clone()).await;
        let _ = tx.send(Msg::Done(r));
    });
    let mut result = None;
    while let Ok(msg) = rx.recv_async().await {
        match msg {
            Msg::Found(version) => {
                this.update(cx, |u, cx| {
                    u.phase = UpdatePhase::Downloading { version, progress: 0 };
                    cx.notify();
                })
                .ok();
            }
            Msg::Progress(p) => {
                this.update(cx, |u, cx| {
                    if let UpdatePhase::Downloading { progress, .. } = &mut u.phase {
                        if *progress != p {
                            *progress = p;
                            cx.notify();
                        }
                    }
                })
                .ok();
            }
            Msg::Done(r) => result = Some(r),
        }
    }
    job.await;
    let _ = this.update(cx, |u, cx| {
        u.busy = false;
        match result {
            Some(Ok(Some((staged, forced)))) => {
                u.phase = UpdatePhase::Ready { version: staged.version.clone() };
                cx.notify();
                if forced {
                    u.install_when_safe(cx);
                }
            }
            Some(Ok(None)) | None => {
                if matches!(u.phase, UpdatePhase::Downloading { .. }) {
                    u.phase = UpdatePhase::Idle;
                    cx.notify();
                }
            }
            Some(Err(e)) => {
                log::warn!("[updater] update check failed: {e}");
                if matches!(u.phase, UpdatePhase::Downloading { .. }) {
                    u.phase = UpdatePhase::Idle;
                    cx.notify();
                }
            }
        }
    });
}

/// Fetch + verify the manifest, decide, download (patch or full), stage.
/// `Ok(None)`: nothing to install. `Ok(Some((staged, forced)))`: ready.
async fn fetch_and_stage(http: reqwest::Client, tx: flume::Sender<Msg>) -> Result<Option<(Staged, bool)>, String> {
    let channel = channel();
    let base = manifest_base(channel);
    let raw = download::fetch_small(&http, &format!("{base}update.json"), 4 << 20).await?;
    let sig_text = download::fetch_small(&http, &format!("{base}update.json.minisig"), 16 << 10).await?;
    let sig_text = String::from_utf8(sig_text).map_err(|e| e.to_string())?;
    let m = Manifest::verify_and_parse(&raw, &sig_text, &pubkey()?).map_err(|e| e.to_string())?;

    let running = installed_version();
    let (install_id, rolled_back) = with_state(|s| (s.install_id.clone(), s.rolled_back.clone()));
    let me = Install { version: &running, channel, install_id: &install_id, platform_key: platform_key(), rolled_back: &rolled_back };
    let (target, artifact, forced) = match lookout_update::decide(&m, &me).map_err(|e| e.to_string())? {
        Decision::Update { version, artifact, forced } => (version, artifact, forced),
        Decision::NotYet { bucket, rollout } => {
            log::info!("[updater] v{} is rolling out ({:.0}%), this install's turn comes at {:.0}%", m.version, rollout * 100.0, bucket * 100.0);
            return Ok(None);
        }
        d => {
            log::debug!("[updater] {d:?}");
            return Ok(None);
        }
    };
    if Some(artifact.format) != platform::expected_format() {
        return Err(format!("v{target} offers a {:?} build for this platform", artifact.format));
    }
    // Already downloaded?
    if let Some(s) = with_state(|s| s.staged.clone()).filter(|s| version::eq(&s.version, &target) && lookout_update::hash::same(&s.payload_sha256, &artifact.payload_sha256)) {
        if dirs().staged_payload().is_file() {
            return Ok(Some((s, forced)));
        }
    }
    log::info!("[updater] v{target} available{} — downloading in the background", if forced { " (required)" } else { "" });
    let _ = tx.send(Msg::Found(target.clone()));

    let d = dirs();
    let work = d.download();
    let base_payload = platform::patch_base(&d, &running);
    let mut last = 0u8;
    let tx2 = tx.clone();
    let got = pipeline::obtain_payload(&http, &artifact, &running, base_payload.as_deref(), &work, move |done, total| {
        let p = if total > 0 { ((done as f64 / total as f64) * 100.0).floor().min(100.0) as u8 } else { 0 };
        if p != last {
            last = p;
            let _ = tx2.send(Msg::Progress(p));
        }
    })
    .await?;
    log::info!("[updater] v{target} downloaded ({})", if got.via_patch { "patch" } else { "full" });

    let staged = Staged {
        version: target.clone(),
        format: artifact.format,
        payload_sha256: artifact.payload_sha256.clone(),
        notes: m.notes.clone(),
        forced,
        failures: 0,
        notified: false,
    };
    let (d2, payload, fmt) = (d.clone(), got.payload.clone(), artifact.format);
    tokio::task::spawn_blocking(move || platform::stage(&d2, &payload, fmt)).await.map_err(|e| e.to_string())??;
    let s2 = staged.clone();
    with_state(|s| s.staged = Some(s2));
    let _ = std::fs::remove_dir_all(&work);
    Ok(Some((staged, forced)))
}

impl Updater {
    /// Check now (the Beta toggle changed).
    pub fn recheck(&mut self, cx: &mut Context<Self>) {
        if self.demo || !supported() {
            return;
        }
        cx.spawn(async move |this, cx| check(this, cx).await).detach();
    }

    /// A required update: install as soon as nothing is being captured.
    fn install_when_safe(&mut self, cx: &mut Context<Self>) {
        if !capture_active(cx) {
            log::info!("[updater] installing a required update now");
            self.install(false, cx);
            return;
        }
        log::info!("[updater] required update held — capture in progress");
        self.held = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(HELD_RETRY).await;
            let done = this.update(cx, |u, cx| {
                if !matches!(u.phase, UpdatePhase::Ready { .. }) {
                    return true;
                }
                if capture_active(cx) {
                    return false;
                }
                u.install(false, cx);
                true
            });
            if !matches!(done, Ok(false)) {
                break;
            }
        }));
    }

    /// The pill was clicked (only clickable when Ready).
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            self.demo = false;
            self.demo_task = None;
            self.phase = UpdatePhase::Idle;
            cx.notify();
            return;
        }
        if matches!(self.phase, UpdatePhase::Ready { .. }) {
            if capture_active(cx) {
                return;
            }
            self.held = None;
            self.install(true, cx);
        }
    }

    /// Install + relaunch. `interactive`: the user asked, so an admin
    /// prompt is fine and a failure is explained.
    fn install(&mut self, interactive: bool, cx: &mut Context<Self>) {
        let Some(staged) = with_state(|s| s.staged.clone()) else { return };
        let version = staged.version.clone();
        self.phase = UpdatePhase::Installing { version: version.clone() };
        cx.notify();
        let task = app::tokio(cx, async move {
            tokio::task::spawn_blocking(move || apply_staged(&staged, true, interactive)).await.map_err(|e| e.to_string())?
        });
        cx.spawn(async move |this, cx| match task.await {
            Ok(()) => {
                log::info!("[updater] v{version} installed — restarting");
                log::logger().flush();
                std::process::exit(0);
            }
            Err(e) => {
                log::error!("[updater] install failed: {e}");
                this.update(cx, |u, cx| {
                    u.phase = UpdatePhase::Ready { version: version.clone() };
                    cx.notify();
                    let notify = interactive
                        || with_state(|s| {
                            s.staged.as_mut().is_some_and(|st| {
                                let first = st.failures >= state::MAX_APPLY_FAILURES && !st.notified;
                                if first {
                                    st.notified = true;
                                }
                                first
                            })
                        });
                    if notify {
                        cx.emit(UpdaterEvent::Stuck { version: version.clone(), detail: Some(e) });
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Dev: enter → +1..5% every 80ms → ready; a click fakes the restart.
    fn start_demo(&mut self, cx: &mut Context<Self>) {
        self.demo = true;
        self.phase = UpdatePhase::Downloading { version: "9.9.9".into(), progress: 0 };
        self.demo_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1200)).await;
            let mut p = 0.0f32;
            loop {
                cx.background_executor().timer(Duration::from_millis(80)).await;
                p += 1.0 + rand::random::<f32>() * 4.0;
                let done = p >= 100.0;
                let alive = this.update(cx, |u, cx| {
                    u.phase = if done {
                        UpdatePhase::Ready { version: "9.9.9".into() }
                    } else {
                        UpdatePhase::Downloading { version: "9.9.9".into(), progress: p.round() as u8 }
                    };
                    cx.notify();
                });
                if done || alive.is_err() {
                    break;
                }
            }
        }));
    }
}
