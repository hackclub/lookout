//! App-wide state and the bridge to the recording engine.
//!
//! The engine (`lookout_core::Core`) runs on a tokio runtime and reports
//! through the [`Frontend`] trait from arbitrary threads. Following Cap's
//! rule, the frontend only ever *sends into a channel*; a GPUI foreground
//! task drains it and updates entities, so nothing re-enters GPUI from a
//! foreign thread or an OS callback.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::gpui::{App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, RenderImage};
use lookout_core::{
    CameraFrame, ControlMode, Core, CoreEvent, Frontend, RecorderPhase, RecorderState,
};

use crate::store::Persisted;

pub const CLIENT_NAME: &str = "Lookout Desktop";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Messages from engine threads to the UI thread.
pub enum CoreMsg {
    Event(CoreEvent),
    TrayTitle { text: String, paused: bool },
}

pub struct GpuiFrontend {
    tx: flume::Sender<CoreMsg>,
    /// "Is the main window focused" — the Tauri shell's answer too.
    pub wants_preview: AtomicBool,
    pub camera: Arc<crate::platform::camera::Camera>,
}

impl Frontend for GpuiFrontend {
    fn emit(&self, event: CoreEvent) {
        let _ = self.tx.send(CoreMsg::Event(event));
    }

    fn wants_preview_frames(&self) -> bool {
        self.wants_preview.load(Ordering::Relaxed)
    }

    fn set_tray_title(&self, time_text: &str, paused: bool) {
        let _ = self.tx.send(CoreMsg::TrayTitle { text: time_text.to_string(), paused });
    }

    fn camera_start(&self, device_id: &str) -> lookout_core::BoxFuture<'static, Result<(), String>> {
        let cam = self.camera.clone();
        let id = device_id.to_string();
        Box::pin(async move { cam.start(&id).await })
    }

    fn camera_grab_frame(&self, device_id: &str) -> lookout_core::BoxFuture<'static, Result<CameraFrame, String>> {
        let cam = self.camera.clone();
        let id = device_id.to_string();
        Box::pin(async move { cam.grab(&id).await })
    }

    fn camera_stop(&self) {
        self.camera.stop();
    }
}

/// Process-wide handles, available as a GPUI global.
pub struct AppState {
    pub core: Arc<Core>,
    pub rt: tokio::runtime::Handle,
    pub frontend: Arc<GpuiFrontend>,
    pub recorder: Entity<RecorderModel>,
    pub http: reqwest::Client,
}

impl Global for AppState {}

/// The persisted store as a global, so any view can read/update it.
pub struct Store(pub Persisted);
impl Global for Store {}

pub fn state(cx: &App) -> &AppState {
    cx.global::<AppState>()
}

pub fn store(cx: &App) -> &Persisted {
    &cx.global::<Store>().0
}

pub fn store_mut(cx: &mut App) -> &mut Persisted {
    &mut cx.global_mut::<Store>().0
}

pub fn api_base(cx: &App) -> String {
    store(cx).api_base()
}

/// Run `fut` on the tokio runtime and await it from GPUI.
pub fn tokio<F, T>(cx: &App, fut: F) -> impl Future<Output = T> + 'static
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = state(cx).rt.spawn(fut);
    async move {
        match handle.await {
            Ok(v) => v,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
    }
}

pub fn tokio_async<F, T>(cx: &AsyncApp, fut: F) -> impl Future<Output = T> + 'static
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let rt = cx.update(|cx| state(cx).rt.clone());
    let handle = rt.spawn(fut);
    async move {
        match handle.await {
            Ok(v) => v,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
    }
}

// ── The open recording, as the UI sees it ───────────────────────────────

/// Mirrors `useRecorderState()`: the latest `RecorderState` the core
/// published, the latest preview frame, and the tray clock text.
pub struct RecorderModel {
    pub state: Option<RecorderState>,
    pub preview: Option<std::sync::Arc<RenderImage>>,
    pub tray_text: String,
    pub tray_paused: bool,
}

#[derive(Clone, Debug)]
pub enum RecorderEvent {
    State,
    Preview,
    Tray,
    Notice { title: String, body: String },
    /// The compositor closed these portal casts (Linux): their nodes are dead.
    CastsRevoked(Vec<u32>),
}

impl EventEmitter<RecorderEvent> for RecorderModel {}

