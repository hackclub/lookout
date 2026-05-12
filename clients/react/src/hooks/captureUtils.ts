import {
  CANVAS_TO_BLOB_TIMEOUT_MS,
  VIDEO_READY_TIMEOUT_MS,
} from "@lookout/shared";
import type { CaptureResult } from "../types.js";

/** Wait for the video element to have decoded dimensions after play(). */
export function waitForVideoReady(
  video: HTMLVideoElement,
  timeoutMs: number = VIDEO_READY_TIMEOUT_MS,
): Promise<void> {
  if (video.videoWidth > 0 && video.videoHeight > 0) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + timeoutMs;
    (function check() {
      if (video.videoWidth > 0 && video.videoHeight > 0) return resolve();
      if (Date.now() > deadline)
        return reject(new Error("Video not ready — no frames received"));
      requestAnimationFrame(check);
    })();
  });
}

/**
 * Capture the current video frame as a JPEG blob, scaled to fit within
 * maxWidth/maxHeight. Returns null if the video or canvas is not ready.
 */
export function captureFrameAsJpeg(
  video: HTMLVideoElement,
  canvas: HTMLCanvasElement,
  settings: { maxWidth: number; maxHeight: number; jpegQuality: number },
): Promise<CaptureResult | null> {
  if (video.videoWidth === 0 || video.videoHeight === 0) {
    return Promise.resolve(null);
  }

  const scale = Math.min(
    settings.maxWidth / video.videoWidth,
    settings.maxHeight / video.videoHeight,
    1,
  );
  canvas.width = Math.round(video.videoWidth * scale);
  canvas.height = Math.round(video.videoHeight * scale);

  const ctx = canvas.getContext("2d");
  if (!ctx) {
    return Promise.resolve(null);
  }

  ctx.drawImage(video, 0, 0, canvas.width, canvas.height);

  // Stamp the capture moment in the client clock. The toBlob callback may
  // resolve milliseconds later — what matters is when the frame was
  // grabbed, not when the JPEG bytes finished encoding.
  const capturedAtMs = Date.now();

  const toBlobPromise = new Promise<CaptureResult | null>((resolve) => {
    canvas.toBlob(
      (blob) => {
        resolve(
          blob
            ? { blob, width: canvas.width, height: canvas.height, capturedAtMs }
            : null,
        );
      },
      "image/jpeg",
      settings.jpegQuality,
    );
  });

  const timeoutPromise = new Promise<null>((resolve) =>
    setTimeout(() => resolve(null), CANVAS_TO_BLOB_TIMEOUT_MS),
  );

  return Promise.race([toBlobPromise, timeoutPromise]);
}
