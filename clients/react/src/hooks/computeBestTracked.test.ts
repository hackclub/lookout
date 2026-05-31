/**
 * Regression tests for computeBestTrackedSeconds — the function that
 * decides what value gets piped into useSessionTimer.
 *
 * The 0.2.x family had a recurring "browser timer shows 2x the
 * recording time" report. Root cause: useLookout used to feed
 * Math.max(server, uploader, (uploads.completed - 1) * intervalSeconds)
 * into the timer. In credit mode, not every successful upload credits a
 * minute — when the chain runs at ~90s round-trip latency, every other
 * capture lands outside the streak window and resets, but
 * `uploads.completed` still advances. The localEstimate term grew at
 * exactly 2× the server's true tracked value.
 *
 * Desktop users were unaffected because DesktopRecorder.tsx never
 * called useLookout — it composes useSessionTimer directly with
 * `capture.trackedSeconds || session.trackedSeconds` and no third
 * derived term.
 *
 * These tests pin the invariant: the value entering the timer is
 * bounded by the larger of the two SERVER-derived inputs, and
 * `uploads.completed` (or any function of it) does not appear in the
 * signature at all.
 */
import { describe, expect, it } from "vitest";
import { computeBestTrackedSeconds } from "./computeBestTracked.js";

const INTERVAL_S = 60;
const STREAK_WINDOW_MS = 30_000;
const INTERVAL_MS = 60_000;

describe("computeBestTrackedSeconds — direct contract", () => {
  it("returns the larger of session and uploader values", () => {
    expect(
      computeBestTrackedSeconds({
        sessionTrackedSeconds: 120,
        uploaderTrackedSeconds: 60,
      }),
    ).toBe(120);
    expect(
      computeBestTrackedSeconds({
        sessionTrackedSeconds: 60,
        uploaderTrackedSeconds: 180,
      }),
    ).toBe(180);
  });

  it("returns 0 when both inputs are 0", () => {
    expect(
      computeBestTrackedSeconds({
        sessionTrackedSeconds: 0,
        uploaderTrackedSeconds: 0,
      }),
    ).toBe(0);
  });

  it("never returns a value greater than max(session, uploader) — the regression invariant", () => {
    // Property check across a wide grid of inputs. If anyone re-adds a
    // third source to bestTracked, this fails.
    for (let session = 0; session <= 600; session += 60) {
      for (let uploader = 0; uploader <= 600; uploader += 60) {
        const result = computeBestTrackedSeconds({
          sessionTrackedSeconds: session,
          uploaderTrackedSeconds: uploader,
        });
        expect(result).toBeLessThanOrEqual(Math.max(session, uploader));
      }
    }
  });
});

// ──────────────────────────────────────────────────────────────────
// Simulation of the actual bug surface.
//
// We replay the server's credit-math against a stream of capturedAt
// timestamps at various upload latencies, then ask: "what would the
// timer display, given the function under test?"
//
// At 90s upload latency on a 60s interval, every other capture resets
// the streak. With localEstimate in play, display = 2 * server.tracked.
// Without it (the current code), display = server.tracked.
// ──────────────────────────────────────────────────────────────────

interface ServerState {
  anchor: number | null;
  count: number;
  tracked: number;
}

/** Pure server credit math (kept inline so the test self-documents
 *  what we're comparing against). */
function creditCapture(
  capturedAt: number,
  state: ServerState,
): { credit: number; nextExpectedAt: number } {
  if (state.anchor === null) {
    state.anchor = capturedAt;
    state.count = 0;
    return { credit: 0, nextExpectedAt: capturedAt + INTERVAL_MS };
  }
  const expected = state.anchor + (state.count + 1) * INTERVAL_MS;
  const delta = Math.abs(capturedAt - expected);
  if (delta <= STREAK_WINDOW_MS) {
    state.count += 1;
    state.tracked += INTERVAL_S;
    return {
      credit: INTERVAL_S,
      nextExpectedAt: state.anchor + (state.count + 1) * INTERVAL_MS,
    };
  }
  state.anchor = capturedAt;
  state.count = 0;
  return { credit: 0, nextExpectedAt: capturedAt + INTERVAL_MS };
}

