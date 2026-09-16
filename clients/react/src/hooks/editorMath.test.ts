import { describe, expect, it } from "vitest";
import { countCutUnits, type CutInterval, type VideoUnit } from "@lookout/shared";
import {
  regionsToCuts,
  cutsToRegions,
  normalizeRegions,
  cutUnitCount,
  unitIsCut,
  regionAtTime,
  gapIndices,
  elapsedLabel,
  shotRulerLabel,
  rulerStep,
  rulerTicks,
  formatUnitsDuration,
  unitMasksToMasks,
  masksToUnitMasks,
  unitBlursToBlurs,
  blursToUnitBlurs,
  findShotAtTime,
  snapToNearestShotBoundary,
  unitClockLabel,
  unitRangeClockLabel,
  assignMaskTracks,
  assignBlurTracks,
  canAddMaskAtTime,
  canAddBlurAtTime,
  activeMasksAtUnit,
  activeBlursAtUnit,
  isMaskActiveAtTime,
  isBlurActiveAtTime,
  computeSafeCursorTime,
  TRACK_PRESETS,
  type UnitRegion,
  type UnitMaskRegion,
  type UnitBlurRegion,
} from "./editorMath.js";
import type { MaskRegion, BlurRegion, VideoShot } from "@lookout/shared";

const T0 = Date.parse("2026-07-01T10:00:00.000Z");

/** n units captured a minute apart, with optional pause gaps: `gapsAfter`
 *  maps unit index → extra minutes of silence before the NEXT unit. */
function makeUnits(n: number, gapsAfter: Record<number, number> = {}): VideoUnit[] {
  const units: VideoUnit[] = [];
  let t = T0;
  for (let i = 0; i < n; i++) {
    units.push({
      capturedAt: new Date(t).toISOString(),
      screenshotId: `ss-${i}`,
    });
    t += 60_000 + (gapsAfter[i] ?? 0) * 60_000;
  }
  return units;
}

describe("regionsToCuts ⇄ cutsToRegions round-trip", () => {
  it("round-trips a middle region", () => {
    const units = makeUnits(10);
    const regions: UnitRegion[] = [{ startUnit: 3, endUnit: 6 }];
    const cuts = regionsToCuts(regions, units);
    expect(cutsToRegions(cuts, units)).toEqual(regions);
  });

  it("round-trips edge regions and multiple regions", () => {
    const units = makeUnits(12);
    const regions: UnitRegion[] = [
      { startUnit: 0, endUnit: 2 },
      { startUnit: 5, endUnit: 6 },
      { startUnit: 9, endUnit: 12 },
    ];
    expect(cutsToRegions(regionsToCuts(regions, units), units)).toEqual(regions);
  });

  it("round-trips across pause gaps without swallowing neighbors", () => {
    // A 3-hour pause between units 4 and 5: the wall-clock interval for a
    // region ending at unit 4 must not extend into unit 5's minute.
    const units = makeUnits(10, { 4: 180 });
    const regions: UnitRegion[] = [{ startUnit: 3, endUnit: 5 }];
    const cuts = regionsToCuts(regions, units);
    expect(cutsToRegions(cuts, units)).toEqual(regions);
    expect(unitIsCut(5, cutsToRegions(cuts, units))).toBe(false);
  });

  it("serializes a region as [firstCutUnit, firstKeptUnit)", () => {
    const units = makeUnits(5);
    const cuts = regionsToCuts([{ startUnit: 1, endUnit: 3 }], units);
    // End is exclusive and anchored to the next kept capture, so that
    // capture is excluded exactly regardless of the gap before it. On an
    // even 60s cadence that coincides with lastCut + 60s.
    expect(cuts).toEqual<CutInterval[]>([
      { start: units[1].capturedAt, end: units[3].capturedAt },
    ]);
  });

  it("drops empty regions", () => {
    const units = makeUnits(5);
    expect(regionsToCuts([{ startUnit: 2, endUnit: 2 }], units)).toEqual([]);
  });
});

