import { useCallback, useState } from "react";
import { useLookoutContext } from "../LookoutProvider.js";
import { HttpError } from "../api/client.js";
import type { CaptureResult, UploadState } from "../types.js";

/** Whether to opt into credit-mode tracking by sending `capturedAt` to the
 *  server on every upload. The new desktop / web build enables this on new
 *  sessions; the legacy build never sets it (and the server keeps the
 *  session in bucket-mode). Toggle one place, get both behaviors. */
const ENABLE_CREDIT_MODE = true;

async function retry<T>(
  fn: () => Promise<T>,
  maxRetries: number,
  delays: number[],
): Promise<T> {
  for (let i = 0; i < maxRetries; i++) {
    try {
      return await fn();
    } catch (err) {
      if (err instanceof HttpError && err.status === 409) throw err;
      if (i === maxRetries - 1) throw err;
      await new Promise((r) => setTimeout(r, delays[i] ?? delays[delays.length - 1]));
    }
  }
  throw new Error("Unreachable");
}

export interface UploadConfirmResult {
  trackedSeconds: number;
  nextExpectedAt: string;
}

export interface UploaderResult {
  /** Run the full pipeline serially: upload + confirm. Returns the
   *  fresh `nextExpectedAt` from THIS capture's confirm response.
   *  Throws on failure (after retries) — the caller (the capture-loop
   *  scheduler) catches and falls back to a local interval. */
  captureUploadConfirm: (capture: CaptureResult) => Promise<UploadConfirmResult>;
  /** Current upload state. */
  uploads: UploadState;
  /** Server-reported tracked seconds after latest confirmation. */
  trackedSeconds: number;
  /** Object URL of last successfully uploaded screenshot. */
  lastScreenshotUrl: string | null;
  /** Last upload error message, if any. */
  lastError: string | null;
  /** True when a 409 conflict was received (session paused server-side). */
  sessionConflict: boolean;
  /** Clear the sessionConflict flag after handling. */
  resetConflict: () => void;
}

/**
 * Serial upload pipeline. Matches the desktop Rust loop: each call to
 * `captureUploadConfirm` runs upload + confirm to completion before
 * returning, and returns the FRESH `nextExpectedAt` from that capture's
 * own confirm response.
 *
 * Replaces the pre-0.2.4 queue-and-fire-and-forget model. The previous
 * model was racy: the tick chain read a shared `nextExpectedAt` ref that
 * lagged behind the in-flight upload by one round-trip, so the ref was
 * always stale → `delay=0` → burst captures (3-5/min instead of 1/min).
 * Serial eliminates the race entirely; the chain knows exactly when to
 * fire next because the value comes from the same capture's response.
 */
export function useUploader(): UploaderResult {
  const { client, config } = useLookoutContext();
  const { maxRetries, retryDelays } = config.retry;

  const [uploads, setUploads] = useState<UploadState>({
    pending: 0,
    completed: 0,
    failed: 0,
  });
  const [trackedSeconds, setTrackedSeconds] = useState(0);
  const [lastScreenshotUrl, setLastScreenshotUrl] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [sessionConflict, setSessionConflict] = useState(false);

  const resetConflict = useCallback(() => setSessionConflict(false), []);

  const captureUploadConfirm = useCallback(
    async (capture: CaptureResult): Promise<UploadConfirmResult> => {
      setUploads((s) => ({ ...s, pending: s.pending + 1 }));
      try {
        const capturedAt = ENABLE_CREDIT_MODE
          ? new Date(capture.capturedAtMs ?? Date.now()).toISOString()
          : undefined;

        const { uploadUrl, screenshotId } = await retry(
          () => client.getUploadUrl({ capturedAt }),
          maxRetries,
          retryDelays,
        );

        await retry(
          () => client.uploadToR2(uploadUrl, capture.blob),
          maxRetries,
          retryDelays,
        );

        const result = await retry(
          () =>
            client.confirmScreenshot({
              screenshotId,
              width: capture.width,
              height: capture.height,
              fileSize: capture.blob.size,
            }),
          maxRetries,
          retryDelays,
        );

        setTrackedSeconds(result.trackedSeconds);
        setLastScreenshotUrl((prev) => {
          if (prev) URL.revokeObjectURL(prev);
          return URL.createObjectURL(capture.blob);
        });
        setUploads((s) => ({
          ...s,
          pending: s.pending - 1,
          completed: s.completed + 1,
        }));
        config.callbacks.onUploadSuccess?.({
          screenshotId,
          trackedSeconds: result.trackedSeconds,
        });

        return {
          trackedSeconds: result.trackedSeconds,
          nextExpectedAt: result.nextExpectedAt,
        };
      } catch (err) {
        const msg = err instanceof Error ? err.message : "Upload failed";
        setLastError(msg);
        setUploads((s) => ({
          ...s,
          pending: s.pending - 1,
          failed: s.failed + 1,
        }));
        config.callbacks.onUploadFailure?.(err instanceof Error ? err : new Error(msg));

        // 409 = session paused/stopped server-side. Surface the signal
        // so the host hook can re-sync session state.
        if (err instanceof HttpError && err.status === 409) {
          setSessionConflict(true);
        }
        throw err;
      }
    },
    [client, maxRetries, retryDelays, config.callbacks],
  );

  return {
    captureUploadConfirm,
    uploads,
    trackedSeconds,
    lastScreenshotUrl,
    lastError,
    sessionConflict,
    resetConflict,
  };
}
