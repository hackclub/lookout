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
    sentry::integrations::log::SentryLogger::with_dest(dest)
}

/// `LOOKOUT_CRASH_TEST=segfault|abort|panic|error` fails on purpose right
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
        other => log::warn!("[crash] unknown LOOKOUT_CRASH_TEST {other:?}"),
    }
}
