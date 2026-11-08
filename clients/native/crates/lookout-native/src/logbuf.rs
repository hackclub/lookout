//! The debug log ring buffer (`logger.ts`): every log line goes to stderr
//! as usual and the last 500 are kept in memory, so "Copy report" can hand
//! support a useful transcript.

use std::collections::VecDeque;
use std::sync::Mutex;

use log::{Level, Log, Metadata, Record};

struct Entry {
    time: chrono::DateTime<chrono::Local>,
    level: Level,
    message: String,
}

static ENTRIES: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

struct Tee {
    inner: env_logger::Logger,
}

impl Log for Tee {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.inner.matches(record) {
            return;
        }
        self.inner.log(record);
        // Only our own crates and the engine go into the support buffer.
        let target = record.target();
        if target.starts_with("lookout") {
            if let Ok(mut e) = ENTRIES.lock() {
                if e.len() >= 500 {
                    e.pop_front();
                }
                e.push_back(Entry { time: chrono::Local::now(), level: record.level(), message: record.args().to_string() });
            }
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

pub fn init() {
    let inner = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn,naga=warn,wgpu_core=warn,wgpu_hal=warn"),
    )
    .build();
    let max = inner.filter();
    if log::set_boxed_logger(Box::new(crate::crash::wrap_logger(Tee { inner }))).is_ok() {
        log::set_max_level(max);
    }
}

/// `getReport(headline)`.
pub fn report(headline: &str) -> String {
    let mut lines = Vec::new();
    if !headline.is_empty() {
        lines.push(format!("Error: {headline}"));
        lines.push(String::new());
    }
    lines.push(format!(
        "Lookout Desktop v{} (native) | {} {} | {}",
        crate::app::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    ));
    lines.push("---".into());
    if let Ok(e) = ENTRIES.lock() {
        for entry in e.iter().rev().take(100).collect::<Vec<_>>().into_iter().rev() {
            lines.push(format!(
                "{} {:<5} {}",
                entry.time.format("%H:%M:%S"),
                entry.level.as_str().to_uppercase(),
                entry.message
            ));
        }
    }
    lines.join("\n")
}
