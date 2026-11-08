//! Text formatting shared by the screens — ports of the TS helpers so the
//! strings match character for character.

use chrono::{Datelike, Local, TimeZone};

/// `formatTime` (useSessionTimer.ts): `h:mm:ss` or `m:ss`.
pub fn format_time(total: i64) -> String {
    let total = total.max(0);
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// `formatTrackedTime`: `1h 5min` / `1h` / `5min` / `< 1min`.
pub fn format_tracked(total: f64) -> String {
    let total = total.max(0.0) as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    if h > 0 && m > 0 {
        format!("{h}h {m}min")
    } else if h > 0 {
        format!("{h}h")
    } else if m > 0 {
        format!("{m}min")
    } else {
        "< 1min".into()
    }
}

/// The tray clock text (`ticker.rs`): `h:mm:ss` or `mm:ss`.
pub fn tray_time(total: i64) -> String {
    let total = total.max(0);
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn parse(iso: &str) -> Option<chrono::DateTime<Local>> {
    chrono::DateTime::parse_from_rfc3339(iso).ok().map(|d| d.with_timezone(&Local))
}

/// `toLocaleDateString(undefined, {month:"short", day:"numeric", year?})`
/// in the en-US shape the webview produced ("Nov 7" / "Nov 7, 2025").
pub fn short_date(iso: &str, always_year: bool) -> String {
    let Some(d) = parse(iso) else { return String::new() };
    let now = Local::now();
    let m = MONTHS[d.month0() as usize];
    if always_year || d.year() != now.year() {
        format!("{m} {}, {}", d.day(), d.year())
    } else {
        format!("{m} {}", d.day())
    }
}

/// `toLocaleDateString()` with defaults (en-US numeric "11/7/2026").
pub fn numeric_date(iso: &str) -> String {
    parse(iso).map(|d| format!("{}/{}/{}", d.month(), d.day(), d.year())).unwrap_or_default()
}

/// `toLocaleTimeString(undefined, {hour:"numeric", minute:"2-digit"})` → "3:42 PM".
pub fn clock_time_ms(unix_ms: i64) -> String {
    let Some(d) = Local.timestamp_millis_opt(unix_ms).single() else { return String::new() };
    let (pm, h12) = d.hour12();
    format!("{}:{:02} {}", h12, d.minute(), if pm { "PM" } else { "AM" })
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(format_time(7), "0:07");
        assert_eq!(format_time(750), "12:30");
        assert_eq!(format_time(3723), "1:02:03");
        assert_eq!(tray_time(307), "05:07");
        assert_eq!(format_tracked(30.0), "< 1min");
        assert_eq!(format_tracked(3900.0), "1h 5min");
        assert_eq!(format_tracked(7200.0), "2h");
    }
}
