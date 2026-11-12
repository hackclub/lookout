//! Diagnostic lines from the shared crates. Native hosts get them on
//! stderr; a host with no stderr (the browser) installs a sink once.

use std::sync::OnceLock;

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Route every diagnostic line through `sink` from now on. Only the first
/// call takes effect.
pub fn set_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// Emit one diagnostic line. Never panics: a closed stderr (launched from
/// a terminal that has since gone away) drops the line instead of taking
/// the app down the way `eprintln!` would.
pub fn line(message: &str) {
    match SINK.get() {
        Some(sink) => sink(message),
        None => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "{message}");
        }
    }
}

/// `eprintln!` that a host can redirect — see [`set_sink`].
#[macro_export]
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}
