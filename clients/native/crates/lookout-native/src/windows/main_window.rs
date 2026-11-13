//! The main window — `MainWindowApp` from `App.tsx`.
//!
//! Owns the route stack and its transitions (framer-motion `AnimatePresence
//! mode="sync"`: the outgoing page slides 14px and fades out over 140ms
//! while the incoming one slides in on a 460/36/0.7 spring and fades in
//! over 160ms after 40ms), the per-platform chrome (macOS 32px drag strip,
//! Linux header bar inside a client-side frame, Windows native), and the
//! window-level overlays (add menu, context menu, dialogs, sheets, pills).

use std::rc::Rc;

use gpui_kit::gpui::{
    AnyElement, AnyView, App, AppContext, Bounds, Context, Decorations, Entity, FocusHandle,
    Focusable, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    NavigationDirection, ParentElement, Pixels,
    Point, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, Window, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowKind, WindowOptions,
    div, point, px, size,
};

use crate::app::{self, store, store_mut};
use crate::motion::{Anim, Curve, Ease, Spring};
use crate::screens::gallery::{GalleryEvent, GalleryPage};
use crate::screens::placeholder::Placeholder;
use crate::screens::add_session::{AddEvent, AddSessionPage};
use crate::screens::permission::{PermissionEvent, PermissionScreen};
use crate::screens::settings::{ConfirmAction, SettingsEvent, SettingsPage};
use crate::screens::record::{RecordEvent, RecordPage};
use crate::screens::session::{SessionEvent, SessionPage};
use crate::ui::overlay::{ModalSlot, render_modals};
use crate::ui::menu::{MenuEvent, MenuItem, MenuPopup};
use crate::ui::sheet::{Sheet, SheetEvent, SheetStyle};
use crate::screens::program_panel::{PanelEvent, ProgramPanel};
use crate::services::{self, LinuxUpdate, Tip, TipBody, TipHero, UpdateBody};
use crate::theme::{self, Theme, is_linux, is_mac, is_windows, theme};
use crate::ui::header_bar::{HeaderBar, HeaderBtn, HeaderBtnKind, HeaderChrome, HeaderNav, MAC_TRAFFIC_LIGHT_POSITION};
use crate::ui::icon::Icon;
use crate::ui::text::{TextExt, family};

/// Visible window size (the Tauri config's fixed 480×640).
pub const WIN_W: f32 = 480.0;
pub const WIN_H: f32 = 640.0;
/// Linux CSD: transparent margin around the visible window for its shadow.
pub const WINDOW_MARGIN: f32 = 40.0;
pub const WINDOW_RADIUS: f32 = 12.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Gallery,
    Add,
    Settings,
    Info,
    Record(String),
    Session(String),
    /// An editor window is open: the main window shows a placeholder.
    EditorOpen(String),
}

impl Route {
    pub fn key(&self) -> String {
        match self {
            Route::Gallery => "gallery:".into(),
            Route::Add => "add:".into(),
            Route::Settings => "settings:".into(),
            Route::Info => "info:".into(),
            Route::Record(t) => format!("record:{t}"),
            Route::Session(t) => format!("session:{t}"),
            Route::EditorOpen(t) => format!("editor-open:{t}"),
        }
    }
    /// Inverse of [`Route::key`].
    pub fn from_key(key: &str) -> Option<Route> {
        let (kind, token) = key.split_once(':')?;
        let token = token.to_string();
        Some(match kind {
            "gallery" => Route::Gallery,
            "add" => Route::Add,
            "settings" => Route::Settings,
            "info" => Route::Info,
            "record" if !token.is_empty() => Route::Record(token),
            "session" if !token.is_empty() => Route::Session(token),
            "editor-open" if !token.is_empty() => Route::EditorOpen(token),
            _ => return None,
        })
    }
    fn is_gallery(&self) -> bool {
        matches!(self, Route::Gallery)
    }
}

/// `routeDirection` (App.tsx:1344-1354).
fn direction(from: &Route, to: &Route) -> f32 {
    match (from, to) {
        (Route::Gallery, t) if !t.is_gallery() => 1.0,
        (f, Route::Gallery) if !f.is_gallery() => -1.0,
        (Route::Record(_), Route::Session(_)) => 1.0,
        (Route::Session(_), Route::Record(_)) => -1.0,
        (Route::Settings, Route::Info) => 1.0,
        (Route::Info, Route::Settings) => -1.0,
        _ => 1.0,
    }
}

#[derive(Clone)]
enum MenuPurpose {
    Add,
    Card { token: String },
    /// The recorder's ⋯ menu.
    Recorder(Entity<crate::screens::record::RecordPage>),
}

pub enum PageView {
    Gallery(Entity<GalleryPage>),
    Record(Entity<RecordPage>),
    Session(Entity<SessionPage>),
    Settings(Entity<SettingsPage>),
    Info(Entity<crate::screens::info::InfoPage>),
    Add(Entity<AddSessionPage>),
    EditorPlaceholder(Entity<EditorPlaceholder>),
    Placeholder(Entity<Placeholder>),
}

impl PageView {
    fn view(&self) -> AnyView {
        match self {
            PageView::Gallery(e) => e.clone().into(),
            PageView::Record(e) => e.clone().into(),
            PageView::Session(e) => e.clone().into(),
            PageView::Settings(e) => e.clone().into(),
            PageView::Info(e) => e.clone().into(),
            PageView::Add(e) => e.clone().into(),
            PageView::EditorPlaceholder(e) => e.clone().into(),
            PageView::Placeholder(e) => e.clone().into(),
        }
    }
    /// The page's own header contribution (`usePublishHeaderNav`).
    fn header(&self, cx: &App) -> Option<HeaderNav> {
        match self {
            PageView::Settings(e) => Some(e.read(cx).header(e.clone())),
            _ => None,
        }
    }
    /// Actions the page puts in the header bar, after the title.
    fn header_actions(&self, cx: &App) -> Vec<AnyElement> {
        match self {
            PageView::Session(e) => e.read(cx).header_actions(e.clone()),
            PageView::Record(e) => e.read(cx).header_actions(e.clone(), cx),
            _ => Vec::new(),
        }
    }
    /// The modal the page wants on top of the whole window.
    fn modal(&self, cx: &App) -> Option<crate::screens::record::ModalReq> {
        match self {
            PageView::Record(r) => r.read(cx).modal(cx),
            _ => None,
        }
    }
}

struct Slot {
    key: String,
    page: PageView,
    x: Anim,
    opacity: Anim,
    leaving: bool,
    /// `overflow-y`: the gallery manages its own scroll; other pages scroll.
    gallery: bool,
}

pub struct MainWindow {
    pub route: Route,
    /// A system material (vibrancy / Mica) is behind the window, so the
    /// window background is left unpainted.
    vibrant: bool,
    /// Launch timing has been handed its first-frame callback.
    first_frame_timed: bool,
    history: Vec<Route>,
    /// Pages left by going back, for Forward (mouse button / Alt+→ / ⌘]).
    /// Top of the stack is the next page.
    forward: Vec<Route>,
    slots: Vec<Slot>,
    focus: FocusHandle,
    controls_on_right: bool,
    /// Linux backdrop dim of the header bar and frame shadow (1 = focused).
    active: Anim,
    modals: Vec<ModalSlot>,
    pending_route: Option<(Route, f32)>,
    pub pending_editor: Option<String>,
    last_deep_link: Option<String>,
    pub focus_requested: bool,
    own_modal: Option<AnyView>,
    redirect_fired: std::collections::HashSet<String>,
    gate: Option<Entity<PermissionScreen>>,
    menu: Option<(Entity<MenuPopup>, MenuPurpose)>,
    pending_menu: Option<(Vec<MenuItem>, Bounds<Pixels>, bool, MenuPurpose)>,
    pending_native_menu: Option<(Vec<crate::platform::mac_menu::Entry>, Bounds<Pixels>)>,
    /// The macOS add menu (a SwiftUI panel) is open. It takes key status from
    /// the main window, which would otherwise dim the header bar as if the
    /// window had gone to the background; the window is still the main one.
    native_menu_open: bool,
    /// Bumped per native menu, so a replaced menu's ending doesn't clear
    /// `native_menu_open` while its successor is up.
    native_menu_gen: u64,
    /// The OS's own answer to "is this window active", kept so the dim can
    /// settle correctly when the native menu closes.
    os_active: bool,
    header_add_anchor: Bounds<Pixels>,
    frame_offset: f32,
    tip: Option<Tip>,
    tip_sheet: Option<Entity<Sheet>>,
    tip_open: bool,
    pending_tip: Option<(Tip, bool)>,
    wanted_moment: Option<(String, String)>,
    linux_update: Option<LinuxUpdate>,
    update_sheet: Option<Entity<Sheet>>,
    /// macOS/Windows background-update pill (`UpdatePill`).
    update_pill: Entity<crate::ui::update_pill::UpdatePill>,
    panel_sheet: Option<(String, Entity<Sheet>)>,
    pending_panel: Option<(String, String)>,
    starting_label: Option<String>,
    starting_o: Anim,
    starting_y: Anim,
    _subs: Vec<Subscription>,
}

