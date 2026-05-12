import { useRef, useCallback, useState } from "react";
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

export interface UploaderResult {
  /** Add a capture to the upload queue. */
  enqueue: (capture: CaptureResult) => void;
  /** Current upload queue state. */
  uploads: UploadState;
  /** Server-reported tracked seconds after latest confirmation. */
  trackedSeconds: number;
  /** Object URL of last successfully uploaded screenshot. */
  lastScreenshotUrl: string | null;
  /** ISO timestamp: when the server expects the next screenshot. */
  nextExpectedAt: string | null;
  /** Snapshot of the latest `nextExpectedAt` from the ref — for callers
   *  (e.g. capture-loop schedulers) that need the freshest value without
   *  re-rendering. Returns the same string `nextExpectedAt` does, but
   *  available synchronously from within effects. */
  getNextExpectedAt: () => string | null;
  /** Last upload error message, if any. */
  lastError: string | null;
  /** True when a 409 conflict was received (session paused server-side). */
  sessionConflict: boolean;
  /** Clear the sessionConflict flag after handling. */
  resetConflict: () => void;
}

export function useUploader(): UploaderResult {
  const { client, config } = useLookoutContext();
  const { maxRetries, retryDelays, maxPendingBuffer } = config.retry;

  const [uploads, setUploads] = useState<UploadState>({
    pending: 0,
    completed: 0,
    failed: 0,
  });
  const [trackedSeconds, setTrackedSeconds] = useState(0);
  const [lastScreenshotUrl, setLastScreenshotUrl] = useState<string | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [sessionConflict, setSessionConflict] = useState(false);
  const nextExpectedAtRef = useRef<string | null>(null);
  const bufferRef = useRef<CaptureResult[]>([]);
  const processingRef = useRef(false);

  const resetConflict = useCallback(() => setSessionConflict(false), []);

  const processQueue = useCallback(async () => {
    if (processingRef.current) return;
    processingRef.current = true;

    while (bufferRef.current.length > 0) {
      const capture = bufferRef.current.shift()!;
      setUploads((s) => ({ ...s, pending: s.pending - 1 }));

      try {
        // Send capturedAt for credit-mode opt-in. When disabled (legacy
        // build), this is omitted and the server stays in bucket mode.
        const capturedAt = ENABLE_CREDIT_MODE
          ? new Date(capture.capturedAtMs ?? Date.now()).toISOString()
          : undefined;
        const { uploadUrl, screenshotId, nextExpectedAt } = await retry(
          () => client.getUploadUrl({ capturedAt }),
          maxRetries,
          retryDelays,
        );
        nextExpectedAtRef.current = nextExpectedAt;

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
        nextExpectedAtRef.current = result.nextExpectedAt;
        setLastScreenshotUrl((prev) => {
          if (prev) URL.revokeObjectURL(prev);
          return URL.createObjectURL(capture.blob);
        });
        setUploads((s) => ({ ...s, completed: s.completed + 1 }));
        config.callbacks.onUploadSuccess?.({
          screenshotId,
          trackedSeconds: result.trackedSeconds,
        });
      } catch (err) {
        const msg = err instanceof Error ? err.message : "Upload failed";
        setLastError(msg);
        setUploads((s) => ({ ...s, failed: s.failed + 1 }));
        config.callbacks.onUploadFailure?.(err instanceof Error ? err : new Error(msg));

        // On 409 (session paused server-side), signal conflict and drain
        // remaining buffer — they'll all 409 too.
        if (err instanceof HttpError && err.status === 409) {
          setSessionConflict(true);
          const remaining = bufferRef.current.length;
          if (remaining > 0) {
            bufferRef.current.length = 0;
            setUploads((s) => ({ ...s, pending: 0, failed: s.failed + remaining }));
          }
          break;
        }
      }
    }

    processingRef.current = false;
  }, [client, maxRetries, retryDelays]);

  const enqueue = useCallback(
    (capture: CaptureResult) => {
      if (bufferRef.current.length >= maxPendingBuffer) {
        bufferRef.current.shift();
        setUploads((s) => ({ ...s, pending: s.pending - 1, failed: s.failed + 1 }));
      }
      bufferRef.current.push(capture);
      setUploads((s) => ({ ...s, pending: s.pending + 1 }));
      processQueue();
    },
    [maxPendingBuffer, processQueue],
  );

  const getNextExpectedAt = useCallback(
    () => nextExpectedAtRef.current,
    [],
  );

  return {
    enqueue,
    uploads,
    trackedSeconds,
    lastScreenshotUrl,
    nextExpectedAt: nextExpectedAtRef.current,
    getNextExpectedAt,
    lastError,
    sessionConflict,
    resetConflict,
  };
}
