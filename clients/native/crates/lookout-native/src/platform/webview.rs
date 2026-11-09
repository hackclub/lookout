//! A native webview (wry) hosted as a child of a GPUI window, for the
//! ProgramPanel sheet. Modelled on gpui-kit's `gpui-webview`.
//!
//! The Tauri app put the program's page in a sandboxed `<iframe>` and spoke
//! `postMessage` to it. Here the page is top-level in its own webview, so an
//! init script stands in for the parent window: `window.parent.postMessage`
//! is routed to wry's IPC, and every message is checked against the panel's
//! origin (the URL wry reports for the page that sent it) before it reaches
//! the app. The sandbox is replaced by policy: the webview may only navigate
//! within the panel's origin, and anything that would open a new window goes
//! to the system browser instead.
//!
//! Linux: X11 uses a native child view. Native Wayland uses WebKitGTK's
//! software renderer in an offscreen GTK widget, then composites snapshots
//! into GPUI and forwards input. GTK is initialised on first use and its
//! main loop is pumped from the GPUI executor while any webview is alive.
//!
//! Native child views draw above GPUI content, so the caller hides them
//! whenever GPUI needs to show something there. Wayland frames are painted
//! and clipped by GPUI like any other image.

use std::cell::Cell;

#[cfg(all(feature = "webview", target_os = "linux"))]
mod wayland;

use gpui_kit::gpui::{App, Bounds, Pixels, Window};

/// What the page did, delivered on the GPUI thread through a channel.
pub enum WebEvent {
    /// The document finished loading (the iframe's `load`).
    Loaded,
    /// A `window.parent.postMessage(data)` from a page on the panel's origin.
    Message(serde_json::Value),
    /// The page tried to open a window (`target=_blank`, `window.open`).
    OpenExternal(String),
    /// The main document failed to load (Linux reports this; elsewhere the
    /// load timeout catches it).
    Failed,
    /// A software-rendered frame from the native Wayland backend.
    #[cfg(all(feature = "webview", target_os = "linux"))]
    Frame(std::sync::Arc<gpui_kit::gpui::RenderImage>),
}

/// The `window.parent.postMessage` shim, installed before any page script
/// (main frame only: a top-level page's `parent` is itself).
const INIT_SCRIPT: &str = r#"(function () {
  if (window.top !== window) return;
  var send = function (data) {
    try { window.ipc.postMessage(JSON.stringify(data)); } catch (e) {}
  };
  var parent = Object.freeze({ postMessage: function (data) { send(data); } });
  try {
    Object.defineProperty(window, "parent", { get: function () { return parent; }, configurable: false });
  } catch (e) {
    try { window.parent = parent; } catch (_) {}
  }
})();"#;

fn origin_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    match u.origin() {
        o @ url::Origin::Tuple(..) => Some(o.ascii_serialization()),
        url::Origin::Opaque(_) => None,
    }
}

pub struct PanelWebView {
    #[cfg(feature = "webview")]
    view: wry::WebView,
    shown: Cell<bool>,
    placed: Cell<Option<Bounds<Pixels>>>,
    edit_revision: Cell<u64>,
    #[cfg(all(feature = "webview", target_os = "linux"))]
    scale: Cell<f32>,
    #[cfg(all(feature = "webview", target_os = "linux"))]
    offscreen: Option<wayland::Offscreen>,
}

