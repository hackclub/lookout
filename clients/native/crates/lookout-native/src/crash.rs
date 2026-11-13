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
    // Also used by the crash reporter process (it clones these options).
    options.before_send = Some(std::sync::Arc::new(soften_reporter_startup));
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
    use sentry::integrations::log::{breadcrumb_from_record, event_from_record, RecordMapping, SentryLogger};
    SentryLogger::with_dest(dest).mapper(|record: &log::Record<'_>| match record.level() {
        log::Level::Error if is_downgraded_error(record.target(), &record.args().to_string()) => {
            let mut b = breadcrumb_from_record(record);
            b.level = sentry::Level::Warning;
            RecordMapping::Breadcrumb(b)
        }
        log::Level::Error => RecordMapping::Event(Box::new(event_from_record(record))),
        log::Level::Warn | log::Level::Info => RecordMapping::Breadcrumb(breadcrumb_from_record(record)),
        log::Level::Debug | log::Level::Trace => RecordMapping::Ignore,
    })
}

/// `error!` lines that describe a normal failure or the machine rather than
/// a bug: kept as warning breadcrumbs instead of Sentry issues.
fn is_downgraded_error(target: &str, message: &str) -> bool {
    (target.starts_with("xcap::") && is_expected_dependency_error(message)) || is_environment_noise(target, message)
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

/// `error!` lines from dependencies we don't vendor that describe the
/// machine rather than a bug, and can't be fixed from here: kept as
/// breadcrumbs instead of Sentry issues.
fn is_environment_noise(target: &str, message: &str) -> bool {
    // X servers with no pointer that has scroll axes (WSLg, Xvfb, VNC):
    // GPUI just has no smooth scrolling to set up (LOOKOUT-NATIVE-R).
    (target.starts_with("gpui_linux::linux::x11") && message.starts_with("Found no xinput mouse pointers"))
        // The Vulkan loader with no driver installed; wgpu falls back to GL,
        // and if nothing can draw, the startup dialog says so.
        || (target.starts_with("wgpu_hal::vulkan") && message.contains("Found no drivers"))
}

/// The crash reporter is a second process that listens on a socket in the
/// temp folder. When Windows refuses that socket (WSAEADDRINUSE on a fresh,
/// random path — antivirus or a temp folder that can't hold AF_UNIX sockets,
/// LOOKOUT-NATIVE-11), the app runs on without native-crash reports; panics
/// and errors still go straight from the app. Worth knowing about, not an
/// error: send it as a warning.
fn soften_reporter_startup(mut event: sentry::protocol::Event<'static>) -> Option<sentry::protocol::Event<'static>> {
    let from_reporter = event.extra.get("event.process").and_then(|v| v.as_str()) == Some("crash-reporter");
    let create_server = event.exception.values.iter().any(|e| {
        e.ty == "CreateServer" || e.value.as_deref().is_some_and(|v| v.starts_with("Failed to create server with socket name"))
    });
    if from_reporter && create_server {
        event.level = sentry::Level::Warning;
        event.tags.insert("crash_reporter".into(), "unavailable".into());
    }
    Some(event)
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
    use super::*;
    use super::is_expected_dependency_error as expected;
    use sentry::protocol::{Event, Exception, Value};

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

    #[test]
    fn known_environment_errors_are_noise() {
        assert!(is_environment_noise("gpui_linux::linux::x11::client", "Found no xinput mouse pointers."));
        assert!(is_environment_noise(
            "wgpu_hal::vulkan::instance",
            "GENERAL [Loader Message (0x0)]\n\tvkCreateInstance: Found no drivers!"
        ));
        assert!(!is_environment_noise("lookout::screens::source_picker", "Found no xinput mouse pointers."));
    }

    #[test]
    fn reporter_socket_failure_is_a_warning() {
        let mut e = Event { level: sentry::Level::Error, ..Default::default() };
        e.extra.insert("event.process".into(), Value::String("crash-reporter".into()));
        e.exception.values.push(Exception { ty: "CreateServer".into(), ..Default::default() });
        assert_eq!(soften_reporter_startup(e).unwrap().level, sentry::Level::Warning);

        // The same failure reported by the app itself is left alone.
        let mut e = Event { level: sentry::Level::Error, ..Default::default() };
        e.exception.values.push(Exception { ty: "CreateServer".into(), ..Default::default() });
        assert_eq!(soften_reporter_startup(e).unwrap().level, sentry::Level::Error);
    }
}
