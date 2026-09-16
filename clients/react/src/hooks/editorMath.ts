// Pure math for the timelapse editor: converting between the video's time
// axis (1 second = 1 capture unit = 1 real-world minute) and the wall-clock
// cut intervals the server stores. Kept DOM-free so it's unit-testable.

import { isCutAt, type MaskRegion, type CutInterval, type VideoUnit, type VideoShot } from "@lookout/shared";
import { SCREENSHOT_INTERVAL_MS } from "@lookout/shared";

/** A cut region in unit space: [startUnit, endUnit) video-second indices.
 *  This is the editor's working representation — integers, so regions are
 *  inherently snapped to capture-unit boundaries. */
export interface UnitRegion {
  startUnit: number;
  endUnit: number;
}

/** Clamp + floor a video-time (seconds) to a valid unit index. */
export function unitAtTime(t: number, unitCount: number): number {
  return Math.max(0, Math.min(unitCount - 1, Math.floor(t)));
}

/**
 * Serialize unit regions to the wall-clock cut intervals the server stores.
 * A region [i, j) covers units i..j-1, i.e. wall-clock
 * [units[i].capturedAt, units[j-1].capturedAt + 60s). Round-trips losslessly
 * through the server's membership rule (ts ∈ [start, end)).
 */
export function regionsToCuts(
  regions: UnitRegion[],
  units: VideoUnit[],
): CutInterval[] {
  return regions
    .filter((r) => r.endUnit > r.startUnit)
    .map((r) => {
      const lastCut = Date.parse(units[r.endUnit - 1].capturedAt);
      const nextKept =
        r.endUnit < units.length ? Date.parse(units[r.endUnit].capturedAt) : null;
      // The end is exclusive, so anchoring it to the next KEPT capture's
      // real timestamp excludes that capture exactly. Assuming a 60s
      // stride instead was wrong: captures jitter (the server credits
      // anything within ±30s of the mark), so a neighbour landing at +57s
      // fell inside the interval and the server counted one more unit cut
      // than the editor showed — enough, on a short recording, to look
      // like the whole thing was selected.
      //
      // Still capped at one interval: across a pause the next capture can
      // be hours later, and the cut shouldn't swallow that whole span.
      const end =
        nextKept === null
          ? lastCut + SCREENSHOT_INTERVAL_MS
          : Math.min(nextKept, lastCut + SCREENSHOT_INTERVAL_MS);
      return {
        start: units[r.startUnit].capturedAt,
        end: new Date(end).toISOString(),
      };
    });
}

/**
 * Project stored wall-clock cuts back into unit regions via the shared
 * membership rule, merging adjacent cut units into contiguous regions.
 * The exact inverse of regionsToCuts for any normalized list.
 */
export function cutsToRegions(
  cuts: CutInterval[],
  units: VideoUnit[],
): UnitRegion[] {
  const regions: UnitRegion[] = [];
  let open: UnitRegion | null = null;
  for (let i = 0; i < units.length; i++) {
    const cut = isCutAt(Date.parse(units[i].capturedAt), cuts);
    if (cut) {
      if (open) open.endUnit = i + 1;
      else open = { startUnit: i, endUnit: i + 1 };
    } else if (open) {
      regions.push(open);
      open = null;
    }
  }
  if (open) regions.push(open);
  return regions;
}

/** Merge overlapping/adjacent regions and drop empties — keeps the editor
 *  state canonical after drags so regions never visually stack. */
export function normalizeRegions(regions: UnitRegion[]): UnitRegion[] {
  const sorted = regions
    .filter((r) => r.endUnit > r.startUnit)
    .slice()
    .sort((a, b) => a.startUnit - b.startUnit);
  const merged: UnitRegion[] = [];
  for (const r of sorted) {
    const last = merged[merged.length - 1];
    if (last && r.startUnit <= last.endUnit) {
      last.endUnit = Math.max(last.endUnit, r.endUnit);
    } else {
      merged.push({ ...r });
    }
  }
  return merged;
}

/** Total units removed by a region list (assumed normalized). */
export function cutUnitCount(regions: UnitRegion[]): number {
  return regions.reduce((n, r) => n + (r.endUnit - r.startUnit), 0);
}

/** Is unit `i` inside any region? */
export function unitIsCut(i: number, regions: UnitRegion[]): boolean {
  return regions.some((r) => i >= r.startUnit && i < r.endUnit);
}

/** The region containing video time `t`, if any. */
export function regionAtTime(
  t: number,
  regions: UnitRegion[],
): UnitRegion | null {
  return regions.find((r) => t >= r.startUnit && t < r.endUnit) ?? null;
}