/** Simulate the v0.2.5 serial chain through `wallClockMs` of recording
 *  with a fixed upload+confirm latency. Returns the values that would
 *  be fed into computeBestTrackedSeconds at the end. */
function simulateChain(uploadLatencyMs: number, wallClockMs: number) {
  const server: ServerState = { anchor: null, count: 0, tracked: 0 };
  let uploadsCompleted = 0;
  let now = 0;
  let lastUploaderTracked = 0;

  while (now < wallClockMs) {
    const capturedAt = now;
    const decision = creditCapture(capturedAt, server);
    uploadsCompleted += 1;
    lastUploaderTracked = server.tracked; // confirm response carries server.tracked
    now += uploadLatencyMs;
    if (now >= wallClockMs) break;
    const delay = Math.min(
      INTERVAL_MS * 2,
      Math.max(0, decision.nextExpectedAt - now),
    );
    now += delay;
  }

  return {
    serverTracked: server.tracked,
    uploaderTracked: lastUploaderTracked,
    uploadsCompleted,
  };
}

describe("computeBestTrackedSeconds — simulated bug surfaces", () => {
  it("normal 1s upload over 10 min: display equals server (1.00x)", () => {
    const s = simulateChain(1_000, 10 * 60_000);
    const display = computeBestTrackedSeconds({
      sessionTrackedSeconds: s.serverTracked,
      uploaderTrackedSeconds: s.uploaderTracked,
    });
    expect(display).toBe(s.serverTracked);
  });

  it("90s upload over 10 min reproduces the exact 2x bug condition", () => {
    // This is the smoking-gun scenario. uploads.completed grows at 1 per
    // tick while server credits only half — the streak alternates
    // credit/reset. If anyone re-adds localEstimate, display jumps to
    // 2 * server here. With the fix, display == server.
    const s = simulateChain(90_000, 10 * 60_000);
    expect(s.uploadsCompleted).toBeGreaterThanOrEqual(2 * (s.serverTracked / INTERVAL_S));

    const display = computeBestTrackedSeconds({
      sessionTrackedSeconds: s.serverTracked,
      uploaderTrackedSeconds: s.uploaderTracked,
    });
    expect(display).toBe(s.serverTracked);

    // And explicitly: the broken `localEstimate` would have produced
    // a doubled value. We verify the math here so the test self-documents
    // what we're guarding against, without exposing localEstimate from
    // the production code.
    const wouldBeBrokenLocalEstimate =
      s.uploadsCompleted >= 2 ? (s.uploadsCompleted - 1) * INTERVAL_S : 0;
    expect(wouldBeBrokenLocalEstimate).toBeGreaterThanOrEqual(2 * s.serverTracked);
  });

  it("very slow 120s upload: display still bounded by server (no inflation)", () => {
    const s = simulateChain(120_000, 10 * 60_000);
    const display = computeBestTrackedSeconds({
      sessionTrackedSeconds: s.serverTracked,
      uploaderTrackedSeconds: s.uploaderTracked,
    });
    expect(display).toBe(s.serverTracked);
  });
});

describe("computeBestTrackedSeconds — type signature is a fence", () => {
  // This isn't a runtime assertion — it's a tripwire for reviewers.
  // The function deliberately takes a typed `BestTrackedInputs` object
  // so any future PR adding a third numeric input (like an upload-count
  // derivation) must touch the type, the docstring, and these tests.
  // If you find yourself reaching for `uploads.completed` in useLookout
  // to feed into this function: stop and read the docstring on
  // computeBestTracked.ts.
  it("signature only accepts the two server-derived values", () => {
    const fn = computeBestTrackedSeconds;
    // Compile-time check: extra fields are flagged by tsc, but at runtime
    // we just verify the function call works with the documented shape.
    expect(typeof fn({ sessionTrackedSeconds: 60, uploaderTrackedSeconds: 60 })).toBe(
      "number",
    );
  });
});
