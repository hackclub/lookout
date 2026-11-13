//! Crash and error reporting to Sentry (`hack-club/lookout-native`).
//!
//! Three layers, so a failure is reported however low it happens:
//! - **Native crashes** (segfault, abort, illegal instruction, Windows access
//!   violation, a fault inside the Swift tray, WebKitGTK, GStreamer, Media
//!   Foundation, the GPU driver…) never reach Rust's panic handler. The
//!   minidump integration runs this binary a second time as an out-of-process
//!   crash reporter; when the app dies it writes a minidump of the app and
//!   sends it as a fatal event.
//! - **Panics** on any thread, with a backtrace.
//! - **`log::error!`** from anywhere (our code, GPUI, wgpu/blade, zbus…)
//!   becomes an event; `warn`/`info` lines become breadcrumbs on it.
//!
//! Release builds report by default. Debug builds only report when
//! `LOOKOUT_SENTRY_DSN` is set; setting it to an empty string turns reporting
//! off in any build.

use sentry::integrations::minidump::MinidumpIntegration;

/// The public DSN of `hack-club/lookout-native` (DSNs are not secrets). A
/// build-time `LOOKOUT_NATIVE_SENTRY_DSN` wins, so CI can point builds
/// elsewhere. (Not `SENTRY_DSN`: that one is the Tauri app's project.)
const DEFAULT_DSN: &str = "https://2be394a3d86e29da25b8cccd5616daf2@o40609.ingest.us.sentry.io/4512394316677120";

fn dsn() -> Option<String> {
    if let Some(v) = std::env::var_os("LOOKOUT_SENTRY_DSN") {
        let v = v.to_string_lossy().trim().to_string();
        return (!v.is_empty()).then_some(v);
    }
    if cfg!(debug_assertions) {
        return None;
    }
    Some(option_env!("LOOKOUT_NATIVE_SENTRY_DSN").filter(|d| !d.is_empty()).unwrap_or(DEFAULT_DSN).to_string())
}

/// Whether this process is the crash reporter started by [`init`]. It must
/// reach [`init`] (which never returns for it) before doing anything with
/// app state: single-instance locks, updates, windows.
pub fn is_reporter_process() -> bool {
    MinidumpIntegration::new().is_crash_reporter_process()
}

/// Starts reporting. Keep the guard alive until exit so events flush.
pub fn init() -> Option<sentry::ClientInitGuard> {
    let dsn = dsn()?;
    // Sentry's HTTP client is built with no rustls crypto provider; use ring,
    // the one our own reqwest already links. Err = already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let minidump = MinidumpIntegration::new()
        .process_name("Lookout Crash Reporter")
        // Don't hand the reporter the `lookout://` links this launch got.
        .inherit_args(false);

    let mut options = sentry::ClientOptions::new()
        .dsn(dsn.as_str())
        .add_integration(minidump)
        .sample_rate(1.0);
    options.release = sentry::release_name!();
    options.environment = Some(if cfg!(debug_assertions) { "development" } else { "production" }.into());
    options.attach_stacktrace = true;
    // Release health: crash-free sessions per version.
    options.auto_session_tracking = true;
    // Never the user's name/email/IP; the minidump is processed and discarded.
    options.send_default_pii = false;
    let guard = sentry::init(options);

    sentry::configure_scope(|scope| {
        scope.set_tag("app", "lookout-native");
    });
    // The reporter process can't see our scope; send it the same tags.
    sentry::with_integration(|m: &MinidumpIntegration, _| {
        m.set_tag("app".into(), Some("lookout-native".into()));
    });
    log::info!("[crash] reporting to Sentry ({})", if cfg!(debug_assertions) { "development" } else { "production" });
    Some(guard)
}

/// Wraps the app logger so `error!` lines become Sentry events and the
/// lines before them become breadcrumbs. A no-op until [`init`] binds a
/// client.
pub fn wrap_logger<L: log::Log>(dest: L) -> impl log::Log {
    use sentry::integrations::log::{breadcrumb_from_record, event_from_record, RecordMapping};
    sentry::integrations::log::SentryLogger::with_dest(dest).mapper(|record| match record.level() {
        log::Level::Error => {
            if record.target().starts_with("xcap::") && is_expected_dependency_error(&record.args().to_string()) {
                let mut crumb = breadcrumb_from_record(record);
                crumb.level = sentry::Level::Warning;
                RecordMapping::Breadcrumb(crumb)
            } else {
                RecordMapping::Event(Box::new(event_from_record(record)))
            }
        }
        log::Level::Warn | log::Level::Info => RecordMapping::Breadcrumb(breadcrumb_from_record(record)),
        log::Level::Debug | log::Level::Trace => RecordMapping::Ignore,
    })
}