describe("regionsToCuts agrees with the server's membership rule", () => {
  /** Server-side count: timestamp membership over the serialized list. */
  const serverCutCount = (units: VideoUnit[], cuts: CutInterval[]) =>
    countCutUnits(units.map((u) => Date.parse(u.capturedAt)), cuts);

  it("does not over-cut when captures arrive early", () => {
    // The reported bug: a 3-minute timelapse with 2 minutes selected was
    // rejected as "would remove the entire timelapse". Captures jitter
    // (the server credits anything within ±30s of the mark), so a 57s gap
    // put the next capture inside an interval that assumed a 60s stride.
    const T = Date.parse("2026-07-27T14:58:00.000Z");
    const units: VideoUnit[] = [
      { capturedAt: new Date(T).toISOString(), screenshotId: "a" },
      { capturedAt: new Date(T + 57_000).toISOString(), screenshotId: "b" },
      { capturedAt: new Date(T + 114_000).toISOString(), screenshotId: "c" },
    ];
    const cuts = regionsToCuts([{ startUnit: 0, endUnit: 2 }], units);
    expect(serverCutCount(units, cuts)).toBe(2);
    expect(cutsToRegions(cuts, units)).toEqual([{ startUnit: 0, endUnit: 2 }]);
  });

  it("holds across a spread of realistic jitter", () => {
    for (const gap of [40_000, 52_000, 57_000, 59_999, 60_000, 63_000, 75_000]) {
      const T = Date.parse("2026-07-27T09:00:00.000Z");
      const units: VideoUnit[] = Array.from({ length: 6 }, (_, i) => ({
        capturedAt: new Date(T + i * gap).toISOString(),
        screenshotId: `u${i}`,
      }));
      for (const region of [
        { startUnit: 0, endUnit: 2 },
        { startUnit: 2, endUnit: 4 },
        { startUnit: 4, endUnit: 6 },
      ]) {
        const cuts = regionsToCuts([region], units);
        expect(serverCutCount(units, cuts)).toBe(region.endUnit - region.startUnit);
      }
    }
  });

  it("never swallows more than an interval across a pause", () => {
    // Anchoring to the next kept capture must not extend a cut across a
    // three-hour pause and remove captures that live inside it.
    const units = makeUnits(6, { 2: 180 });
    const cuts = regionsToCuts([{ startUnit: 1, endUnit: 3 }], units);
    const span = Date.parse(cuts[0].end) - Date.parse(units[2].capturedAt);
    expect(span).toBeLessThanOrEqual(60_000);
    expect(serverCutCount(units, cuts)).toBe(2);
  });

  it("agrees for a cut running to the very end", () => {
    const units = makeUnits(5);
    const cuts = regionsToCuts([{ startUnit: 3, endUnit: 5 }], units);
    expect(serverCutCount(units, cuts)).toBe(2);
  });
});

describe("normalizeRegions", () => {
  it("merges overlapping and adjacent regions, sorts, drops empties", () => {
    expect(
      normalizeRegions([
        { startUnit: 6, endUnit: 8 },
        { startUnit: 1, endUnit: 3 },
        { startUnit: 3, endUnit: 5 },
        { startUnit: 4, endUnit: 4 },
      ]),
    ).toEqual([
      { startUnit: 1, endUnit: 5 },
      { startUnit: 6, endUnit: 8 },
    ]);
  });

  it("counts cut units", () => {
    expect(
      cutUnitCount([
        { startUnit: 1, endUnit: 5 },
        { startUnit: 6, endUnit: 8 },
      ]),
    ).toBe(6);
  });
});

describe("regionAtTime", () => {
  const regions: UnitRegion[] = [{ startUnit: 2, endUnit: 4 }];
  it("hits inside, misses outside (end-exclusive)", () => {
    expect(regionAtTime(2, regions)).toEqual(regions[0]);
    expect(regionAtTime(3.99, regions)).toEqual(regions[0]);
    expect(regionAtTime(4, regions)).toBeNull();
    expect(regionAtTime(1.5, regions)).toBeNull();
  });
});

