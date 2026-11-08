import { describe, expect, it } from "vitest";
import { ACTIVITY_MAX_SAMPLES, sanitizeActivity } from "./activity.js";

const sample = (over: Record<string, unknown> = {}) => ({
  at: 1_750_000_000_000,
  method: "ax",
  cursor: { display: 1, x: 0.25, y: 0.75, inCapture: true },
  frontmost: { app: "com.microsoft.VSCode", captured: true },
  windows: [{ id: 77, active: true }],
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
});
