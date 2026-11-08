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

/// Elapsed time as a timecode, `H:MM:SS` padded to `00:00:00`. `unit` is in
/// capture units (real minutes), so fractions carry real seconds.
pub fn timecode(unit: f64) -> String {
    let secs = (unit.max(0.0) * 60.0).round() as i64;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// A span of real time from a count of units: `45m`, `1h 5m`, `2h`.
pub fn span_label(units: f64) -> String {
    let m = units.max(0.0).round() as i64;
    match (m / 60, m % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// Ruler ticks inside `[from, to]` (units): `(unit, major)`, a minor tick
/// halfway between majors.
pub fn ruler_ticks_in(from: f64, to: f64, step: i64) -> Vec<(f64, bool)> {
    let mut out = Vec::new();
    if step <= 0 || to <= from {
        return out;
    }
    let half = step as f64 / 2.0;
    let mut u = (from / half).floor() * half;
    while u <= to + 1e-9 {
        if u >= from - 1e-9 {
            let major = ((u / step as f64) - (u / step as f64).round()).abs() < 1e-9;
            out.push((u, major));
        }
        u += half;
    }
    out
}

// ── Timeline view (zoom + pan) ──────────────────────────────────────────

/// The visible window of the timeline: `zoom` = total / visible units,
/// `start` = first visible unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub start: f64,
    pub zoom: f64,
}

impl View {
    pub const FIT: View = View { start: 0.0, zoom: 1.0 };

    pub fn visible(&self, n: usize) -> f64 {
        n as f64 / self.zoom.max(1.0)
    }

    /// Keep the zoom in `[1, max]` and the window inside the timeline.
    pub fn clamped(self, n: usize, max_zoom: f64) -> View {
        let zoom = self.zoom.clamp(1.0, max_zoom.max(1.0));
        let visible = n as f64 / zoom;
        View { zoom, start: self.start.clamp(0.0, (n as f64 - visible).max(0.0)) }
    }

    /// Zoom to `zoom`, keeping `anchor` (a unit) at the same fraction `frac`
    /// of the width.
    pub fn zoomed(self, n: usize, zoom: f64, anchor: f64, frac: f64, max_zoom: f64) -> View {
        let zoom = zoom.clamp(1.0, max_zoom.max(1.0));
        View { zoom, start: anchor - frac * n as f64 / zoom }.clamped(n, max_zoom)
    }
}

/// Deepest zoom: one unit at most `max_unit_px` wide.
pub fn max_zoom(n: usize, track_w: f32, max_unit_px: f32) -> f64 {
    if n == 0 || track_w <= 0.0 {
        return 1.0;
    }
    (n as f64 * max_unit_px as f64 / track_w as f64).max(1.0)
}

// ── Masks (lookout PR #198) ─────────────────────────────────────────────

/// Masks that may overlap in time; more need another lane but can't be
/// added (the PR's `MAX_MASK_TRACKS`).
pub const MAX_MASK_LANES: usize = 3;
/// `MAX_MASK_REGIONS` in `packages/shared/src/masks.ts`.
pub const MAX_MASKS: usize = 100;

/// A pixelated box, timed in video seconds (= units, fractional) and placed
/// in normalized `[0, 1]` frame coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    pub id: String,
    pub start: f64,
    pub end: f64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Mask {
    pub fn active_at(&self, t: f64) -> bool {
        t >= self.start - 0.001 && t < self.end - 0.001
    }
}

/// How the worker pixelates a mask (`packages/worker/src/segments.ts`):
/// crop an even-aligned box, scale it to `down_w × down_h` with
/// nearest-neighbour, then back up the same way. All in frame pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pixelation {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub down_w: u32,
    pub down_h: u32,
}

impl Pixelation {
    /// The worker's rounding, applied to the coordinates as they're saved.
    pub fn new(m: &Mask, frame_w: u32, frame_h: u32) -> Option<Pixelation> {
        if frame_w < 2 || frame_h < 2 {
            return None;
        }
        let (fw, fh) = (frame_w as i64, frame_h as i64);
        let px = |v: f32, len: i64| (round4(v as f64) * len as f64).round() as i64;
        let (raw_x, raw_y, raw_w, raw_h) = (px(m.x, fw), px(m.y, fh), px(m.w, fw), px(m.h, fh));
        let x = (raw_x & !1).min((fw - 2) & !1).max(0);
        let y = (raw_y & !1).min((fh - 2) & !1).max(0);
        let w = (raw_w & !1).min((fw - x) & !1).max(2);
        let h = (raw_h & !1).min((fh - y) & !1).max(2);
        let down = |len: i64| ((len / 16 / 2) * 2).max(2);
        Some(Pixelation {
            x: x as u32,
            y: y as u32,
            w: w as u32,
            h: h as u32,
            down_w: down(w) as u32,
            down_h: down(h) as u32,
        })
    }

    /// Source offset (within the box) that block `i` of `down` shows:
    /// swscale's point sampling picks `floor((i + 0.5) · len / down)`.
    pub fn sample(i: u32, down: u32, len: u32) -> u32 {
        (((2 * i + 1) as u64 * len as u64) / (2 * down as u64)) as u32
    }

    /// First output offset (within the box) belonging to block `j`: the
    /// upscale maps pixel `p` to block `floor((p + 0.5) · down / len)`.
    pub fn edge(j: u32, down: u32, len: u32) -> u32 {
        if j == 0 {
            return 0;
        }
        let num = 2 * j as i64 * len as i64 - down as i64;
        let den = 2 * down as i64;
        ((num + den - 1) / den).clamp(0, len as i64) as u32
    }
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// `unitMasksToMasks`: wall-clock bounds from the covered units, plus the
/// exact video seconds.
pub fn masks_to_json(masks: &[Mask], units: &[VideoUnit]) -> serde_json::Value {
    let n = units.len();
    let out: Vec<serde_json::Value> = masks
        .iter()
        .filter(|m| n > 0 && m.end > m.start && m.w > 0.0 && m.h > 0.0)
        .map(|m| {
            let s_idx = (m.start.floor().max(0.0) as usize).min(n - 1);
            let e_idx = (m.end.ceil() as usize).min(n).max(s_idx + 1);
            let last = parse_ms(&units[e_idx - 1].captured_at);
            let end = match units.get(e_idx) {
                Some(next) => parse_ms(&next.captured_at).min(last + SCREENSHOT_INTERVAL_MS),
                None => last + SCREENSHOT_INTERVAL_MS,
            };
            serde_json::json!({
                "id": m.id,
                "start": units[s_idx].captured_at,
                "end": iso(end),
                "startSec": round4(m.start),
                "endSec": round4(m.end),
                "x": round4(m.x as f64),
                "y": round4(m.y as f64),
                "width": round4(m.w as f64),
                "height": round4(m.h as f64),
            })
        })
        .collect();
    serde_json::Value::Array(out)
}

/// `masksToUnitMasks`: prefer the stored video seconds, else map the
/// wall-clock bounds onto units.
pub fn masks_from_json(v: &serde_json::Value, units: &[VideoUnit]) -> Vec<Mask> {
    let Some(arr) = v.as_array() else { return Vec::new() };
    let n = units.len();
    arr.iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let f = |k: &str| m.get(k).and_then(|x| x.as_f64());
            let (x, y, w, h) = (f("x")?, f("y")?, f("width")?, f("height")?);
            let (start, end) = match (f("startSec"), f("endSec")) {
                (Some(s), Some(e)) if e > s => (s, e),
                _ => {
                    let s_ms = parse_ms(m.get("start")?.as_str()?);
                    let e_ms = parse_ms(m.get("end")?.as_str()?);
                    let mut start = 0usize;
                    let mut end = n;
                    for (i, u) in units.iter().enumerate() {
                        let t = parse_ms(&u.captured_at);
                        if t <= s_ms {
                            start = i;
                        }
                        if t < e_ms {
                            end = i + 1;
                        }
                    }
                    if end <= start {
                        end = (start + 1).min(n);
                    }
                    (start as f64, end as f64)
                }
            };
            let id = m.get("id").and_then(|x| x.as_str()).map(String::from).unwrap_or_else(|| format!("mask-{i}"));
            Some(Mask { id, start, end, x: x as f32, y: y as f32, w: w as f32, h: h as f32 })
        })
        .collect()
}

