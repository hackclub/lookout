import { describe, expect, it } from "vitest";
import {
  STREAK_WINDOW_MS,
  CAPTURED_AT_PAST_TOLERANCE_MS,
  CAPTURED_AT_FUTURE_TOLERANCE_MS,
  SCREENSHOT_INTERVAL_MS,
} from "@lookout/shared";
import { creditCapture, validateCapturedAt } from "./timing.js";

const T0 = new Date("2025-01-01T00:00:00.000Z");
const ms = (d: Date, deltaMs: number) => new Date(d.getTime() + deltaMs);

describe("creditCapture", () => {
  it("seeds with 0 credit when no anchor exists", () => {
    const d = creditCapture(T0, null, 0);
    expect(d.credit).toBe(0);
    expect(d.newAnchor).toEqual(T0);
    expect(d.newCount).toBe(0);
    expect(d.expectedAt).toBeNull();
    expect(d.inWindow).toBe(false);
    expect(d.nextExpectedAt).toEqual(ms(T0, SCREENSHOT_INTERVAL_MS));
  });

  it("credits 60s when capture lands exactly on the expected mark", () => {
    const capturedAt = ms(T0, SCREENSHOT_INTERVAL_MS);
    const d = creditCapture(capturedAt, T0, 0);
    expect(d.credit).toBe(60);
    expect(d.newAnchor).toEqual(T0);
    expect(d.newCount).toBe(1);
    expect(d.expectedAt).toEqual(ms(T0, SCREENSHOT_INTERVAL_MS));
    expect(d.inWindow).toBe(true);
    expect(d.nextExpectedAt).toEqual(ms(T0, SCREENSHOT_INTERVAL_MS * 2));
  });

  it("credits 60s at the inner edge of the streak window (+29s)", () => {
    const capturedAt = ms(T0, SCREENSHOT_INTERVAL_MS + (STREAK_WINDOW_MS - 1_000));
    const d = creditCapture(capturedAt, T0, 0);
    expect(d.credit).toBe(60);
    expect(d.inWindow).toBe(true);
  });

  it("credits 60s exactly at the window boundary (+30s)", () => {
    const capturedAt = ms(T0, SCREENSHOT_INTERVAL_MS + STREAK_WINDOW_MS);
    const d = creditCapture(capturedAt, T0, 0);
    expect(d.credit).toBe(60);
    expect(d.inWindow).toBe(true);
  });

  it("resets when capture lands outside the window (+31s)", () => {
    const capturedAt = ms(T0, SCREENSHOT_INTERVAL_MS + STREAK_WINDOW_MS + 1_000);
    const d = creditCapture(capturedAt, T0, 0);
    expect(d.credit).toBe(0);
    expect(d.newAnchor).toEqual(capturedAt);
    expect(d.newCount).toBe(0);
    expect(d.expectedAt).toEqual(ms(T0, SCREENSHOT_INTERVAL_MS));
    expect(d.inWindow).toBe(false);
    expect(d.nextExpectedAt).toEqual(ms(capturedAt, SCREENSHOT_INTERVAL_MS));
  });

  it("resets on early capture outside window (-31s)", () => {
    const capturedAt = ms(T0, SCREENSHOT_INTERVAL_MS - STREAK_WINDOW_MS - 1_000);
    const d = creditCapture(capturedAt, T0, 0);
    expect(d.credit).toBe(0);
    expect(d.inWindow).toBe(false);
    expect(d.newAnchor).toEqual(capturedAt);
  });

  it("advances streak count: anchor + (count+1)*60 is the expected mark", () => {
    // Streak has already credited 5 captures, so expected for the next
    // capture is anchor + 6*60s.
    const capturedAt = ms(T0, SCREENSHOT_INTERVAL_MS * 6);
    const d = creditCapture(capturedAt, T0, 5);
    expect(d.credit).toBe(60);
    expect(d.newCount).toBe(6);
    expect(d.nextExpectedAt).toEqual(ms(T0, SCREENSHOT_INTERVAL_MS * 7));
  });

  it("steady-state invariant: nextExpectedAt of capture N equals expectedAt of capture N+1", () => {
    // Simulate a 5-capture chain where each one's capturedAt equals the
    // previous nextExpectedAt. Every capture must credit.
    let anchor: Date | null = null;
    let count = 0;
    let prevNextExpected: Date | null = null;
    for (let n = 0; n < 5; n++) {
      const capturedAt = prevNextExpected ?? T0;
      const d = creditCapture(capturedAt, anchor, count);
      if (n === 0) {
        expect(d.credit).toBe(0); // seed
      } else {
        expect(d.credit).toBe(60);
        expect(d.inWindow).toBe(true);
      }
      anchor = d.newAnchor;
      count = d.newCount;
      prevNextExpected = d.nextExpectedAt;
    }
    // After 5 iterations: 1 seed + 4 credits = 240s total
    expect(count).toBe(4);
  });
});

