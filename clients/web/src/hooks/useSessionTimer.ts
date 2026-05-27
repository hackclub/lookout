import { useState, useEffect, useRef } from "react";
import { SCREENSHOT_INTERVAL_MS } from "@lookout/shared";

/** Max seconds the display may drift ahead of `serverTrackedSeconds`
 *  between credits. One capture interval: if the next capture credits,
 *  the freeze unfreezes smoothly (display == new server value). If
 *  captures stall, the timer freezes here so the user sees a hint that
 *  something is wrong instead of an over-optimistic count. */
const MAX_INTERPOLATION_S = Math.floor(SCREENSHOT_INTERVAL_MS / 1000);

/**
 * Display timer for the recording session.
 *
 * `serverTrackedSeconds` is the ground truth. Between server updates we
 * interpolate at wall-clock rate for liveness, but the interpolation is
 * **capped at one capture interval** so the display can never overshoot
 * the next credit. That makes stop/compile a no-surprise transition:
 * worst-case drop is one interval, not the full session length.
 *
 * Unfreeze contract: when `serverTrackedSeconds` advances (a credit
 * lands), the sync effect resets `lastSyncRef` and the displayed value
 * jumps to the new server value — which equals the frozen value when
 * captures were on schedule, so the user sees no visible jump.
 */
export function useSessionTimer(
  serverTrackedSeconds: number,
  isActive: boolean,
) {
  const [displaySeconds, setDisplaySeconds] = useState(serverTrackedSeconds);
  const lastSyncRef = useRef(Date.now());

  // Sync to server whenever it changes. Resets the interpolation anchor
  // so the cap re-starts from the new credit — this is what unfreezes
  // the timer after a stall.
  useEffect(() => {
    setDisplaySeconds(serverTrackedSeconds);
    lastSyncRef.current = Date.now();
  }, [serverTrackedSeconds]);

  // When the session leaves the active state (pause/stop), snap to the
  // server value. No more interpolation; what the user sees is what the
  // server will report. Returns from the effect so no interval starts.
  useEffect(() => {
    if (!isActive) {
      setDisplaySeconds(serverTrackedSeconds);
      return;
    }

    // Reset the anchor when resuming so pause duration doesn't get
    // counted as interpolated time after the first tick.
    lastSyncRef.current = Date.now();

    const interval = setInterval(() => {
      const elapsed = Math.min(
        MAX_INTERPOLATION_S,
        Math.floor((Date.now() - lastSyncRef.current) / 1000),
      );
      setDisplaySeconds(serverTrackedSeconds + elapsed);
    }, 1000);

    return () => clearInterval(interval);
  }, [isActive, serverTrackedSeconds]);

  return displaySeconds;
}

/**
 * Format seconds as HH:MM:SS
 */
export function formatTime(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;

  if (h > 0) {
    return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  }
  return `${m}:${String(s).padStart(2, "0")}`;
}
