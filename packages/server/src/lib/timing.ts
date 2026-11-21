import {
  RATE_LIMIT_PER_MINUTE,
  CAPTURED_AT_PAST_TOLERANCE_MS,
  CAPTURED_AT_FUTURE_TOLERANCE_MS,
  STREAK_WINDOW_MS,
  CREDIT_PER_CAPTURE_S,
  SCREENSHOT_INTERVAL_MS,
} from "@lookout/shared";
import { nowMs } from "./clock.js";

/**
 * Compute the minute bucket for a screenshot based on server timestamp.
 * Used by bucket-mode (legacy) sessions only.
 */
export function computeMinuteBucket(
  requestedAt: Date,
  sessionStartedAt: Date,
): number {
  const diffMs = requestedAt.getTime() - sessionStartedAt.getTime();
  return Math.floor(diffMs / 60_000);
}

// ──────────────────────────────────────────────────────────────────
// Credit-mode helpers
// ──────────────────────────────────────────────────────────────────

/** Distinct reasons a capturedAt is not trusted. Each maps 1:1 to one
 *  check so logs and dashboards can be cleanly grepped. */
export type CapturedAtValidationError =
  | "captured_at_future"
  | "captured_at_too_old"
  | "captured_at_before_session_start"
  | "captured_at_not_monotonic";

export interface CapturedAtValidationOk {
  ok: true;
}
export interface CapturedAtValidationFail {
  ok: false;
  code: CapturedAtValidationError;
}
export type CapturedAtValidation =
  | CapturedAtValidationOk
  | CapturedAtValidationFail;

/**
 * Resolve the `captured_at` to actually use: the client's claim when it
 * checks out, server time otherwise.
 *
 * Server time is authoritative and unforgeable, so substituting it is
 * strictly SAFER than accepting the client's claim — a hostile client gains
 * nothing by sending a wild timestamp, it just gets its capture stamped with
 * the moment the server saw it. What it costs is precision: a capture stamped
 * on arrival includes upload latency, so a skewed client's credit is measured
 * a little late rather than not at all. That is the right trade against
 * losing the recording.
 *
 * Every failed check is handled this way, not only the envelope ones. The
 * monotonic and pre-session checks used to refuse with a 400 on the theory
 * that they catch a misbehaving request rather than a wrong clock, but in
 * practice they fired on clocks: a clock 30s–5min fast planted its first
 * stamp in the future, and every corrected stamp after it read as
 * non-monotonic; a clock stepped backwards mid-session (Windows syncing its
 * time) did the same. The desktop app drops a refused capture and tries
 * again next minute, so the recording lost its first minutes while the user
 * watched "0m" and gave up. Refusing never protected anything either: a
 * replayed stamp adopted as server time is indistinguishable from a fresh
 * capture, which the rate limit and the streak maths already govern.
 *
 * Returns the timestamp to store plus whether a substitution happened and
 * why, so the route can tell the client (which then corrects its own offset
 * from the `serverTime` in the response) and operators can see it in
 * telemetry.
 */
export function adoptedCapturedAt(
  clientCapturedAt: Date,
  serverNow: Date,
  sessionStartedAt: Date,
  latestCapturedAt: Date | null,
):
  | { capturedAt: Date; adopted: false }
  | { capturedAt: Date; adopted: true; reason: CapturedAtValidationError } {
  const check = validateCapturedAt(
    clientCapturedAt,
    serverNow,
    sessionStartedAt,
    latestCapturedAt,
  );
  if (check.ok) {
    return { capturedAt: clientCapturedAt, adopted: false };
  }
  return { capturedAt: serverNow, adopted: true, reason: check.code };
}

/**
 * Validate a client-attested `capturedAt` against the trust envelope and the
 * session's existing state. Returns a tagged result naming which check
 * failed, for the logs.
 *
 * - Envelope: `serverNow - 5min ≤ capturedAt ≤ serverNow + 5s`
 * - Must be ≥ session.startedAt
 * - Must be strictly > the latest CONFIRMED capturedAt (monotonic). The
 *   caller must pass the latest confirmed capture only — an unconfirmed
 *   (presign-time) row is an upload that never landed, and using it as the
 *   floor blocks the clip→JPEG fallback retry that deliberately reuses the
 *   failed upload's stamp.
 *
 * A failure is a wrong clock far more often than a bad actor — use
 * `adoptedCapturedAt`, which substitutes server time, rather than refusing
 * on this directly.
 */
