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

/** Distinct error codes for capturedAt validation. Each maps 1:1 to a
 *  rejection condition so logs and dashboards can be cleanly grepped. */
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
 * True for the two failures that mean "this client's clock is wrong" rather
 * than "this client is misbehaving".
 *
 * The distinction matters because the two deserve opposite treatment. A
 * non-monotonic or pre-session timestamp says something is wrong with the
 * request; a timestamp five minutes off says nothing except that the user's
 * system clock is off, which is common, invisible to them, and none of their
 * doing. Rejecting the latter used to fail the upload-url request outright,
 * so a skewed clock cost the user their entire recording — every upload 400'd
 * before a presigned URL was ever issued. The caller substitutes server time
 * for these instead. See adoptedCapturedAt.
 */
export function isClockSkewError(code: CapturedAtValidationError): boolean {
  return code === "captured_at_future" || code === "captured_at_too_old";
}

/**
 * Resolve the `captured_at` to actually use, adopting server time when the
 * client's clock is too far off to trust.
 *
 * Server time is authoritative and unforgeable, so substituting it is
 * strictly SAFER than accepting the client's claim — a hostile client gains
 * nothing by sending a wild timestamp, it just gets its capture stamped with
 * the moment the server saw it. What it costs is precision: a capture stamped
 * on arrival includes upload latency, so a skewed client's credit is measured
 * a little late rather than not at all. That is the right trade against
 * losing the recording.
 *
 * Returns the timestamp to store plus whether a substitution happened, so the
 * route can tell the client (which can then correct its own offset from the
 * `serverTime` in the response) and operators can see it in telemetry.
 */
export function adoptedCapturedAt(
  clientCapturedAt: Date,
  serverNow: Date,
  sessionStartedAt: Date,
  latestCapturedAt: Date | null,
):
  | { ok: true; capturedAt: Date; adopted: boolean }
  | { ok: false; code: CapturedAtValidationError } {
  const first = validateCapturedAt(
    clientCapturedAt,
    serverNow,
    sessionStartedAt,
    latestCapturedAt,
  );
  if (first.ok) {
    return { ok: true, capturedAt: clientCapturedAt, adopted: false };
  }
  if (!isClockSkewError(first.code)) {
    return { ok: false, code: first.code };
  }

  // Clock skew: stamp with server time instead. Re-validate, because the
  // substituted value still has to satisfy monotonicity and the session
  // start — if it doesn't, something other than the clock is wrong and the
  // caller should still refuse.
  const second = validateCapturedAt(
    serverNow,
    serverNow,
    sessionStartedAt,
    latestCapturedAt,
  );
  if (!second.ok) {
    return { ok: false, code: second.code };
  }
  return { ok: true, capturedAt: serverNow, adopted: true };
}

/**
 * Validate a client-attested `capturedAt` against the trust envelope and the
 * session's existing state. Returns a tagged result so the caller can map to
 * a specific 4xx error code.
 *
 * - Envelope: `serverNow - 5min ≤ capturedAt ≤ serverNow + 5min`
 * - Must be ≥ session.startedAt
 * - Must be strictly > the latest CONFIRMED capturedAt (monotonic). The
 *   caller must pass the latest confirmed capture only — an unconfirmed
 *   (presign-time) row is an upload that never landed, and using it as the
 *   floor blocks the clip→JPEG fallback retry that deliberately reuses the
 *   failed upload's stamp. Equality is only allowed when the caller is in an
 *   idempotent retry path (same screenshotId); that check lives at the route
 *   handler since it requires the row lookup.
 *
 * Envelope failures are usually a wrong clock rather than a bad actor — use
 * `adoptedCapturedAt` to absorb them rather than calling this directly.
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
