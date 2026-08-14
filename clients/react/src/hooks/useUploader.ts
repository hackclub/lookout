import { useCallback, useRef, useState } from "react";
import {
  CAPTURE_FORMAT_CONTENT_TYPES,
  ClockOffset,
  type CaptureFormat,
} from "@lookout/shared";
import { useLookoutContext } from "../LookoutProvider.js";
import { HttpError } from "../api/client.js";
import type { UploadState } from "../types.js";

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

/**
 * The server granted a different format than the clip we hold.
 *
 * Distinct from a transient failure on purpose: it means the session's clip
 * support went away underneath us, so retrying the same clip will fail
 * identically forever. The capture loop reacts by switching the session to
 * JPEG captures rather than burning a minute an hour on it.
 */
export class ClipFormatRejectedError extends Error {
  readonly granted: CaptureFormat;
  constructor(requested: CaptureFormat, granted: CaptureFormat) {
    super(
      `Server granted "${granted}" for a "${requested}" clip — switching to JPEG captures`,
    );
    this.name = "ClipFormatRejectedError";
    this.granted = granted;
  }
}

/** Unified upload payload: a single JPEG frame (format omitted/"jpeg")
 *  or a per-minute clip ("webm"/"mp4" from the ClipRecorder). */
export interface UploadPayload {
  blob: Blob;
  width: number;
  height: number;
  capturedAtMs?: number;
  format?: CaptureFormat;
  /** Frames inside a clip. Omitted for JPEG captures. */
  frameCount?: number;
  /** Set when a clip hit its frame cap because uploads were running behind
   *  the capture cadence. Client-side telemetry only — never sent. */
  truncated?: boolean;
  /** JPEG used for the UI preview when `blob` isn't an image. */
  previewBlob?: Blob | null;
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
  captureUploadConfirm: (capture: UploadPayload) => Promise<UploadConfirmResult>;
  /** Best estimate of the SERVER's current wall-clock, in ms since epoch.
   *  `Date.now()` corrected by the running clock-offset estimate; identical
   *  to `Date.now()` on a healthy clock. The capture scheduler MUST use
   *  this — `nextExpectedAt` is server wall-clock, and subtracting the raw
   *  local clock from it bakes the machine's skew into every tick delay
   *  (>30s of skew zeroes the credit, >60s halves the capture rate against
   *  the 2x-interval clamp). */
  serverNowMs: () => number;
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

  // Running estimate of how far this device's clock is from the server's.
  // A ref, not state: it's read on the next capture, and a re-render on every
  // upload would be pure noise. Every upload-url response carries the
  // server's own clock, so the estimate improves once a minute for free.
  const clockOffsetRef = useRef(new ClockOffset());

  // Stable across renders so schedulers can hold it in a ref or a dep list
  // without re-subscribing.
  const serverNowMs = useCallback(
    () => clockOffsetRef.current.correct(Date.now()),
    [],
  );

  const captureUploadConfirm = useCallback(
    async (capture: UploadPayload): Promise<UploadConfirmResult> => {
      setUploads((s) => ({ ...s, pending: s.pending + 1 }));
      try {
        // Correct the capture moment into server time. A no-op for a healthy
        // clock; for a skewed one it's the difference between every capture
        // landing in the ±30s credit window and none of them doing so.
        const localCapturedAtMs = capture.capturedAtMs ?? Date.now();
        const capturedAt = ENABLE_CREDIT_MODE
          ? new Date(clockOffsetRef.current.correct(localCapturedAtMs)).toISOString()
          : undefined;
        const format: CaptureFormat = capture.format ?? "jpeg";

        const sentAt = Date.now();
        const urlResponse = await retry(
          () =>
            client.getUploadUrl({
              capturedAt,
              format: format === "jpeg" ? undefined : format,
            }),
          maxRetries,
          retryDelays,
        );
        // Fold the server's clock into the estimate. Bracketed by the local
        // instants either side of the request so the round trip isn't charged
        // to the offset.
        if (urlResponse.serverTime) {
          clockOffsetRef.current.observe(
            urlResponse.serverTime,
            sentAt,
            Date.now(),
          );
          if (urlResponse.capturedAtAdopted) {
            console.warn(
              `[lookout] this device's clock is ~${Math.round(
                clockOffsetRef.current.offset / 1000,
              )}s off from the server, so that capture was stamped on arrival. ` +
                `Later captures are corrected automatically.`,
            );
          }
        }
        const { uploadUrl, screenshotId } = urlResponse;
        // Defense-in-depth: the capture loop only records clips when the
        // session said clipsEnabled, so a downgrade here (granted format ≠
        // requested) means server state changed under us. The presigned URL
        // is signed for the granted content type — uploading the clip
        // against it would fail the signature, so fail fast instead.
        if (format !== "jpeg" && urlResponse.format !== format) {
          throw new ClipFormatRejectedError(
            format,
            urlResponse.format ?? "jpeg",
          );
        }

        await retry(
          () =>
            client.uploadToR2(
              uploadUrl,
              capture.blob,
              CAPTURE_FORMAT_CONTENT_TYPES[format],
            ),
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
              ...(capture.frameCount ? { frameCount: capture.frameCount } : {}),
            }),
          maxRetries,
          retryDelays,
        );

        setTrackedSeconds(result.trackedSeconds);
        // Clips aren't <img>-renderable — preview with the cut-time JPEG
        // snapshot instead, and keep the previous preview if none came.
        const previewBlob =
          format === "jpeg" ? capture.blob : capture.previewBlob ?? null;
        if (previewBlob) {
          setLastScreenshotUrl((prev) => {
            if (prev) URL.revokeObjectURL(prev);
            return URL.createObjectURL(previewBlob);
          });
        }
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
    serverNowMs,
    uploads,
    trackedSeconds,
    lastScreenshotUrl,
    lastError,
    sessionConflict,
    resetConflict,
  };
}