export function validateCapturedAt(
  capturedAt: Date,
  serverNow: Date,
  sessionStartedAt: Date,
  latestCapturedAt: Date | null,
): CapturedAtValidation {
  const capMs = capturedAt.getTime();
  const nowMs = serverNow.getTime();

  if (capMs > nowMs + CAPTURED_AT_FUTURE_TOLERANCE_MS) {
    return { ok: false, code: "captured_at_future" };
  }
  if (capMs < nowMs - CAPTURED_AT_PAST_TOLERANCE_MS) {
    return { ok: false, code: "captured_at_too_old" };
  }
  // Allow up to 2s of slop before sessionStartedAt to absorb the race
  // between two parallel first-uploads: the winner sets started_at to
  // its own clientCapturedAt; the loser's clientCapturedAt may be a
  // hair earlier. 2s is much shorter than any human-perceptible window.
  if (capMs < sessionStartedAt.getTime() - 2_000) {
    return { ok: false, code: "captured_at_before_session_start" };
  }
  if (latestCapturedAt && capMs <= latestCapturedAt.getTime()) {
    return { ok: false, code: "captured_at_not_monotonic" };
  }
  return { ok: true };
}

/** Decision computed at confirm time. Drives the screenshot+session writes. */
export interface CreditDecision {
  /** New value to store in `screenshots.credited_seconds` (0 or 60). */
  credit: number;
  /** New value for `sessions.streak_anchor_at`. */
  newAnchor: Date;
  /** New value for `sessions.streak_credited_count`. */
  newCount: number;
  /** Server-predicted expected mark for THIS capture; stored in
   *  `screenshots.expected_at`. NULL when this capture is the streak seed
   *  (no anchor existed). */
  expectedAt: Date | null;
  /** Whether this capture continued the streak (true), reset it (false),
   *  or seeded it (false). Useful for telemetry. */
  inWindow: boolean;
  /** Wall-clock target the client should aim for on the *next* capture. */
  nextExpectedAt: Date;
}

/**
 * Pure function. Decide how to credit a capture given current streak state.
 *
 * Decision tree:
 *   - No anchor yet (seed): credit 0, anchor = capturedAt, count = 0,
 *     expectedAt = null. nextExpectedAt = capturedAt + 60s.
 *   - Anchor exists and |capturedAt - expected| ≤ 30s (credit): credit 60,
 *     anchor unchanged, count++, expectedAt = the computed expected.
 *     nextExpectedAt = anchor + (newCount+1)*60s.
 *   - Anchor exists but out of window (reset): credit 0, anchor =
 *     capturedAt, count = 0, expectedAt = the computed expected (record
 *     what we were aiming for, for telemetry).
 *     nextExpectedAt = capturedAt + 60s.
 */
export function creditCapture(
  capturedAt: Date,
  streakAnchorAt: Date | null,
  streakCreditedCount: number,
): CreditDecision {
  const intervalMs = SCREENSHOT_INTERVAL_MS;
  const creditS = CREDIT_PER_CAPTURE_S;
  const windowMs = STREAK_WINDOW_MS;

  // Seed: no anchor yet
  if (streakAnchorAt === null) {
    return {
      credit: 0,
      newAnchor: capturedAt,
      newCount: 0,
      expectedAt: null,
      inWindow: false,
      nextExpectedAt: new Date(capturedAt.getTime() + intervalMs),
    };
  }

  const expectedAt = new Date(
    streakAnchorAt.getTime() + (streakCreditedCount + 1) * intervalMs,
  );
  const delta = Math.abs(capturedAt.getTime() - expectedAt.getTime());

  if (delta <= windowMs) {
    // Credit: continue the streak
    const newCount = streakCreditedCount + 1;
    return {
      credit: creditS,
      newAnchor: streakAnchorAt,
      newCount,
      expectedAt,
      inWindow: true,
      nextExpectedAt: new Date(
        streakAnchorAt.getTime() + (newCount + 1) * intervalMs,
      ),
    };
  }

  // Reset: capturedAt fell outside the window. Record what we expected so
  // operators can see how far off the client drifted before the reset.
  return {
    credit: 0,
    newAnchor: capturedAt,
    newCount: 0,
    expectedAt,
    inWindow: false,
    nextExpectedAt: new Date(capturedAt.getTime() + intervalMs),
  };
}

