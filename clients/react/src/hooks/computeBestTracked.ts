/**
 * Computes the "best" tracked-seconds value to feed into useSessionTimer.
 *
 * The ONLY legitimate inputs are server-derived values:
 *   - `sessionTrackedSeconds`: the server's authoritative count, from
 *     status polling / stop response.
 *   - `uploaderTrackedSeconds`: the value returned by the last confirm
 *     response (server-derived, also authoritative).
 *
 * **Anything else is a lie** — in particular, any function of
 * `uploads.completed` (e.g. `(completed - 1) * intervalSeconds`)
 * over-counts in credit mode because not every successful upload
 * credits a minute. See useLookout.ts for the regression this guards.
 *
 * This file mirrors the model the desktop client uses
 * (DesktopRecorder.tsx: `capture.trackedSeconds || session.trackedSeconds`),
 * which has been working reliably for thousands of users.
 *
 * The function intentionally takes a single typed parameter object so
 * adding a third numeric source is a discoverable, reviewed change —
 * not an inline `Math.max(..., somethingDerived)` slipped past review.
 */
export interface BestTrackedInputs {
  sessionTrackedSeconds: number;
  uploaderTrackedSeconds: number;
}

export function computeBestTrackedSeconds(inputs: BestTrackedInputs): number {
  return Math.max(inputs.sessionTrackedSeconds, inputs.uploaderTrackedSeconds);
}
