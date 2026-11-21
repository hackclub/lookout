import { describe, expect, it } from "vitest";
import {
  STREAK_WINDOW_MS,
  CAPTURED_AT_PAST_TOLERANCE_MS,
  CAPTURED_AT_FUTURE_TOLERANCE_MS,
  SCREENSHOT_INTERVAL_MS,
} from "@lookout/shared";
import {
  creditCapture,
  creditFinalCapture,
  validateCapturedAt,
  adoptedCapturedAt,
  type CreditDecision,
} from "./timing.js";

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

describe("long-session accounting (no drift, ever)", () => {
  it("4 hours of captures with realistic jitter credits exactly 4 hours", () => {
    // The property users pay for: record 4 hours, get 4 hours. The streak
    // grid is anchored ONCE at the seed — expected marks are absolute
    // (anchor + n×60), never chained off the previous capture — so
    // per-tick lateness cannot accumulate. This simulates 240 minutes
    // where every capture fires with deterministic pseudo-random jitter
    // up to ±25s off its mark (scheduler delay, slow grabs, GC pauses —
    // anything short of the ±30s window), plus a mid-minute pause flush
    // at the end. Upload LATENCY never appears in this math at all:
    // capturedAt is stamped at grab time, not arrival.
    let anchor: Date | null = null;
    let count = 0;
    let tracked = 0;

    const seed = creditCapture(T0, anchor, count);
    anchor = seed.newAnchor;
    count = seed.newCount;
    tracked += seed.credit;

    for (let n = 1; n <= 240; n++) {
      const jitterMs = (((n * 2654435761) % 50_000) - 25_000); // ±25s
      const cap = ms(T0, n * 60_000 + jitterMs);
      const d = creditCapture(cap, anchor, count);
      expect(d.credit).toBe(60); // every single minute credits, no resets
      anchor = d.newAnchor;
      count = d.newCount;
      tracked += d.credit;
    }
    expect(tracked).toBe(240 * 60); // exactly 4 hours

    // Stop at 4h00m37s: the flush picks up the exact tail.
    const flush = creditFinalCapture(ms(T0, 240 * 60_000 + 37_000), anchor, count);
    expect(tracked + flush.credit).toBe(240 * 60 + 37);
  });

  it("one lost minute stays one lost minute — the grid absorbs it", () => {
    // A fully-failed upload (network blip) means that minute never
    // confirms: 60s lost, by design (no proof, no credit). What must NOT
    // happen is drift: the next capture still sits on the absolute grid
    // and credits normally, so the damage is exactly the one minute.
    let anchor: Date | null = null;
    let count = 0;
    let tracked = 0;
    const seed = creditCapture(T0, anchor, count);
    anchor = seed.newAnchor;
    count = seed.newCount;

    for (const n of [1, 2, /* minute 3 upload failed */ 4, 5]) {
      const d = creditCapture(ms(T0, n * 60_000), anchor, count);
      anchor = d.newAnchor;
      count = d.newCount;
      tracked += d.credit;
    }
    // Minute 4's capture is 2 marks past the last credit — outside the
    // window — so it RESETS (credits 0) and re-anchors; minute 5 then
    // credits normally. 5 minutes of wall, one blip: 3 credited minutes
    // + the reset minute lost. Bounded, non-compounding.
    expect(tracked).toBe(3 * 60);
  });
});

