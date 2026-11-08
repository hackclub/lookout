//! `editorMath.ts` + the cut helpers from `packages/shared/src/cuts.ts`.
//! One video second = one capture unit = one real-world minute.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SCREENSHOT_INTERVAL_MS: i64 = 60_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoUnit {
    pub captured_at: String,
    #[serde(default)]
    pub screenshot_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CutInterval {
    pub start: String,
    pub end: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: i64,
    pub end: i64,
}

pub fn parse_ms(iso: &str) -> i64 {
    DateTime::parse_from_rfc3339(iso).map(|d| d.timestamp_millis()).unwrap_or(0)
}

fn iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

pub fn unit_at_time(t: f64, n: usize) -> i64 {
    (t.floor() as i64).clamp(0, (n as i64 - 1).max(0))
}

pub fn is_cut_at(ms: i64, cuts: &[CutInterval]) -> bool {
    cuts.iter().any(|c| ms >= parse_ms(&c.start) && ms < parse_ms(&c.end))
}

pub fn regions_to_cuts(regions: &[Region], units: &[VideoUnit]) -> Vec<CutInterval> {
    regions
        .iter()
        .filter(|r| r.end > r.start && (r.end as usize) <= units.len())
        .map(|r| {
            let last_cut = parse_ms(&units[(r.end - 1) as usize].captured_at);
            let next_kept = (r.end as usize) < units.len();
            let end = if next_kept {
                parse_ms(&units[r.end as usize].captured_at).min(last_cut + SCREENSHOT_INTERVAL_MS)
            } else {
                last_cut + SCREENSHOT_INTERVAL_MS
            };
            CutInterval { start: units[r.start as usize].captured_at.clone(), end: iso(end) }
        })
        .collect()
}

pub fn cuts_to_regions(cuts: &[CutInterval], units: &[VideoUnit]) -> Vec<Region> {
    let mut out = Vec::new();
    let mut open: Option<Region> = None;
    for (i, u) in units.iter().enumerate() {
        let cut = is_cut_at(parse_ms(&u.captured_at), cuts);
        if cut {
            match &mut open {
                Some(r) => r.end = i as i64 + 1,
                None => open = Some(Region { start: i as i64, end: i as i64 + 1 }),
            }
        } else if let Some(r) = open.take() {
            out.push(r);
        }
    }
    if let Some(r) = open {
        out.push(r);
    }
    out
}

/// Sort and merge overlapping **or adjacent** regions.
pub fn normalize(regions: &[Region]) -> Vec<Region> {
    let mut sorted: Vec<Region> = regions.iter().copied().filter(|r| r.end > r.start).collect();
    sorted.sort_by_key(|r| r.start);
    let mut merged: Vec<Region> = Vec::new();
    for r in sorted {
        if let Some(last) = merged.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        merged.push(r);
    }
    merged
}

pub fn region_at_time(t: f64, regions: &[Region]) -> Option<Region> {
    regions.iter().copied().find(|r| t >= r.start as f64 && t < r.end as f64)
}

pub fn gap_indices(units: &[VideoUnit]) -> Vec<usize> {
    (1..units.len())
        .filter(|&i| parse_ms(&units[i].captured_at) - parse_ms(&units[i - 1].captured_at) > SCREENSHOT_INTERVAL_MS * 3 / 2)
        .collect()
}

pub fn elapsed_label(unit: f64, total: usize) -> String {
    let m = unit.round().max(0.0) as i64;
    if total < 60 {
        return format!("{m}m");
    }
    format!("{}:{:02}", m / 60, m % 60)
}

const NICE_STEPS: [i64; 12] = [1, 2, 5, 10, 15, 20, 30, 60, 120, 180, 360, 720];

pub fn ruler_step(n: usize, track_w: f32) -> i64 {
    if n == 0 || track_w <= 0.0 {
        return 1;
    }
    let px_per_unit = track_w / n as f32;
    let needed = 88.0 / px_per_unit;
    NICE_STEPS.iter().copied().find(|s| *s as f32 >= needed).unwrap_or(720)
}

pub fn ruler_ticks(n: usize, step: i64) -> Vec<(f64, bool)> {
    let mut out = Vec::new();
    if n == 0 || step <= 0 {
        return out;
    }
    let half = step as f64 / 2.0;
    let mut u = 0.0;
    while u <= n as f64 + 1e-9 {
        let major = ((u / step as f64) - (u / step as f64).round()).abs() < 1e-9;
        out.push((u, major));
        u += half;
    }
    out
}

pub fn count_cut_units(units: &[VideoUnit], cuts: &[CutInterval]) -> usize {
    units.iter().filter(|u| is_cut_at(parse_ms(&u.captured_at), cuts)).count()
}

/// `compileEstimateMs` / `estimateBuildProgress` / `interpolateBuildProgress`.
pub mod build_progress {
    pub const CAP: f32 = 0.95;
    pub fn estimate_ms(units: usize) -> f32 {
        6000.0 + units as f32 * 90.0
    }
    pub fn estimate(elapsed_ms: f32, est_ms: f32) -> f32 {
        1.0 - (-2.2 * elapsed_ms / est_ms.max(1.0)).exp()
    }
    pub fn interpolate(anchor: f32, since_ms: f32, est_ms: f32) -> f32 {
        let from = anchor.clamp(0.0, CAP);
        let budget = (CAP - from).min(2000.0 / est_ms.max(1.0));
        from + budget * (1.0 - (-2.2 * since_ms / 2000.0).exp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(n: usize) -> Vec<VideoUnit> {
        (0..n)
            .map(|i| VideoUnit { captured_at: iso(1_700_000_000_000 + i as i64 * 60_000), screenshot_id: i.to_string() })
            .collect()
    }

    #[test]
    fn regions_round_trip() {
        let u = units(10);
        let regions = vec![Region { start: 2, end: 4 }, Region { start: 7, end: 10 }];
        let cuts = regions_to_cuts(&regions, &u);
        assert_eq!(cuts_to_regions(&cuts, &u), regions);
        assert_eq!(count_cut_units(&u, &cuts), 5);
    }

    #[test]
    fn normalize_merges_adjacent() {
        let r = normalize(&[Region { start: 5, end: 6 }, Region { start: 1, end: 3 }, Region { start: 3, end: 4 }]);
        assert_eq!(r, vec![Region { start: 1, end: 4 }, Region { start: 5, end: 6 }]);
    }

    #[test]
    fn ruler() {
        assert_eq!(ruler_step(60, 600.0), 10);
        assert_eq!(elapsed_label(12.0, 47), "12m");
        assert_eq!(elapsed_label(75.0, 120), "1:15");
    }
}
