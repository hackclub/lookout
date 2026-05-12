/**
 * Centralized clock for the server.
 *
 * All time-sensitive code paths (capture timestamp validation, streak math,
 * auto-pause/stop thresholds) must read `now()` from here rather than calling
 * `Date.now()` or `new Date()` directly. This single seam lets the test
 * harness advance simulated time arbitrarily without forking production code.
 *
 * See: future-pitfalls #1 in the plan.
 */
let clockImpl: () => Date = () => new Date();

/** Current wall-clock time. Returns a fresh Date each call. */
export function now(): Date {
  return clockImpl();
}

/** Current wall-clock time as ms since epoch. */
export function nowMs(): number {
  return clockImpl().getTime();
}

/** Override the clock. Test-only — production code never calls this. */
export function setClock(fn: () => Date): void {
  clockImpl = fn;
}

/** Restore the production clock. */
export function resetClock(): void {
  clockImpl = () => new Date();
}