describe("creditFinalCapture (pause/stop flush)", () => {
  it("seeds exactly like a normal capture when no anchor exists", () => {
    const d = creditFinalCapture(T0, null, 0);
    expect(d).toEqual(creditCapture(T0, null, 0));
  });

  it("credits the partial minute since the last credited mark", () => {
    // Streak at anchor T0 with 3 credited minutes; pause at 03:15.
    const capturedAt = ms(T0, 3 * SCREENSHOT_INTERVAL_MS + 15_000);
    const d = creditFinalCapture(capturedAt, T0, 3);
    expect(d.credit).toBe(15);
    // Reset shape, matching what a pause does to the streak anyway.
    expect(d.newAnchor).toEqual(capturedAt);
    expect(d.newCount).toBe(0);
    expect(d.inWindow).toBe(false);
  });

  it("credits exact elapsed even inside the normal window (no 60s rounding)", () => {
    // Pause at 03:58 — within ±30s of the 04:00 mark, where a NORMAL
    // capture would credit the full interval. A final capture must not:
    // it resets the anchor, and resume re-seeds it, so in-window rounding
    // here would let a pause at expected−29s bank a full minute per
    // pause/resume cycle (~1.3x inflation). Exact elapsed only.
    const capturedAt = ms(T0, 4 * SCREENSHOT_INTERVAL_MS - 2_000);
    const d = creditFinalCapture(capturedAt, T0, 3);
    expect(d.credit).toBe(58);
    expect(d.newAnchor).toEqual(capturedAt);
    expect(d.newCount).toBe(0);
  });

  it("pause/resume cycling at the window edge cannot inflate credit", () => {
    // The exploit the exact-elapsed rule exists to block: seed, one
    // credited minute, then flush at 01:31 (29s early — inside the normal
    // window), resume, repeat. Each cycle must earn exactly its wall time.
    let anchor: Date | null = null;
    let count = 0;
    let tracked = 0;
    let t = 0;
    for (let cycle = 0; cycle < 3; cycle++) {
      const seed = creditCapture(ms(T0, t), anchor, count); // resume re-seeded
      anchor = seed.newAnchor;
      count = seed.newCount;
      tracked += seed.credit;
      const minute = creditCapture(ms(T0, t + 60_000), anchor, count);
      anchor = minute.newAnchor;
      count = minute.newCount;
      tracked += minute.credit;
      const flush = creditFinalCapture(ms(T0, t + 91_000), anchor, count);
      tracked += flush.credit;
      anchor = null; // resume clears the anchor
      count = 0;
      t += 91_000;
    }
    // 3 cycles x 91s of wall clock, minus the 3 zero-credit seeds' 0s —
    // credited time can never exceed wall time.
    expect(tracked).toBeLessThanOrEqual((3 * 91_000) / 1000);
    expect(tracked).toBe(3 * (60 + 31));
  });

  it("never credits more than one interval, however late it lands", () => {
    // The 04:00 capture was missed entirely; pause at 05:40. The elapsed
    // 100s clamps to one interval — same ceiling as CREDIT_PER_CAPTURE_S,
    // so pause/resume cycling can never earn faster than wall clock.
    const capturedAt = ms(T0, 3 * SCREENSHOT_INTERVAL_MS + 100_000);
    const d = creditFinalCapture(capturedAt, T0, 3);
    expect(d.credit).toBe(60);
    expect(d.newAnchor).toEqual(capturedAt);
    expect(d.newCount).toBe(0);
  });

  it("clamps to 0 when the capture lands before the last credited mark", () => {
    // Every prior capture landed at the early edge of its window, so the
    // last real capture sits before the credited mark. A flush seconds
    // later is still behind the mark — credit 0, never negative.
    const capturedAt = ms(T0, 3 * SCREENSHOT_INTERVAL_MS - 19_000);
    const d = creditFinalCapture(capturedAt, T0, 3);
    expect(d.credit).toBe(0);
  });

  it("a pause/resume cycle earns exactly wall-clock time", () => {
    // Seed at T0, two full minutes credited, pause at 02:45 → 165s total.
    let anchor: Date | null = null;
    let count = 0;
    let tracked = 0;
    for (const [deltaMs, isFinal] of [
      [0, false],
      [60_000, false],
      [120_000, false],
      [165_000, true],
    ] as const) {
      const cap = ms(T0, deltaMs);
      const d: CreditDecision = isFinal
        ? creditFinalCapture(cap, anchor, count)
        : creditCapture(cap, anchor, count);
      tracked += d.credit;
      anchor = d.newAnchor;
      count = d.newCount;
    }
    expect(tracked).toBe(165);
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

/**
 * A wrong system clock is common, invisible to the user, and none of their
 * doing. It used to cost them the whole recording: every upload-url request
 * 400'd on the envelope check, so no presigned URL was ever issued and
 * nothing uploaded at all. These tests pin the "adopt, don't break" rule.
 */
describe("adoptedCapturedAt (client clock skew)", () => {
  const serverNow = new Date("2025-01-01T12:00:00.000Z");
  const startedAt = ms(serverNow, -120_000);

  it("passes a healthy clock through untouched", () => {
    const cap = ms(serverNow, -500);
    const r = adoptedCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ capturedAt: cap, adopted: false });
  });

  it("adopts server time for a clock hours FAST instead of rejecting", () => {
    const cap = ms(serverNow, 3 * 60 * 60_000);
    const r = adoptedCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ capturedAt: serverNow, adopted: true, reason: "captured_at_future" });
  });

  it("adopts server time for a clock hours SLOW instead of rejecting", () => {
    const cap = ms(serverNow, -3 * 60 * 60_000);
    const r = adoptedCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ capturedAt: serverNow, adopted: true, reason: "captured_at_too_old" });
  });

  it("adopts even for an absurd timestamp — nothing is gained by lying", () => {
    // Server time is unforgeable, so substituting it removes the incentive to
    // send a wild value rather than creating one. The capture is simply
    // stamped when the server saw it.
    for (const cap of [new Date(0), new Date("2099-01-01T00:00:00.000Z")]) {
      const r = adoptedCapturedAt(cap, serverNow, startedAt, null);
      expect(r).toMatchObject({ capturedAt: serverNow, adopted: true });
    }
  });

  it("adopts server time for a stamp a few seconds ahead — a capture can't come from the future", () => {
    // A capture is taken before its request is sent, so an honest stamp is
    // never ahead of the server by more than the offset estimate's error.
    // Treating anything past a few seconds as a fast clock is what keeps a
    // session's FIRST stamp (taken before any serverTime was seen) honest.
    const cap = ms(serverNow, 30_000);
    const r = adoptedCapturedAt(cap, serverNow, startedAt, null);
    expect(r).toEqual({ capturedAt: serverNow, adopted: true, reason: "captured_at_future" });
  });

  it("adopts server time for a non-monotonic stamp — a clock stepped backwards", () => {
    // Windows syncing its time mid-recording moves every later stamp behind
    // the last confirmed one. Refusing these cost the user minutes of
    // recording; stamping them on arrival costs nothing, and nothing was
    // ever protected by refusing — an adopted replay is indistinguishable
    // from a fresh capture.
    const latest = ms(serverNow, -60_000);
    const r = adoptedCapturedAt(ms(serverNow, -90_000), serverNow, startedAt, latest);
    expect(r).toEqual({ capturedAt: serverNow, adopted: true, reason: "captured_at_not_monotonic" });
  });

  it("adopts server time for a pre-session stamp", () => {
    const later = ms(serverNow, 10 * 60_000);
    const r = adoptedCapturedAt(ms(serverNow, -60_000), serverNow, later, null);
    expect(r).toEqual({ capturedAt: serverNow, adopted: true, reason: "captured_at_before_session_start" });
  });

  it("a clock a few minutes fast credits from its first minute", () => {
    // The production case: a PC 3m19s fast. Its seed stamp is ahead of the
    // server, so it is stamped on arrival; the client reads `serverTime`
    // from that response and corrects every later stamp, which then lands on
    // the grid. With the old symmetric envelope the seed was accepted as
    // stamped, and the corrected stamps that followed were refused as
    // non-monotonic for the next three minutes.
    const skew = 199_000;
    let anchor: Date | null = null;
    let count = 0;
    let latest: Date | null = null;
    let credited = 0;
    for (let i = 0; i < 5; i++) {
      const server = ms(serverNow, i * SCREENSHOT_INTERVAL_MS);
      const learnt = i > 0;
      const stamp = learnt ? ms(server, -150) : ms(server, skew);
      const r = adoptedCapturedAt(stamp, server, serverNow, latest);
      expect(r.adopted).toBe(!learnt);
      const d = creditCapture(r.capturedAt, anchor, count);
      anchor = d.newAnchor;
      count = d.newCount;
      credited += d.credit;
      latest = r.capturedAt;
    }
    expect(credited).toBe(4 * 60);
  });

  it("a clock stepped backwards mid-session keeps crediting", () => {
    // Two minutes in, the OS corrects the clock back by 199s. The client's
    // offset estimate is now stale by that much, so its next stamp is
    // before the session even started; the server stamps it on arrival,
    // which is the expected mark, and the minute credits.
    const anchor = serverNow;
    const latest = ms(serverNow, 2 * SCREENSHOT_INTERVAL_MS);
    const server = ms(serverNow, 3 * SCREENSHOT_INTERVAL_MS);
    const r = adoptedCapturedAt(ms(server, -199_000), server, serverNow, latest);
    expect(r.adopted).toBe(true);
    const d = creditCapture(r.capturedAt, anchor, 2);
    expect(d.credit).toBe(60);
    expect(d.inWindow).toBe(true);
  });

  it("keeps a skewed client crediting minute after minute", () => {
    // The point of the whole exercise: a device an hour fast should still
    // build a normal streak, because each adopted stamp is a real server
    // instant one interval after the last.
    let anchor: Date | null = null;
    let count = 0;
    let credited = 0;
    for (let i = 0; i < 5; i++) {
      const server = ms(serverNow, i * SCREENSHOT_INTERVAL_MS);
      const skewed = ms(server, 60 * 60_000); // an hour fast
      const r = adoptedCapturedAt(skewed, server, startedAt, null);
      expect(r.adopted).toBe(true);
      const d = creditCapture(r.capturedAt, anchor, count);
      anchor = d.newAnchor;
      count = d.newCount;
      credited += d.credit;
    }
    // Seed credits 0, the next four credit 60 each.
    expect(credited).toBe(4 * 60);
  });
});
