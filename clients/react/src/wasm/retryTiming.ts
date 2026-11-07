import { MAX_UPLOAD_RETRIES, UPLOAD_RETRY_DELAYS_MS } from "@lookout/shared";

/**
 * The SDK's `retry` settings as a patch for the engine's `Timing`, or null
 * when they are the defaults (the engine's own defaults are the same
 * numbers, so there is nothing to change).
 *
 * Same meaning as the SDK's original uploader: `maxRetries` is attempts per
 * upload step, the first included; the wait before retry `i` is
 * `retryDelays[i]`, falling back to the last entry. The engine holds three
 * delays and repeats the third, so a list longer than three only differs
 * from the old behaviour from the fourth retry on, and only if those later
 * entries differ from the third.
 */
export function retryTimingPatch(retry: {
  maxRetries: number;
  retryDelays: number[];
}): { maxAttempts: number; retryDelaysMs: [number, number, number] } | null {
  const sameDelays =
    retry.retryDelays.length === UPLOAD_RETRY_DELAYS_MS.length &&
    retry.retryDelays.every((d, i) => d === UPLOAD_RETRY_DELAYS_MS[i]);
  if (retry.maxRetries === MAX_UPLOAD_RETRIES && sameDelays) return null;
  const at = (i: number): number => {
    const d = retry.retryDelays[i] ?? retry.retryDelays[retry.retryDelays.length - 1] ?? 0;
    return Number.isFinite(d) && d > 0 ? Math.round(d) : 0;
  };
  return {
    maxAttempts: Math.max(1, Math.floor(retry.maxRetries) || 1),
    retryDelaysMs: [at(0), at(1), at(2)],
  };
}