/// Whether the main window may close now. Mid-capture it pauses the
/// session first and quits once that lands (the Tauri shell's
/// `ExitRequested` handling), so no captured time is lost.
pub fn main_should_close(cx: &mut App) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static QUITTING: AtomicBool = AtomicBool::new(false);
    let capturing = app::state(cx).recorder.read(cx).state.as_ref().map(|s| s.capturing).unwrap_or(false);
    if !capturing || QUITTING.load(Ordering::Relaxed) {
        crate::crash::note_quit("main window closed");
        return true;
    }
    QUITTING.store(true, Ordering::Relaxed);
    log::info!("[app] closing during a capture — pausing the session first");
    let core = app::state(cx).core.clone();
    let task = app::tokio(cx, async move {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), core.recorder_pause()).await;
    });
    cx.spawn(async move |cx| {
        task.await;
        crate::crash::note_quit("main window closed during a capture");
        cx.update(|cx| cx.quit());
    })
    .detach();
    false
}

/// The current announcement (shown as the gallery banner).
#[derive(Default)]
pub struct CurrentAnnouncement(pub Option<services::Announcement>);
impl gpui_kit::gpui::Global for CurrentAnnouncement {}

/// Whether this window draws the Linux header bar (client-side decorations).
pub fn uses_header_bar(window: &Window) -> bool {
    is_linux() && matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Radius for fills that reach the visible window edge. GPUI's overflow
/// mask is rectangular, so overlay backgrounds must round themselves.
pub fn window_corner_radius(window: &Window) -> f32 {
    if !uses_header_bar(window) {
        return 0.0;
    }
    match window.window_decorations() {
        Decorations::Client { tiling } if !(tiling.top || tiling.bottom || tiling.left || tiling.right) => WINDOW_RADIUS,
        _ => 0.0,
    }
}

pub fn open(cx: &mut App) -> anyhow::Result<WindowHandle<MainWindow>> {
    let csd = is_linux() && crate::platform::desktop_appearance::client_frames();
    let (w, h) = if csd { (WIN_W + 2.0 * WINDOW_MARGIN, WIN_H + 2.0 * WINDOW_MARGIN) } else { (WIN_W, WIN_H) };
    let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: if is_mac() {
            Some(TitlebarOptions {
                title: Some(format!("Lookout v{}", app::VERSION).into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(MAC_TRAFFIC_LIGHT_POSITION.0), px(MAC_TRAFFIC_LIGHT_POSITION.1))),
            })
        } else if is_windows() {
            // The header bar replaces the native title bar (caption buttons included).
            Some(TitlebarOptions { title: Some(format!("Lookout v{}", app::VERSION).into()), appears_transparent: true, ..Default::default() })
        } else {
            Some(TitlebarOptions { title: Some("Lookout".into()), ..Default::default() })
        },
        window_decorations: if is_linux() {
            Some(if crate::platform::desktop_appearance::client_frames() { WindowDecorations::Client } else { WindowDecorations::Server })
        } else {
            None
        },
        window_background: if is_linux() && crate::platform::desktop_appearance::client_frames() {
            WindowBackgroundAppearance::Transparent
        } else if is_linux() {
            WindowBackgroundAppearance::Opaque
        } else if is_mac() {
            // See platform::vibrancy: our own Sidebar material goes behind GPUI.
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::MicaBackdrop
        },
        is_resizable: false,
        is_minimizable: true,
        app_owns_titlebar_drag: is_mac(),
        app_id: Some(crate::store::APP_ID.into()),
        kind: WindowKind::Normal,
        window_min_size: Some(size(px(w), px(h))),
        icon: crate::platform::window_icon(),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let t = Theme::for_appearance(window.appearance()).with_accent(crate::platform::accent());
        cx.set_global(t);
        cx.new(|cx| MainWindow::new(window, cx))
    })
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut subs = Vec::new();
        subs.push(cx.observe_window_appearance(window, |_, window, cx| {
            let t = Theme::for_appearance(window.appearance()).with_accent(crate::platform::accent());
            cx.set_global(t);
            cx.notify();
        }));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            this.os_active = window.is_window_active();
            let active = this.os_active || this.native_menu_open;
            app::state(cx).frontend.wants_preview.store(active, std::sync::atomic::Ordering::Relaxed);
            this.active.set_css(if active { 1.0 } else { 0.0 }, 160.0, Ease::EASE_OUT);
            if active {
                // `visibilitychange → visible` refetches the gallery.
                for slot in &this.slots {
                    if let PageView::Gallery(g) = &slot.page {
                        g.update(cx, |g, cx| g.refresh(cx));
                    }
                }
            }
            cx.notify();
        }));
        app::state(cx).frontend.wants_preview.store(window.is_window_active(), std::sync::atomic::Ordering::Relaxed);
        // Quitting mid-recording pauses the session first (the Tauri shell's
        // ExitRequested handler), so no captured time is lost.
        window.on_window_should_close(cx, |_, cx| main_should_close(cx));
        let controls_on_right = crate::platform::desktop_appearance::desktop_appearance().controls_on_right;
        // Background updates (macOS/Windows): the pill, and the dialog for an
        // update that won't install.
        let updater = crate::updater::init(cx);
        subs.push(cx.observe(&updater, |_, _, cx| cx.notify()));
        subs.push(cx.subscribe(&updater, |this, _, ev: &crate::updater::UpdaterEvent, cx| {
            let crate::updater::UpdaterEvent::Stuck { version, detail } = ev;
            this.report_stuck_update(version, detail.as_deref(), cx);
        }));
        let update_pill = cx.new(|cx| crate::ui::update_pill::UpdatePill::new(updater, is_windows(), cx));
        let mut mw = Self {
            vibrant: crate::platform::vibrancy::apply(window),
            first_frame_timed: false,
            route: Route::Gallery,
            history: Vec::new(),
            forward: Vec::new(),
            slots: Vec::new(),
            focus,
            controls_on_right,
            active: Anim::new(1.0),
            modals: Vec::new(),
            pending_route: None,
            pending_editor: None,
            last_deep_link: None,
            focus_requested: false,
            own_modal: None,
            redirect_fired: Default::default(),
            gate: None,
            menu: None,
            pending_menu: None,
            pending_native_menu: None,
            native_menu_open: false,
            native_menu_gen: 0,
            os_active: window.is_window_active(),
            header_add_anchor: Bounds::default(),
            frame_offset: 0.0,
            tip: None,
            tip_sheet: None,
            tip_open: false,
            pending_tip: None,
            wanted_moment: None,
            linux_update: None,
            update_sheet: None,
            update_pill,
            panel_sheet: None,
            pending_panel: None,
            starting_label: None,
            starting_o: Anim::new(0.0),
            starting_y: Anim::new(8.0),
            _subs: subs,
        };
        mw.next_gate(cx);
        // After an update: its notes once, in the tip sheet.
        if let Some((v, notes)) = crate::updater::take_whats_new() {
            mw.pending_tip = Some((crate::updater::whats_new_tip(&v, &notes), false));
        }
        // Tip and announcement every 15 minutes; Linux release check every 6 hours.
        cx.spawn(async move |this, cx| loop {
            if let Some(tip) = services::fetch_tip(cx).await {
                let alive = this.update(cx, |mw, cx| {
                    mw.tip = tip;
                    // A moment that arrived before the tip did gets its chance now.
                    if let Some((moment, token)) = mw.wanted_moment.take() {
                        mw.open_tip(&moment, &token, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
            if let Some(a) = services::fetch_announcement(cx).await {
                cx.update(|cx| cx.set_global(CurrentAnnouncement(a)));
                this.update(cx, |mw, cx| mw.refresh_gallery_view(cx)).ok();
            }
            cx.background_executor().timer(std::time::Duration::from_secs(15 * 60)).await;
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            let u = services::check_linux_update(cx).await;
            if this.update(cx, |mw, cx| {
                mw.linux_update = u;
                cx.notify();
            }).is_err() {
                break;
            }
            cx.background_executor().timer(std::time::Duration::from_secs(6 * 3600)).await;
        })
        .detach();
        // Program registry for the + menu (refreshed each time it opens).
        cx.spawn(async move |_, cx| {
            let _ = crate::programs::fetch(cx, None).await;
        })
        .detach();
        let page = mw.make_page(&Route::Gallery, window, cx);
        mw.slots.push(Slot { key: Route::Gallery.key(), page, x: Anim::new(0.0), opacity: Anim::new(1.0), leaving: false, gallery: true });
        mw
    }

    fn make_page(&mut self, route: &Route, window: &mut Window, cx: &mut Context<Self>) -> PageView {
        match route {
            Route::Gallery => {
                let g = cx.new(GalleryPage::new);
                self._subs.push(cx.subscribe(&g, |this, _, ev, cx| this.on_gallery_event(ev, cx)));
                PageView::Gallery(g)
            }
            Route::Record(token) => {
                let token = token.clone();
                let r = cx.new(|cx| RecordPage::new(token, window, cx));
                self._subs.push(cx.subscribe(&r, |this, page, ev: &RecordEvent, cx| match ev {
                    RecordEvent::Menu { anchor, can_change_source } => this.open_recorder_menu(page, *anchor, *can_change_source, cx),
                    _ => this.on_record_event(ev, cx),
                }));
                self._subs.push(cx.observe(&r, |_, _, cx| cx.notify()));
                PageView::Record(r)
            }
            Route::Session(token) => {
                let token = token.clone();
                let p = cx.new(|cx| SessionPage::new(token, cx));
                self._subs.push(cx.subscribe(&p, |this, page, ev: &SessionEvent, cx| this.on_session_event(page, ev, cx)));
                PageView::Session(p)
            }
            Route::EditorOpen(token) => {
                let token = token.clone();
                PageView::EditorPlaceholder(cx.new(|_| EditorPlaceholder { token, focusing: Anim::new(1.0) }))
            }
            Route::Settings => {
                let p = cx.new(SettingsPage::new);
                self._subs.push(cx.subscribe(&p, |this, page, ev: &SettingsEvent, cx| this.on_settings_event(page, ev, cx)));
                PageView::Settings(p)
            }
            Route::Info => PageView::Info(cx.new(crate::screens::info::InfoPage::new)),
            Route::Add => {
                let p = cx.new(AddSessionPage::new);
                self._subs.push(cx.subscribe(&p, |this, page, ev: &AddEvent, cx| this.on_add_event(page, ev, cx)));
                PageView::Add(p)
            }
        }
    }

    /// macOS: the setup page (permissions), until it has been completed, and
    /// again whenever a required one is missing right now (revoked in System
    /// Settings, or never carried over to this build) rather than trusting
    /// the saved "done" forever. Camera is optional, so only Screen
    /// Recording and Accessibility bring it back.
    fn next_gate(&mut self, cx: &mut Context<Self>) {
        use crate::platform::permissions::{self, Kind};
        let completed = store(cx).perm_screen && store(cx).perm_camera && store(cx).perm_accessibility;
        let required_granted = || permissions::check(Kind::Screen) && permissions::check(Kind::Accessibility);
        if !is_mac() || (completed && required_granted()) {
            self.gate = None;
            return;
        }
        if self.gate.is_some() {
            return;
        }
        let g = cx.new(PermissionScreen::new);
        self._subs.push(cx.subscribe(&g, move |this, _, ev: &PermissionEvent, cx| {
            let PermissionEvent::Done { cache } = ev;
            // Bypassing setup passes without saving, so the page returns next launch.
            if *cache {
                let st = store_mut(cx);
                st.perm_screen = true;
                st.perm_camera = true;
                st.perm_accessibility = true;
                st.save();
            }
            this.gate = None;
            cx.notify();
        }));
        self.gate = Some(g);
    }

    fn on_settings_event(&mut self, page: Entity<SettingsPage>, ev: &SettingsEvent, cx: &mut Context<Self>) {
        match ev {
            SettingsEvent::Back => self.go_gallery(cx),
            SettingsEvent::About => self.navigate(Route::Info, cx),
            SettingsEvent::Reload => {
                // `window.location.reload()`: start over on the new server.
                self.slots.clear();
                self.history.clear();
                self.forward.clear();
                self.route = Route::Settings;
                self.go(Route::Gallery, cx);
            }
            SettingsEvent::Confirm { title, message, ok, action } => {
                let page2 = page.clone();
                let action = match action {
                    ConfirmAction::Unlink(p) => ConfirmAction::Unlink(p.clone()),
                    ConfirmAction::SwitchServer(t) => ConfirmAction::SwitchServer(t.clone()),
                };
                let action = std::rc::Rc::new(action);
                self.confirm(title, message, ok, true, move |_, cx| {
                    let action = action.clone();
                    page2.update(cx, |s, cx| match &*action {
                        ConfirmAction::Unlink(p) => s.do_unlink(p.clone(), cx),
                        ConfirmAction::SwitchServer(t) => s.probe_and_save(t.clone(), cx),
                    })
                }, cx);
            }
        }
    }

    fn on_add_event(&mut self, page: Entity<AddSessionPage>, ev: &AddEvent, cx: &mut Context<Self>) {
        match ev {
            AddEvent::Back => self.go_gallery(cx),
            AddEvent::Start(token) => {
                store_mut(cx).add_token(token);
                self.handle_deep_links(vec![format!("lookout://session/?token={token}")], cx);
            }
            AddEvent::DeepLink(url) => self.handle_deep_links(vec![url.clone()], cx),
            AddEvent::OpenProgram(program) => {
                let label = program.label();
                let task = crate::programs::open_program(program.clone(), cx.entity(), cx);
                cx.spawn(async move |_, cx| {
                    let outcome = task.await;
                    page.update(cx, |p, cx| p.program_outcome(label, outcome, cx));
                })
                .detach();
            }
        }
    }

    fn on_session_event(&mut self, page: Entity<SessionPage>, ev: &SessionEvent, cx: &mut Context<Self>) {
        let token = page.read(cx).token.clone();
        match ev {
            SessionEvent::Back => self.go_gallery(cx),
            SessionEvent::Archive => {
                let me = cx.entity();
                self.confirm(
                    "Archive Session",
                    "Are you sure you want to archive this session?",
                    "Archive",
                    true,
                    move |_, cx| {
                        store_mut(cx).archive_token(&token);
                        me.update(cx, |mw, cx| mw.go_gallery(cx));
                    },
                    cx,
                );
            }
            SessionEvent::Edit => {
                self.pending_editor = Some(token);
                cx.notify();
            }
            SessionEvent::RecordingFinished { panel_url, panel_resolved, .. } => {
                if let Some(url) = panel_url.clone() {
                    let base = app::api_base(cx);
                    let pending = !store(cx).panel_state.contains_key(&token);
                    if !*panel_resolved && pending && crate::screens::program_panel::acceptable(&url, &base) {
                        self.pending_panel = Some((token.clone(), url));
                        cx.notify();
                    } else if !*panel_resolved
                        && store(cx).panel_state.get(&token).map(String::as_str) == Some("dismissed")
                        && crate::screens::program_panel::acceptable(&url, &base)
                    {
                        self.offer_panel_prompt(page.clone(), token.clone(), url, cx);
                    }
                }
            }
            SessionEvent::Complete { redirect_url, panel_url } => {
                // The redirect hook fires once per token, only when there is
                // no in-app panel to show instead.
                let panel_ok = panel_url.as_deref().map(|u| crate::programs::acceptable_endpoint(Some(u))).unwrap_or(false);
                if let (Some(url), false) = (redirect_url.clone(), panel_ok) {
                    if self.redirect_fired.insert(token) && crate::programs::acceptable_endpoint(Some(&url)) {
                        cx.open_url(&url);
                    }
                }
            }
        }
    }

    fn on_record_event(&mut self, ev: &RecordEvent, cx: &mut Context<Self>) {
        match ev {
            RecordEvent::Back => self.go_gallery(cx),
            RecordEvent::ViewSession(token) => {
                crate::screens::recorder::close_recording(cx);
                crate::app::store_mut(cx).add_token(token);
                self.navigate(Route::Session(token.clone()), cx);
            }
            RecordEvent::OpenEditor(token) => {
                log::info!("[app] open editor for {}", &token[..8]);
                self.pending_editor = Some(token.clone());
                cx.notify();
            }
            RecordEvent::PickerShown => {
                if let Route::Record(t) = self.route.clone() {
                    self.open_tip("source", &t, cx);
                }
            }
            RecordEvent::Menu { .. } => {}
        }
    }

    /// The recorder's ⋯ menu, hung from its header button like the + menu.
    /// Clicking ⋯ while it is open closes it.
    fn open_recorder_menu(
        &mut self,
        page: Entity<crate::screens::record::RecordPage>,
        anchor: Bounds<Pixels>,
        can_change_source: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some((m, _)) = &self.menu {
            m.update(cx, |m, cx| m.dismiss(None, cx));
            return;
        }
        let off = px(self.frame_offset);
        let anchor = Bounds { origin: point(anchor.origin.x - off, anchor.origin.y - off), size: anchor.size };
        let items = vec![MenuItem::item("change-source", "Change source…", Icon::ArrowsLeftRightBold).disabled(!can_change_source)];
        self.pending_menu = Some((items, anchor, false, MenuPurpose::Recorder(page)));
        cx.notify();
    }

    fn on_gallery_event(&mut self, ev: &GalleryEvent, cx: &mut Context<Self>) {
        match ev {
            GalleryEvent::Open(token, status) => {
                let _ = status;
                let route = if matches!(status.as_str(), "pending" | "active" | "paused") {
                    Route::Record(token.clone())
                } else {
                    Route::Session(token.clone())
                };
                self.navigate(route, cx);
            }
            GalleryEvent::Settings => self.navigate(Route::Settings, cx),
            GalleryEvent::Add(anchor) => self.open_add_menu(*anchor, cx),
            GalleryEvent::ContextMenu { token, status, position } => {
                let mut items = vec![MenuItem::item("open", "Open", Icon::ArrowSquareOutBold)];
                if status == "complete" {
                    items.push(MenuItem::item("edit", "Open in Editor", Icon::LkPencil));
                }
                items.push(MenuItem::Separator);
                items.push(MenuItem::item("archive", "Archive", Icon::XBold));
                let off = px(self.frame_offset);
                let anchor = Bounds { origin: point(position.x - off, position.y - off), size: size(px(0.0), px(0.0)) };
                self.pending_menu = Some((items, anchor, true, MenuPurpose::Card { token: token.clone() }));
                cx.notify();
            }
            GalleryEvent::Archive(token) => {
                let token = token.clone();
                let me = cx.entity();
                self.confirm(
                    "Archive Session",
                    "Are you sure you want to archive this session?",
                    "Archive",
                    true,
                    move |_, cx| {
                        store_mut(cx).archive_token(&token);
                        me.update(cx, |mw, cx| mw.refresh_gallery(cx));
                    },
                    cx,
                );
            }
        }
    }

    /// `handleDeepLinkUrls`: route `lookout://` links (sessions and pairing
    /// callbacks). A link to another session while recording pauses the
    /// current one first.
    pub fn handle_deep_links(&mut self, urls: Vec<String>, cx: &mut Context<Self>) {
        for url in urls {
            if self.last_deep_link.as_deref() == Some(url.as_str()) {
                return;
            }
            if crate::deeplink::parse_pair_callback(&url).is_some() {
                self.last_deep_link = Some(url.clone());
                crate::programs::complete_pairing_and_open(url, cx.entity(), cx);
                return;
            }
            let Some(token) = crate::deeplink::extract_token(&url) else { continue };
            log::info!("[app] extracted token: {}...", &token[..8]);
            self.last_deep_link = Some(url.clone());
            store_mut(cx).add_token(&token);
            let core = app::state(cx).core.clone();
            let base = app::api_base(cx);
            let pause_other = matches!(&self.route, Route::Record(t) if *t != token);
            let tok = token.clone();
            let fut = app::tokio(cx, async move {
                if pause_other {
                    let _ = core.recorder_pause().await;
                }
                let cfg = lookout_core::SessionConfig { token: tok, api_base_url: base };
                let status = core.session_status(&cfg).await.ok().and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from));
                let program = core.session_get(&cfg).await.ok().and_then(|v| v.get("program").and_then(|s| s.as_str()).map(String::from));
                (status, program)
            });
            cx.spawn(async move |this, cx| {
                let (status, program) = fut.await;
                this.update(cx, |mw, cx| {
                    if let Some(program) = program {
                        store_mut(cx).deeplink_sessions.insert(program, token.clone());
                        store(cx).save();
                    }
                    let route = match status.as_deref() {
                        Some("stopped" | "compiling" | "complete" | "failed") => Route::Session(token.clone()),
                        _ => Route::Record(token.clone()),
                    };
                    mw.navigate(route, cx);
                    mw.focus_requested = true;
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
    }

    fn refresh_gallery_view(&mut self, cx: &mut Context<Self>) {
        for slot in &self.slots {
            if let PageView::Gallery(g) = &slot.page {
                g.update(cx, |_, cx| cx.notify());
            }
        }
    }

    /// `openTip(moment, token)`: show the tip sheet if this moment is
    /// eligible, and record that it was used.
    pub fn open_tip(&mut self, moment: &str, token: &str, cx: &mut Context<Self>) {
        let Some(tip) = self.tip.clone() else {
            self.wanted_moment = Some((moment.to_string(), token.to_string()));
            return;
        };
        if !services::should_show_tip(&tip, moment, token, cx) {
            return;
        }
        services::mark_tip_moment(&tip, moment, token, cx);
        let has_hero = tip.image_url.is_some();
        self.pending_tip = Some((tip, has_hero));
        cx.notify();
    }

    /// The program a panel URL belongs to: registry match by origin, else
    /// the bare hostname, else "This program".
    fn program_for(&self, url: &str, cx: &App) -> (String, Option<String>) {
        let origin = url::Url::parse(url).ok().map(|u| u.origin().ascii_serialization());
        for p in crate::programs::registry(cx) {
            for candidate in [Some(p.new_session_url.clone()), p.pair_url.clone(), p.start_url.clone()].into_iter().flatten() {
                if url::Url::parse(&candidate).ok().map(|u| u.origin().ascii_serialization()) == origin {
                    return (p.label(), p.icon_url.clone());
                }
            }
        }
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()));
        (host.unwrap_or_else(|| "This program".into()), None)
    }

    /// `PanelPrompt` under the video: the program still wants details.
    fn offer_panel_prompt(&mut self, page: Entity<SessionPage>, token: String, url: String, cx: &mut Context<Self>) {
        let (label, _) = self.program_for(&url, cx);
        let me = cx.entity();
        let prompt = cx.new(|_| PanelPrompt { label, token, url, main: me });
        page.update(cx, |p, cx| {
            p.below_video = Some(prompt.into());
            cx.notify();
        });
    }

    /// The panel was completed: drop the "needs a few details" card from the
    /// session page it sits on (the page stays mounted, so nothing else would).
    fn clear_panel_prompt(&mut self, token: &str, cx: &mut Context<Self>) {
        for slot in &self.slots {
            if let PageView::Session(page) = &slot.page {
                if page.read(cx).token == token {
                    page.update(cx, |p, cx| {
                        p.below_video = None;
                        cx.notify();
                    });
                }
            }
        }
    }

    /// `handleAdd`: the + menu (programs, separator, "Start from link"), or
    /// straight to the add page when there are no programs. Clicking + while
    /// the menu is open closes it.
    pub fn open_add_menu(&mut self, anchor: Bounds<Pixels>, cx: &mut Context<Self>) {
        if let Some((m, _)) = &self.menu {
            m.update(cx, |m, cx| m.dismiss(None, cx));
            return;
        }
        // Refresh the registry behind the menu each time.
        cx.spawn(async move |_, cx| {
            let _ = crate::programs::fetch(cx, None).await;
        })
        .detach();
        let programs = crate::programs::registry(cx);
        if programs.is_empty() {
            self.navigate(Route::Add, cx);
            return;
        }
        let mut items: Vec<MenuItem> = programs
            .iter()
            .map(|p| MenuItem::Item {
                id: format!("program:{}", p.name),
                label: p.label(),
                icon_url: p.icon_url.clone(),
                fallback: Icon::ArrowSquareOutBold,
                disabled: false,
            })
            .collect();
        items.push(MenuItem::Separator);
        items.push(MenuItem::item("create-new", "Start from link", Icon::PlusBold));
        if is_mac() {
            // The native SwiftUI panel; on failure fall back to the add page.
            let entries: Vec<crate::platform::mac_menu::Entry> = programs
                .iter()
                .map(|p| crate::platform::mac_menu::Entry {
                    id: Some(format!("program:{}", p.name)),
                    label: Some(p.label()),
                    symbol: Some("arrow.up.forward.app".into()),
                    icon_url: p.icon_url.clone(),
                    separator: false,
                })
                .chain([
                    crate::platform::mac_menu::Entry { id: None, label: None, symbol: None, icon_url: None, separator: true },
                    crate::platform::mac_menu::Entry {
                        id: Some("create-new".into()),
                        label: Some("Start from link".into()),
                        symbol: Some("plus".into()),
                        icon_url: None,
                        separator: false,
                    },
                ])
                .collect();
            self.pending_native_menu = Some((entries, anchor));
            cx.notify();
            return;
        }
        self.pending_menu = Some((items, anchor, false, MenuPurpose::Add));
        cx.notify();
    }

    fn on_menu_choice(&mut self, purpose: MenuPurpose, choice: Option<String>, cx: &mut Context<Self>) {
        let Some(choice) = choice else { return };
        match purpose {
            MenuPurpose::Add => {
                if choice == "create-new" {
                    self.navigate(Route::Add, cx);
                } else if let Some(name) = choice.strip_prefix("program:") {
                    if let Some(p) = crate::programs::registry(cx).into_iter().find(|p| p.name == name) {
                        let task = crate::programs::open_program(p, cx.entity(), cx);
                        cx.spawn(async move |this, cx| {
                            if task.await == crate::programs::OpenOutcome::Failed {
                                this.update(cx, |mw, cx| mw.navigate(Route::Add, cx)).ok();
                            }
                        })
                        .detach();
                    }
                }
            }
            MenuPurpose::Recorder(page) => {
                if choice == "change-source" {
                    page.update(cx, |p, cx| p.menu_change_source(cx));
                }
            }
            MenuPurpose::Card { token } => match choice.as_str() {
                "open" => {
                    let status = store(cx).gallery_cache.get(&token).map(|c| c.summary.status.clone()).unwrap_or_default();
                    self.on_gallery_event(&GalleryEvent::Open(token, status), cx);
                }
                "edit" => {
                    self.pending_editor = Some(token);
                    cx.notify();
                }
                "archive" => self.on_gallery_event(&GalleryEvent::Archive(token), cx),
                _ => {}
            },
        }
    }

    /// The "Starting {program}…" pill while an instant start is in flight.
    pub fn set_starting(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        match label {
            Some(l) => {
                self.starting_label = Some(l);
                // Fully gone → mount from `initial`; still exiting →
                // AnimatePresence re-enters from wherever it is.
                if self.starting_o.get() <= 0.001 && !self.starting_o.is_animating() {
                    self.starting_y.jump(8.0);
                }
                self.starting_o.set(1.0, Curve::tween(300.0, Ease::MOTION_DEFAULT));
                self.starting_y.set(0.0, Curve::Spring(Spring::MOTION_XY));
            }
            None => {
                // `exit.transition: { duration: 0.12 }` — duration-only, so
                // motion's base ease "easeOut" (motion-value.mjs:27).
                self.starting_o.set(0.0, Curve::tween(120.0, Ease::EASE_OUT));
                self.starting_y.set(8.0, Curve::tween(120.0, Ease::EASE_OUT));
            }
        }
        cx.notify();
    }

    /// A one-button message dialog (Tauri `message()`).
    pub fn alert(&mut self, title: &str, message: &str, cx: &mut Context<Self>) {
        if let Some(h) = native_handle(cx) {
            use gpui_kit::gpui::PromptLevel;
            crate::ui::overlay::native_dialog(h, PromptLevel::Info, title.into(), message.into(), "OK".into(), None, |_, _| {}, cx);
            return;
        }
        let me = cx.entity();
        let (title, message) = (title.to_string(), message.to_string());
        let view = cx.new(|cx| {
            let me2 = me.clone();
            let mut d = crate::ui::overlay::ConfirmDialog::new(
                title,
                message,
                "OK",
                cx,
                move |_, cx| me.update(cx, |mw, cx| mw.close_own_modal(cx)),
                move |_, cx| me2.update(cx, |mw, cx| mw.close_own_modal(cx)),
            );
            d.cancel_label = None;
            d
        });
        self.own_modal = Some(view.into());
        cx.notify();
    }

    /// A confirm dialog (Tauri `confirm()`); `on_ok` runs after it closes.
    pub fn confirm(
        &mut self,
        title: &str,
        message: &str,
        ok_label: &str,
        destructive: bool,
        on_ok: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(h) = native_handle(cx) {
            use gpui_kit::gpui::PromptLevel;
            crate::ui::overlay::native_dialog(h, PromptLevel::Warning, title.into(), message.into(), ok_label.into(), Some("Cancel".into()), on_ok, cx);
            return;
        }
        let me = cx.entity();
        let (title, message, ok_label) = (title.to_string(), message.to_string(), ok_label.to_string());
        let on_ok = std::rc::Rc::new(on_ok);
        let view = cx.new(|cx| {
            let me2 = me.clone();
            let mut d = crate::ui::overlay::ConfirmDialog::new(
                title,
                message,
                ok_label,
                cx,
                move |window, cx| {
                    me.update(cx, |mw, cx| mw.close_own_modal(cx));
                    on_ok(window, cx);
                },
                move |_, cx| me2.update(cx, |mw, cx| mw.close_own_modal(cx)),
            );
            d.destructive = destructive;
            d
        });
        self.own_modal = Some(view.into());
        cx.notify();
    }

    /// `reportStuckUpdate`: an update that won't install, with a way out.
    pub fn report_stuck_update(&mut self, version: &str, detail: Option<&str>, cx: &mut Context<Self>) {
        let mut message = format!(
            "Lookout couldn't install version {version}.\n\nYour sessions and settings are safe — the app will keep working on this version. You can install the update yourself from the releases page."
        );
        if let Some(d) = detail.filter(|d| !d.is_empty()) {
            message.push_str(&format!("\n\nDetails: {d}"));
        }
        if let Some(h) = native_handle(cx) {
            use gpui_kit::gpui::PromptLevel;
            crate::ui::overlay::native_dialog(
                h,
                PromptLevel::Warning,
                "Update couldn't be installed".into(),
                message,
                "Open Releases Page".into(),
                Some("Not Now".into()),
                |_, cx| cx.open_url(crate::updater::RELEASES_URL),
                cx,
            );
            return;
        }
        let me = cx.entity();
        let view = cx.new(|cx| {
            let me2 = me.clone();
            let mut d = crate::ui::overlay::ConfirmDialog::new(
                "Update couldn't be installed",
                message,
                "Open Releases Page",
                cx,
                move |_, cx| {
                    me.update(cx, |mw, cx| mw.close_own_modal(cx));
                    cx.open_url(crate::updater::RELEASES_URL);
                },
                move |_, cx| me2.update(cx, |mw, cx| mw.close_own_modal(cx)),
            );
            d.cancel_label = Some("Not Now".into());
            d
        });
        self.own_modal = Some(view.into());
        cx.notify();
    }

    pub fn close_own_modal(&mut self, cx: &mut Context<Self>) {
        self.own_modal = None;
        cx.notify();
    }

    pub fn refresh_gallery(&mut self, cx: &mut Context<Self>) {
        for slot in &self.slots {
            if let PageView::Gallery(g) = &slot.page {
                g.update(cx, |g, cx| g.refresh(cx));
            }
        }
    }

    /// Go to `route` with the route transition.
    pub fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if route == self.route {
            return;
        }
        self.history.push(self.route.clone());
        self.forward.clear();
        self.go(route, cx);
    }

    /// Where the user is, for `platform::relaunch`. The editor placeholder
    /// isn't saved as a route: reopening the editor window brings it back.
    pub fn relaunch_snapshot(&self, cx: &App) -> crate::platform::relaunch::Snapshot {
        let keep = |r: &&Route| !matches!(r, Route::EditorOpen(_));
        let mut history: Vec<String> = self.history.iter().filter(keep).map(Route::key).collect();
        let route = match &self.route {
            Route::EditorOpen(_) => history.pop().unwrap_or_else(|| Route::Gallery.key()),
            r => r.key(),
        };
        let editors = cx
            .try_global::<crate::windows::editor::EditorWindows>()
            .map(|w| w.0.keys().cloned().collect())
            .unwrap_or_default();
        crate::platform::relaunch::Snapshot { history, route, editors, ..Default::default() }
    }

    /// Put back a [`Self::relaunch_snapshot`]; editor windows reopen after.
    pub fn restore_snapshot(&mut self, snap: &crate::platform::relaunch::Snapshot, cx: &mut Context<Self>) {
        let usable = |r: &Route| !matches!(r, Route::EditorOpen(_));
        let Some(route) = Route::from_key(&snap.route).filter(usable) else { return };
        if route == self.route {
            return;
        }
        self.history = snap.history.iter().filter_map(|k| Route::from_key(k)).filter(usable).collect();
        self.go(route, cx);
    }

    /// `history.back()` — Escape / Backspace / back buttons.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        let prev = self.history.pop().unwrap_or(Route::Gallery);
        if prev != self.route {
            self.push_forward(self.route.clone());
        }
        self.go(prev, cx);
    }

    /// `history.forward()`: undo the last Back.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.forward.pop() else { return };
        self.history.push(self.route.clone());
        self.go(next, cx);
    }

    /// The editor placeholder only means something while its window is
    /// open, so it never becomes a Forward target.
    fn push_forward(&mut self, route: Route) {
        if !matches!(route, Route::EditorOpen(_)) {
            self.forward.push(route);
        }
    }

    /// Back / Forward from the mouse side buttons and keyboard shortcuts.
    /// Back steps out of a Settings sub-page before leaving Settings.
    fn history_nav(&mut self, dir: NavigationDirection, window: &mut Window, cx: &mut Context<Self>) {
        if self.gate.is_some() || !self.modals.is_empty() || self.in_recorder(cx) {
            return;
        }
        if crate::ui::input::any_input_focused(window, cx) {
            return;
        }
        match dir {
            NavigationDirection::Back => {
                if self.route.is_gallery() || self.cancel_source_switch(cx) {
                    return;
                }
                let settings_sub = self.slots.iter().rev().find(|s| !s.leaving).and_then(|s| match &s.page {
                    PageView::Settings(p) if !p.read(cx).at_menu() => Some(p.clone()),
                    _ => None,
                });
                match settings_sub {
                    Some(p) => p.update(cx, |p, cx| p.back(cx)),
                    None => self.back(cx),
                }
            }
            NavigationDirection::Forward => self.forward(cx),
        }
    }

    fn on_nav_button(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let MouseButton::Navigate(dir) = ev.button {
            self.history_nav(dir, window, cx);
        }
    }

    fn go(&mut self, route: Route, cx: &mut Context<Self>) {
        let dir = direction(&self.route, &route);
        for slot in self.slots.iter_mut().filter(|s| !s.leaving) {
            slot.leaving = true;
            slot.x.set(-14.0 * dir, Curve::Spring(Spring::ROUTE));
            slot.opacity.set(0.0, Curve::tween(140.0, Ease::EASE_OUT));
        }
        if matches!(self.route, Route::Record(_)) {
            // DesktopRecorder unmount → recorder_close; RecordPage unmount →
            // release the portal casts.
            crate::screens::recorder::close_recording(cx);
            crate::screens::record::release_screencast(cx);
        }
        self.pending_route = Some((route.clone(), dir));
        self.route = route;
        cx.notify();
    }

    /// Mount the page for a route change requested without a window.
    fn mount_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((route, dir)) = self.pending_route.take() else { return };
        let key = route.key();
        // Re-entering a page that is still animating out: bring it back.
        // Re-entering a page that is still animating out: AnimatePresence
        // keeps the same child and animates it back to `center` from its
        // current x/opacity (not from `enter`).
        if let Some(i) = self.slots.iter().position(|s| s.key == key) {
            let mut slot = self.slots.remove(i);
            slot.leaving = false;
            slot.x.set(0.0, Curve::Spring(Spring::ROUTE));
            slot.opacity.set_delayed(1.0, Curve::tween(160.0, Ease::EASE_OUT), 40.0);
            self.slots.push(slot);
            return;
        }
        let page = self.make_page(&route, window, cx);
        let mut opacity = Anim::new(0.0);
        opacity.set_delayed(1.0, Curve::tween(160.0, Ease::EASE_OUT), 40.0);
        self.slots.push(Slot {
            key,
            gallery: route.is_gallery(),
            page,
            x: Anim::from_to(14.0 * dir, 0.0, Curve::Spring(Spring::ROUTE)),
            opacity,
            leaving: false,
        });
    }

    /// The record page while its picker is changing the open recording's
    /// source.
    fn source_switch(&self, cx: &App) -> Option<Entity<crate::screens::record::RecordPage>> {
        match self.slots.iter().rev().find(|s| !s.leaving).map(|s| &s.page) {
            Some(PageView::Record(r)) if r.read(cx).switching() => Some(r.clone()),
            _ => None,
        }
    }

    /// Back during a source change: to the recorder, not out of the page
    /// (which would close the recording). Whether there was one to cancel.
    fn cancel_source_switch(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(page) = self.source_switch(cx) else { return false };
        page.update(cx, |p, cx| p.cancel_switch(cx));
        cx.notify();
        true
    }

    /// The record page is showing the live recorder (not the source picker).
    fn in_recorder(&self, cx: &App) -> bool {
        match self.slots.iter().rev().find(|s| !s.leaving).map(|s| &s.page) {
            Some(PageView::Record(r)) => r.read(cx).is_recording_view(),
            _ => false,
        }
    }

    fn header_nav(&self, cx: &App) -> HeaderNav {
        if self.gate.is_some() {
            return HeaderNav { title: "".into(), subtitle: None, on_back: None, app_icon: false };
        }
        let current = self.slots.iter().rev().find(|s| !s.leaving);
        if let Some(nav) = current.and_then(|s| s.page.header(cx)) {
            return nav;
        }
        let back = |this: &Self| -> Option<Rc<dyn Fn(&mut Window, &mut App)>> {
            let _ = this;
            Some(Rc::new(|window: &mut Window, cx: &mut App| {
                if let Some(h) = window.root::<MainWindow>().flatten() {
                    h.update(cx, |mw, cx| mw.go_gallery(cx));
                }
            }))
        };
        match &self.route {
            Route::Gallery => HeaderNav { title: "Lookout".into(), subtitle: None, on_back: None, app_icon: true },
            Route::Settings => HeaderNav { title: "Settings".into(), subtitle: None, on_back: back(self), app_icon: false },
            // Back returns to wherever it was opened from (Settings or the app menu).
            Route::Info => HeaderNav {
                title: "About Lookout".into(),
                subtitle: None,
                on_back: Some(Rc::new(|window: &mut Window, cx: &mut App| {
                    if let Some(h) = window.root::<MainWindow>().flatten() {
                        h.update(cx, |mw, cx| mw.back(cx));
                    }
                })),
                app_icon: false,
            },
            Route::Add => HeaderNav { title: "Start a recording".into(), subtitle: None, on_back: back(self), app_icon: false },
            Route::Record(_) => {
                // The record page names its step: the source question, or
                // "Recording" once capture runs.
                let (title, subtitle) = match current.map(|s| &s.page) {
                    Some(PageView::Record(r)) => (r.read(cx).header_title(), r.read(cx).header_subtitle(cx)),
                    _ => ("Recording", None),
                };
                // No way out mid-capture: leaving the page closes the recorder.
                // Changing source, Back returns to the recorder instead.
                let on_back = if self.in_recorder(cx) {
                    None
                } else if self.source_switch(cx).is_some() {
                    Some(Rc::new(|window: &mut Window, cx: &mut App| {
                        if let Some(h) = window.root::<MainWindow>().flatten() {
                            h.update(cx, |mw, cx| {
                                mw.cancel_source_switch(cx);
                            });
                        }
                    }) as Rc<dyn Fn(&mut Window, &mut App)>)
                } else {
                    back(self)
                };
                HeaderNav { title: title.into(), subtitle: subtitle.map(Into::into), on_back, app_icon: false }
            }
            Route::Session(_) => HeaderNav { title: "Timelapse".into(), subtitle: None, on_back: back(self), app_icon: false },
            Route::EditorOpen(_) => HeaderNav { title: "Lookout".into(), subtitle: Some("Editing in another window".into()), on_back: None, app_icon: true },
        }
    }

    /// One step of the CI screenshot tour (`smoke::TOUR`).
    pub fn smoke_step(&mut self, step: &str, _window: &mut Window, cx: &mut Context<Self>) {
        match step {
            "gallery" => {
                // A fresh runner would otherwise sit on the permission screens.
                self.gate = None;
                self.go_gallery(cx);
            }
            "settings" => self.navigate(Route::Settings, cx),
            "tray" => {
                self.go_gallery(cx);
                crate::platform::tray::sync(true, "1:23:45", false, cx);
            }
            "panel" => {
                crate::platform::tray::sync(false, "", false, cx);
                if let Some(token) = store(cx).active_tokens().into_iter().next() {
                    self.pending_panel = Some((token.clone(), format!("http://localhost:8787/panel/{token}")));
                }
            }
            "add-menu" => {
                self.panel_sheet = None;
                let a = self.header_add_anchor;
                self.open_add_menu(a, cx);
            }
            "confirm" => self.confirm("Archive Session", "Are you sure you want to archive this session?", "Archive", true, |_, _| {}, cx),
            _ => {}
        }
        cx.notify();
    }

    /// The pages' own Back buttons: straight home. Forward can retrace
    /// the way back to where the user was, like a browser.
    pub fn go_gallery(&mut self, cx: &mut Context<Self>) {
        if !self.route.is_gallery() {
            self.forward.clear();
            self.push_forward(self.route.clone());
            while let Some(r) = self.history.pop() {
                if r.is_gallery() {
                    break;
                }
                self.push_forward(r);
            }
        }
        self.history.clear();
        self.go(Route::Gallery, cx);
        self.refresh_gallery(cx);
    }

    fn header_actions(&self, _t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // A permission screen stands in for the whole app (the Tauri app
        // rendered it instead of the gallery, header included): no Settings
        // or + until it's resolved.
        if self.gate.is_some() {
            return Vec::new();
        }
        // Subpages publish their own actions (Archive, Stop Session) here
        // rather than on a row of their own under the bar.
        if !self.route.is_gallery() {
            let current = self.slots.iter().rev().find(|s| !s.leaving);
            return current.map(|s| s.page.header_actions(cx)).unwrap_or_default();
        }
        let e1 = cx.entity();
        let e2 = cx.entity();
        vec![
            HeaderBtn::icon("hb-settings", HeaderBtnKind::Action, Icon::GearSixFill, 15.0, move |_, _, cx| {
                e1.update(cx, |mw, cx| mw.navigate(Route::Settings, cx))
            })
            .tooltip("Settings")
            .into_any_element(),
            div()
                .relative()
                .child(
                    HeaderBtn::icon("hb-add", HeaderBtnKind::Action, Icon::PlusBold, 17.0, move |_, _, cx| {
                        e2.update(cx, |mw, cx| {
                            let a = mw.header_add_anchor;
                            mw.open_add_menu(a, cx)
                        })
                    })
                    .tooltip("Start"),
                )
                .child({
                    let probe = cx.entity();
                    gpui_kit::gpui::canvas(move |b, window, cx| {
                        // Window coordinates relative to the visible frame.
                        let off = if uses_header_bar(window) { WINDOW_MARGIN } else { 0.0 };
                        let b = Bounds { origin: point(b.origin.x - px(off), b.origin.y - px(off)), size: b.size };
                        probe.update(cx, |mw, _| mw.header_add_anchor = b);
                    }, |_, _, _, _| {})
                    .absolute()
                    .inset_0()
                })
                .into_any_element(),
        ]
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        if (key == "escape" || key == "backspace") && !self.route.is_gallery() && !window.default_prevented() {
            if crate::ui::input::any_input_focused(window, cx) || self.in_recorder(cx) {
                return;
            }
            if !self.cancel_source_switch(cx) {
                self.back(cx);
            }
            return;
        }
        // Alt+←/→ (Windows, Linux), ⌘[ / ⌘] (macOS): browser history keys.
        let m = &ev.keystroke.modifiers;
        let dir = if cfg!(target_os = "macos") {
            match key {
                "[" if m.platform && !m.alt && !m.control && !m.shift => Some(NavigationDirection::Back),
                "]" if m.platform && !m.alt && !m.control && !m.shift => Some(NavigationDirection::Forward),
                _ => None,
            }
        } else {
            match key {
                "left" if m.alt && !m.platform && !m.control && !m.shift => Some(NavigationDirection::Back),
                "right" if m.alt && !m.platform && !m.control && !m.shift => Some(NavigationDirection::Forward),
                _ => None,
            }
        };
        if let Some(dir) = dir {
            if !window.default_prevented() {
                self.history_nav(dir, window, cx);
            }
        }
    }
}

