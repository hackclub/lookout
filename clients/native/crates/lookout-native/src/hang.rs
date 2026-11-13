//! App-hang watchdog: reports a main thread that stops running tasks.
//!
//! A frozen app ("Not Responding" on Windows) never reaches the panic hook or
//! the crash reporter, so Sentry would see nothing. A task on the main thread
//! bumps a heartbeat every 250 ms; a plain thread watches it, and once it is
//! [`HANG_AFTER`] stale it sends an error event. On Windows the event carries
//! a minidump of the whole process, so Sentry shows every thread's stack at
//! the moment of the hang (the main thread's id is the `main_thread_id` tag).
//! Recovering adds a breadcrumb with how long it lasted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui_kit::gpui::App;

const BEAT: Duration = Duration::from_millis(250);
const CHECK: Duration = Duration::from_millis(500);
const HANG_AFTER: Duration = Duration::from_secs(5);
/// A few per run: one hang usually repeats, and each Windows report is a dump.
const MAX_REPORTS: u32 = 3;

static START: OnceLock<Instant> = OnceLock::new();
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn start(cx: &mut App) {
    LAST_BEAT_MS.store(now_ms(), Ordering::Release);
    cx.spawn(async move |cx| loop {
        LAST_BEAT_MS.store(now_ms(), Ordering::Release);
        cx.background_executor().timer(BEAT).await;
    })
    .detach();

    #[cfg(windows)]
    let main_thread = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    #[cfg(not(windows))]
    let main_thread = 0u32;

    let _ = std::thread::Builder::new().name("lookout-hang-watchdog".into()).spawn(move || watch(main_thread));

    // `LOOKOUT_CRASH_TEST=hang`: block the main thread for 8s after startup.
    if std::env::var("LOOKOUT_CRASH_TEST").as_deref() == Ok("hang") {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            log::warn!("[crash] LOOKOUT_CRASH_TEST=hang: blocking the main thread for 8s");
            std::thread::sleep(Duration::from_secs(8));
        })
        .detach();
    }
}

fn watch(main_thread: u32) {
    let mut reports = 0;
    let mut hang_reported_at: Option<u64> = None;
    let mut last_check = now_ms();
    // A sleep that overran by seconds means the machine was asleep (or this
    // process was suspended), not that the main thread hung: start counting
    // again from the wake.
    let mut ignore_before = 0u64;
    loop {
        std::thread::sleep(CHECK);
        let now = now_ms();
        if now.saturating_sub(last_check) > CHECK.as_millis() as u64 + 2000 {
            ignore_before = now;
        }
        last_check = now;

        let beat = LAST_BEAT_MS.load(Ordering::Acquire);
        let since = beat.max(ignore_before);
        let stale = now.saturating_sub(since);
        if stale >= HANG_AFTER.as_millis() as u64 {
            if hang_reported_at.is_none() {
                hang_reported_at = Some(since);
                if reports < MAX_REPORTS {
                    reports += 1;
                    report(stale, beat, main_thread);
                }
            }
        } else if let Some(began) = hang_reported_at.take() {
            log::warn!("[hang] main thread responsive again after {:.1}s", beat.saturating_sub(began) as f64 / 1000.0);
        }
    }
}

fn report(stale_ms: u64, beat: u64, main_thread: u32) {
    let message = format!("App hang: main thread blocked for {:.1}s", stale_ms as f64 / 1000.0);
    log::warn!("[hang] {message}");
    if sentry::Hub::current().client().is_none() {
        return;
    }
    let dump = minidump();
    // Some Windows dumps show the main thread idle in its message loop: the
    // hang ended while the dump was being written. Say so, so those aren't
    // mistaken for a stuck message loop.
    let over = LAST_BEAT_MS.load(Ordering::Acquire) != beat;
    sentry::with_scope(
        |scope| {
            scope.set_tag("hang", "true");
            scope.set_tag("main_thread_id", main_thread.to_string());
            scope.set_tag("hang_over_at_dump", over.to_string());
            if let Some(buffer) = dump {
                scope.add_attachment(sentry::protocol::Attachment {
                    buffer,
                    filename: "hang.dmp".into(),
                    content_type: Some("application/octet-stream".into()),
                    ty: Some(sentry::protocol::AttachmentType::Minidump),
                });
            }
        },
        || sentry::capture_message(&message, sentry::Level::Error),
    );
}

/// A minidump of this process (all threads' stacks), written from this
/// watchdog thread while the main thread is stuck.
#[cfg(windows)]
fn minidump() -> Option<Vec<u8>> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Diagnostics::Debug::{MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWriteDump};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};

    let path = std::env::temp_dir().join(format!("lookout-hang-{}.dmp", std::process::id()));
    let result = (|| -> anyhow::Result<Vec<u8>> {
        let file = std::fs::File::create(&path)?;
        unsafe {
            MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                HANDLE(file.as_raw_handle()),
                MiniDumpNormal | MiniDumpWithThreadInfo,
                None,
                None,
                None,
            )?;
        }
        drop(file);
        Ok(std::fs::read(&path)?)
    })();
    let _ = std::fs::remove_file(&path);
    match result {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            log::warn!("[hang] couldn't write a minidump: {e:#}");
            None
        }
    }
}

#[cfg(not(windows))]
fn minidump() -> Option<Vec<u8>> {
    None
}