/**
 * Recording pauses to mark on the timeline: indices `i` where the gap
 * between unit i-1 and unit i exceeds ~1.5 capture intervals (i.e. the
 * recording paused/stalled between those two video seconds).
 */
export function gapIndices(units: VideoUnit[]): number[] {
  const gaps: number[] = [];
  for (let i = 1; i < units.length; i++) {
    const delta =
      Date.parse(units[i].capturedAt) - Date.parse(units[i - 1].capturedAt);
    if (delta > SCREENSHOT_INTERVAL_MS * 1.5) gaps.push(i);
  }
  return gaps;
}

/** "1h 23m" / "23m" / "45s" — compact duration for the editor footer. */
export function formatUnitsDuration(unitCount: number): string {
  const totalMinutes = unitCount; // one unit = one real-world minute
  if (totalMinutes < 1) return "0m";
  const h = Math.floor(totalMinutes / 60);
  const m = totalMinutes % 60;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  return `${m}m`;
}

/** Wall-clock label (local) for a unit. Includes AM/PM where the locale
 *  uses it — this is the one place the *time of day* is stated, so it must
 *  not be mistakable for a duration. */
export function unitClockLabel(unit?: VideoUnit | null): string {
  if (!unit || !unit.capturedAt) return "";
  const d = new Date(unit.capturedAt);
  if (isNaN(d.getTime())) return "";
  return d.toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
}

/** Wall-clock span label (e.g. "11:56 AM – 12:04 PM" or "11:56 AM"). */
export function unitRangeClockLabel(
  startUnit: number,
  endUnit: number,
  units: VideoUnit[],
): string {
  if (!units || units.length === 0) return "";
  const sIdx = Math.max(0, Math.min(units.length - 1, Math.floor(startUnit)));
  const eIdx = Math.max(0, Math.min(units.length - 1, Math.max(sIdx, Math.ceil(endUnit) - 1)));
  const sUnit = units[sIdx];
  const eUnit = units[eIdx];
  if (!sUnit) return "";
  const sLabel = unitClockLabel(sUnit);
  if (!sLabel) return "";
  if (sIdx === eIdx || !eUnit) return sLabel;
  const eLabel = unitClockLabel(eUnit);
  if (!eLabel || sLabel === eLabel) return sLabel;
  return `${sLabel} – ${eLabel}`;
}

/**
 * Ruler label: how far into the *recording* a unit sits, since one unit is
 * one recorded minute.
 *
 * Deliberately not wall-clock. A ruler reading "1:29 … 1:45" on a
 * 17-minute timelapse is correct (those are times of day) but reads as
 * "1 minute 29 seconds", which makes the whole timeline look wrong. Under
 * an hour this is "5m"; past that, "1:05" as hours:minutes.
 */