impl Focusable for MainWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_timed {
            self.first_frame_timed = true;
            window.on_next_frame(|_, _| crate::startup::first_frame());
        }
        if crate::smoke::enabled() {
            crate::smoke::frame();
            window.request_animation_frame();
        }
        let t = theme(cx).clone();
        let csd = uses_header_bar(window);
        crate::theme::set_header_bar_shown(true);
        crate::theme::set_vibrant(self.vibrant);
        self.frame_offset = if csd { WINDOW_MARGIN } else { 0.0 };
        self.mount_pending(window, cx);
        if std::mem::take(&mut self.focus_requested) {
            window.activate_window();
        }
        // Keyboard shortcuts (Escape/Backspace → back) need a focused root;
        // reclaim focus only when nothing else holds it, so text fields keep theirs.
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        if let Some((entries, anchor)) = self.pending_native_menu.take() {
            match crate::platform::mac_menu::show(window, &entries, anchor) {
                Some(rx) => {
                    self.native_menu_open = true;
                    self.native_menu_gen += 1;
                    let menu_gen = self.native_menu_gen;
                    cx.spawn(async move |this, cx| {
                        let choice = rx.recv_async().await;
                        this.update(cx, |mw, cx| {
                            if mw.native_menu_gen != menu_gen {
                                return; // replaced by a newer menu
                            }
                            mw.native_menu_open = false;
                            // Settle on the window's real state (it may have
                            // lost focus to another app while the menu was up).
                            mw.active.set_css(if mw.os_active { 1.0 } else { 0.0 }, 160.0, Ease::EASE_OUT);
                            cx.notify();
                            if let Ok(choice) = choice {
                                mw.on_menu_choice(MenuPurpose::Add, choice, cx);
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
                None => self.navigate(Route::Add, cx),
            }
        }
        if let Some((items, anchor, at_pointer, purpose)) = self.pending_menu.take() {
            let m = cx.new(|cx| MenuPopup::new(items, anchor, at_pointer, window, cx));
            let p2 = purpose.clone();
            self._subs.push(cx.observe(&m, |this, m, cx| {
                if m.read(cx).finished() {
                    this.menu = None;
                    cx.notify();
                }
            }));
            self._subs.push(cx.subscribe(&m, move |this, _, ev: &MenuEvent, cx| {
                let MenuEvent::Select(choice) = ev;
                this.on_menu_choice(p2.clone(), choice.clone(), cx);
            }));
            self.menu = Some((m, purpose));
        }
        if let Some((tip, has_hero)) = self.pending_tip.take() {
            let hero: Option<AnyView> = tip.image_url.clone().map(|u| cx.new(|cx| TipHero::new(u, cx)).into());
            let body: AnyView = cx.new(|_| TipBody { tip: tip.clone(), has_hero }).into();
            let sheet = cx.new(|cx| Sheet::new(hero, body, cx));
            self._subs.push(cx.subscribe(&sheet, |this, _, ev: &SheetEvent, cx| match ev {
                SheetEvent::Dismiss => this.tip_open = false,
                SheetEvent::Closed => {
                    this.tip_sheet = None;
                    cx.notify();
                }
            }));
            sheet.update(cx, |s, cx| s.set_open(true, window, cx));
            self.tip_sheet = Some(sheet);
            self.tip_open = true;
        }
        if let Some((token, url)) = self.pending_panel.take() {
            let (label, icon) = self.program_for(&url, cx);
            let body = cx.new(|cx| ProgramPanel::new(token.clone(), url, label, icon, cx));
            let sheet = cx.new(|cx| {
                let mut s = Sheet::new(None, body.clone().into(), cx);
                s.style = SheetStyle::Panel;
                s
            });
            body.update(cx, |b, _| b.sheet = Some(sheet.downgrade()));
            let t2 = token.clone();
            let sheet2 = sheet.clone();
            self._subs.push(cx.subscribe_in(&body, window, move |_, _, ev: &PanelEvent, window, cx| {
                if let PanelEvent::Done = ev {
                    store_mut(cx).panel_state.insert(t2.clone(), "done".into());
                    store(cx).save();
                }
                sheet2.update(cx, |s, cx| s.set_open(false, window, cx));
            }));
            let t3 = token.clone();
            self._subs.push(cx.subscribe(&sheet, move |this, _, ev: &SheetEvent, cx| match ev {
                SheetEvent::Dismiss => {
                    store_mut(cx).panel_state.insert(t3.clone(), "dismissed".into());
                    store(cx).save();
                }
                SheetEvent::Closed => {
                    store_mut(cx).panel_state.entry(t3.clone()).or_insert_with(|| "dismissed".into());
                    store(cx).save();
                    this.panel_sheet = None;
                    if store(cx).panel_state.get(&t3).map(String::as_str) == Some("done") {
                        this.clear_panel_prompt(&t3, cx);
                        this.refresh_gallery_view(cx);
                        this.open_tip("panel-done", &t3, cx);
                    }
                    cx.notify();
                }
            }));
            sheet.update(cx, |s, cx| s.set_open(true, window, cx));
            self.panel_sheet = Some((token, sheet));
        }
        let on_main = self.route.is_gallery() && self.gate.is_none();
        if !on_main {
            self.update_pill.update(cx, |p, _| p.unmount());
        }
        let want_update = self.linux_update.is_some() && on_main && !self.tip_open;
        if want_update && self.update_sheet.is_none() {
            let u = self.linux_update.clone().unwrap();
            let body: AnyView = cx.new(|_| UpdateBody::new(u)).into();
            let sheet = cx.new(|cx| Sheet::new(None, body, cx));
            self._subs.push(cx.subscribe(&sheet, |this, _, ev: &SheetEvent, cx| match ev {
                SheetEvent::Dismiss => {
                    // Closing remembers this version so we ask once per release.
                    if let Some(u) = this.linux_update.take() {
                        store_mut(cx).update_nag_dismissed = Some(u.version);
                        store(cx).save();
                    }
                }
                SheetEvent::Closed => {
                    this.update_sheet = None;
                    cx.notify();
                }
            }));
            sheet.update(cx, |s, cx| s.set_open(true, window, cx));
            self.update_sheet = Some(sheet);
        } else if !want_update {
            if let Some(s) = &self.update_sheet {
                s.update(cx, |s, cx| s.set_open(false, window, cx));
            }
        }
        if self.menu.as_ref().map(|(m, _)| m.read(cx).finished()).unwrap_or(false) {
            self.menu = None;
            window.focus(&self.focus, cx);
        }
        if let Some(token) = self.pending_editor.take() {
            cx.defer(move |cx| crate::windows::editor::open(token, cx));
        }

        // The current page's modal: enter when it appears, fade when it goes.
        let want = match &self.own_modal {
            Some(v) => {
                let me = cx.entity();
                Some((
                    v.clone(),
                    crate::ui::overlay::Entrance::Overlay,
                    crate::ui::overlay::ModalWidth::Max(420.0),
                    Some(std::rc::Rc::new(move |_: &mut Window, cx: &mut App| me.update(cx, |mw, cx| mw.close_own_modal(cx)))
                        as std::rc::Rc<dyn Fn(&mut Window, &mut App)>),
                ))
            }
            None => self.slots.iter().rev().find(|s| !s.leaving).and_then(|s| s.page.modal(cx)),
        };
        let want_id = want.as_ref().map(|w| w.0.entity_id());
        for m in &mut self.modals {
            if Some(m.id) != want_id {
                m.leave();
            }
        }
        if let Some((view, entrance, width, dismiss)) = want {
            if !self.modals.iter().any(|m| m.id == view.entity_id() && !m.leaving) {
                self.modals.retain(|m| m.id != view.entity_id());
                self.modals.push(ModalSlot::new(view, entrance, width, dismiss));
            }
        }

        // Advance the route transition; drop pages that finished leaving.
        let mut moving = false;
        for s in &mut self.slots {
            moving |= s.x.tick();
            moving |= s.opacity.tick();
        }
        self.slots.retain(|s| !(s.leaving && s.opacity.get() <= 0.001 && !s.opacity.is_animating()));
        moving |= self.active.tick();
        for m in &mut self.modals {
            moving |= m.tick();
        }
        moving |= self.starting_o.tick() | self.starting_y.tick();
        self.modals.retain(|m| !m.finished());
        if moving {
            window.request_animation_frame();
        }

        let pages: Vec<AnyElement> = self
            .slots
            .iter()
            .map(|s| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(s.x.get()))
                    .w_full()
                    .opacity(s.opacity.get())
                    .child(s.page.view())
                    .into_any_element()
            })
            .collect();

        let content = match &self.gate {
            Some(g) => div().flex_1().min_h(px(0.0)).relative().child(g.clone()),
            None => div().flex_1().min_h(px(0.0)).relative().overflow_hidden().children(pages),
        };

        let active = self.active.get();
        let mut body = div()
            .id("main-body")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            // Mouse Back/Forward side buttons, caught on the way down so no
            // page element can swallow them.
            .capture_any_mouse_down(cx.listener(Self::on_nav_button))
            .size_full()
            .flex()
            .flex_col()
            .relative()
            .font_family(family())
            .text_color(t.text_primary);

        {
            // One header bar on every platform (Linux's design): page title,
            // Back and the page actions, with each OS's own window controls.
            let nav = self.header_nav(cx);
            let mut actions = self.header_actions(&t, cx);
            if on_main && self.update_pill.read(cx).visible(cx) {
                actions.insert(0, self.update_pill.clone().into_any_element());
            }
            body = body.child(
                HeaderBar::new(nav, self.controls_on_right, 0.55 + 0.45 * active)
                    .chrome(HeaderChrome::current(csd))
                    .actions(actions)
                    .on_close(|window, cx| {
                        if main_should_close(cx) {
                            window.remove_window();
                        }
                    }),
            );
        }
        body = body.child(content);
        let win_w = f32::from(window.bounds().size.width) - if csd { 2.0 * WINDOW_MARGIN } else { 0.0 };
        let so = self.starting_o.get();
        if so > 0.001 {
            if let Some(label) = self.starting_label.clone() {
                body = body.child(
                    div()
                        .absolute()
                        .bottom(px(20.0 - self.starting_y.get()))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .opacity(so.min(1.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .px(px(14.0))
                                .py(px(8.0))
                                .rounded(px(10.0))
                                .border_1()
                                .border_color(t.border_default)
                                .bg(t.bg_panel)
                                .text_color(t.text_secondary)
                                .ts(12.0)
                                .shadow(vec![theme::sh(0.0, 4.0, 18.0, 0.0, theme::black(0.18))])
                                .child(crate::ui::spinner::Spinner::new(crate::ui::spinner::SpinnerSize::Sm))
                                .child(format!("Starting {label}…")),
                        ),
                );
            }
        }
        if let Some(sh) = &self.update_sheet {
            body = body.child(sh.clone());
        }
        if let Some((_, sh)) = &self.panel_sheet {
            body = body.child(sh.clone());
        }
        if let Some(sh) = &self.tip_sheet {
            body = body.child(sh.clone());
        }
        if let Some((m, _)) = &self.menu {
            body = body.child(m.clone());
        }
        body = body.children(render_modals(&self.modals, &t, win_w, window_corner_radius(window)));

        if csd {
            linux_frame(body.into_any_element(), &t, active, window)
        } else {
            let root = div().size_full();
            let root = if self.vibrant { root } else { root.bg(t.bg_body) };
            root.child(body).into_any_element()
        }
    }
}

/// The Linux client-side frame: a 40px transparent margin carrying the
/// window's own shadow, and a 480×640 rounded "visible window" inside it
/// (`linuxChrome.ts`). Snapped/tiled windows drop the margin and rounding.
pub fn linux_frame(content: AnyElement, t: &Theme, active: f32, window: &mut Window) -> AnyElement {
    let tiling = match window.window_decorations() {
        Decorations::Client { tiling } => tiling,
        Decorations::Server => Default::default(),
    };
    let tiled = tiling.top || tiling.bottom || tiling.left || tiling.right;
    let margin = if tiled { 0.0 } else { WINDOW_MARGIN };
    window.set_client_inset(px(margin));
    let radius = window_corner_radius(window);
    let lerp = |a: f32, b: f32| a + (b - a) * active;
    let shadows = if tiled {
        vec![]
    } else {
        vec![
            theme::sh(0.0, 0.0, 0.0, 1.0, t.window_border),
            theme::sh(0.0, lerp(1.0, 2.0), lerp(4.0, 8.0), 0.0, theme::black(lerp(0.06, 0.1))),
            theme::sh(0.0, lerp(4.0, 8.0), lerp(16.0, 28.0), 0.0, theme::black(lerp(0.09, 0.14))),
        ]
    };
    div()
        .size_full()
        .p(px(margin))
        .child(
            div()
                .size_full()
                .rounded(px(radius))
                .shadow(shadows)
                .bg(t.bg_body)
                .overflow_hidden()
                .child(content),
        )
        .into_any_element()
}

#[allow(dead_code)]
fn _unused(_: Hsla, _: Point<Pixels>, _: SharedString, _: WindowAppearance) {}


/// `EditorOpenPlaceholder` — shown in the main window while an editor is
/// open. Clicking (or Enter/Space) brings the editor to the front.
pub struct EditorPlaceholder {
    token: String,
    focusing: Anim,
}

impl Render for EditorPlaceholder {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        if self.focusing.tick() {
            window.request_animation_frame();
        }
        let token = self.token.clone();
        div()
            .id("editor-placeholder")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .p(px(24.0))
            .text_center()
            .cursor_pointer()
            .opacity(self.focusing.get())
            .child(Icon::LkEditorWindow.el(40.0, t.text_tertiary))
            .child(div().ts(14.0).w700().text_color(t.text_primary).child("Edit your timelapse in the edit window."))
            .child(div().ts(12.0).text_color(t.text_tertiary).child("Click here to bring it to the front."))
            .on_click(cx.listener(move |p, _, _, cx| {
                p.focusing.set_css(0.6, 150.0, Ease::CSS_EASE);
                crate::windows::editor::focus(&token, cx);
                p.focusing.set_css(1.0, 150.0, Ease::CSS_EASE);
                cx.notify();
            }))
    }
}

/// `lookout-editor-opened`: swap the main window's content for the placeholder.
pub fn editor_opened(token: String, cx: &mut App) {
    if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
        h.update(cx, |mw, _, cx| {
            let route = Route::EditorOpen(token);
            if mw.route != route {
                mw.navigate(route, cx);
            }
        })
        .ok();
    }
}

/// `lookout-edited`: the editor published. Leave the placeholder for the
/// session view (remounted fresh), refresh the gallery, and run the
/// completion hand-off (redirect) when the server says it's complete.
pub fn on_edited(token: String, result: crate::screens::editor::ApplyResult, cx: &mut App) {
    if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
        h.update(cx, |mw, _, cx| {
            mw.slots.retain(|s| s.key != Route::Session(token.clone()).key());
            mw.history.retain(|r| *r != Route::EditorOpen(token.clone()));
            mw.forward.clear();
            mw.go(Route::Session(token.clone()), cx);
            mw.refresh_gallery(cx);
            if result.status.as_deref() == Some("complete") {
                let panel_ok = result.panel_url.as_deref().map(|u| crate::programs::acceptable_endpoint(Some(u))).unwrap_or(false);
                if let (Some(url), false) = (result.redirect_url.clone(), panel_ok) {
                    if mw.redirect_fired.insert(token.clone()) && crate::programs::acceptable_endpoint(Some(&url)) {
                        cx.open_url(&url);
                    }
                }
            }
        })
        .ok();
    }
}


