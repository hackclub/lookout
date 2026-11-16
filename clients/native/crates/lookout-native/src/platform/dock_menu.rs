//! macOS: the Dock icon's menu. While a timelapse is open: the timelapse,
//! Pause/Resume and Stop (what the menu-bar item offers). Always: Start
//! Timelapse, then the five most recent timelapses.
//!
//! macOS asks for the menu when it's clicked, but GPUI hands over a menu
//! built ahead of time, so [`sync`] rebuilds it whenever its contents
//! change: on recorder updates and when the gallery refreshes.

use gpui_kit::gpui;
use gpui_kit::gpui::{App, Global, MenuItem, actions};

use crate::app::{self, store};
use crate::platform::tray::TrayAction;
use crate::windows::main_window::Route;

actions!(lookout_dock, [OpenRecording, PauseRecording, ResumeRecording, StopRecording]);

/// Open one of the recent timelapses.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = lookout_dock, no_json)]
pub struct OpenTimelapse {
    pub token: String,
}

const RECENT: usize = 5;

#[derive(Clone, PartialEq, Debug)]
enum Entry {
    Open(String),
    Pause,
    Resume,
    Stop,
    Start,
    Recent { token: String, label: String },
}

#[derive(Default)]
struct Shown(Option<Vec<Entry>>);
impl Global for Shown {}

pub fn init(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.set_global(Shown::default());
    cx.on_action(|_: &OpenRecording, cx| cx.defer(|cx| crate::windows::handle_tray_action(TrayAction::Open, cx)));
    cx.on_action(|_: &PauseRecording, cx| crate::windows::handle_tray_action(TrayAction::Pause, cx));
    cx.on_action(|_: &ResumeRecording, cx| crate::windows::handle_tray_action(TrayAction::Resume, cx));
    cx.on_action(|_: &StopRecording, cx| cx.defer(|cx| crate::windows::handle_tray_action(TrayAction::Stop, cx)));
    cx.on_action(|a: &OpenTimelapse, cx| {
        let token = a.token.clone();
        cx.defer(move |cx| open_timelapse(&token, cx));
    });
    sync(cx);
}

/// Rebuild the menu if what it should show changed.
pub fn sync(cx: &mut App) {
    if !cx.has_global::<Shown>() {
        return;
    }
    let entries = entries(cx);
    if cx.global::<Shown>().0.as_ref() == Some(&entries) {
        return;
    }
    let items: Vec<MenuItem> = entries
        .iter()
        .enumerate()
        .flat_map(|(i, e)| {
            // A separator between the recorder's group, Start, and the recents.
            let sep = i > 0 && matches!((&entries[i - 1], e), (Entry::Stop, Entry::Start) | (Entry::Start, Entry::Recent { .. }));
            let item = match e {
                Entry::Open(name) => MenuItem::action(format!("Show “{name}”"), OpenRecording),
                Entry::Pause => MenuItem::action("Pause Recording", PauseRecording),
                Entry::Resume => MenuItem::action("Resume Recording", ResumeRecording),
                Entry::Stop => MenuItem::action("Stop Recording", StopRecording),
                Entry::Start => MenuItem::action("Start Timelapse", crate::platform::app_menu::StartTimelapse),
                Entry::Recent { token, label } => MenuItem::action(label.clone(), OpenTimelapse { token: token.clone() }),
            };
            sep.then(MenuItem::separator).into_iter().chain(std::iter::once(item))
        })
        .collect();
    cx.set_dock_menu(items);
    cx.global_mut::<Shown>().0 = Some(entries);
}

fn entries(cx: &App) -> Vec<Entry> {
    use lookout_core::RecorderPhase;
    let mut out = Vec::new();
    let rec = app::state(cx).recorder.read(cx);
    let live = rec.state.as_ref().filter(|s| {
        matches!(s.phase, RecorderPhase::Recording | RecorderPhase::Paused | RecorderPhase::Pausing | RecorderPhase::Resuming)
    });
    if let Some(s) = live {
        out.push(Entry::Open(if s.session_name.trim().is_empty() { "Timelapse".into() } else { s.session_name.clone() }));
        out.push(if rec.paused() { Entry::Resume } else { Entry::Pause });
        out.push(Entry::Stop);
    }
    out.push(Entry::Start);
    let open = live.map(|s| s.token.clone());
    let st = store(cx);
    for token in st.active_tokens().into_iter().filter(|t| Some(t) != open.as_ref()).take(RECENT) {
        let name = st.gallery_cache.get(&token).map(|c| c.summary.name.trim().to_string()).unwrap_or_default();
        let label = if name.is_empty() { "Untitled timelapse".to_string() } else { name };
        out.push(Entry::Recent { token, label });
    }
    out
}

/// Open a timelapse the way the gallery does: the recorder while it's live,
/// its page once it's done (or failed). Also the notifications' Open.
pub fn open_timelapse(token: &str, cx: &mut App) {
    let status = {
        let rec = app::state(cx).recorder.read(cx);
        rec.state.as_ref().filter(|s| s.token == token).and_then(|s| s.session_status.clone())
    }
    .or_else(|| store(cx).gallery_cache.get(token).map(|c| c.summary.status.clone()))
    .unwrap_or_default();
    cx.activate(true);
    crate::windows::focus_main(cx);
    if let Some(h) = cx.try_global::<crate::windows::MainHandle>().map(|h| h.0) {
        let route = if matches!(status.as_str(), "" | "pending" | "active" | "paused") {
            Route::Record(token.to_string())
        } else {
            Route::Session(token.to_string())
        };
        let _ = h.update(cx, |mw, _, cx| {
            mw.navigate(route, cx);
            mw.focus_requested = true;
        });
    }
}