describe("gapIndices", () => {
  it("flags pauses, ignores normal cadence and jitter", () => {
    const units = makeUnits(8, { 2: 30, 5: 5 });
    expect(gapIndices(units)).toEqual([3, 6]);
  });

  it("tolerates ±30s scheduling jitter", () => {
    const units = makeUnits(3);
    // 80s between captures is within 1.5× the interval — not a pause.
    units[2] = {
      ...units[2],
      capturedAt: new Date(Date.parse(units[1].capturedAt) + 80_000).toISOString(),
    };
    expect(gapIndices(units)).toEqual([]);
  });
});

describe("elapsedLabel", () => {
  it("is never mistakable for a duration in the wrong unit", () => {
    // The bug this replaced: a 17-minute timelapse labelled with wall
    // clock ("1:29" … "1:45") is correct but reads as 1m29s, making the
    // whole timeline look broken. Short sessions get an explicit unit.
    expect(elapsedLabel(0, 17)).toBe("0m");
    expect(elapsedLabel(5, 17)).toBe("5m");
    expect(elapsedLabel(16, 17)).toBe("16m");
  });

  it("switches to hours:minutes once minutes stop being readable", () => {
    expect(elapsedLabel(0, 180)).toBe("0:00");
    expect(elapsedLabel(65, 180)).toBe("1:05");
    expect(elapsedLabel(120, 180)).toBe("2:00");
  });

  it("rounds half-step tick positions to whole minutes", () => {
    expect(elapsedLabel(7.5, 17)).toBe("8m");
  });
});

describe("shotRulerLabel", () => {
  it("labels the first tick as Shot 1", () => {
    expect(shotRulerLabel(0, 4, 1)).toBe("Shot 1");
  });

  it("labels subsequent ticks by shot index when step is 1", () => {
    expect(shotRulerLabel(1, 4, 1)).toBe("Shot 2");
    expect(shotRulerLabel(2, 4, 1)).toBe("Shot 3");
    expect(shotRulerLabel(3, 4, 1)).toBe("Shot 4");
  });

  it("labels end tick with total shots count", () => {
    expect(shotRulerLabel(4, 4, 1)).toBe("4 shots");
  });

  it("labels large sessions with shot numbers", () => {
    expect(shotRulerLabel(10, 50, 5)).toBe("Shot 10");
    expect(shotRulerLabel(50, 50, 5)).toBe("50 shots");
  });
});

describe("rulerStep", () => {
  it("picks a step people read without arithmetic", () => {
    // 48 minutes across 900px → ~19px/min; a label needs ~88px, so ~5min.
    expect(rulerStep(48, 900)).toBe(5);
    // The same recording in a narrow window steps up rather than crowding.
    expect(rulerStep(48, 300)).toBeGreaterThan(rulerStep(48, 900));
    // A long session steps up too.
    expect(rulerStep(600, 900)).toBeGreaterThanOrEqual(60);
  });

  it("only ever returns round values", () => {
    const allowed = [1, 2, 5, 10, 15, 20, 30, 60, 120, 180, 360, 720];
    for (const units of [3, 17, 48, 121, 400, 1200]) {
      for (const w of [200, 480, 900, 1600]) {
        expect(allowed).toContain(rulerStep(units, w));
      }
    }
  });

  it("guarantees labels clear the minimum spacing", () => {
    for (const units of [10, 48, 300]) {
      for (const w of [300, 900, 1600]) {
        const step = rulerStep(units, w, 88);
        const pxPerLabel = (step / units) * w;
        // The largest step is a ceiling, so only clamp-limited cases may
        // fall short — everything else must satisfy the spacing rule.
        if (step !== 720) expect(pxPerLabel).toBeGreaterThanOrEqual(88);
      }
    }
  });

  it("degrades safely on empty input", () => {
    expect(rulerStep(0, 900)).toBe(1);
    expect(rulerStep(48, 0)).toBe(1);
  });
});