impl RecorderModel {
    pub fn new() -> Self {
        Self { state: None, preview: None, tray_text: "00:00".into(), tray_paused: false }
    }

    /// Whether the menu-bar / tray indicator should be visible.
    pub fn tray_visible(&self) -> bool {
        self.state
            .as_ref()
            .map(|s| !matches!(s.phase, RecorderPhase::LoadError | RecorderPhase::Finished))
            .unwrap_or(false)
    }

    pub fn paused(&self) -> bool {
        self.state.as_ref().map(|s| s.control_mode == ControlMode::Paused).unwrap_or(false)
    }

    /// Drop the preview frame (it shows a source that's no longer recorded).
    pub fn clear_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.preview.take() {
            retire_preview(p, cx);
        }
    }

    fn apply(&mut self, msg: CoreMsg, cx: &mut Context<Self>) {
        match msg {
            CoreMsg::Event(CoreEvent::RecorderState(s)) => {
                // Closed, or switched to another source: the frame is stale.
                // (A switch's flush can still send one of the old source.)
                let same_source = matches!((&s, &self.state), (Some(a), Some(b)) if a.source == b.source);
                if !same_source {
                    self.clear_preview(cx);
                }
                self.state = s;
                cx.emit(RecorderEvent::State);
                cx.notify();
            }
            CoreMsg::Event(CoreEvent::PreviewFrame(frame)) => {
                if let Some(img) = decode_preview(&frame.preview_base64) {
                    if let Some(p) = self.preview.replace(img) {
                        retire_preview(p, cx);
                    }
                    cx.emit(RecorderEvent::Preview);
                    cx.notify();
                }
            }
            CoreMsg::Event(CoreEvent::RecorderNotice(n)) => {
                cx.emit(RecorderEvent::Notice { title: n.title.clone(), body: n.body.clone() });
            }
            CoreMsg::Event(CoreEvent::ScreencastRevoked(r)) => {
                log::info!("[screencast] compositor revoked node(s) {:?}", r.node_ids);
                cx.emit(RecorderEvent::CastsRevoked(r.node_ids));
            }
            CoreMsg::Event(CoreEvent::Progress(p)) => log::debug!("[capture] {p}"),
            CoreMsg::Event(other) => log::debug!("[core] {}", other.name()),
            CoreMsg::TrayTitle { text, paused } => {
                self.tray_text = text;
                self.tray_paused = paused;
                cx.emit(RecorderEvent::Tray);
            }
        }
    }
}

/// Free a superseded preview frame: its pixels, and its texture in the
/// sprite atlas of every window that painted it (a `RenderImage` stays
/// uploaded until `drop_image`). Deferred so it also leaves the atlas of a
/// window that is mid-update right now. These frames used to pile up for
/// the whole session (a full-size frame each), until allocating the next
/// decode failed (LOOKOUT-NATIVE-1B).
fn retire_preview(image: std::sync::Arc<RenderImage>, cx: &mut Context<RecorderModel>) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

fn decode_preview(b64: &str) -> Option<std::sync::Arc<RenderImage>> {
    let bytes = lookout_core::base64_decode(b64).ok()?;
    crate::images::decode_to_render_image(&bytes).ok()
}

/// Build the engine and start draining its channel into [`RecorderModel`].
pub fn init(cx: &mut App, rt: tokio::runtime::Handle) {
    let (tx, rx) = flume::unbounded::<CoreMsg>();
    let frontend = Arc::new(GpuiFrontend {
        tx,
        wants_preview: AtomicBool::new(false),
        camera: Arc::new(crate::platform::camera::Camera::new()),
    });
    let core = {
        let _guard = rt.enter();
        Core::new(frontend.clone(), CLIENT_NAME, VERSION)
    };
    let recorder = cx.new(|_| RecorderModel::new());
    let http = reqwest::Client::builder()
        .user_agent(format!("{CLIENT_NAME}/{VERSION}"))
        .build()
        .expect("http client");
    cx.set_global(AppState { core: core.clone(), rt, frontend, recorder: recorder.clone(), http });

    // Push the persisted Filtered Apps list into the engine at launch.
    let apps = store(cx).blacklisted_apps.clone();
    let _ = core.set_blacklisted_apps(apps);

    cx.spawn(async move |cx| {
        while let Ok(msg) = rx.recv_async().await {
            recorder.update(cx, |m, cx| m.apply(msg, cx));
        }
    })
    .detach();
}
