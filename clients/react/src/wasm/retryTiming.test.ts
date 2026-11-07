import { describe, expect, it } from "vitest";
import { retryTimingPatch } from "./retryTiming.js";

describe("retryTimingPatch", () => {
  it("leaves the engine alone for the defaults", () => {
    expect(retryTimingPatch({ maxRetries: 3, retryDelays: [2_000, 4_000, 8_000] })).toBeNull();
  });

  it("carries a custom attempt count", () => {
    expect(retryTimingPatch({ maxRetries: 6, retryDelays: [2_000, 4_000, 8_000] })).toEqual({
      maxAttempts: 6,
      retryDelaysMs: [2_000, 4_000, 8_000],
    });
  });

  it("repeats the last delay when the list is short, as the old uploader did", () => {
    expect(retryTimingPatch({ maxRetries: 3, retryDelays: [500] })).toEqual({
      maxAttempts: 3,
      retryDelaysMs: [500, 500, 500],
    });
  });

  it("never asks for fewer than one attempt or a negative wait", () => {
    expect(retryTimingPatch({ maxRetries: 0, retryDelays: [] })).toEqual({
      maxAttempts: 1,
      retryDelaysMs: [0, 0, 0],
    });
    expect(retryTimingPatch({ maxRetries: 2, retryDelays: [-5, Number.NaN] })?.retryDelaysMs).toEqual([0, 0, 0]);
  });
});
