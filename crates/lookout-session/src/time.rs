//! Wall-clock helpers: milliseconds since the Unix epoch and the ISO-8601
//! form the server speaks, without pulling in a date-time crate.

pub fn current_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Format milliseconds-since-epoch as `YYYY-MM-DDTHH:MM:SS.sssZ` — what
/// `Date.parse()` and Go's `time.Parse(time.RFC3339)` both accept without
/// surprises.
pub fn unix_ms_to_iso(unix_ms: i64) -> String {
    let total_secs = unix_ms.div_euclid(1_000);
    let millis = unix_ms.rem_euclid(1_000) as u32;

    // Civil date math via days-since-epoch — Howard Hinnant's algorithm.
    let days = total_secs.div_euclid(86_400);
    let time_of_day = total_secs.rem_euclid(86_400) as u32;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Convert days-since-1970-01-01 to civil (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year, month, day, hour, minute, second, millis
    )
}

/// Parse an ISO-8601 timestamp like `2024-09-12T18:34:21.123Z` to milliseconds
/// since the Unix epoch. Returns None on any parse failure. Implementation
/// uses civil-date math (Howard Hinnant) so we don't pull in chrono just
/// for this one call site.
pub fn parse_iso_to_unix_ms(s: &str) -> Option<i64> {
    // Expected layout: YYYY-MM-DDTHH:MM:SS[.fff][Z|+HH:MM|-HH:MM]
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    let second: u32 = s.get(17..19)?.parse().ok()?;
    // Fractional seconds + timezone offset.
    let mut ms: i64 = 0;
    let mut idx = 19;
    if bytes.get(idx).copied() == Some(b'.') {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac = &s[frac_start..idx];
        // Take the first 3 digits as milliseconds, ignore the rest.
        let trimmed: String = frac.chars().take(3).collect();
        let padded = format!("{:0<3}", trimmed); // pad right to 3 chars
        ms = padded.parse().ok()?;
    }
    let mut tz_offset_min: i64 = 0;
    if let Some(&c) = bytes.get(idx) {
        if c == b'Z' {
            // UTC, no offset
        } else if c == b'+' || c == b'-' {
            let sign: i64 = if c == b'+' { 1 } else { -1 };
            let h: i64 = s.get(idx + 1..idx + 3)?.parse().ok()?;
            let m: i64 = if bytes.get(idx + 3) == Some(&b':') {
                s.get(idx + 4..idx + 6)?.parse().ok()?
            } else {
                s.get(idx + 3..idx + 5)?.parse().ok()?
            };
            tz_offset_min = sign * (h * 60 + m);
        }
    }

    // Civil date → days-since-epoch (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    let total_secs =
        days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64;
    Some((total_secs - tz_offset_min * 60) * 1000 + ms)
}
