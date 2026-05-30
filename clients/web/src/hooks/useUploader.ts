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

export interface UploadConfirmResult {
  trackedSeconds: number;
  nextExpectedAt: string;
}

/**
 * Serial upload pipeline. The caller (the tick chain) `await`s this
 * function — when it returns, the screenshot has been uploaded AND
 * confirmed, and the returned `nextExpectedAt` is the FRESH server
 * value for this specific capture.
 *
 * This is the core-loop change introduced in 0.2.4: previously the web
 * client used a fire-and-forget queue, and the tick chain read a shared
 * `nextExpectedAt` ref that lagged behind the in-flight upload. That
 * caused `setTimeout(tick, 0)` bursts whenever the ref was stale (= one
 * upload-roundtrip behind the chain). Matching the desktop's serial
 * model eliminates the race entirely.
 */
export function useUploader() {
  const [state, setState] = useState<UploadState>({
    pending: 0,
    completed: 0,
    failed: 0,
  });
  const [trackedSeconds, setTrackedSeconds] = useState(0);
  const [lastImageUrl, setLastImageUrl] = useState<string | null>(null);

  const captureUploadConfirm = useCallback(
    async (capture: CaptureResult): Promise<UploadConfirmResult> => {
      setState((s) => ({ ...s, pending: s.pending + 1 }));
      try {
        const capturedAt = ENABLE_CREDIT_MODE
          ? new Date(capture.capturedAtMs ?? Date.now()).toISOString()
          : undefined;

        const { uploadUrl, screenshotId } = await retry(() =>
          api.getUploadUrl({ capturedAt }),
        );
        await retry(() => api.uploadToR2(uploadUrl, capture.blob));
        const result = await retry(() =>
          api.confirmScreenshot({
            screenshotId,
            width: capture.width,
            height: capture.height,
            fileSize: capture.blob.size,
          }),
        );

        setTrackedSeconds(result.trackedSeconds);
        setLastImageUrl((prev) => {
          if (prev) URL.revokeObjectURL(prev);
          return URL.createObjectURL(capture.blob);
        });
        setState((s) => ({
          ...s,
          pending: s.pending - 1,
          completed: s.completed + 1,
        }));

        return {
          trackedSeconds: result.trackedSeconds,
          nextExpectedAt: result.nextExpectedAt,
        };
      } catch (err) {
        setState((s) => ({
          ...s,
          pending: s.pending - 1,
          failed: s.failed + 1,
        }));
        throw err;
      }
    },
    [],
  );

  return {
    captureUploadConfirm,
    uploadState: state,
    trackedSeconds,
    lastImageUrl,
  };
}