describe("validateCapturedAt", () => {
  const serverNow = T0;
  // Keep startedAt within the past envelope so "before session start" can
  // be exercised independently of "too old". Beyond the envelope, "too old"
  // is the relevant rejection and a separate test covers it.
  const startedAt = ms(T0, -120_000); // 2 min ago

  it("accepts a capturedAt right at server-now", () => {
    expect(validateCapturedAt(serverNow, serverNow, startedAt, null)).toEqual({
      ok: true,
    });
  });

  it("rejects future-skewed capturedAt beyond envelope", () => {
    const cap = ms(serverNow, CAPTURED_AT_FUTURE_TOLERANCE_MS + 1_000);
    const r = validateCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ ok: false, code: "captured_at_future" });
  });

  it("accepts at exactly the future-envelope boundary", () => {
    const cap = ms(serverNow, CAPTURED_AT_FUTURE_TOLERANCE_MS);
    expect(validateCapturedAt(cap, serverNow, startedAt, null)).toEqual({
      ok: true,
    });
  });

  it("rejects past-skewed capturedAt beyond envelope", () => {
    const cap = ms(serverNow, -CAPTURED_AT_PAST_TOLERANCE_MS - 1_000);
    const r = validateCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ ok: false, code: "captured_at_too_old" });
  });

  it("rejects capturedAt more than 2s before session.startedAt", () => {
    const cap = ms(startedAt, -3_000); // 3s before startedAt, past slop window
    const r = validateCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ ok: false, code: "captured_at_before_session_start" });
  });

  it("accepts capturedAt within the 2s slop before startedAt (activation race)", () => {
    const cap = ms(startedAt, -1_500); // within 2s tolerance
    expect(validateCapturedAt(cap, serverNow, startedAt, null)).toEqual({
      ok: true,
    });
  });

  it("accepts capturedAt equal to startedAt (boundary)", () => {
    expect(validateCapturedAt(startedAt, serverNow, startedAt, null)).toEqual({
      ok: true,
    });
  });

  it("rejects non-monotonic capturedAt (strictly less than latest)", () => {
    const latest = ms(serverNow, -30_000);
    const cap = ms(serverNow, -60_000);
    const r = validateCapturedAt(cap, serverNow, startedAt, latest);
    expect(r).toEqual({ ok: false, code: "captured_at_not_monotonic" });
  });

  it("rejects equal capturedAt against latest (idempotent retries handled at route layer)", () => {
    const latest = ms(serverNow, -30_000);
    const r = validateCapturedAt(latest, serverNow, startedAt, latest);
    expect(r).toEqual({ ok: false, code: "captured_at_not_monotonic" });
  });

  it("accepts strictly greater capturedAt", () => {
    const latest = ms(serverNow, -60_000);
    const cap = ms(serverNow, -30_000);
    expect(validateCapturedAt(cap, serverNow, startedAt, latest)).toEqual({
      ok: true,
    });
  });
});
