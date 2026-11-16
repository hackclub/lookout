//! What a notification's buttons do. Recorder notices are tagged
//! `recorder:<token>`, so a response knows which timelapse it's about even
//! if another one is open by then.

use gpui_kit::gpui::{App, SystemNotificationAction, SystemNotificationResponse};
use lookout_core::NoticeKind;

use crate::app;

pub const RECORDER_TAG: &str = "recorder:";

const RESUME: &str = "resume";
const OPEN: &str = "open";

/// The buttons for a recorder notice.
pub fn actions(kind: NoticeKind) -> Vec<SystemNotificationAction> {
    let open = SystemNotificationAction { id: OPEN.into(), label: "Open".into() };
    match kind {
        NoticeKind::Paused => vec![SystemNotificationAction { id: RESUME.into(), label: "Resume".into() }, open],
        NoticeKind::CaptureError | NoticeKind::Failed => vec![open],
        NoticeKind::Other => Vec::new(),
    }
}

/// Act on notification clicks for the app's lifetime.
pub fn init(cx: &mut App) {
    cx.on_system_notification_response(handle);
}

fn handle(r: SystemNotificationResponse, cx: &mut App) {
    let Some(token) = r.tag.strip_prefix(RECORDER_TAG).map(String::from) else { return };
    // A button press arrives while the notification center has the app's
    // attention: act once this dispatch has returned.
    cx.defer(move |cx| match r.action_id.as_deref() {
        Some(RESUME) => resume(&token, cx),
        _ => open(&token, cx),
    });
}

/// Resume, as the menu-bar item's Resume does, if that timelapse is still the
/// one open. A lost source needs a new pick, which only the window can do.
fn resume(token: &str, cx: &mut App) {
    let rec = app::state(cx).recorder.read(cx);
    let current = rec.state.as_ref().is_some_and(|s| s.token == token);
    let lost = rec.state.as_ref().is_some_and(|s| s.source_lost.is_some());
    if !current || lost {
        return open(token, cx);
    }
    crate::windows::handle_tray_action(crate::platform::tray::TrayAction::Resume, cx);
}

/// Bring Lookout forward on that timelapse.
fn open(token: &str, cx: &mut App) {
    crate::platform::dock_menu::open_timelapse(token, cx);
}
