import { useState, useEffect, useRef } from "react";
import { SCREENSHOT_INTERVAL_MS } from "@lookout/shared";

/** Max seconds the display may drift ahead of `serverTrackedSeconds`
 *  between credits. One capture interval — if the next capture credits,
 *  the display jumps to the new server value (== frozen value) and
 *  unfreezes smoothly. If captures stall, the freeze stays put so the
 *  user sees something is wrong instead of an inflated count. */
const MAX_INTERPOLATION_S = Math.floor(SCREENSHOT_INTERVAL_MS / 1000);

/**
 * Display timer for the recording session.
 *
 * `serverTrackedSeconds` is the ground truth. We interpolate at
 * wall-clock rate between credits for liveness, but the interpolation
 * is **capped at one capture interval**. Display never overshoots the
 * next credit by more than that, so stop/compile reveals at most one
 * minute of drop — no "halving" surprise.
 *
 * `baseRef` ratchets forward (never backward) so a stale-read
 * idempotent retry returning a lower `trackedSeconds` doesn't cause
 * the display to jump back. With the cap, ratcheting can only get the
 * display 60s ahead of the true value, bounded.
 *
 * Unfreeze contract: when `serverTrackedSeconds` advances, `baseRef`
 * ratchets up and `lastSyncRef` resets — display jumps to the new
 * value and the next interpolation cycle starts from there.
 */
export function useSessionTimer(
  serverTrackedSeconds: number,
  isActive: boolean,
): number {
  const [displaySeconds, setDisplaySeconds] = useState(serverTrackedSeconds);
  const lastSyncRef = useRef(Date.now());
  const baseRef = useRef(serverTrackedSeconds);

  // Ratchet baseRef forward on every server update. Resets the
  // interpolation anchor — this is what unfreezes the timer.
  useEffect(() => {
    const newBase = Math.max(baseRef.current, serverTrackedSeconds);
    if (newBase !== baseRef.current) {
      baseRef.current = newBase;
      setDisplaySeconds(newBase);
      lastSyncRef.current = Date.now();
    }
  }, [serverTrackedSeconds]);

  useEffect(() => {
    // When the session leaves active (pause/stop), snap display to the
    // ratcheted base. No further interpolation. Worst-case drop the user
    // sees is bounded by MAX_INTERPOLATION_S (cap above).
    if (!isActive) {
      setDisplaySeconds(baseRef.current);
      return;
    }

    lastSyncRef.current = Date.now();

    let raf: number;
    let lastRenderedSecond = -1;
    const tick = () => {
      const elapsed = Math.min(
        MAX_INTERPOLATION_S,
        Math.floor((Date.now() - lastSyncRef.current) / 1000),
      );
      if (elapsed !== lastRenderedSecond) {
        lastRenderedSecond = elapsed;
        setDisplaySeconds(baseRef.current + elapsed);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
      // Don't bake elapsed into baseRef here. If we did, baseRef would
      // grow past the server's true value during pauses or stalls, and
      // the next server update couldn't ratchet forward (max() would
      // keep the inflated baseRef). Server credits after resume re-anchor
      // baseRef via the sync effect above.
    };
  }, [isActive, serverTrackedSeconds]);

  return displaySeconds;
}

/** Format seconds as H:MM:SS or M:SS (for live timer display). */
export function formatTime(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  if (h > 0) {
    return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  }
  return `${m}:${String(s).padStart(2, "0")}`;
}

/** Format seconds as human-readable tracked time (e.g. "1h 34min", "12min", "< 1min"). */
export function formatTrackedTime(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  if (h > 0 && m > 0) return `${h}h ${m}min`;
  if (h > 0) return `${h}h`;
  if (m > 0) return `${m}min`;
  return "< 1min";
}