#[cfg(all(feature = "webview", target_os = "linux"))]
static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Drop for PanelWebView {
    fn drop(&mut self) {
        #[cfg(all(feature = "webview", target_os = "linux"))]
        LIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Everything needed to build a [`PanelWebView`] without touching GPUI.
///
/// Building is split from [`PanelWebView::prepare`] because on Windows
/// WebView2 pumps the thread's message queue while it waits for its
/// controller, and that pump runs queued GPUI tasks. If the App is borrowed
/// at that moment (inside `render` or any `update`), the first task that
/// touches it panics with "RefCell already borrowed". So call
/// [`PendingWebView::build`] with no App borrow held: from a spawned task,
/// between `update` calls.
pub struct PendingWebView {
    #[cfg(feature = "webview")]
    url: String,
    #[cfg(feature = "webview")]
    origin: String,
    #[cfg(feature = "webview")]
    dark: bool,
    #[cfg(feature = "webview")]
    background: (u8, u8, u8),
    #[cfg(feature = "webview")]
    parent: Parent,
    #[cfg(all(feature = "webview", target_os = "linux"))]
    scale: f32,
}

impl PanelWebView {
    /// Gather what building a webview showing `url` (already carrying
    /// `lookout_theme`) as a child of `window` needs. Build it with
    /// [`PendingWebView::build`], then [`PanelWebView::place`] it.
    #[allow(unused_variables)]
    pub fn prepare(url: &str, dark: bool, background: (u8, u8, u8), window: &Window, cx: &mut App) -> anyhow::Result<PendingWebView> {
        #[cfg(not(feature = "webview"))]
        {
            anyhow::bail!("built without the `webview` feature")
        }
        #[cfg(feature = "webview")]
        {
            let origin = origin_of(url).ok_or_else(|| anyhow::anyhow!("panel URL has no origin"))?;
            #[cfg(target_os = "linux")]
            let parent = {
                let parent = linux::Parent::new(window)?;
                linux::ensure_gtk(cx, matches!(parent, linux::Parent::Wayland))?;
                parent
            };
            #[cfg(not(target_os = "linux"))]
            let parent = {
                use wry::raw_window_handle::HasWindowHandle;
                Parent(HasWindowHandle::window_handle(window).map_err(|e| anyhow::anyhow!("{e}"))?.as_raw())
            };
            Ok(PendingWebView {
                url: url.to_string(),
                origin,
                dark,
                background,
                parent,
                #[cfg(target_os = "linux")]
                scale: window.scale_factor(),
            })
        }
    }
}

#[cfg(all(feature = "webview", target_os = "linux"))]
type Parent = linux::Parent;

/// The GPUI window's native handle, held past the borrow of the `Window`.
#[cfg(all(feature = "webview", not(target_os = "linux")))]
struct Parent(wry::raw_window_handle::RawWindowHandle);

#[cfg(all(feature = "webview", not(target_os = "linux")))]
impl wry::raw_window_handle::HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<wry::raw_window_handle::WindowHandle<'_>, wry::raw_window_handle::HandleError> {
        // SAFETY: the GPUI window outlives the webview, which the panel (a
        // view inside that window) owns. If the window closes before the
        // build, the platform rejects the dead handle and the build fails.
        Ok(unsafe { wry::raw_window_handle::WindowHandle::borrow_raw(self.0) })
    }
}