/**
 * Credit a FINAL capture — the one a client flushes at pause/stop time so a
 * partial minute isn't thrown away (pause at 03:15 used to resume at 03:00).
 *
 * Clients flag it with `final: true` on the confirm body; old clients never
 * send the flag and go through `creditCapture` unchanged. The rules:
 *
 *   - No anchor (seed): identical to `creditCapture` — there is no credited
 *     mark to measure a partial minute from, so it seeds with 0.
 *   - Otherwise: credit the wall-clock elapsed since the LAST credited mark
 *     (anchor + count·interval), clamped to [0, one interval].
 *
 * Deliberately NO in-window full-interval generosity here, even when the
 * final capture lands inside the normal ±window. In the normal path that
 * generosity is safe because the anchor never moves on a credit — landing
 * 29s early doesn't compound, the grid stays put. A final capture RESETS
 * the anchor (and resume re-seeds it), so passing the in-window rule
 * through would let a pause at expected−29s bank a full interval and then
 * restart the grid, inflating ~30% per pause/resume cycle. Exact elapsed,
 * clamped, is exploit-free: `capturedAt` is validated against the trust
 * envelope and monotonicity like any other capture, the credit can never
 * exceed one interval (the CREDIT_PER_CAPTURE_S ceiling), and pause/resume
 * cycling earns exactly wall-clock time.
 */
export function creditFinalCapture(
  capturedAt: Date,
  streakAnchorAt: Date | null,
  streakCreditedCount: number,
): CreditDecision {
  const normal = creditCapture(capturedAt, streakAnchorAt, streakCreditedCount);
  // Seed: the normal rules already do the right thing.
  if (streakAnchorAt === null) return normal;

  const lastCreditedMarkMs =
    streakAnchorAt.getTime() + streakCreditedCount * SCREENSHOT_INTERVAL_MS;
  const elapsedS = Math.floor(
    (capturedAt.getTime() - lastCreditedMarkMs) / 1000,
  );
  return {
    ...normal,
    // Reset shape regardless of window: the session is pausing, and
    // resume re-seeds the anchor anyway.
    newAnchor: capturedAt,
    newCount: 0,
    inWindow: false,
    credit: Math.min(Math.max(0, elapsedS), CREDIT_PER_CAPTURE_S),
    nextExpectedAt: new Date(capturedAt.getTime() + SCREENSHOT_INTERVAL_MS),
  };
}

/**
 * Simple in-memory rate limiter per session.
 * Tracks upload-url requests per 60-second sliding window.
 */
const windows = new Map<
  string,
  { count: number; windowStart: number }
>();

export function checkRateLimit(sessionId: string): {
  allowed: boolean;
  retryAfterMs?: number;
} {
  const now = nowMs();
  const windowMs = 60_000;
  const entry = windows.get(sessionId);

  if (!entry || now - entry.windowStart >= windowMs) {
    windows.set(sessionId, { count: 1, windowStart: now });
    return { allowed: true };
  }

  if (entry.count >= RATE_LIMIT_PER_MINUTE) {
    const retryAfterMs = windowMs - (now - entry.windowStart);
    return { allowed: false, retryAfterMs };
  }

  entry.count++;
  return { allowed: true };
}

/**
 * Generic in-memory rate limiter.
 * Tracks requests per 60-second sliding window under a namespaced key.
 */
const genericWindows = new Map<
  string,
  { count: number; windowStart: number }
>();

export function checkGenericRateLimit(
  namespace: string,
  key: string,
  maxPerMinute: number,
): { allowed: boolean; retryAfterMs?: number } {
  const compositeKey = `${namespace}:${key}`;
  const now = nowMs();
  const windowMs = 60_000;
  const entry = genericWindows.get(compositeKey);

  if (!entry || now - entry.windowStart >= windowMs) {
    genericWindows.set(compositeKey, { count: 1, windowStart: now });
    return { allowed: true };
  }

  if (entry.count >= maxPerMinute) {
    const retryAfterMs = windowMs - (now - entry.windowStart);
    return { allowed: false, retryAfterMs };
  }

  entry.count++;
  return { allowed: true };
}

/**
 * Clean up stale rate limit entries (call periodically).
 */
export function cleanupRateLimits() {
  const now = nowMs();
  for (const [key, entry] of windows) {
    if (now - entry.windowStart > 120_000) {
      windows.delete(key);
    }
  }
  for (const [key, entry] of genericWindows) {
    if (now - entry.windowStart > 120_000) {
      genericWindows.delete(key);
    }
  }
}
