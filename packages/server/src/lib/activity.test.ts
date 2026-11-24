import { describe, expect, it } from "vitest";
import { ACTIVITY_MAX_SAMPLES, sanitizeActivity } from "./activity.js";

const sample = (over: Record<string, unknown> = {}) => ({
  at: 1_750_000_000_000,
  method: "ax",
  cursor: { display: 1, x: 0.25, y: 0.75, inCapture: true },
  frontmost: {
    app: "com.microsoft.VSCode",
    captured: true,
    window: { display: 1, x: -0.0047, y: 0.025, width: 0.5, height: 0.95 },
  },
  windows: [{ id: 77, active: true }],
  displays: [
    { id: 1, x: 0, y: 0, width: 1512, height: 982, pixelWidth: 3024, pixelHeight: 1964, captured: true },
    { id: 2, x: 1512, y: -200, width: 2560, height: 1440, captured: false },
  ],
  ...over,
});

describe("sanitizeActivity", () => {
  it("keeps a well-formed payload as sent", () => {
    const raw = { v: 1, samples: [sample()] };
    expect(sanitizeActivity(raw)).toEqual(raw);
  });

  it("is null for anything that isn't an activity payload", () => {
    for (const raw of [undefined, null, 1, "x", [], {}, { samples: "no" }, { samples: [] }]) {
      expect(sanitizeActivity(raw)).toBeNull();
    }
  });

  it("drops samples without a time or method, keeps the rest", () => {
    const out = sanitizeActivity({
      v: 1,
      samples: [sample({ at: "soon" }), sample({ method: "AX!" }), sample({ at: 2 })],
    });
    expect(out?.samples.map((s) => s.at)).toEqual([2]);
  });

  it("nulls an off-shape cursor or frontmost instead of the sample", () => {
    const out = sanitizeActivity({
      v: 1,
      samples: [
        sample({ cursor: { display: 1, x: 1.5, y: 0, inCapture: true } }),
        sample({ cursor: { display: -1, x: 0, y: 0, inCapture: true } }),
        sample({ frontmost: { app: "   " } }),
        sample({ frontmost: { app: "a".repeat(1000), captured: "yes" } }),
      ],
    });
    expect(out?.samples[0].cursor).toBeNull();
    expect(out?.samples[1].cursor).toBeNull();
    expect(out?.samples[2].frontmost).toBeNull();
    expect(out?.samples[3].frontmost).toEqual({ app: "a".repeat(256), captured: null });
  });

  it("strips unknown fields and bad window entries", () => {
    const out = sanitizeActivity({
      v: 1,
      extra: true,
      samples: [
        {
          ...sample(),
          title: "secret.txt — VS Code",
          windows: [{ id: 1, active: true, title: "x" }, { id: "2", active: false }],
        },
      ],
    });
    expect(out?.samples[0]).not.toHaveProperty("title");
    expect(out?.samples[0].windows).toEqual([{ id: 1, active: true }]);
    expect(out).not.toHaveProperty("extra");
  });

  it("keeps only the newest samples past the cap", () => {
    const samples = Array.from({ length: ACTIVITY_MAX_SAMPLES + 5 }, (_, i) => sample({ at: i + 1 }));
    const out = sanitizeActivity({ v: 1, samples });
    expect(out?.samples).toHaveLength(ACTIVITY_MAX_SAMPLES);
    expect(out?.samples[0].at).toBe(6);
  });

  it("keeps the app's display name trimmed and capped, drops a blank or non-string one", () => {
    const fm = (name: unknown) => sample({ frontmost: { app: "Code.exe", name, captured: true } });
    const out = sanitizeActivity({
      v: 1,
      samples: [fm("  Visual Studio Code "), fm("   "), fm(42), fm("n".repeat(1000))],
    });
    expect(out?.samples.map((s) => s.frontmost?.name)).toEqual([
      "Visual Studio Code",
      undefined,
      undefined,
      "n".repeat(256),
    ]);
    expect(out?.samples[1].frontmost).toEqual({ app: "Code.exe", captured: true });
  });

  it("keeps the frontmost window's frame only when it is a plausible fraction of a display", () => {
    const fm = (window: unknown) => sample({ frontmost: { app: "Code", captured: true, window } });
    const out = sanitizeActivity({
      v: 1,
      samples: [
        fm({ display: 1, x: 0.1, y: 0.1, width: 0, height: 0.5 }),
        fm({ display: 1, x: 99, y: 0, width: 0.5, height: 0.5 }),
        fm({ display: "1", x: 0, y: 0, width: 0.5, height: 0.5 }),
        fm({ display: 2, x: -0.1, y: 0, width: 2.4, height: 1, title: "secret" }),
      ],
    });
    expect(out?.samples.slice(0, 3).map((s) => s.frontmost)).toEqual(
      Array(3).fill({ app: "Code", captured: true }),
    );
    expect(out?.samples[3].frontmost?.window).toEqual({ display: 2, x: -0.1, y: 0, width: 2.4, height: 1 });
  });

  it("keeps well-formed displays, drops bad ones and a half resolution", () => {
    const out = sanitizeActivity({
      v: 1,
      samples: [
        sample({
          displays: [
            { id: 1, x: 0, y: 0, width: 1920, height: 1080, pixelWidth: 1920, captured: true },
            { id: 2, x: 0, y: 0, width: -5, height: 1080, captured: false },
            { id: 3, x: 0, y: 0, width: 800, height: 600 },
            { id: 4, x: 1920, y: 0, width: 800, height: 600, captured: false, name: "DELL U2720Q" },
          ],
        }),
        sample({ displays: [{ id: -1 }] }),
        sample({ displays: Array.from({ length: 40 }, (_, i) => ({ id: i, x: 0, y: 0, width: 1, height: 1, captured: false })) }),
      ],
    });
    expect(out?.samples[0].displays).toEqual([
      { id: 1, x: 0, y: 0, width: 1920, height: 1080, captured: true },
      { id: 4, x: 1920, y: 0, width: 800, height: 600, captured: false },
    ]);
    expect(out?.samples[1]).not.toHaveProperty("displays");
    expect(out?.samples[2].displays).toHaveLength(16);
  });
});