impl PendingWebView {
    /// Build the webview, restricted to the page's origin. Must run with no
    /// App borrow held (see the type's docs). Starts at 1×1; `place` it.
    pub fn build(self) -> anyhow::Result<(PanelWebView, flume::Receiver<WebEvent>)> {
        #[cfg(not(feature = "webview"))]
        {
            anyhow::bail!("built without the `webview` feature")
        }
        #[cfg(feature = "webview")]
        {
            let Self { url, origin, dark, background, parent, .. } = self;
            let url = url.as_str();
            let (tx, rx) = flume::unbounded();

            let ipc_tx = tx.clone();
            let ipc_origin = origin.clone();
            let nav_origin = origin.clone();
            let win_tx = tx.clone();
            #[cfg(target_os = "linux")]
            let fail_tx = tx.clone();
            let load_tx = tx.clone();
            let load_origin = origin.clone();
            let builder = wry::WebViewBuilder::new()
                .with_url(url)
                .with_initialization_script(INIT_SCRIPT)
                .with_background_color((background.0, background.1, background.2, 255))
                // X11 rejects a zero-sized window (BadValue), so start at 1×1;
                // `place` sizes it before it is ever shown.
                .with_bounds(wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                    size: wry::dpi::PhysicalSize::new(1, 1).into(),
                })
                .with_visible(true)
                .with_focused(false)
                .with_devtools(cfg!(debug_assertions))
                .with_ipc_handler(move |req: wry::http::Request<String>| {
                    // The page that sent it, as the engine sees it — not
                    // anything the page could claim about itself.
                    let sender = origin_of(&req.uri().to_string());
                    if sender.as_deref() != Some(ipc_origin.as_str()) {
                        log::warn!("[panel] dropped a message from {:?}", sender);
                        return;
                    }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(req.body()) {
                        let _ = ipc_tx.send(WebEvent::Message(v));
                    }
                })
                .with_navigation_handler(move |to: String| {
                    let ok = to.starts_with("about:")
                        || to.starts_with("data:")
                        || to.starts_with("blob:")
                        || origin_of(&to).as_deref() == Some(nav_origin.as_str());
                    if !ok {
                        log::warn!("[panel] blocked navigation to {to}");
                    }
                    ok
                })
                .with_new_window_req_handler(move |to: String, _| {
                    if to.starts_with("http://") || to.starts_with("https://") {
                        let _ = win_tx.send(WebEvent::OpenExternal(to));
                    }
                    wry::NewWindowResponse::Deny
                })
                .with_on_page_load_handler(move |ev, at: String| {
                    // Only the panel's own document counts (WebKit also
                    // reports its blank initial page and error pages).
                    if let wry::PageLoadEvent::Finished = ev {
                        if origin_of(&at).as_deref() == Some(load_origin.as_str()) {
                            let _ = load_tx.send(WebEvent::Loaded);
                        }
                    }
                });

            #[cfg(target_os = "linux")]
            let builder = if matches!(parent, linux::Parent::Wayland) {
                builder.with_initialization_script(include_str!("webview/wayland_input.js"))
            } else {
                builder
            };

            #[cfg(target_os = "windows")]
            let builder = {
                use wry::WebViewBuilderExtWindows as _;
                builder.with_theme(if dark { wry::Theme::Dark } else { wry::Theme::Light })
            };

            #[cfg(target_os = "linux")]
            linux::set_theme(dark);
            #[cfg(not(target_os = "linux"))]
            let view = builder.build_as_child(&parent)?;
            #[cfg(target_os = "linux")]
            let (view, offscreen) = match &parent {
                linux::Parent::X11(parent) => (builder.build_as_child(parent)?, None),
                linux::Parent::Wayland => {
                    use wry::WebViewBuilderExtUnix as _;
                    let host = wayland::Offscreen::new();
                    let view = builder.build_gtk(&host.container)?;
                    host.attach(&view, tx.clone());
                    log::info!("[panel] using native Wayland software webview");
                    (view, Some(host))
                }
            };

            #[cfg(target_os = "linux")]
            {
                use webkit2gtk::WebViewExt as _;
                use wry::WebViewExtUnix as _;
                let fail_tx = fail_tx.clone();
                view.webview().connect_load_failed(move |_, _, uri, err| {
                    log::warn!("[panel] {uri} failed to load: {err}");
                    let _ = fail_tx.send(WebEvent::Failed);
                    false
                });
                LIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                linux::match_scale_factor(&view, self.scale);
            }
            let this = PanelWebView {
                view,
                shown: Cell::new(true),
                placed: Cell::new(None),
                edit_revision: Cell::new(0),
                #[cfg(target_os = "linux")]
                scale: Cell::new(self.scale),
                #[cfg(target_os = "linux")]
                offscreen,
            };
            Ok((this, rx))
        }
    }
}

impl PanelWebView {
    pub fn composited(&self) -> bool {
        #[cfg(all(feature = "webview", target_os = "linux"))]
        {
            return self.offscreen.is_some();
        }
        #[cfg(not(all(feature = "webview", target_os = "linux")))]
        {
            false
        }
    }

