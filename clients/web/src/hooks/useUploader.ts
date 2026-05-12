import { useRef, useCallback, useState } from "react";
import { api } from "../api/client.js";
import type { CaptureResult } from "./useScreenCapture.js";

interface UploadState {
  pending: number;
  completed: number;
  failed: number;
}

const MAX_RETRIES = 3;
const RETRY_DELAYS = [2000, 4000, 8000];

/** Toggle the web client into credit-mode by sending `capturedAt`. Old
 *  builds (without this flag) keep using bucket-mode on the server. */
const ENABLE_CREDIT_MODE = true;

async function retry<T>(
  fn: () => Promise<T>,
  retries: number = MAX_RETRIES,
): Promise<T> {
  for (let i = 0; i < retries; i++) {
    try {
      return await fn();
    } catch (err) {
      if (i === retries - 1) throw err;
      await new Promise((r) => setTimeout(r, RETRY_DELAYS[i]));
    }
  }
  throw new Error("Unreachable");
}

export function useUploader() {
  const [state, setState] = useState<UploadState>({
    pending: 0,
    completed: 0,
    failed: 0,
  });
  const [trackedSeconds, setTrackedSeconds] = useState(0);
  const [lastImageUrl, setLastImageUrl] = useState<string | null>(null);
  const nextExpectedAtRef = useRef<string | null>(null);
  const bufferRef = useRef<CaptureResult[]>([]);
  const processingRef = useRef(false);

  const processQueue = useCallback(async () => {
    if (processingRef.current) return;
    processingRef.current = true;

    while (bufferRef.current.length > 0) {
      const capture = bufferRef.current.shift()!;
      setState((s) => ({ ...s, pending: s.pending - 1 }));

      try {
        // Step 1: Get presigned URL (server records timestamp). Send
        // capturedAt to opt into credit mode; omit for legacy bucket.
        const capturedAt = ENABLE_CREDIT_MODE
          ? new Date(capture.capturedAtMs ?? Date.now()).toISOString()
          : undefined;
        const { uploadUrl, screenshotId, nextExpectedAt } = await retry(() =>
          api.getUploadUrl({ capturedAt }),
        );
        nextExpectedAtRef.current = nextExpectedAt;

        // Step 2: Upload to R2
        await retry(() => api.uploadToR2(uploadUrl, capture.blob));

        // Step 3: Confirm upload
        const result = await retry(() =>
          api.confirmScreenshot({
            screenshotId,
            width: capture.width,
            height: capture.height,
            fileSize: capture.blob.size,
          }),
        );

        setTrackedSeconds(result.trackedSeconds);
        nextExpectedAtRef.current = result.nextExpectedAt;
        // Create a preview URL from the blob
        setLastImageUrl((prev) => {
          if (prev) URL.revokeObjectURL(prev);
          return URL.createObjectURL(capture.blob);
        });
        setState((s) => ({ ...s, completed: s.completed + 1 }));
      } catch {
        setState((s) => ({ ...s, failed: s.failed + 1 }));
        // Non-fatal: lost screenshot, continue
      }
    }

    processingRef.current = false;
  }, []);

  const enqueueUpload = useCallback(
    (capture: CaptureResult) => {
      // Cap buffer at 5 pending to avoid memory issues
      if (bufferRef.current.length >= 5) {
        bufferRef.current.shift();
        setState((s) => ({ ...s, pending: s.pending - 1, failed: s.failed + 1 }));
      }

      bufferRef.current.push(capture);
      setState((s) => ({ ...s, pending: s.pending + 1 }));
      processQueue();
    },
    [processQueue],
  );

  const getNextExpectedAt = useCallback(
    () => nextExpectedAtRef.current,
    [],
  );

  return {
    enqueueUpload,
    uploadState: state,
    trackedSeconds,
    lastImageUrl,
    nextExpectedAt: nextExpectedAtRef.current,
    getNextExpectedAt,
  };
}