/// `assignMaskTracks`: the lane for each mask (same order as `masks`),
/// first lane without a time collision.
pub fn assign_lanes(masks: &[Mask]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..masks.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&masks[a], &masks[b]);
        a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal).then(a.end.partial_cmp(&b.end).unwrap_or(std::cmp::Ordering::Equal))
    });
    let mut lanes: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut out = vec![0; masks.len()];
    for i in order {
        let m = &masks[i];
        let free = lanes.iter().position(|l| l.iter().all(|&(s, e)| m.end <= s + 0.001 || m.start >= e - 0.001));
        let lane = free.unwrap_or_else(|| {
            lanes.push(Vec::new());
            lanes.len() - 1
        });
        lanes[lane].push((m.start, m.end));
        out[i] = lane;
    }
    out
}

/// Masks overlapping `[start, end)`, other than `except`.
pub fn masks_overlapping(masks: &[Mask], start: f64, end: f64, except: Option<&str>) -> usize {
    masks.iter().filter(|m| Some(m.id.as_str()) != except && m.start < end - 0.001 && m.end > start + 0.001).count()
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
    fn masks_round_trip() {
        let u = units(10);
        let m = vec![Mask { id: "a".into(), start: 2.5, end: 4.25, x: 0.1, y: 0.2, w: 0.3, h: 0.4 }];
        let v = masks_to_json(&m, &u);
        assert_eq!(v[0]["start"], u[2].captured_at);
        assert_eq!(v[0]["end"], iso(parse_ms(&u[5].captured_at)));
        assert_eq!(v[0]["startSec"], 2.5);
        let back = masks_from_json(&v, &u);
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].start, back[0].end), (2.5, 4.25));
        // Without seconds, the wall-clock bounds map to whole units.
        let mut v2 = v.clone();
        v2[0].as_object_mut().unwrap().remove("startSec");
        let back = masks_from_json(&v2, &u);
        assert_eq!((back[0].start, back[0].end), (2.0, 5.0));
    }

    /// Block runs `(first, end, source)` as ffmpeg 8 produces them for
    /// `scale=down:…:flags=neighbor,scale=len:…:flags=neighbor`.
    #[test]
    fn pixelation_matches_ffmpeg() {
        let cases: [(u32, &[(u32, u32, u32)]); 4] = [
            (36, &[(0, 18, 9), (18, 36, 27)]),
            (
                100,
                &[
                    (0, 17, 8),
                    (17, 33, 25),
                    (33, 50, 41),
                    (50, 67, 58),
                    (67, 83, 75),
                    (83, 100, 91),
                ],
            ),
            (
                250,
                &[
                    (0, 18, 8),
                    (18, 36, 26),
                    (36, 54, 44),
                    (54, 71, 62),
                    (71, 89, 80),
                    (89, 107, 98),
                    (107, 125, 116),
                    (125, 143, 133),
                    (143, 161, 151),
                    (161, 179, 169),
                    (179, 196, 187),
                    (196, 214, 205),
                    (214, 232, 223),
                    (232, 250, 241),
                ],
            ),
            (
                254,
                &[
                    (0, 18, 9),
                    (18, 36, 27),
                    (36, 54, 45),
                    (54, 73, 63),
                    (73, 91, 81),
                    (91, 109, 99),
                    (109, 127, 117),
                    (127, 145, 136),
                    (145, 163, 154),
                    (163, 181, 172),
                    (181, 200, 190),
                    (200, 218, 208),
                    (218, 236, 226),
                    (236, 254, 244),
                ],
            ),
        ];
        for (len, runs) in cases {
            let down = runs.len() as u32;
            let m = Mask {
                id: "m".into(),
                start: 0.0,
                end: 1.0,
                x: 0.0,
                y: 0.0,
                w: len as f32 / 1000.0,
                h: 0.5,
            };
            assert_eq!(
                Pixelation::new(&m, 1000, 100).unwrap().down_w,
                down,
                "len {len}"
            );
            for (j, &(a, b, s)) in runs.iter().enumerate() {
                let j = j as u32;
                assert_eq!(
                    (
                        Pixelation::edge(j, down, len),
                        Pixelation::edge(j + 1, down, len),
                        Pixelation::sample(j, down, len)
                    ),
                    (a, b, s),
                    "len {len} block {j}"
                );
            }
        }
        // Even-aligned and kept inside the frame.
        let m = Mask {
            id: "m".into(),
            start: 0.0,
            end: 1.0,
            x: 0.9,
            y: 0.0013,
            w: 0.5,
            h: 0.0001,
        };
        let p = Pixelation::new(&m, 1920, 1080).unwrap();
        assert_eq!((p.x, p.y, p.w, p.h, p.down_h), (1728, 0, 192, 2, 2));
    }

    #[test]
    fn lanes() {
        let mk = |s: f64, e: f64| Mask { id: format!("{s}"), start: s, end: e, x: 0.0, y: 0.0, w: 0.1, h: 0.1 };
        let m = vec![mk(0.0, 2.0), mk(1.0, 3.0), mk(2.0, 4.0), mk(1.5, 2.5)];
        assert_eq!(assign_lanes(&m), vec![0, 1, 0, 2]);
        assert_eq!(masks_overlapping(&m, 1.6, 1.8, None), 3);
    }

    #[test]
    fn view_math() {
        let v = View::FIT.zoomed(100, 4.0, 50.0, 0.5, 10.0);
        assert_eq!(v, View { start: 37.5, zoom: 4.0 });
        // Anchored near the end, the window stays inside the timeline.
        let v = View::FIT.zoomed(100, 4.0, 99.0, 0.1, 10.0);
        assert_eq!(v.start, 75.0);
        assert_eq!(View { start: 0.0, zoom: 40.0 }.clamped(100, 10.0).zoom, 10.0);
        assert_eq!(timecode(90.5), "01:30:30");
        assert_eq!(span_label(65.0), "1h 5m");
        assert_eq!(ruler_ticks_in(9.0, 21.0, 10), vec![(10.0, true), (15.0, false), (20.0, true)]);
    }

    #[test]
    fn ruler() {
        assert_eq!(ruler_step(60, 600.0), 10);
        assert_eq!(elapsed_label(12.0, 47), "12m");
        assert_eq!(elapsed_label(75.0, 120), "1:15");
    }
}