    #[allow(unused_variables)]
    pub fn pointer(
        &self,
        position: gpui_kit::gpui::Point<Pixels>,
        button: Option<gpui_kit::gpui::MouseButton>,
        down: Option<bool>,
        modifiers: gpui_kit::gpui::Modifiers,
    ) {
        #[cfg(all(feature = "webview", target_os = "linux"))]
        if let (Some(host), Some(bounds)) = (&self.offscreen, self.placed.get()) {
            host.pointer(&self.view, position, bounds.origin, button, down, modifiers);
        }
    }

    #[allow(unused_variables)]
    pub fn scroll(&self, event: &gpui_kit::gpui::ScrollEvent) {
        #[cfg(all(feature = "webview", target_os = "linux"))]
        if let (Some(host), Some(bounds)) = (&self.offscreen, self.placed.get()) {
            host.scroll(&self.view, event.position, bounds.origin, event.delta.pixel_delta(gpui_kit::gpui::px(40.0)), event.modifiers);
        }
    }

    #[allow(unused_variables)]
    pub fn key(&self, stroke: &gpui_kit::gpui::Keystroke, down: bool) {
        #[cfg(all(feature = "webview", target_os = "linux"))]
        if let Some(host) = &self.offscreen {
            host.key(&self.view, stroke, down);
        }
    }

    pub fn edit_revision(&self) -> u64 {
        self.edit_revision.get()
    }

    #[allow(unused_variables)]
    pub fn edit(&self, operation: &str, range: Option<std::ops::Range<usize>>, text: &str, selection: Option<std::ops::Range<usize>>) {
        if !self.composited() {
            return;
        }
        self.edit_revision.set(self.edit_revision.get() + 1);
        let script = format!(
            "window.__lookoutEdit?.({}, {}, {}, {}, {})",
            serde_json::json!(operation),
            serde_json::json!(range.map(|r| [r.start, r.end])),
            serde_json::json!(text),
            serde_json::json!(selection.map(|r| [r.start, r.end])),
            self.edit_revision.get()
        );
        #[cfg(feature = "webview")]
        if let Err(e) = self.view.evaluate_script(&script) {
            log::warn!("[panel] webview editing failed: {e}");
        }
    }

    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.placed.get()
    }

    /// Put the native view over `bounds` (window coordinates), or hide it.
    /// Native children update only on changes; Wayland requests snapshots at up to 30 fps.
    #[allow(unused_variables)]
    pub fn place(&self, bounds: Option<Bounds<Pixels>>, window: &mut Window) {
        let bounds = bounds.filter(|b| f32::from(b.size.width) >= 1.0 && f32::from(b.size.height) >= 1.0);
        #[cfg(feature = "webview")]
        {
            #[cfg(target_os = "linux")]
            if let Some(host) = &self.offscreen {
                host.place(bounds, window, &self.view);
                self.placed.set(bounds);
                return;
            }
            #[cfg(target_os = "linux")]
            if self.scale.replace(window.scale_factor()) != window.scale_factor() {
                linux::match_scale_factor(&self.view, window.scale_factor());
                self.placed.set(None);
            }
            match bounds {
                Some(b) => {
                    if self.placed.get() != Some(b) {
                        #[cfg(target_os = "linux")]
                        let rect = linux::device_bounds(b, window.scale_factor());
                        #[cfg(not(target_os = "linux"))]
                        let rect = wry::Rect {
                            position: wry::dpi::LogicalPosition::new(f64::from(b.origin.x), f64::from(b.origin.y)).into(),
                            size: wry::dpi::LogicalSize::new(f64::from(b.size.width), f64::from(b.size.height)).into(),
                        };
                        if let Err(e) = self.view.set_bounds(rect) {
                            log::warn!("[panel] webview bounds: {e}");
                        }
                        self.placed.set(Some(b));
                    }
                    if !self.shown.replace(true) {
                        let _ = self.view.set_visible(true);
                    }
                }
                None => {
                    if self.shown.replace(false) {
                        let _ = self.view.focus_parent();
                        let _ = self.view.set_visible(false);
                    }
                }
            }
        }
    }
}

