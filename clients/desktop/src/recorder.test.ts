/**
 * The webview's clock derivation must agree with the core's: both the main
 * window and the tray popup tick from the `RecorderClock` the core hands
 * them, and the menu bar ticks from the same clock in Rust. These pin the
 * JavaScript side of that contract (the Rust side is `clock.rs`).
 */
import { describe, expect, it, vi } from "vitest";

// recorder.ts pulls in the logged `invoke` wrapper, which installs window
// error handlers at import time; there is no window under Node.
vi.mock("./logger.js", () => ({ invoke: vi.fn() }));

import { deriveRecorderDisplaySeconds, isPrompting, recorderErrorMessage, type RecorderClock } from "./recorder.js";

const T0 = 1_700_000_000_000;
const MAX = 75;

function clock(overrides: Partial<RecorderClock> = {}): RecorderClock {
  return {
    baseSeconds: 0,
    anchorAtUnixMs: T0,
    running: true,
    holdAtUnixMs: null,
    maxInterpolationSeconds: MAX,
    ...overrides,
  };
}

describe("deriveRecorderDisplaySeconds", () => {
  it("interpolates at wall-clock rate from the anchor while running", () => {
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 120 }), T0)).toBe(120);
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 120 }), T0 + 30_000)).toBe(150);
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 120 }), T0 + 30_999)).toBe(150);
  });

  it("caps interpolation at one capture interval plus latency slack", () => {
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 120 }), T0 + 90_000)).toBe(120 + MAX);
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 120 }), T0 + 600_000)).toBe(120 + MAX);
  });

  it("shows the bare base when not running", () => {
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 120, running: false }), T0 + 30_000)).toBe(120);
  });

  it("freezes at the hold instant while a pause is in flight", () => {
    const c = clock({ baseSeconds: 0, holdAtUnixMs: T0 + 23_000 });
    expect(deriveRecorderDisplaySeconds(c, T0 + 23_000)).toBe(23);
    expect(deriveRecorderDisplaySeconds(c, T0 + 28_000)).toBe(23);
    expect(deriveRecorderDisplaySeconds(c, T0 + 90_000)).toBe(23);
  });

  it("never goes negative when the anchor is ahead of now", () => {
    expect(deriveRecorderDisplaySeconds(clock({ baseSeconds: 60, anchorAtUnixMs: T0 + 5_000 }), T0)).toBe(60);
  });
});

describe("helpers", () => {
  it("knows which phases show the naming prompt", () => {
    expect(isPrompting("stopPrompting")).toBe(true);
    expect(isPrompting("stopFinishing")).toBe(true);
    expect(isPrompting("stopPausing")).toBe(false);
    expect(isPrompting("recording")).toBe(false);
  });

  it("renders the core's error payloads", () => {
    expect(recorderErrorMessage({ kind: "api", status: 500, message: "HTTP 500" })).toBe("HTTP 500");
    expect(recorderErrorMessage({ kind: "invalidPhase", phase: "paused" })).toContain("paused");
    expect(recorderErrorMessage({ kind: "sourceLost" })).toContain("pick one again");
    expect(recorderErrorMessage(new Error("plain"))).toBe("plain");
  });
});
