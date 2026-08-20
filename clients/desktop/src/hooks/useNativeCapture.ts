import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../logger.js";
import { listen } from "@tauri-apps/api/event";
import { SCREENSHOT_INTERVAL_MS, MAX_WIDTH, MAX_HEIGHT, JPEG_QUALITY } from "@lookout/shared";

/** Race a promise against a timeout. Rejects with a clear message if the timeout fires first. */
function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  return Promise.race([
    promise,
    new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error(`${label} timed out after ${ms / 1000}s`)), ms),
    ),
  ]);
}

const CAPTURE_TIMEOUT_MS = 45_000; // 45s — well under the 60s capture interval

export interface CaptureSource {
  type: "monitor" | "window" | "camera" | "pipewire";
  id: number | string; // number for monitor/window/pipewire, string (deviceId) for camera
}

interface CaptureUploadResult {
  confirmed: boolean;
  trackedSeconds: number;
  nextExpectedAt: string;
  previewBase64: string;
  previewWidth: number;
  previewHeight: number;
}

/** Capture a single frame from a camera, returned as base64 JPEG. */
export type CameraFrameCapture = () => Promise<{
  base64: string;
  width: number;
  height: number;
} | null>;

/**
 * Desktop-native capture hook. Uses Tauri IPC to:
 * 1. Take a native screenshot via xcap (Rust) — or grab a camera frame via the provided callback
 * 2. Upload directly from Rust (no CORS)
 * 3. Confirm with the server
 * 4. Return the captured frame as a preview URL
 *
 * For screen/window/pipewire sources, the 60s capture timer runs in Rust
 * (immune to WebView/App Nap throttling). For camera sources, the timer
 * runs in JS (browser must be active to capture frames from the video element).
 */