#[cfg(all(feature = "webview", target_os = "linux"))]
mod linux {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use gpui_kit::gpui::{App, Bounds, Pixels, Window};
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    use wry::raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle};

    static PUMPING: AtomicBool = AtomicBool::new(false);

    /// Initialise GTK with the window's display backend once, and pump its main loop from GPUI
    /// while any webview is alive (plus a moment after, so teardown runs).
    pub fn ensure_gtk(cx: &mut App, wayland: bool) -> anyhow::Result<()> {
        if !gtk::is_initialized_main_thread() {
            // Native child views require X11. The offscreen backend instead
            // uses the session's Wayland connection without opening a toplevel.
            gtk::gdk::set_allowed_backends(if wayland { "wayland" } else { "x11" });
            gtk::init().map_err(|e| anyhow::anyhow!("GTK init failed: {e}"))?;
        }
        if PUMPING.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        cx.spawn(async move |cx| {
            let mut idle = 0;
            loop {
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
                if super::LIVE.load(Ordering::SeqCst) == 0 {
                    idle += 1;
                    if idle > 60 {
                        break;
                    }
                } else {
                    idle = 0;
                }
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
            PUMPING.store(false, Ordering::SeqCst);
        })
        .detach();
        Ok(())
    }

    /// WebKitGTK answers `prefers-color-scheme` from GTK's own preference.
    pub fn set_theme(dark: bool) {
        use gtk::prelude::GtkSettingsExt as _;
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_application_prefer_dark_theme(dark);
        }
    }

    pub enum Parent {
        X11(X11Parent),
        Wayland,
    }

    impl Parent {
        pub fn new(window: &Window) -> anyhow::Result<Self> {
            if matches!(HasWindowHandle::window_handle(window)?.as_raw(), RawWindowHandle::Wayland(_)) {
                Ok(Self::Wayland)
            } else {
                X11Parent::new(window).map(Self::X11)
            }
        }
    }

    /// GPUI's X11 window (reported through XCB) as the Xlib parent wry wants.
    pub struct X11Parent(XlibWindowHandle);

    impl X11Parent {
        pub fn new(window: &Window) -> anyhow::Result<Self> {
            match HasWindowHandle::window_handle(window).map_err(|e| anyhow::anyhow!("{e}"))?.as_raw() {
                RawWindowHandle::Xcb(h) => {
                    let mut x = XlibWindowHandle::new(h.window.get().into());
                    x.visual_id = h.visual_id.map_or(0, |id| id.get().into());
                    Ok(Self(x))
                }
                RawWindowHandle::Xlib(h) => Ok(Self(h)),
                _ => anyhow::bail!("an embedded webview needs an X11 window (this one is Wayland)"),
            }
        }
    }

    impl HasWindowHandle for X11Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // SAFETY: the GPUI window outlives the webview, which the panel
            // (a view inside that window) owns.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(self.0)) })
        }
    }

    /// wry scales bounds by GDK's integer factor, not GPUI's: pass device px.
    pub fn device_bounds(b: Bounds<Pixels>, scale: f32) -> wry::Rect {
        let d = |v: Pixels| (f32::from(v) * scale).round();
        wry::Rect {
            position: PhysicalPosition::new(d(b.origin.x), d(b.origin.y)).into(),
            size: PhysicalSize::new(d(b.size.width), d(b.size.height)).into(),
        }
    }

    /// Zoom so CSS pixels match GPUI's (fractional) scale.
    pub fn match_scale_factor(view: &wry::WebView, scale: f32) {
        use gtk::prelude::WidgetExt as _;
        use wry::WebViewExtUnix as _;
        let gdk = view.webview().scale_factor().max(1);
        let _ = view.zoom(f64::from(scale) / f64::from(gdk));
    }
}
