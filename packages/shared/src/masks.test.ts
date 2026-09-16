import { describe, expect, it } from "vitest";
import {
  normalizeMasks,
  isMaskActiveAt,
  activeMasksAt,
  type MaskRegion,
} from "./masks.js";

describe("masks", () => {
  const baseTime = 1_700_000_000_000;
  const t = (offsetSec: number) => new Date(baseTime + offsetSec * 1000).toISOString();

  it("rejects non-array input", () => {
    const res = normalizeMasks("not-an-array");
    expect(res.ok).toBe(false);
  });

  it("validates and normalizes valid mask regions", () => {
    const raw = [
      {
        id: "mask-1",
        start: t(10),
        end: t(20),
        x: 0.1,
        y: 0.2,
        width: 0.3,
        height: 0.4,
      },
      {
        id: "mask-2",
        start: t(5),
        end: t(8),
        x: 0.5,
        y: 0.5,
        width: 0.2,
        height: 0.2,
      },
    ];

    const res = normalizeMasks(raw);
    expect(res.ok).toBe(true);
    if (!res.ok) return;

    expect(res.masks).toHaveLength(2);
    // Should be sorted by start time: mask-2 (t(5)) then mask-1 (t(10))
    expect(res.masks[0].id).toBe("mask-2");
    expect(res.masks[1].id).toBe("mask-1");
  });

  it("clamps out-of-bounds coordinates to [0, 1]", () => {
    const raw = [
      {
        id: "mask-oob",
        start: t(0),
        end: t(10),
        x: -0.5,
        y: 0.8,
        width: 1.5,
        height: 0.5,
      },
    ];

    const res = normalizeMasks(raw);
    expect(res.ok).toBe(true);
    if (!res.ok) return;

    expect(res.masks[0].x).toBe(0);
    expect(res.masks[0].y).toBe(0.8);
    // width clamped to 1 - x = 1.0
    expect(res.masks[0].width).toBe(1);
    // height clamped to 1 - y = 0.2
    expect(res.masks[0].height).toBe(0.2);
  });

  it("drops zero-area mask regions", () => {
    const raw = [
      {
        id: "mask-zero",
        start: t(0),
        end: t(10),
        x: 0.5,
        y: 0.5,
        width: 0,
        height: 0.1,
      },
    ];

    const res = normalizeMasks(raw);
    expect(res.ok).toBe(true);
    if (!res.ok) return;
    expect(res.masks).toHaveLength(0);
  });

  it("determines active masks at a specific time", () => {
    const masks: MaskRegion[] = [
      {
        id: "b1",
        start: t(10),
        end: t(20),
        x: 0.1,
        y: 0.1,
        width: 0.2,
        height: 0.2,
      },
      {
        id: "b2",
        start: t(15),
        end: t(25),
        x: 0.5,
        y: 0.5,
        width: 0.2,
        height: 0.2,
      },
    ];

    expect(isMaskActiveAt(baseTime + 5_000, masks[0])).toBe(false);
    expect(isMaskActiveAt(baseTime + 12_000, masks[0])).toBe(true);
    expect(isMaskActiveAt(baseTime + 20_000, masks[0])).toBe(false);

    // Simultaneous masks at t = 18s
    const active = activeMasksAt(baseTime + 18_000, masks);
    expect(active).toHaveLength(2);
    expect(active.map((b) => b.id)).toEqual(["b1", "b2"]);
  });

  it("preserves startSec and endSec with sub-second precision", () => {
    const raw = [
      {
        id: "mask-subsec",
        start: t(0),
        end: t(1),
        startSec: 0.142857,
        endSec: 0.285714,
        x: 0.1,
        y: 0.1,
        width: 0.5,
        height: 0.5,
      },
    ];

    const res = normalizeMasks(raw);
    expect(res.ok).toBe(true);
    if (!res.ok) return;

    expect(res.masks[0].startSec).toBe(0.1429);
    expect(res.masks[0].endSec).toBe(0.2857);
  });
});