export function elapsedLabel(unitIndex: number, totalUnits: number): string {
  const m = Math.max(0, Math.round(unitIndex));
  if (totalUnits < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}:${String(m % 60).padStart(2, "0")}`;
}

export function shotRulerLabel(unit: number, totalUnits: number, step: number): string {
    if (unit === 0) return "Shot 1";
    if (unit >= totalUnits) return `${totalUnits} shots`;
    if (step === 1) return `Shot ${unit + 1}`;
    return `Shot ${unit}`;
}

/** Steps a person reads without doing arithmetic — the reason a ruler
 *  labels 0/8/16/24 and never 0/7/14/21. In units (= minutes). */
const NICE_STEPS = [1, 2, 5, 10, 15, 20, 30, 60, 120, 180, 360, 720];

/**
 * Choose a ruler labelling interval: the smallest "nice" step that keeps
 * labels at least `minLabelPx` apart at the current track width. Returns
 * the step in units, so the caller can place a label every `step` and a
 * minor tick every `step / 2`.
 */
export function rulerStep(
  unitCount: number,
  trackWidthPx: number,
  minLabelPx = 88,
): number {
  if (unitCount <= 0 || trackWidthPx <= 0) return 1;
  const pxPerUnit = trackWidthPx / unitCount;
  const needed = minLabelPx / pxPerUnit;
  return NICE_STEPS.find((s) => s >= needed) ?? NICE_STEPS[NICE_STEPS.length - 1];
}

/** Tick positions for a ruler: every `step` units, plus the midpoints. */
export function rulerTicks(
  unitCount: number,
  step: number,
): Array<{ unit: number; major: boolean }> {
  const ticks: Array<{ unit: number; major: boolean }> = [];
  if (unitCount <= 0 || step <= 0) return ticks;
  const half = step / 2;
  for (let u = 0; u <= unitCount; u += half) {
    // Floating-point half-steps land a hair off an integer multiple;
    // compare on the rounded value so majors are never missed.
    const major = Math.abs(u / step - Math.round(u / step)) < 1e-9;
    ticks.push({ unit: u, major });
  }
  return ticks;
}

// Mask region in timeline units: [startUnit, endUnit) with normalized coords.
export interface UnitMaskRegion {
  id: string;
  startUnit: number;
  endUnit: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

export type UnitBlurRegion = UnitMaskRegion;

// Convert timeline unit regions to ISO wall-clock mask intervals.
export function unitMasksToMasks(
  regions: UnitMaskRegion[],
  units: VideoUnit[],
): MaskRegion[] {
  if (units.length === 0) return [];
  const masks: MaskRegion[] = [];
  for (const r of regions) {
    if (r.endUnit <= r.startUnit || r.width <= 0 || r.height <= 0) continue;
    const sIdx = Math.max(0, Math.min(units.length - 1, Math.floor(r.startUnit)));
    const eIdx = Math.max(sIdx + 1, Math.min(units.length, Math.ceil(r.endUnit)));
    const startUnit = units[sIdx];
    const lastUnit = units[Math.min(units.length - 1, Math.max(0, eIdx - 1))];
    if (!startUnit || !lastUnit) continue;
    const lastCut = Date.parse(lastUnit.capturedAt);
    const nextKept = eIdx < units.length && units[eIdx] ? Date.parse(units[eIdx].capturedAt) : null;
    const end =
      nextKept === null
        ? lastCut + SCREENSHOT_INTERVAL_MS
        : Math.min(nextKept, lastCut + SCREENSHOT_INTERVAL_MS);
    masks.push({
      id: r.id,
      start: startUnit.capturedAt,
      end: new Date(end).toISOString(),
      startSec: Math.round(r.startUnit * 10_000) / 10_000,
      endSec: Math.round(r.endUnit * 10_000) / 10_000,
      x: r.x,
      y: r.y,
      width: r.width,
      height: r.height,
    });
  }
  return masks;
}

export const unitBlursToBlurs = unitMasksToMasks;

// Convert ISO wall-clock mask intervals to timeline unit regions.
export function masksToUnitMasks(
  masks: MaskRegion[],
  units: VideoUnit[],
): UnitMaskRegion[] {
  if (units.length === 0 || masks.length === 0) return [];
  return masks.map((m) => {
    let startUnit = 0;
    let endUnit = units.length;

    if (
      typeof m.startSec === "number" &&
      typeof m.endSec === "number" &&
      m.endSec > m.startSec
    ) {
      startUnit = m.startSec;
      endUnit = m.endSec;
    }
    else {
      const startMs = Date.parse(m.start);
      const endMs = Date.parse(m.end);

      for (let i = 0; i < units.length; i++) {
        const uTime = Date.parse(units[i].capturedAt);
        if (uTime <= startMs) {
          startUnit = i;
        }
        if (uTime < endMs) {
          endUnit = i + 1;
        }
      }
      if (endUnit <= startUnit) {
        endUnit = Math.min(units.length, startUnit + 1);
      }
    }

    return {
      id: m.id,
      startUnit,
      endUnit,
      x: m.x,
      y: m.y,
      width: m.width,
      height: m.height,
    };
  });
}

export const blursToUnitBlurs = masksToUnitMasks;

// Find the video shot active at time t.
export function findShotAtTime(t: number, shots?: VideoShot[]): VideoShot | null {
  if (!shots || shots.length === 0) return null;
  const match = shots.find((s) => t >= s.startSec && t < s.endSec);
  if (match) return match;
  const last = shots[shots.length - 1];
  if (last && (t >= last.endSec || Math.abs(t - last.endSec) < 0.05)) return last;
  const first = shots[0];
  if (first && t < first.startSec) return first;
  return null;
}

// Snap time t to the nearest shot start or end boundary.
export function snapToNearestShotBoundary(
  t: number,
  shots?: VideoShot[],
  unitCount = 0,
): number {
  if (!shots || shots.length === 0) {
    return Math.max(0, Math.min(unitCount, Math.round(t)));
  }
  let closest = 0;
  let minDiff = Infinity;
  for (const s of shots) {
    const d1 = Math.abs(t - s.startSec);
    if (d1 < minDiff) {
      minDiff = d1;
      closest = s.startSec;
    }
    const d2 = Math.abs(t - s.endSec);
    if (d2 < minDiff) {
      minDiff = d2;
      closest = s.endSec;
    }
  }
  return closest;
}

export interface TrackPreset {
    name: string;
    color: string;
    border: string;
    bgUnselected: string;
    bgSelected: string;
    bgHover: string;
    badgeBg: string;
    badgeText: string;
    handleColor: string;
}

export const MAX_MASK_TRACKS = 3;
export const MAX_BLUR_TRACKS = MAX_MASK_TRACKS;

export const TRACK_PRESETS: readonly TrackPreset[] = [
    {
        name: "Blue",
        color: "#3b82f6",
        border: "#3b82f6",
        bgUnselected: "rgba(59, 130, 246, 0.18)",
        bgSelected: "rgba(59, 130, 246, 0.32)",
        bgHover: "rgba(59, 130, 246, 0.24)",
        badgeBg: "#2563eb",
        badgeText: "#ffffff",
        handleColor: "#60a5fa",
    },
    {
        name: "Violet",
        color: "#8b5cf6",
        border: "#8b5cf6",
        bgUnselected: "rgba(139, 92, 246, 0.18)",
        bgSelected: "rgba(139, 92, 246, 0.32)",
        bgHover: "rgba(139, 92, 246, 0.24)",
        badgeBg: "#7c3aed",
        badgeText: "#ffffff",
        handleColor: "#a78bfa",
    },
    {
        name: "Yellow",
        color: "#eab308",
        border: "#eab308",
        bgUnselected: "rgba(234, 179, 8, 0.18)",
        bgSelected: "rgba(234, 179, 8, 0.32)",
        bgHover: "rgba(234, 179, 8, 0.24)",
        badgeBg: "#ca8a04",
        badgeText: "#ffffff",
        handleColor: "#facc15",
    },
] as const;

export interface MaskTrackAllocation {
    assignments: Record<string, number>;
    trackCount: number;
    tracks: UnitMaskRegion[][];
}

export type BlurTrackAllocation = MaskTrackAllocation;

// Assign masks to timeline tracks (up to 3) without overlapping collisions.
export function assignMaskTracks(masks: UnitMaskRegion[]): MaskTrackAllocation {
    const sorted = [...masks].sort((a, b) => {
        if (Math.abs(a.startUnit - b.startUnit) > 0.001) return a.startUnit - b.startUnit;
        return a.endUnit - b.endUnit;
    });

    const tracks: UnitMaskRegion[][] = Array.from({ length: MAX_MASK_TRACKS }, () => []);
    const assignments: Record<string, number> = {};
    const EPS = 0.001;

    for (const mask of sorted) {
        let placedTrack = -1;
        for (let t = 0; t < MAX_MASK_TRACKS; t++) {
            const track = tracks[t];
            const hasCollision = track.some(
                (existing) => !(mask.endUnit <= existing.startUnit + EPS || mask.startUnit >= existing.endUnit - EPS),
            );
            if (!hasCollision) {
                placedTrack = t;
                track.push(mask);
                break;
            }
        }

        if (placedTrack === -1) {
            placedTrack = MAX_MASK_TRACKS - 1;
            tracks[placedTrack].push(mask);
        }
        assignments[mask.id] = placedTrack;
    }

    let trackCount = 1;
    for (let t = 0; t < MAX_MASK_TRACKS; t++) {
        if (tracks[t].length > 0) trackCount = t + 1;
    }

    return { assignments, trackCount, tracks };
}

export const assignBlurTracks = assignMaskTracks;

// Get all masks active at the given unit time.
export function activeMasksAtUnit(t: number, masks: UnitMaskRegion[]): UnitMaskRegion[] {
    return masks.filter((m) => t >= m.startUnit - 0.001 && t < m.endUnit - 0.001);
}

export const activeBlursAtUnit = activeMasksAtUnit;

// Check if a new mask can be added at time t without exceeding track capacity.
export function canAddMaskAtTime(t: number, masks: UnitMaskRegion[]): boolean {
    return activeMasksAtUnit(t, masks).length < MAX_MASK_TRACKS;
}

export const canAddBlurAtTime = canAddMaskAtTime;

// Check if a mask is active on screen at video time t.
export function isMaskActiveAtTime(t: number, m: UnitMaskRegion): boolean {
    return t >= m.startUnit - 0.01 && t <= m.endUnit + 0.005;
}

export const isBlurActiveAtTime = isMaskActiveAtTime;

export function computeSafeCursorTime(startSec: number, endSec: number, targetTime: number): number {
    return Math.max(startSec + 0.005, Math.min(endSec - 0.005, targetTime));
}
