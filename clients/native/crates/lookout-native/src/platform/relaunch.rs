//! Picking up where we left off after System Settings' "Quit & Reopen".
//!
//! Turning on a privacy permission (Screen & System Audio Recording) for a
//! running app makes System Settings offer to quit and reopen it. That quit
//! arrives as an ordinary `quit` Apple Event, so we look at who sent it while
//! the app is terminating (`NSAppleEventManager.currentAppleEvent` is still
//! the quit event inside `applicationWillTerminate:`). When it came from
//! System Settings we write a small snapshot of where the user was, and the
//! relaunch reads it back. Any other quit (Cmd-Q, the tray, the Dock, logout)
//! clears the snapshot, so a normal launch always starts fresh.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// System Settings relaunches immediately; a snapshot older than this is a
/// relaunch that never happened (or was declined), so it is ignored.
const MAX_AGE_MS: i64 = 2 * 60 * 1000;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// When it was written (ms since the epoch).
    pub at_ms: i64,
    /// Main window back stack (`Route::key`s), oldest first.
    pub history: Vec<String>,
    /// The route that was showing.
    pub route: String,
    /// Sessions with an editor window open.
    pub editors: Vec<String>,
    /// The quit's sender, for the log.
    pub sender: Option<String>,
}

fn path() -> PathBuf {
    crate::store::config_dir().join("relaunch-restore.json")
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Write the snapshot. Called while terminating, so it is synchronous.
pub fn save(mut snap: Snapshot) {
    snap.at_ms = now_ms();
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_vec(&snap) {
        Ok(bytes) => match std::fs::write(&p, bytes) {
            Ok(()) => log::info!("[relaunch] saved state for reopen: {} (sender {:?})", snap.route, snap.sender),
            Err(e) => log::warn!("[relaunch] couldn't save state: {e}"),
        },
        Err(e) => log::warn!("[relaunch] couldn't encode state: {e}"),
    }
}

/// Drop any snapshot (a normal quit).
pub fn clear() {
    let _ = std::fs::remove_file(path());
}

/// Read and delete the snapshot; `None` unless it is fresh.
pub fn take() -> Option<Snapshot> {
    let p = path();
    let bytes = std::fs::read(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    let snap: Snapshot = serde_json::from_slice(&bytes).ok()?;
    let age = now_ms() - snap.at_ms;
    if !(0..=MAX_AGE_MS).contains(&age) {
        log::info!("[relaunch] ignoring a stale snapshot ({}s old)", age / 1000);
        return None;
    }
    Some(snap)
}

/// Who asked us to quit, when it was another app's `quit` Apple Event.
#[derive(Debug)]
pub struct QuitSender {
    pub pid: i32,
    pub bundle_id: Option<String>,
}

impl QuitSender {
    /// Whether this quit is System Settings applying a privacy permission.
    ///
    /// The Privacy & Security pane runs as an app extension, so the sender
    /// may be System Settings itself (`com.apple.systempreferences`) or one
    /// of its `com.apple.settings.*` extensions. When the sender can't be
    /// named but we sent the user to a privacy pane this run, that's the
    /// only plausible source too. The Dock, loginwindow and scripts that
    /// quit us are left alone.
    pub fn is_permission_relaunch(&self, opened_privacy_pane: bool) -> bool {
        match self.bundle_id.as_deref() {
            Some(id) => {
                id.starts_with("com.apple.systempreferences")
                    || id.starts_with("com.apple.settings")
                    || id == "com.apple.tccd"
                    || (opened_privacy_pane && !matches!(id, "com.apple.dock" | "com.apple.loginwindow"))
            }
            None => opened_privacy_pane,
        }
    }
}

/// The sender of the Apple Event being handled right now, if it is a
/// `quit` from another process. Only meaningful while terminating.
#[cfg(target_os = "macos")]
pub fn current_quit_sender() -> Option<QuitSender> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    const fn four_cc(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }
    const K_CORE_EVENT_CLASS: u32 = four_cc(b"aevt");
    const K_AE_QUIT_APPLICATION: u32 = four_cc(b"quit");
    const KEY_SENDER_PID_ATTR: u32 = four_cc(b"spid");

    let mgr_cls = AnyClass::get(c"NSAppleEventManager")?;
    unsafe {
        let mgr: *mut AnyObject = msg_send![mgr_cls, sharedAppleEventManager];
        if mgr.is_null() {
            return None;
        }
        let ev: *mut AnyObject = msg_send![mgr, currentAppleEvent];
        if ev.is_null() {
            // Our own `terminate:` (Cmd-Q, tray Quit, updater).
            return None;
        }
        let class: u32 = msg_send![ev, eventClass];
        let id: u32 = msg_send![ev, eventID];
        if class != K_CORE_EVENT_CLASS || id != K_AE_QUIT_APPLICATION {
            return None;
        }
        let pid_desc: *mut AnyObject = msg_send![ev, attributeDescriptorForKeyword: KEY_SENDER_PID_ATTR];
        if pid_desc.is_null() {
            return Some(QuitSender { pid: 0, bundle_id: None });
        }
        let pid: i32 = msg_send![pid_desc, int32Value];
        if pid == std::process::id() as i32 {
            return None;
        }
        let bundle_id = AnyClass::get(c"NSRunningApplication").and_then(|cls| {
            let app: *mut AnyObject = msg_send![cls, runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                return None;
            }
            let bid: Option<Retained<NSString>> = msg_send![app, bundleIdentifier];
            bid.map(|s| s.to_string())
        });
        Some(QuitSender { pid, bundle_id })
    }
}

#[cfg(not(target_os = "macos"))]
pub fn current_quit_sender() -> Option<QuitSender> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: Option<&str>) -> QuitSender {
        QuitSender { pid: 1, bundle_id: id.map(str::to_string) }
    }

    #[test]
    fn recognises_system_settings() {
        assert!(s(Some("com.apple.systempreferences")).is_permission_relaunch(false));
        assert!(s(Some("com.apple.settings.PrivacySecurity.extension")).is_permission_relaunch(false));
        assert!(!s(Some("com.apple.dock")).is_permission_relaunch(true));
        assert!(!s(Some("com.apple.loginwindow")).is_permission_relaunch(true));
        assert!(!s(Some("com.apple.ScriptEditor2")).is_permission_relaunch(false));
        assert!(s(Some("com.apple.ScriptEditor2")).is_permission_relaunch(true));
        assert!(s(None).is_permission_relaunch(true));
        assert!(!s(None).is_permission_relaunch(false));
    }
}