/// xcap's Windows backend logs these at error level, but they're normal
/// failures, so they become breadcrumbs instead of Sentry issues. Matched
/// on the call and the exact Win32 error, so a different failure of the
/// same call still reports.
/// - `ReleaseDC` → ERROR_INVALID_WINDOW_HANDLE (1400): the window being
///   captured closed mid-capture; its DC went with it (LOOKOUT-NATIVE-S).
/// - `GetModuleBaseNameW` → ACCESS_DENIED (5) and `GetFileVersionInfoSizeW`
///   → ERROR_RESOURCE_TYPE_NOT_FOUND (1813), from `Window::app_name`. We
///   name apps ourselves (`lookout_core`'s `win_app_name`), so these only
///   matter if something calls xcap's again (LOOKOUT-NATIVE-H, -M, -X).
fn is_expected_dependency_error(message: &str) -> bool {
    const EXPECTED: &[(&str, &str)] = &[
        ("ReleaseDC(", "WIN32_ERROR(1400)"),
        ("GetModuleBaseNameW(", "WIN32_ERROR(5)"),
        ("GetFileVersionInfoSizeW(", "WIN32_ERROR(1813)"),
    ];
    EXPECTED.iter().any(|(call, code)| message.starts_with(call) && message.ends_with(code))
}

/// `LOOKOUT_CRASH_TEST=segfault|abort|panic|error` (and `hang`, see
/// `hang.rs`) fails on purpose right
/// after startup, to check each reporting layer end to end on a real machine.
pub fn self_test() {
    let Ok(kind) = std::env::var("LOOKOUT_CRASH_TEST") else { return };
    log::warn!("[crash] LOOKOUT_CRASH_TEST={kind}");
    match kind.as_str() {
        // A real invalid read, not a raised signal, so it looks like the bug it tests.
        "segfault" => unsafe {
            std::ptr::read_volatile(std::ptr::without_provenance::<u8>(8));
        },
        "abort" => std::process::abort(),
        "panic" => panic!("LOOKOUT_CRASH_TEST panic"),
        "error" => log::error!("[crash] LOOKOUT_CRASH_TEST error"),
        "hang" => {}
        other => log::warn!("[crash] unknown LOOKOUT_CRASH_TEST {other:?}"),
    }
}

static QUIT_REASON: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// Records that the user asked to quit (closed the main window, chose
/// Quit…), so [`on_exit`] can tell that exit from one nobody asked for.
pub fn note_quit(reason: &'static str) {
    log::info!("[app] quit: {reason}");
    *QUIT_REASON.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
}

/// Call once `app.run` has returned. An exit nobody asked for (the app
/// "just closed") is reported, with the lines before it as breadcrumbs.
pub fn on_exit() {
    match *QUIT_REASON.lock().unwrap_or_else(|e| e.into_inner()) {
        Some(reason) => log::info!("[app] exiting ({reason})"),
        None => log::error!("[app] Lookout quit without being asked to"),
    }
}

#[cfg(test)]
mod tests {
    use super::is_expected_dependency_error as expected;

    #[test]
    fn only_known_xcap_failures_are_downgraded() {
        // Messages exactly as Sentry received them.
        assert!(expected("ReleaseDC(HDC(0xffffffff8c010b24)) failed: WIN32_ERROR(1400)"));
        assert!(expected("GetModuleBaseNameW(HANDLE(0x900)) failed: WIN32_ERROR(5)"));
        assert!(expected("GetFileVersionInfoSizeW(PCWSTR(0x49b5ffe898)) failed: WIN32_ERROR(1813)"));
        // Other codes from the same calls still report.
        assert!(!expected("ReleaseDC(HDC(0x1)) failed: WIN32_ERROR(6)"));
        assert!(!expected("GetModuleBaseNameW(HANDLE(0x900)) failed: WIN32_ERROR(50)"));
        assert!(!expected("GetFileVersionInfoSizeW(PCWSTR(0x1)) failed: WIN32_ERROR(18130)"));
        assert!(!expected("DeleteDC(HDC(0x1)) failed: WIN32_ERROR(1400)"));
    }
}
