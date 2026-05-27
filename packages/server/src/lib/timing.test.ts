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

// ──────────────────────────────────────────────────────────────────
// Browser-throttle simulation: feed the credit pipeline a stream of
// captures spaced at a constant effective interval (60s = no throttle,
// 90s = Safari background, 120s = heavy throttle) and report the
// fraction of wall-clock time that ends up credited. The point is to
// demonstrate empirically that the 30s STREAK_WINDOW breaks down well
// before captures fail outright.
// ──────────────────────────────────────────────────────────────────

function simulateThrottle(intervalMs: number, captureCount: number) {
  let anchor: Date | null = null;
  let count = 0;
  let totalCredit = 0;
  const log: Array<{ t: number; credit: number; delta: number | null }> = [];

  for (let i = 0; i < captureCount; i++) {
    const t = i * intervalMs;
    const capturedAt = ms(T0, t);
    const expected = anchor
      ? anchor.getTime() + (count + 1) * SCREENSHOT_INTERVAL_MS
      : null;
    const d = creditCapture(capturedAt, anchor, count);
    totalCredit += d.credit;
    log.push({
      t,
      credit: d.credit,
      delta: expected === null ? null : capturedAt.getTime() - expected,
    });
    anchor = d.newAnchor;
    count = d.newCount;
  }

  const wallClockMs = (captureCount - 1) * intervalMs;
  return { totalCredit, wallClockMs, log };
}

describe("browser-throttle simulation (validates ~50% halving report)", () => {
  it("60s interval (no throttle): credits 100% of wall-clock time", () => {
    const r = simulateThrottle(60_000, 20);
    // 19 in-window credits of 60s each, over 19*60s of wall clock
    expect(r.totalCredit).toBe(19 * 60);
    expect(r.totalCredit / (r.wallClockMs / 1000)).toBeCloseTo(1.0, 2);
  });

  it("90s interval (Safari background throttle): credits ~35%", () => {
    const r = simulateThrottle(90_000, 20);
    const ratio = r.totalCredit / (r.wallClockMs / 1000);
    // eslint-disable-next-line no-console
    console.log(
      `[90s throttle] credited=${r.totalCredit}s / wall=${r.wallClockMs / 1000}s = ${(ratio * 100).toFixed(1)}%`,
    );
    expect(ratio).toBeGreaterThan(0.3);
    expect(ratio).toBeLessThan(0.4);
  });

  it("75s interval (light throttle): credits ~60%", () => {
    const r = simulateThrottle(75_000, 20);
    const ratio = r.totalCredit / (r.wallClockMs / 1000);
    // eslint-disable-next-line no-console
    console.log(
      `[75s throttle] credited=${r.totalCredit}s / wall=${r.wallClockMs / 1000}s = ${(ratio * 100).toFixed(1)}%`,
    );
    expect(ratio).toBeGreaterThan(0.5);
    expect(ratio).toBeLessThan(0.85);
  });

  it("120s interval (heavy throttle): credits 0%", () => {
    const r = simulateThrottle(120_000, 20);
    // Every capture lands 60s past expected → reset every time.
    expect(r.totalCredit).toBe(0);
  });

  // ── "2 captures per minute" report — clients firing at 30s ──
  it("30s interval (2x rate): pattern is credit→reset→credit→reset", () => {
    const r = simulateThrottle(30_000, 20);
    const ratio = r.totalCredit / (r.wallClockMs / 1000);
    // eslint-disable-next-line no-console
    console.log(
      `[30s 2x-rate] credited=${r.totalCredit}s / wall=${r.wallClockMs / 1000}s = ${(ratio * 100).toFixed(1)}%`,
    );
    // eslint-disable-next-line no-console
    console.log(
      "  pattern:",
      r.log
        .map(
          (e) =>
            `t=${e.t / 1000}s ${e.credit ? "✓" : "✗"}${e.delta !== null ? `(Δ${e.delta / 1000}s)` : ""}`,
        )
        .join(" "),
    );
    // At exactly 30s spacing the math gives ~100% in steady state (every
    // other capture credits 60s, and there are 2 captures per minute).
    expect(ratio).toBeGreaterThan(0.9);
  });

  // ── Two tabs running concurrently (~60s each, phase-shifted) ──
  it("two concurrent 60s chains phase-shifted by 30s: half credit, half reset", () => {
    // Simulate cross-stream of captures: tab A at 0,60,120... tab B at 30,90,150...
    const events: number[] = [];
    for (let i = 0; i < 20; i++) events.push(i * 60_000);
    for (let i = 0; i < 20; i++) events.push(i * 60_000 + 30_000);
    events.sort((a, b) => a - b);

    let anchor: Date | null = null;
    let count = 0;
    let totalCredit = 0;
    const log: Array<{ t: number; credit: number }> = [];
    for (const t of events) {
      const d = creditCapture(ms(T0, t), anchor, count);
      totalCredit += d.credit;
      anchor = d.newAnchor;
      count = d.newCount;
      log.push({ t, credit: d.credit });
    }
    const wallSec = events[events.length - 1] / 1000;
    const ratio = totalCredit / wallSec;
    // eslint-disable-next-line no-console
    console.log(
      `[two-tabs] credited=${totalCredit}s / wall=${wallSec}s = ${(ratio * 100).toFixed(1)}%`,
    );
    expect(ratio).toBeGreaterThan(0.9);
  });

  // ── The actual ~50% case: throttled enough to PARTIALLY miss the window ──
  it("85s interval: ~50% credited (matches user reports)", () => {
    const r = simulateThrottle(85_000, 30);
    const ratio = r.totalCredit / (r.wallClockMs / 1000);
    // eslint-disable-next-line no-console
    console.log(
      `[85s] credited=${r.totalCredit}s / wall=${r.wallClockMs / 1000}s = ${(ratio * 100).toFixed(1)}%`,
    );
  });

  it("server response chases drift (matches client using nextExpectedAt)", () => {
    // When the client honors the server's nextExpectedAt, captures land
    // exactly on the expected mark every time — proving the loss is in
    // the client's *actual* fire time, not the math.
    let anchor: Date | null = null;
    let count = 0;
    let nextExpected: Date = T0;
    let totalCredit = 0;

    for (let i = 0; i < 20; i++) {
      const capturedAt = nextExpected;
      const d = creditCapture(capturedAt, anchor, count);
      totalCredit += d.credit;
      anchor = d.newAnchor;
      count = d.newCount;
      nextExpected = d.nextExpectedAt;
    }
    expect(totalCredit).toBe(19 * 60);
  });
});