export function useNativeCapture(
  token: string,
  apiBaseUrl: string,
  sources: CaptureSource[],
  /** For camera sources: a callback that grabs a JPEG frame from the active stream. */
  cameraCapture?: CameraFrameCapture,
  /** Called when the capture loop discovers the server moved the session to a terminal state. */
  onSessionTerminated?: (status: string) => void,
  /** The session's server-known tracked seconds, used to seed a freshly
   *  started Rust tray timer. Needed because `trackedSeconds` below is
   *  hook-local state that starts at 0 — on a session that already has
   *  recorded time it can't seed anything until the first confirm lands. */
  sessionTrackedSeconds = 0,
) {
  const [isCapturing, setIsCapturing] = useState(false);
  const [trackedSeconds, setTrackedSeconds] = useState(0);
  const [screenshotCount, setScreenshotCount] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [lastScreenshotUrl, setLastScreenshotUrl] = useState<string | null>(null);
  const [lastCaptureAt, setLastCaptureAt] = useState<number | null>(null);

  const configuredRef = useRef(false);
  const sourcesRef = useRef(sources);
  sourcesRef.current = sources;

  // Track whether we're actively capturing so in-flight requests can
  // check if the user intentionally stopped (avoids auto-resume race).
  const capturingRef = useRef(false);

  // Best known tracked seconds, for seeding a freshly started Rust tray timer.
  // Both inputs are server-derived; take the higher one so neither a
  // not-yet-confirmed capture nor a lagging session poll seeds a low baseline.
  const trackedSecondsRef = useRef(0);
  trackedSecondsRef.current = Math.max(trackedSeconds, sessionTrackedSeconds);

  // Track blob URL for cleanup
  const blobUrlRef = useRef<string | null>(null);

  // Keep cameraCapture in a ref so captureOnce always sees the latest version
  const cameraCaptureRef = useRef(cameraCapture);
  cameraCaptureRef.current = cameraCapture;

  // Keep onSessionTerminated in a ref for stable closure access
  const onSessionTerminatedRef = useRef(onSessionTerminated);
  onSessionTerminatedRef.current = onSessionTerminated;

  // Detect whether sources are camera-only (JS timer) vs screen/window (Rust timer)
  const isCamera = sources.length === 1 && sources[0].type === "camera";
  const isCameraRef = useRef(isCamera);
  isCameraRef.current = isCamera;

  // ── Helper: convert base64 preview to blob URL ──
  const updatePreview = useCallback((previewBase64: string) => {
    if (!previewBase64) return;
    const bytes = Uint8Array.from(atob(previewBase64), (c) => c.charCodeAt(0));
    const blob = new Blob([bytes], { type: "image/jpeg" });
    const url = URL.createObjectURL(blob);
    if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
    blobUrlRef.current = url;
    setLastScreenshotUrl(url);
  }, []);

  // ─────────────────────────────────────────────────────────────────
  // Camera-only capture (JS-side timer, unchanged from original)
  // ─────────────────────────────────────────────────────────────────
  const captureOnceCamera = useCallback(async () => {
    const captureFn = cameraCaptureRef.current;
    if (!captureFn) {
      console.warn("[capture] camera source but no captureFrame callback provided, skipping");
      return;
    }
    const frame = await captureFn();
    if (!frame) {
      console.warn("[capture] camera frame capture returned null, skipping this interval");
      return;
    }
    console.log(`[capture] camera frame captured ${frame.width}x${frame.height} (${Math.round(frame.base64.length * 3 / 4 / 1024)}KB)`);
    try {
      const result = await withTimeout(
        invoke<CaptureUploadResult>("upload_frame", {
          base64: frame.base64,
          width: frame.width,
          height: frame.height,
        }),
        CAPTURE_TIMEOUT_MS,
        "upload_frame",
      );

      setTrackedSeconds(result.trackedSeconds);
      setLastCaptureAt(Date.now());
      setScreenshotCount((c) => {
        const n = c + 1;
        console.log(`[capture] screenshot #${n} done, tracked: ${result.trackedSeconds}s`);
        return n;
      });
      setError(null);
      updatePreview(result.previewBase64);
      // Sync the Rust tray timer so the menu bar time stays accurate
      invoke("sync_tray_tracked_seconds", { trackedSeconds: result.trackedSeconds }).catch(console.error);
    } catch (err) {
      const msg = err instanceof Error
        ? err.message + (err.stack ? "\n" + err.stack : "")
        : String(err);
      console.error(`[capture] camera capture failed: ${msg}`);
      setError(msg);

      if (!capturingRef.current) return;
      try {
        const res = await fetch(`${apiBaseUrl}/api/sessions/${token}/status`);
        if (res.ok) {
          const data = await res.json();
          if (data.status === "paused") {
            if (!capturingRef.current) return;
            await fetch(`${apiBaseUrl}/api/sessions/${token}/resume`, { method: "POST" });
            console.log("[capture] session auto-resumed after camera capture failure");
            setError(null);
          } else if (data.status !== "active" && data.status !== "pending") {
            console.warn(`[capture] session is ${data.status}, stopping capture`);
            setIsCapturing(false);
            onSessionTerminatedRef.current?.(data.status);
          }
        }
      } catch {
        // Best-effort
      }
    }
  }, [apiBaseUrl, token, updatePreview]);

  const captureOnceCameraRef = useRef(captureOnceCamera);
  captureOnceCameraRef.current = captureOnceCamera;

  // Camera-only JS timer (drift-compensating setTimeout chain)
  useEffect(() => {
    if (!isCapturing || !isCamera) return;

    let cancelled = false;
    let timerId: ReturnType<typeof setTimeout> | null = null;
    let nextTargetTime = Date.now();
    let lastTickTime = Date.now();
    const SLEEP_THRESHOLD = SCREENSHOT_INTERVAL_MS * 2.5;

    const scheduleNext = () => {
      if (cancelled) return;
      nextTargetTime += SCREENSHOT_INTERVAL_MS;
      const delay = Math.max(0, nextTargetTime - Date.now());
      timerId = setTimeout(tick, delay);
    };

    const tick = async () => {
      if (cancelled) return;
      const now = Date.now();
      const elapsed = now - lastTickTime;
      lastTickTime = now;

      if (elapsed > SLEEP_THRESHOLD) {
        console.warn(`[capture] detected sleep (gap: ${Math.round(elapsed / 1000)}s), checking session status...`);
        nextTargetTime = now;
        try {
          const res = await fetch(`${apiBaseUrl}/api/sessions/${token}/status`);
          if (res.ok) {
            const data = await res.json();
            console.log(`[capture] session status after sleep: ${data.status}`);
            if (typeof data.trackedSeconds === "number") {
              setTrackedSeconds(data.trackedSeconds);
            }
            if (data.status === "paused") {
              await fetch(`${apiBaseUrl}/api/sessions/${token}/resume`, { method: "POST" });
              console.log("[capture] session resumed after sleep");
            } else if (data.status !== "active" && data.status !== "pending") {
              console.warn(`[capture] session is ${data.status}, stopping capture`);
              capturingRef.current = false;
              setIsCapturing(false);
              onSessionTerminatedRef.current?.(data.status);
              return;
            }
          }
        } catch (e) {
          console.error("[capture] sleep recovery failed:", e);
        }
      }

      try {
        await captureOnceCameraRef.current();
      } finally {
        scheduleNext();
      }
    };

    console.log(`[capture] camera capture loop started, interval: ${SCREENSHOT_INTERVAL_MS}ms`);
    timerId = setTimeout(tick, 0);
    return () => {
      console.log("[capture] camera capture loop stopped");
      cancelled = true;
      if (timerId !== null) clearTimeout(timerId);
    };
  }, [isCapturing, isCamera, apiBaseUrl, token]);

  // ─────────────────────────────────────────────────────────────────
  // Screen/window/pipewire: Rust-side capture loop + event listeners
  // ─────────────────────────────────────────────────────────────────
  useEffect(() => {
    if (!isCapturing || isCamera) return;

    const nativeSources = sourcesRef.current
      .filter((s): s is { type: "monitor" | "window" | "pipewire"; id: number } =>
        s.type !== "camera" && typeof s.id === "number"
      );

    if (nativeSources.length === 0) return;

    console.log(`[capture] starting Rust capture loop for ${nativeSources.length} sources`);

    invoke("start_capture_loop", {
      sources: nativeSources,
      maxWidth: MAX_WIDTH,
      maxHeight: MAX_HEIGHT,
      jpegQuality: Math.round(JPEG_QUALITY * 100),
    }).then(() => {
      // A freshly started Rust tray timer counts from 0 — seed it with the
      // last known tracked seconds so the menu-bar time doesn't reset while
      // waiting for the first confirm (visible on pause → resume).
      if (trackedSecondsRef.current > 0) {
        invoke("sync_tray_tracked_seconds", { trackedSeconds: trackedSecondsRef.current }).catch(console.error);
      }
    }).catch((err) => {
      console.error("[capture] failed to start Rust capture loop:", err);
      setError(String(err));
    });

    // Listen for results from the Rust capture loop.
    // Each listen() returns a Promise<UnlistenFn>. We must await them
    // during cleanup to avoid leaking listeners on fast unmount.
    const listenerPromises = [
      listen<CaptureUploadResult>("capture-tick-result", (event) => {
        const result = event.payload;
        setTrackedSeconds(result.trackedSeconds);
        setLastCaptureAt(Date.now());
        setScreenshotCount((c) => {
          const n = c + 1;
          console.log(`[capture] screenshot #${n} done (Rust), tracked: ${result.trackedSeconds}s`);
          return n;
        });
        setError(null);
        updatePreview(result.previewBase64);
      }),
      // In-between live-preview frames from the capture loop (20/min while
      // the window is focused). Same redaction-aware capture path as the
      // uploads, delivered through the same preview pipeline — so
      // lastScreenshotUrl is simply live whenever someone's looking.
      listen<{ previewBase64: string; previewWidth: number; previewHeight: number }>(
        "capture-preview-frame",
        (event) => {
          updatePreview(event.payload.previewBase64);
        },
      ),
      listen<{ message: string }>("capture-tick-error", (event) => {
        console.error(`[capture] Rust capture error: ${event.payload.message}`);
        setError(event.payload.message);
      }),
      listen<number>("capture-tracked-seconds", (event) => {
        setTrackedSeconds(event.payload);
      }),
      listen<{ status: string }>("capture-session-terminated", (event) => {
        console.warn(`[capture] session terminated: ${event.payload.status}`);
        capturingRef.current = false;
        setIsCapturing(false);
        onSessionTerminatedRef.current?.(event.payload.status);
      }),
      // Wayland: the user stopped the share from the system indicator. The
      // portal session is gone, so every further capture would come back
      // empty — stop rather than tick away over a dead stream.
      listen<{ nodeIds: number[] }>("screencast-revoked", (event) => {
        console.warn(`[capture] screen sharing revoked (nodes ${event.payload.nodeIds.join(", ")})`);
        capturingRef.current = false;
        setIsCapturing(false);
        setError(
          "Screen sharing was stopped from your system's sharing indicator. " +
            "Pick a source again to keep recording.",
        );
      }),
      // Same outcome, found the slow way: the capture loop gave up after the
      // source failed repeatedly without the portal ever telling us.
      listen<{ message: string }>("capture-source-lost", (event) => {
        console.error(`[capture] capture source lost: ${event.payload.message}`);
        capturingRef.current = false;
        setIsCapturing(false);
        setError(
          `Lost the screen capture source — recording stopped. Pick a source again to ` +
            `continue. (${event.payload.message})`,
        );
      }),
    ];

    return () => {
      console.log("[capture] stopping Rust capture loop");
      invoke("stop_capture_loop").catch(console.error);
      listenerPromises.forEach((p) => p.then((unlisten) => unlisten()));
    };
  }, [isCapturing, isCamera, updatePreview]);

  // Clean up blob URL on unmount
  useEffect(() => {
    return () => {
      if (blobUrlRef.current) {
        URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = null;
      }
    };
  }, []);

  const startCapturing = useCallback(async () => {
    if (!configuredRef.current) {
      console.log(`[capture] configuring with token: ${token.slice(0, 8)}...`);
      await invoke("configure", { token, apiBaseUrl });
      configuredRef.current = true;
    }
    console.log("[capture] starting capture");
    capturingRef.current = true;
    setIsCapturing(true);
    setError(null);
  }, [token, apiBaseUrl]);

  const stopCapturing = useCallback(() => {
    console.log("[capture] stopping capture");
    capturingRef.current = false;
    setIsCapturing(false);
  }, []);

  return {
    isCapturing,
    trackedSeconds,
    screenshotCount,
    error,
    lastScreenshotUrl,
    lastCaptureAt,
    startCapturing,
    stopCapturing,
  };
}