describe("rulerTicks", () => {
  it("emits a major tick on each step and a minor between", () => {
    const ticks = rulerTicks(20, 5);
    expect(ticks.filter((t) => t.major).map((t) => t.unit)).toEqual([0, 5, 10, 15, 20]);
    expect(ticks.filter((t) => !t.major).map((t) => t.unit)).toEqual([2.5, 7.5, 12.5, 17.5]);
  });

  it("marks majors correctly despite half-step float drift", () => {
    // 0.5 increments accumulate error; majors must not be missed.
    const ticks = rulerTicks(60, 1);
    expect(ticks.filter((t) => t.major)).toHaveLength(61);
  });

  it("degrades safely on empty input", () => {
    expect(rulerTicks(0, 5)).toEqual([]);
    expect(rulerTicks(20, 0)).toEqual([]);
  });
});

describe("formatUnitsDuration", () => {
  it("formats minutes and hours", () => {
    expect(formatUnitsDuration(0)).toBe("0m");
    expect(formatUnitsDuration(45)).toBe("45m");
    expect(formatUnitsDuration(60)).toBe("1h");
    expect(formatUnitsDuration(83)).toBe("1h 23m");
  });
});

describe("unitMasksToMasks ⇄ masksToUnitMasks round-trip", () => {
    it("converts unit masks to wall-clock masks and back", () => {
        const units = makeUnits(10);
        const original: UnitMaskRegion[] = [
            {
                id: "mask-1",
                startUnit: 2,
                endUnit: 5,
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            },
        ];

        const masks = unitMasksToMasks(original, units);
        expect(masks).toHaveLength(1);
        expect(masks[0].id).toBe("mask-1");
        expect(masks[0].start).toBe(units[2].capturedAt);
        expect(masks[0].end).toBe(units[5].capturedAt);
        expect(masks[0].x).toBe(0.1);
        expect(masks[0].y).toBe(0.2);
        expect(masks[0].width).toBe(0.3);
        expect(masks[0].height).toBe(0.4);

        const roundTripped = masksToUnitMasks(masks, units);
        expect(roundTripped).toEqual(original);
    });

    it("assignMaskTracks distributes overlapping masks", () => {
        const masks: UnitMaskRegion[] = [
            { id: "m1", startUnit: 0, endUnit: 10, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "m2", startUnit: 5, endUnit: 15, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
        ];
        const { assignments, trackCount } = assignMaskTracks(masks);
        expect(trackCount).toBe(2);
        expect(assignments["m1"]).toBe(0);
        expect(assignments["m2"]).toBe(1);
    });

    it("dynamically allocates tracks when 4 or more masks overlap without collision", () => {
        const masks: UnitMaskRegion[] = [
            { id: "m1", startUnit: 0, endUnit: 10, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "m2", startUnit: 1, endUnit: 9, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
            { id: "m3", startUnit: 2, endUnit: 8, x: 0.3, y: 0.3, width: 0.2, height: 0.2 },
            { id: "m4", startUnit: 3, endUnit: 7, x: 0.4, y: 0.4, width: 0.2, height: 0.2 },
            { id: "m5", startUnit: 4, endUnit: 6, x: 0.5, y: 0.5, width: 0.2, height: 0.2 },
        ];

        const { assignments, trackCount, tracks } = assignMaskTracks(masks);
        expect(trackCount).toBe(5);
        expect(tracks).toHaveLength(5);
        expect(assignments["m1"]).toBe(0);
        expect(assignments["m2"]).toBe(1);
        expect(assignments["m3"]).toBe(2);
        expect(assignments["m4"]).toBe(3);
        expect(assignments["m5"]).toBe(4);

        for (const track of tracks) {
            for (let i = 0; i < track.length; i++) {
                for (let j = i + 1; j < track.length; j++) {
                    const a = track[i];
                    const b = track[j];
                    const noOverlap = a.endUnit <= b.startUnit + 0.001 || a.startUnit >= b.endUnit - 0.001;
                    expect(noOverlap).toBe(true);
                }
            }
        }
    });
});

describe("unitBlursToBlurs ⇄ blursToUnitBlurs round-trip", () => {
    it("converts unit blurs to wall-clock blurs and back", () => {
        const units = makeUnits(10);
        const original: UnitBlurRegion[] = [
            {
                id: "blur-1",
                startUnit: 2,
                endUnit: 5,
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            },
        ];

        const blurs = unitBlursToBlurs(original, units);
        expect(blurs).toHaveLength(1);
        expect(blurs[0].id).toBe("blur-1");
        expect(blurs[0].start).toBe(units[2].capturedAt);
        expect(blurs[0].end).toBe(units[5].capturedAt);
        expect(blurs[0].x).toBe(0.1);
        expect(blurs[0].y).toBe(0.2);
        expect(blurs[0].width).toBe(0.3);
        expect(blurs[0].height).toBe(0.4);

        const roundTripped = blursToUnitBlurs(blurs, units);
        expect(roundTripped).toEqual(original);
    });

    it("handles multiple blur regions", () => {
        const units = makeUnits(8);
        const original: UnitBlurRegion[] = [
            {
                id: "b1",
                startUnit: 0,
                endUnit: 2,
                x: 0.05,
                y: 0.05,
                width: 0.2,
                height: 0.2,
            },
            {
                id: "b2",
                startUnit: 4,
                endUnit: 8,
                x: 0.5,
                y: 0.5,
                width: 0.4,
                height: 0.3,
            },
        ];

        const blurs = unitBlursToBlurs(original, units);
        const roundTripped = blursToUnitBlurs(blurs, units);
        expect(roundTripped).toEqual(original);
    });

    it("filters out degenerate blur regions", () => {
        const units = makeUnits(5);
        const invalid: UnitBlurRegion[] = [
            {
                id: "zero-time",
                startUnit: 2,
                endUnit: 2,
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.2,
            },
            {
                id: "zero-width",
                startUnit: 1,
                endUnit: 3,
                x: 0.1,
                y: 0.1,
                width: 0,
                height: 0.2,
            },
        ];

        expect(unitBlursToBlurs(invalid, units)).toEqual([]);
    });

    it("handles empty arrays gracefully", () => {
        const units = makeUnits(5);
        expect(unitBlursToBlurs([], units)).toEqual([]);
        expect(unitBlursToBlurs([{ id: "1", startUnit: 0, endUnit: 1, x: 0, y: 0, width: 0.5, height: 0.5 }], [])).toEqual([]);
        expect(blursToUnitBlurs([], units)).toEqual([]);
        expect(blursToUnitBlurs([{ id: "1", start: "2026-07-01T10:00:00.000Z", end: "2026-07-01T10:01:00.000Z", x: 0, y: 0, width: 0.5, height: 0.5 }], [])).toEqual([]);
    });

    it("preserves sub-second startSec and endSec across round-trip", () => {
        const units = makeUnits(5);
        const subSecBlurs: UnitBlurRegion[] = [
            {
                id: "shot-1",
                startUnit: 0.1429,
                endUnit: 0.2857,
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
            },
        ];

        const serialized = unitBlursToBlurs(subSecBlurs, units);
        expect(serialized[0].startSec).toBe(0.1429);
        expect(serialized[0].endSec).toBe(0.2857);

        const roundTripped = blursToUnitBlurs(serialized, units);
        expect(roundTripped[0].startUnit).toBe(0.1429);
        expect(roundTripped[0].endUnit).toBe(0.2857);
    });
});

describe("shot helpers", () => {
    const testShots: VideoShot[] = [
        { id: "shot-0-0", unitIndex: 0, frameIndex: 0, startSec: 0.0, endSec: 0.1429, duration: 0.1429 },
        { id: "shot-0-1", unitIndex: 0, frameIndex: 1, startSec: 0.1429, endSec: 0.2857, duration: 0.1429 },
        { id: "shot-0-2", unitIndex: 0, frameIndex: 2, startSec: 0.2857, endSec: 0.4286, duration: 0.1429 },
        { id: "shot-1-0", unitIndex: 1, frameIndex: 0, startSec: 1.0, endSec: 2.0, duration: 1.0 },
    ];

    it("finds the active shot at a given time", () => {
        expect(findShotAtTime(0.05, testShots)?.id).toBe("shot-0-0");
        expect(findShotAtTime(0.20, testShots)?.id).toBe("shot-0-1");
        expect(findShotAtTime(0.35, testShots)?.id).toBe("shot-0-2");
        expect(findShotAtTime(1.5, testShots)?.id).toBe("shot-1-0");
        expect(findShotAtTime(0.5, [])).toBeNull();
    });

    it("snaps to nearest shot boundary", () => {
        // Near 0.1429
        expect(snapToNearestShotBoundary(0.13, testShots, 2)).toBe(0.1429);
        expect(snapToNearestShotBoundary(0.15, testShots, 2)).toBe(0.1429);
        // Near 0.2857
        expect(snapToNearestShotBoundary(0.29, testShots, 2)).toBe(0.2857);
        // Fallback without shots
        expect(snapToNearestShotBoundary(1.4, undefined, 2)).toBe(1);
    });
});

describe("unitClockLabel & unitRangeClockLabel", () => {
    it("formats a single unit's clock time", () => {
        const units = makeUnits(5);
        expect(unitClockLabel(units[0])).toBeTruthy();
        expect(unitClockLabel(undefined)).toBe("");
        expect(unitClockLabel(null)).toBe("");
    });

    it("formats a range of units", () => {
        const units = makeUnits(5);
        const single = unitRangeClockLabel(0, 1, units);
        expect(single).toBe(unitClockLabel(units[0]));

        const range = unitRangeClockLabel(0, 4, units);
        expect(range).toContain(" – ");
        expect(range.startsWith(unitClockLabel(units[0]))).toBe(true);

        expect(unitRangeClockLabel(0, 4, [])).toBe("");
    });
});

describe("assignBlurTracks & multi-track presets", () => {
    it("has 3 presets: blue, violet, yellow", () => {
        expect(TRACK_PRESETS).toHaveLength(3);
        expect(TRACK_PRESETS[0].name).toBe("Blue");
        expect(TRACK_PRESETS[1].name).toBe("Violet");
        expect(TRACK_PRESETS[2].name).toBe("Yellow");
    });

    it("puts non-overlapping blurs on Track 0 (single timeline)", () => {
        const blurs: UnitBlurRegion[] = [
            { id: "b1", startUnit: 0, endUnit: 5, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "b2", startUnit: 6, endUnit: 10, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
            { id: "b3", startUnit: 12, endUnit: 15, x: 0.3, y: 0.3, width: 0.2, height: 0.2 },
        ];

        const { assignments, trackCount, tracks } = assignBlurTracks(blurs);
        expect(trackCount).toBe(1);
        expect(assignments["b1"]).toBe(0);
        expect(assignments["b2"]).toBe(0);
        expect(assignments["b3"]).toBe(0);
        expect(tracks[0]).toHaveLength(3);
        expect(tracks[1]).toHaveLength(0);
        expect(tracks[2]).toHaveLength(0);
    });

    it("spills overlapping blurs to Track 1 only when Track 0 is occupied", () => {
        const blurs: UnitBlurRegion[] = [
            { id: "b1", startUnit: 0, endUnit: 10, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "b2", startUnit: 5, endUnit: 15, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
            { id: "b3", startUnit: 12, endUnit: 20, x: 0.3, y: 0.3, width: 0.2, height: 0.2 },
        ];

        const { assignments, trackCount, tracks } = assignBlurTracks(blurs);
        expect(trackCount).toBe(2);
        expect(assignments["b1"]).toBe(0);
        expect(assignments["b2"]).toBe(1);
        expect(assignments["b3"]).toBe(0);
        expect(tracks[0].map((b) => b.id)).toEqual(["b1", "b3"]);
        expect(tracks[1].map((b) => b.id)).toEqual(["b2"]);
    });

    it("spills to Track 2 when both Track 0 and Track 1 are occupied (3 simultaneous blurs)", () => {
        const blurs: UnitBlurRegion[] = [
            { id: "b1", startUnit: 0, endUnit: 10, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "b2", startUnit: 2, endUnit: 8, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
            { id: "b3", startUnit: 4, endUnit: 6, x: 0.3, y: 0.3, width: 0.2, height: 0.2 },
        ];

        const { assignments, trackCount } = assignBlurTracks(blurs);
        expect(trackCount).toBe(3);
        expect(assignments["b1"]).toBe(0);
        expect(assignments["b2"]).toBe(1);
        expect(assignments["b3"]).toBe(2);
    });

    it("allocates distinct collision-free tracks when 4 or more blurs overlap", () => {
        const blurs: UnitBlurRegion[] = [
            { id: "b1", startUnit: 0, endUnit: 10, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "b2", startUnit: 2, endUnit: 8, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
            { id: "b3", startUnit: 4, endUnit: 6, x: 0.3, y: 0.3, width: 0.2, height: 0.2 },
            { id: "b4", startUnit: 5, endUnit: 6, x: 0.4, y: 0.4, width: 0.2, height: 0.2 },
        ];

        const { assignments, trackCount, tracks } = assignBlurTracks(blurs);
        expect(trackCount).toBe(4);
        expect(tracks).toHaveLength(4);
        expect(assignments["b1"]).toBe(0);
        expect(assignments["b2"]).toBe(1);
        expect(assignments["b3"]).toBe(2);
        expect(assignments["b4"]).toBe(3);
    });

    it("checks max 3 blur limit correctly at a given time", () => {
        const blurs: UnitBlurRegion[] = [
            { id: "b1", startUnit: 0, endUnit: 10, x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
            { id: "b2", startUnit: 2, endUnit: 8, x: 0.2, y: 0.2, width: 0.2, height: 0.2 },
        ];

        expect(canAddBlurAtTime(1, blurs)).toBe(true);
        expect(canAddBlurAtTime(5, blurs)).toBe(true);

        const threeBlurs: UnitBlurRegion[] = [
            ...blurs,
            { id: "b3", startUnit: 4, endUnit: 6, x: 0.3, y: 0.3, width: 0.2, height: 0.2 },
        ];

        expect(canAddBlurAtTime(5, threeBlurs)).toBe(false);
        expect(canAddBlurAtTime(1, threeBlurs)).toBe(true);
        expect(canAddBlurAtTime(7, threeBlurs)).toBe(true);
    });

    it("evaluates isBlurActiveAtTime with boundary tolerances", () => {
        const blur: UnitBlurRegion = {
            id: "b1",
            startUnit: 2.0,
            endUnit: 5.0,
            x: 0.1,
            y: 0.1,
            width: 0.2,
            height: 0.2,
        };

        expect(isBlurActiveAtTime(2.0, blur)).toBe(true);
        expect(isBlurActiveAtTime(3.5, blur)).toBe(true);
        expect(isBlurActiveAtTime(5.0, blur)).toBe(true);
        expect(isBlurActiveAtTime(1.995, blur)).toBe(true);
        expect(isBlurActiveAtTime(5.004, blur)).toBe(true);
        expect(isBlurActiveAtTime(1.98, blur)).toBe(false);
        expect(isBlurActiveAtTime(5.01, blur)).toBe(false);
    });

    it("computes safe cursor time clamped strictly inside shot boundaries", () => {
        expect(computeSafeCursorTime(1.0, 3.0, 1.0)).toBe(1.005);
        expect(computeSafeCursorTime(1.0, 3.0, 3.0)).toBe(2.995);
        expect(computeSafeCursorTime(1.0, 3.0, 2.0)).toBe(2.0);
        expect(computeSafeCursorTime(1.0, 3.0, 0.5)).toBe(1.005);
        expect(computeSafeCursorTime(1.0, 3.0, 4.0)).toBe(2.995);
    });
});


