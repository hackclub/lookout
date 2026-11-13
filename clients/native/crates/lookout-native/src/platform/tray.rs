//! The recording indicator outside the window — the tray / menu-bar item.
//!
//! | OS | indicator | click |
//! |---|---|---|
//! | macOS | `NSStatusItem` with the clock text (13.5pt monospaced digits) and a pause glyph | toggles the tray popup |
//! | Windows | tray icon with tooltip | left-click toggles the tray popup |
//! | Linux | GNOME: the shell extension's red pill over D-Bus; elsewhere a StatusNotifierItem with title + menu | menu |
//!
//! Shown while a recording is open (any phase but load-error / finished),
//! hidden otherwise. Every OS callback only sends a [`TrayAction`] into a
//! channel that a GPUI task drains — nothing touches GPUI from a tray thread.

use std::sync::Arc;

use gpui_kit::gpui::{App, Bounds, Global, Pixels};
use parking_lot::Mutex;

#[derive(Clone, Debug)]
pub enum TrayAction {
    Pause,
    Resume,
    Stop,
    Open,
    /// The menu-bar item / tray icon was clicked (its screen rect, if known).
    Toggle(Option<Bounds<Pixels>>),
}

#[derive(Clone, Default, PartialEq)]
pub struct TrayState {
    pub visible: bool,
    pub text: String,
    pub paused: bool,
}

pub struct Tray {
    pub state: Arc<Mutex<TrayState>>,
    imp: imp::Imp,
}

impl Global for Tray {}

pub fn init(tx: flume::Sender<TrayAction>, rt: tokio::runtime::Handle, cx: &mut App) {
    let state = Arc::new(Mutex::new(TrayState::default()));
    let imp = imp::Imp::new(tx, state.clone(), rt, cx);
    cx.set_global(Tray { state, imp });
}

/// Push the indicator state (visibility from the recorder, text from the
/// engine's once-a-second ticker). A paused/recording flip updates now.
pub fn sync(visible: bool, text: &str, paused: bool, cx: &mut App) {
    let Some(tray) = cx.try_global::<Tray>() else { return };
    let next = TrayState { visible, text: text.to_string(), paused };
    {
        let mut cur = tray.state.lock();
        if *cur == next {
            return;
        }
        *cur = next.clone();
    }
    tray.imp.apply(&next);
}

/// The tooltip text the Tauri shell used.
pub fn tooltip(text: &str, paused: bool) -> String {
    if paused { format!("Lookout — paused at {text}") } else { format!("Lookout — {text} recorded") }
}