/// `PanelPrompt.tsx` — rendered in the session view's `belowVideo` slot.
pub struct PanelPrompt {
    label: String,
    token: String,
    url: String,
    main: Entity<MainWindow>,
}

impl Render for PanelPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (main, token, url) = (self.main.clone(), self.token.clone(), self.url.clone());
        div()
            .flex()
            .items_start()
            .gap(px(12.0))
            .p(px(12.0))
            .mb(px(16.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(t.border_default)
            .bg(t.bg_surface)
            .child(
                div()
                    .size(px(28.0))
                    .rounded(px(8.0))
                    .bg(t.bg_selected)
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .child(Icon::InfoFill.el(16.0, t.text_secondary)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(div().ts(13.0).w500().text_color(t.text_primary).child(format!("{} needs a few details", self.label)))
                    .child(div().ts_lh(12.0, 1.5).text_color(t.text_tertiary).mt(px(2.0)).child(format!(
                        "You closed this earlier. Your timelapse is saved either way, but {} asked for some information before it can finish up.",
                        self.label
                    )))
                    .child(div().mt(px(12.0)).child(
                        crate::ui::button::Button::new("panel-continue", "Continue")
                            .variant(crate::ui::button::Variant::Secondary)
                            .size(crate::ui::button::BtnSize::Sm)
                            .on_click(move |_, _, cx| {
                                let (token, url) = (token.clone(), url.clone());
                                main.update(cx, |mw, cx| {
                                    mw.pending_panel = Some((token, url));
                                    cx.notify();
                                })
                            }),
                    )),
            )
    }
}

/// The main window's handle when dialogs should be native (macOS/Windows).
fn native_handle(cx: &App) -> Option<gpui_kit::gpui::AnyWindowHandle> {
    if !crate::ui::overlay::native_dialogs() {
        return None;
    }
    cx.try_global::<crate::windows::MainHandle>().map(|h| h.0.into())
}
