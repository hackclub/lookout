import { useCallback, useEffect, useRef, useState } from "react";
import { useLookoutContext } from "../LookoutProvider.js";
import { useScreenCapture } from "./useScreenCapture.js";
import { useCameraCapture } from "./useCameraCapture.js";
import { useUploader } from "./useUploader.js";
import { useSession } from "./useSession.js";
import { useSessionTimer } from "./useSessionTimer.js";
import type { LookoutState, LookoutActions, RecorderStatus } from "../types.js";

/**
 * Primary hook for Lookout integration.
 * Composes all lower-level hooks and orchestrates the capture-upload loop.
 */
export function useLookout(): { state: LookoutState; actions: LookoutActions } {
  const { config, client } = useLookoutContext();
  const callbacksRef = useRef(config.callbacks);
  callbacksRef.current = config.callbacks;

  const captureMode = config.capture.mode;

  const session = useSession();
  const screenCapture = useScreenCapture();
  const cameraCapture = useCameraCapture();
  // Delegate to the active capture source — both hooks are always called
  // (React rules of hooks) but only the active one's methods are invoked.
  const capture = captureMode === "camera" ? cameraCapture : screenCapture;
  const uploader = useUploader();

  // Estimate local tracked seconds from upload count when server hasn't caught up.
  // Server uses (count(distinct minute_buckets) - 1) * 60, so the first bucket
  // reports 0. Locally we can estimate: (completed - 1) * intervalSeconds once
  // we have ≥2 uploads, giving the user an immediate non-zero value.
  const intervalSeconds = Math.floor(config.capture.intervalMs / 1000);
  const localEstimate =
    uploader.uploads.completed >= 2
      ? (uploader.uploads.completed - 1) * intervalSeconds
      : 0;
  const bestTrackedSeconds = Math.max(
    session.trackedSeconds,
    uploader.trackedSeconds,
    localEstimate,
  );

  const displaySeconds = useSessionTimer(
    bestTrackedSeconds,
    capture.isSharing && (session.status === "active" || session.status === "pending"),
  );

  // Holds either a setInterval ID (legacy bucket-mode fallback) or
  // setTimeout ID (credit-mode self-scheduling chain). Cleared on unmount.
  const intervalRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const capturingRef = useRef(false);
  const prevStatusRef = useRef<RecorderStatus>(session.status);
  const intentionalPauseRef = useRef(false);
  const uploaderRef = useRef(uploader);
  uploaderRef.current = uploader;

  // Sync best tracked seconds to session
  useEffect(() => {
    if (bestTrackedSeconds > session.trackedSeconds) {
      session.updateTrackedSeconds(bestTrackedSeconds);
    }
  }, [bestTrackedSeconds, session.trackedSeconds, session.updateTrackedSeconds]);

  // Fire onStatusChange callback
  useEffect(() => {
    const prev = prevStatusRef.current;
    const next = session.status;
    if (prev !== next) {
      callbacksRef.current.onStatusChange?.(prev, next);
      if (next === "failed") {
        callbacksRef.current.onCompilationFailed?.();
      }
      prevStatusRef.current = next;
    }
  }, [session.status]);

  // Capture callback stored in a ref so the interval always calls the latest
  // version without needing to clear/recreate the interval on every render.
  const captureAndUploadRef = useRef(async () => {
    const result = await capture.takeScreenshot();
    if (result) {
      callbacksRef.current.onCapture?.(result);
      uploader.enqueue(result);
    }
  });
  captureAndUploadRef.current = async () => {
    const result = await capture.takeScreenshot();
    if (result) {
      callbacksRef.current.onCapture?.(result);
      uploader.enqueue(result);
    }
  };

  // Start/stop capture interval based on sharing + session state.
  // Uses a ref for the callback so the interval survives re-renders
  // without being cleared (fixes React StrictMode + parent re-render issues).
  const isActive = session.status === "active" || session.status === "pending";

  useEffect(() => {
    if (!capture.isSharing || !isActive) return;

    capturingRef.current = true;
    let cancelled = false;

    // Self-scheduling chain. After each capture-upload cycle, look at the
    // latest `nextExpectedAt` the server returned and schedule the next
    // fire for that wall-clock moment. If the moment has already passed
    // (sleep/App Nap/network blip), fire immediately. Falls back to the
    // configured interval when nextExpectedAt isn't available.
    const tick = async () => {
      if (cancelled) return;
      await captureAndUploadRef.current();
      if (cancelled) return;
      const nextIso = uploaderRef.current.getNextExpectedAt();
      let delayMs: number;
      if (nextIso) {
        const parsed = Date.parse(nextIso);
        delayMs = Number.isFinite(parsed) ? parsed - Date.now() : config.capture.intervalMs;
      } else {
        delayMs = config.capture.intervalMs;
      }
      // Catch-up-on-miss: if the target is in the past, schedule with 0ms
      // so we fire ASAP rather than waiting another full interval.
      if (delayMs < 0) delayMs = 0;
      intervalRef.current = setTimeout(tick, delayMs);
    };

    // Kick off the first capture immediately. After it resolves the chain
    // takes over.
    tick();

    return () => {
      capturingRef.current = false;
      cancelled = true;
      if (intervalRef.current !== null) {
        clearTimeout(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [capture.isSharing, isActive, config.capture.intervalMs]);

  // Auto-resume when screen sharing *starts* while session is paused
  // (e.g., user clicked "Share Screen & Resume" after a reload).
  // Only triggers on the false→true transition of isSharing, so that
  // calling pause() while already sharing does not immediately re-resume.
  const wasSharingRef = useRef(capture.isSharing);
  useEffect(() => {
    const wasSharing = wasSharingRef.current;
    wasSharingRef.current = capture.isSharing;

    if (!wasSharing && capture.isSharing && session.status === "paused") {
      intentionalPauseRef.current = false;
      session.resume().then(() => {
        callbacksRef.current.onResume?.();
      }).catch(() => {});
    }
  }, [capture.isSharing, session.status, session.resume]);

  // Auto-pause when screen sharing ends unexpectedly (mid-session loss)
  // OR on mount when session is active but stream was lost (page reload).
  // capturingRef.current distinguishes mid-session loss from mount recovery,
  // but both cases should auto-pause so the server doesn't stay active with no captures.
  useEffect(() => {
    if (!capture.isSharing && session.status === "active") {
      if (capturingRef.current) {
        // Mid-session: stream ended while capturing
        capturingRef.current = false;
        if (intervalRef.current) {
          clearInterval(intervalRef.current);
          intervalRef.current = null;
        }
        callbacksRef.current.onShareStop?.();
      }
      // Both cases: pause the server session so it doesn't accumulate dead time
      session.pause().catch(() => {});
    }
  }, [capture.isSharing, session.status, session.pause]);

  // Sync session status when uploader detects a 409 conflict
  useEffect(() => {
    if (uploader.sessionConflict) {
      session.syncStatus().then(() => uploader.resetConflict());
    }
  }, [uploader.sessionConflict, session.syncStatus, uploader.resetConflict]);

  // Auto-resume when server paused the session while we're still sharing
  // (e.g., stale lastScreenshotAt triggered the cron auto-pause).
  // Intentional user pauses are excluded via intentionalPauseRef.
  useEffect(() => {
    if (capture.isSharing && session.status === "paused" && !intentionalPauseRef.current) {
      session.resume().then(() => {
        callbacksRef.current.onResume?.();
      }).catch(() => {});
    }
  }, [capture.isSharing, session.status, session.resume]);

  // Auto-start
  useEffect(() => {
    if (
      config.autoStart &&
      !capture.isSharing &&
      (session.status === "pending" || session.status === "active")
    ) {
      capture.startSharing().catch(() => {});
    }
  }, [config.autoStart, session.status, capture.isSharing, capture.startSharing]);

  // Actions
  const startSharing = useCallback(async () => {
    try {
      await capture.startSharing();
      callbacksRef.current.onShareStart?.();
      // Auto-resume is handled by the useEffect above reacting to
      // capture.isSharing becoming true while session.status is "paused"
    } catch (err) {
      const e = err instanceof Error ? err : new Error(String(err));
      let message: string;
      const isCamera = captureMode === "camera";
      if (e.name === "NotAllowedError") {
        message = isCamera
          ? "Camera permission was denied. Please allow camera access and try again."
          : "Screen sharing permission was denied. Please try again and select a screen to share.";
      } else if (e.name === "AbortError") {
        message = isCamera ? "Camera access was cancelled." : "Screen sharing was cancelled.";
      } else {
        message = e.message || (isCamera ? "Failed to start camera." : "Failed to start screen sharing.");
      }
      callbacksRef.current.onError?.(new Error(message), "startSharing");
      session.setError(message);
    }
  }, [capture.startSharing, session, captureMode]);

  const stopSharing = useCallback(() => {
    capture.stopSharing();
    callbacksRef.current.onShareStop?.();
  }, [capture.stopSharing]);

  const pause = useCallback(async () => {
    intentionalPauseRef.current = true;
    if (intervalRef.current) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
    capturingRef.current = false;
    await session.pause();
    callbacksRef.current.onPause?.({ totalActiveSeconds: session.totalActiveSeconds });
  }, [session.pause, session.totalActiveSeconds]);

  const resume = useCallback(async () => {
    intentionalPauseRef.current = false;
    await session.resume();
    callbacksRef.current.onResume?.();
  }, [session.resume]);

  const stop = useCallback(async (options?: { name?: string }) => {
    if (intervalRef.current) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
    capturingRef.current = false;
    capture.stopSharing();
    await session.stop(options?.name);
    callbacksRef.current.onStop?.({
      trackedSeconds: session.trackedSeconds,
      totalActiveSeconds: session.totalActiveSeconds,
    });
  }, [session.stop, session.trackedSeconds, session.totalActiveSeconds, capture.stopSharing]);

  // Fetch video URL when session reaches "complete"
  const [videoUrl, setVideoUrl] = useState<string | null>(null);
  useEffect(() => {
    if (session.status !== "complete") return;
    let cancelled = false;
    client.getVideo().then((data: { videoUrl: string }) => {
      if (!cancelled) {
        setVideoUrl(data.videoUrl);
        callbacksRef.current.onComplete?.({ videoUrl: data.videoUrl });
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [session.status, client]);

  const isRecording = capture.isSharing && (session.status === "active" || session.status === "pending");

  const state: LookoutState = {
    status: session.status,
    isSharing: capture.isSharing,
    isRecording,
    trackedSeconds: bestTrackedSeconds,
    displaySeconds,
    screenshotCount: Math.max(session.screenshotCount, uploader.uploads.completed),
    uploads: uploader.uploads,
    lastScreenshotUrl: uploader.lastScreenshotUrl,
    videoUrl,
    error: session.error,
    captureMode,
    availableCameras: cameraCapture.devices,
    selectedCameraId: cameraCapture.selectedDeviceId,
    isPreviewing: cameraCapture.isPreviewing,
    previewStream: cameraCapture.previewStream,
  };

  const actions: LookoutActions = {
    startSharing,
    stopSharing,
    pause,
    resume,
    stop,
    selectCamera: cameraCapture.selectDevice,
    startPreview: cameraCapture.startPreview,
    stopPreview: cameraCapture.stopPreview,
  };

  return { state, actions };
}