// ── Linux: GNOME pill (D-Bus) + StatusNotifierItem ─────────────────────

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    const BUS_NAME: &str = "com.hackclub.lookout.Indicator";
    const LEGACY_BUS_NAME: &str = "com.hackclub.Lookout";
    const OBJECT_PATH: &str = "/com/hackclub/Lookout/Indicator";
    const UUID: &str = "lookout-indicator@hackclub.com";
    const METADATA_JSON: &str = include_str!("../../../../assets/gnome-extension/metadata.json");
    const EXTENSION_JS: &str = include_str!("../../../../assets/gnome-extension/extension.js");
    const STYLESHEET_CSS: &str = include_str!("../../../../assets/gnome-extension/stylesheet.css");
    const PILL_ICON: &[u8] = include_bytes!("../../../../assets/gnome-extension/icons/lookout.png");

    static PILL_ATTACHED: AtomicBool = AtomicBool::new(false);

    pub struct Imp {
        state: Arc<Mutex<TrayState>>,
        rt: tokio::runtime::Handle,
        sni: Arc<tokio::sync::Mutex<Option<ksni::Handle<Sni>>>>,
        tx: flume::Sender<TrayAction>,
        pill_tx: tokio::sync::watch::Sender<TrayState>,
    }

    struct Sni {
        state: TrayState,
        tx: flume::Sender<TrayAction>,
        icon: ksni::Icon,
    }

    fn icon() -> ksni::Icon {
        let img = image::load_from_memory(crate::assets::TRAY_ICON_PNG).map(|i| i.into_rgba8()).unwrap_or_default();
        let (w, h) = img.dimensions();
        // ARGB32, network byte order.
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for p in img.pixels() {
            data.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
        }
        ksni::Icon { width: w as i32, height: h as i32, data }
    }

    impl ksni::Tray for Sni {
        fn id(&self) -> String {
            "timelapse_tray".into()
        }
        fn title(&self) -> String {
            if self.state.paused { format!("⏸ {}", self.state.text) } else { self.state.text.clone() }
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![self.icon.clone()]
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: tooltip(&self.state.text, self.state.paused), ..Default::default() }
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            let paused = self.state.paused;
            vec![
                StandardItem {
                    label: if paused { "Resume recording".into() } else { "Pause recording".into() },
                    activate: Box::new(move |t: &mut Self| {
                        let _ = t.tx.send(if paused { TrayAction::Resume } else { TrayAction::Pause });
                    }),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                StandardItem {
                    label: "Stop recording".into(),
                    activate: Box::new(|t: &mut Self| {
                        let _ = t.tx.send(TrayAction::Stop);
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Open Lookout".into(),
                    activate: Box::new(|t: &mut Self| {
                        let _ = t.tx.send(TrayAction::Open);
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    struct Indicator {
        state: TrayState,
        tx: flume::Sender<TrayAction>,
        on_attach: flume::Sender<bool>,
    }

    #[zbus::interface(name = "com.hackclub.Lookout.Indicator")]
    impl Indicator {
        fn get_state(&self) -> (bool, String, bool) {
            (self.state.visible, self.state.text.clone(), self.state.paused)
        }
        fn attach(&self) {
            PILL_ATTACHED.store(true, Ordering::Relaxed);
            let _ = self.on_attach.send(true);
        }
        fn detach(&self) {
            PILL_ATTACHED.store(false, Ordering::Relaxed);
            let _ = self.on_attach.send(false);
        }
        fn pause(&self) {
            let _ = self.tx.send(TrayAction::Pause);
        }
        fn resume(&self) {
            let _ = self.tx.send(TrayAction::Resume);
        }
        fn stop(&self) {
            let _ = self.tx.send(TrayAction::Stop);
        }
        fn open(&self) {
            let _ = self.tx.send(TrayAction::Open);
        }
        #[zbus(signal)]
        async fn state_changed(
            emitter: &zbus::object_server::SignalEmitter<'_>,
            active: bool,
            time: &str,
            paused: bool,
        ) -> zbus::Result<()>;
    }

    impl Imp {
        pub fn new(tx: flume::Sender<TrayAction>, state: Arc<Mutex<TrayState>>, rt: tokio::runtime::Handle, _cx: &mut App) -> Self {
            let (pill_tx, pill_rx) = tokio::sync::watch::channel(TrayState::default());
            let (attach_tx, attach_rx) = flume::unbounded::<bool>();
            let sni: Arc<tokio::sync::Mutex<Option<ksni::Handle<Sni>>>> = Arc::new(tokio::sync::Mutex::new(None));
            let me = Self { state: state.clone(), rt: rt.clone(), sni: sni.clone(), tx: tx.clone(), pill_tx };
            if is_gnome() {
                std::thread::spawn(ensure_extension_installed);
            }
            let tx2 = tx.clone();
            rt.spawn(async move {
                if let Err(e) = serve_pill(tx2, attach_tx, pill_rx).await {
                    log::info!("[gnome-indicator] service unavailable: {e}");
                }
            });
            // Pill attach/detach swaps which indicator shows.
            let (state2, sni2, tx3) = (state, sni, tx);
            rt.spawn(async move {
                while let Ok(_attached) = attach_rx.recv_async().await {
                    let s = state2.lock().clone();
                    refresh_sni(&sni2, &s, &tx3).await;
                }
            });
            me
        }

        pub fn apply(&self, s: &TrayState) {
            let _ = self.pill_tx.send(s.clone());
            let (sni, s, tx) = (self.sni.clone(), s.clone(), self.tx.clone());
            let _ = &self.state;
            self.rt.spawn(async move { refresh_sni(&sni, &s, &tx).await });
        }
    }

    async fn refresh_sni(sni: &tokio::sync::Mutex<Option<ksni::Handle<Sni>>>, s: &TrayState, tx: &flume::Sender<TrayAction>) {
        use ksni::TrayMethods;
        let mut guard = sni.lock().await;
        let want = s.visible && !PILL_ATTACHED.load(Ordering::Relaxed);
        match (want, guard.as_ref()) {
            (true, Some(h)) => {
                let s = s.clone();
                h.update(move |t| t.state = s).await;
            }
            (true, None) => {
                let tray = Sni { state: s.clone(), tx: tx.clone(), icon: icon() };
                match tray.spawn().await {
                    Ok(h) => *guard = Some(h),
                    Err(e) => log::info!("[tray] StatusNotifierItem unavailable: {e}"),
                }
            }
            (false, Some(_)) => {
                if let Some(h) = guard.take() {
                    h.shutdown().await;
                }
            }
            (false, None) => {}
        }
    }

    async fn serve_pill(
        tx: flume::Sender<TrayAction>,
        on_attach: flume::Sender<bool>,
        mut rx: tokio::sync::watch::Receiver<TrayState>,
    ) -> zbus::Result<()> {
        let connection = zbus::connection::Builder::session()?
            .name(BUS_NAME)?
            .serve_at(OBJECT_PATH, Indicator { state: TrayState::default(), tx, on_attach })?
            .build()
            .await?;
        let _ = connection.request_name(LEGACY_BUS_NAME).await;
        let iface = connection.object_server().interface::<_, Indicator>(OBJECT_PATH).await?;
        while rx.changed().await.is_ok() {
            let next = rx.borrow().clone();
            {
                let mut cur = iface.get_mut().await;
                if cur.state == next {
                    continue;
                }
                cur.state = next.clone();
            }
            let _ = Indicator::state_changed(iface.signal_emitter(), next.visible, &next.text, next.paused).await;
        }
        Ok(())
    }

    fn is_gnome() -> bool {
        std::env::var("XDG_CURRENT_DESKTOP")
            .map(|d| d.split(':').any(|x| x.eq_ignore_ascii_case("GNOME")))
            .unwrap_or(false)
    }

    /// Install (or refresh) the pill extension and enable it once.
    fn ensure_extension_installed() {
        let Some(data) = dirs::data_dir() else { return };
        let dir = data.join("gnome-shell/extensions").join(UUID);
        let current = std::fs::read_to_string(dir.join("metadata.json")).map(|m| m == METADATA_JSON).unwrap_or(false);
        if !current {
            let _ = std::fs::create_dir_all(dir.join("icons"));
            for (name, contents) in [("metadata.json", METADATA_JSON), ("extension.js", EXTENSION_JS), ("stylesheet.css", STYLESHEET_CSS)] {
                if let Err(e) = std::fs::write(dir.join(name), contents) {
                    log::warn!("[gnome-indicator] could not install the extension: {name}: {e}");
                    return;
                }
            }
            let _ = std::fs::write(dir.join("icons/lookout.png"), PILL_ICON);
            log::info!("[gnome-indicator] extension installed; the pill appears at next login");
        }
        let marker = dir.join(".lookout-enabled-once");
        if marker.exists() {
            return;
        }
        let out = std::process::Command::new("gsettings").args(["get", "org.gnome.shell", "enabled-extensions"]).output();
        if let Ok(out) = out {
            let raw = String::from_utf8_lossy(&out.stdout).to_string();
            if !raw.contains(UUID) {
                let mut list: Vec<String> = raw
                    .trim()
                    .trim_start_matches("@as")
                    .trim()
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|s| s.trim().trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                list.push(UUID.to_string());
                let value = format!("[{}]", list.iter().map(|u| format!("'{u}'")).collect::<Vec<_>>().join(", "));
                let _ = std::process::Command::new("gsettings").args(["set", "org.gnome.shell", "enabled-extensions", &value]).status();
            }
            let _ = std::fs::write(marker, "");
        }
    }
}

// ── Windows: tray-icon ──────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod imp {
    use super::*;
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent, MouseButton as TMouse, MouseButtonState};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, TranslateMessage, WM_APP,
        WM_USER,
    };

    /// Posted to the tray thread: apply the latest pending state.
    const WM_TRAY_APPLY: u32 = WM_APP + 0x4c;

    /// The icon lives on its own thread with its own message loop. Every
    /// tooltip change (once a second while recording) is a synchronous
    /// `Shell_NotifyIconW` round trip to Explorer, which blocks for seconds
    /// when Explorer isn't answering; on the main thread that froze the app
    /// (LOOKOUT-NATIVE-B hang reports). Updates coalesce: only the latest
    /// state is applied.
    pub struct Imp {
        thread_id: Option<u32>,
        pending: Arc<Mutex<Option<TrayState>>>,
    }

    impl Imp {
        pub fn new(tx: flume::Sender<TrayAction>, _state: Arc<Mutex<TrayState>>, _rt: tokio::runtime::Handle, _cx: &mut App) -> Self {
            TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
                if let TrayIconEvent::Click { button: TMouse::Left, button_state: MouseButtonState::Up, rect, .. } = ev {
                    let b = Bounds {
                        origin: gpui_kit::gpui::point(gpui_kit::gpui::px(rect.position.x as f32), gpui_kit::gpui::px(rect.position.y as f32)),
                        size: gpui_kit::gpui::size(gpui_kit::gpui::px(rect.size.width as f32), gpui_kit::gpui::px(rect.size.height as f32)),
                    };
                    let _ = tx.send(TrayAction::Toggle(Some(b)));
                }
            }));
            let pending: Arc<Mutex<Option<TrayState>>> = Arc::default();
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<u32>(1);
            let thread_pending = pending.clone();
            let spawned = std::thread::Builder::new().name("lookout-tray".into()).spawn(move || {
                // SAFETY: plain Win32 calls on this thread's own message queue.
                unsafe {
                    // Create the queue before anyone posts to it.
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
                    let _ = ready_tx.send(windows::Win32::System::Threading::GetCurrentThreadId());
                    let mut icon: Option<TrayIcon> = None;
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        if msg.hwnd.is_invalid() && msg.message == WM_TRAY_APPLY {
                            let next = thread_pending.lock().take();
                            if let Some(s) = next {
                                apply(&mut icon, &s);
                            }
                            continue;
                        }
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            });
            let thread_id = match spawned {
                Ok(_) => ready_rx.recv().ok(),
                Err(e) => {
                    log::warn!("[tray] couldn't start the tray thread: {e}");
                    None
                }
            };
            Self { thread_id, pending }
        }

        pub fn apply(&self, s: &TrayState) {
            let Some(thread_id) = self.thread_id else { return };
            *self.pending.lock() = Some(s.clone());
            // SAFETY: posts a private message to our own tray thread.
            if let Err(e) = unsafe { PostThreadMessageW(thread_id, WM_TRAY_APPLY, Default::default(), Default::default()) } {
                log::warn!("[tray] couldn't reach the tray thread: {e}");
            }
        }
    }

    /// Runs on the tray thread.
    fn apply(icon: &mut Option<TrayIcon>, s: &TrayState) {
        if !s.visible {
            *icon = None;
            return;
        }
        let tip = tooltip(&s.text, s.paused);
        match icon.as_ref() {
            Some(icon) => {
                let _ = icon.set_tooltip(Some(tip));
            }
            None => {
                let img = image::load_from_memory(crate::assets::APP_ICON_PNG).map(|i| i.into_rgba8()).ok();
                if let Some(img) = img {
                    let (w, h) = img.dimensions();
                    if let Ok(i) = Icon::from_rgba(img.into_raw(), w, h) {
                        *icon = TrayIconBuilder::new().with_icon(i).with_tooltip(tip).build().ok();
                    }
                }
            }
        }
    }
}

// ── macOS: the SwiftUI status item (swift/lookout-tray) ─────────────────

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::os::raw::c_char;
    use std::sync::OnceLock;

    unsafe extern "C" {
        fn lookout_tray_set_callback(cb: extern "C" fn(f64, f64, f64, f64));
        fn lookout_tray_show(text: *const c_char, icon: *const u8, icon_len: i32);
        fn lookout_tray_update(text: *const c_char, paused: i32);
        fn lookout_tray_hide();
    }

    static TX: OnceLock<flume::Sender<TrayAction>> = OnceLock::new();
    static ICON: &[u8] = include_bytes!("../../../../assets/tray-template.png");

    /// Clicks come back with the item's rect (logical points, top-left origin).
    extern "C" fn on_click(x: f64, y: f64, w: f64, h: f64) {
        if let Some(tx) = TX.get() {
            use gpui_kit::gpui::{point, px, size};
            let rect = Bounds { origin: point(px(x as f32), px(y as f32)), size: size(px(w as f32), px(h as f32)) };
            let _ = tx.send(TrayAction::Toggle(Some(rect)));
        }
    }

    pub struct Imp {
        shown: Mutex<Option<bool>>,
    }

    impl Imp {
        pub fn new(tx: flume::Sender<TrayAction>, _state: Arc<Mutex<TrayState>>, _rt: tokio::runtime::Handle, _cx: &mut App) -> Self {
            let _ = TX.set(tx);
            Self { shown: Mutex::new(None) }
        }

        /// SwiftUI rolls the digits (`.contentTransition(.numericText())`)
        /// inside `withAnimation(.spring(response: 0.4, dampingFraction: 0.9))`.
        pub fn apply(&self, s: &TrayState) {
            let Ok(text) = CString::new(s.text.clone()) else { return };
            let mut shown = self.shown.lock();
            match (s.visible, *shown) {
                (false, Some(_)) => {
                    unsafe { lookout_tray_hide() };
                    *shown = None;
                }
                (false, None) => {}
                (true, None) => {
                    unsafe {
                        lookout_tray_set_callback(on_click);
                        lookout_tray_show(text.as_ptr(), ICON.as_ptr(), ICON.len() as i32);
                        lookout_tray_update(text.as_ptr(), s.paused as i32);
                    }
                    *shown = Some(s.paused);
                }
                (true, Some(_)) => {
                    unsafe { lookout_tray_update(text.as_ptr(), s.paused as i32) };
                    *shown = Some(s.paused);
                }
            }
        }
    }
}
