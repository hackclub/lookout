import { useCallback, useEffect, useRef, useState } from "react";
import {
  CLIP_FRAME_INTERVAL_MS,
  CLIP_FIRST_CUT_DELAY_MS,
  MAX_CLIP_UPLOAD_FAILURES,
} from "@lookout/shared";
import { useLookoutContext } from "../LookoutProvider.js";
import { useScreenCapture } from "./useScreenCapture.js";
import { useCameraCapture } from "./useCameraCapture.js";
import {
  useUploader,
  ClipFormatRejectedError,
  type UploadPayload,
  type UploadConfirmResult,
} from "./useUploader.js";
import { useSession } from "./useSession.js";
import { useSessionTimer } from "./useSessionTimer.js";
import { useSilentAudioKeepAlive } from "./useSilentAudioKeepAlive.js";
import { computeBestTrackedSeconds } from "./computeBestTracked.js";
import { ClipRecorder } from "./clipRecorder.js";
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

  // The timer reads ONLY server-authoritative values. See
  // computeBestTrackedSeconds for why — the previous code added a
  // third input derived from `uploads.completed`, which inflated the
  // display whenever uploads succeeded but didn't credit (e.g. ~90s
  // upload latency where every other capture lands outside the streak
  // window). The visible symptom was "timer shows 2x the recording
  // time", reported by browser users while desktop users were fine
  // (desktop bypasses this code path entirely).
  const bestTrackedSeconds = computeBestTrackedSeconds({
    sessionTrackedSeconds: session.trackedSeconds,
    uploaderTrackedSeconds: uploader.trackedSeconds,
  });

  const displaySeconds = useSessionTimer(
    bestTrackedSeconds,
    capture.isSharing && (session.status === "active" || session.status === "pending"),
  );

  // Holds either a setInterval ID (legacy bucket-mode fallback) or
  // setTimeout ID (credit-mode self-scheduling chain). Cleared on unmount.
  const intervalRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Clip recorder for sessions with clips enabled (screen mode only).
  // Null = classic one-JPEG-per-minute captures.
  const clipRecorderRef = useRef<ClipRecorder | null>(null);
  const capturingRef = useRef(false);
  const prevStatusRef = useRef<RecorderStatus>(session.status);
  const intentionalPauseRef = useRef(false);
  // Per-action re-entrancy guards. A pause/resume/stop request is a single
  // POST to a rate-limited endpoint (10 req/min per token). When the button
  // feels unresponsive, users rage-click — and without a guard each click
  // fired its own request, bursting past the limit and tripping 429s. These
  // suppress duplicate invocations of the *same* action while one is in
  // flight; distinct actions (e.g. pause then stop) are unaffected. Refs, not
  // state: the dedup must take effect synchronously within the same tick, and
  // it never needs to trigger a re-render.
  const pauseInFlightRef = useRef(false);
  const resumeInFlightRef = useRef(false);
  const stopInFlightRef = useRef(false);

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

  // Refs to the latest action implementations so the chain's tick body
  // always calls the freshest function without re-running the effect.
  const takeScreenshotRef = useRef(capture.takeScreenshot);
  takeScreenshotRef.current = capture.takeScreenshot;
  const captureUploadConfirmRef = useRef(uploader.captureUploadConfirm);
  captureUploadConfirmRef.current = uploader.captureUploadConfirm;
  const serverNowRef = useRef(uploader.serverNowMs);
  serverNowRef.current = uploader.serverNowMs;

  // Start/stop capture interval based on sharing + session state.
  const isActive = session.status === "active" || session.status === "pending";

  // Keep the page "audible" while recording so the browser doesn't
  // throttle our setTimeout chain when the user backgrounds the tab or
  // enables Low Power Mode. The live MediaStream track already exempts
  // us from some throttling, but Low Power Mode user reports suggest
  // it's not enough on its own. Harmless on desktop (Tauri) — the Rust
  // capture loop is independent of this.
  useSilentAudioKeepAlive(capture.isSharing && isActive);

  useEffect(() => {
    if (!capture.isSharing || !isActive) return;

    capturingRef.current = true;
    let cancelled = false;

    // Clip mode: the session accepts clips (known from the session fetch,
    // BEFORE any upload), this browser can encode them, and we're capturing
    // the screen (camera mode stays on JPEG). The recorder starts grabbing
    // frames immediately so the very first upload is already a clip — the
    // compiled timelapse has motion from second zero, never a still.
    const frameIntervalMs = session.frameIntervalMs ?? CLIP_FRAME_INTERVAL_MS;
    let clipRecorder: ClipRecorder | null = null;
    if (
      captureMode !== "camera" &&
      session.clipsEnabled &&
      ClipRecorder.isSupported()
    ) {
      const video = screenCapture.getVideo();
      if (video) {
        try {
          clipRecorder = new ClipRecorder(video, frameIntervalMs, {
            maxWidth: config.capture.maxWidth,
            maxHeight: config.capture.maxHeight,
            jpegQuality: config.capture.jpegQuality,
            // Denser cadence for the short opening clip, which is cut after
            // CLIP_FIRST_CUT_DELAY_MS — well under one frame interval — so
            // that first upload carries a few frames rather than one.
            openingFrameIntervalMs: Math.max(
              500,
              Math.round(CLIP_FIRST_CUT_DELAY_MS / 4),
            ),
          });
          clipRecorder.start();
        } catch (err) {
          console.warn(
            "[lookout] clip recorder unavailable, using JPEG captures:",
            err,
          );
          clipRecorder = null;
        }
      }
    }
    clipRecorderRef.current = clipRecorder;

    // Capture-upload chain — mirrors the desktop Rust loop in
    // `clients/desktop/src-tauri/src/lib.rs::capture_loop_task`, including
    // its concurrency shape.
    //
    // The upload runs CONCURRENTLY with recording rather than blocking it.
    // The previous version awaited the full round trip before scheduling the
    // next tick, which made every second of upload latency a second the
    // recorder wasn't cutting on schedule. On a slow uplink that compounds:
    // clips stretch to cover minutes each (a clip renders as ONE second of
    // video however long it took to record, so that footage is genuinely
    // lost), capturedAt drifts past the server's ±30s streak window so the
    // minute credits nothing, and the oversized clip is refused on arrival.
    // Uploading off the critical path keeps the cut cadence tied to the
    // clock instead of to the network.
    //
    // Strictly ONE upload in flight, exactly as desktop does it: the next
    // tick settles the previous upload before cutting. That preserves
    // capturedAt monotonicity and the per-session rate-limit assumptions,
    // and it is what stops a bad connection from fanning out into parallel
    // uploads that make the congestion worse.
    let inFlight: Promise<UploadConfirmResult> | null = null;

    // Clip-failure accounting, mirroring the desktop loop's latch. Clips are
    // an enhancement; a JPEG a minute is the contract. Anything that makes
    // clips unworkable must degrade to that instead of costing the user
    // minutes, however many devices and browsers this runs on.
    let clipFailures = 0;
    const disableClips = (why: string) => {
      if (!clipRecorderRef.current) return;
      console.warn(
        `[lookout] ${why} — recording one JPEG per minute for the rest of ` +
          `this session.`,
      );
      clipRecorderRef.current.stop();
      clipRecorderRef.current = null;
    };

    /**
     * Upload a capture, and if a CLIP upload fails, retry the same tick as a
     * single JPEG.
     *
     * The retry reuses the clip's own cut-time JPEG snapshot and — critically
     * — its `capturedAtMs`, so the capture still lands inside the server's
     * ±30s streak window and the minute credits. Without this a failed clip
     * upload cost the whole minute: the desktop client had the fallback, the
     * web client didn't.
     */
    const uploadWithFallback = async (
      payload: UploadPayload,
    ): Promise<UploadConfirmResult> => {
      const isClip = payload.format != null && payload.format !== "jpeg";
      try {
        const result = await captureUploadConfirmRef.current(payload);
        if (isClip) clipFailures = 0; // a clip landed: earlier trouble was transient
        return result;
      } catch (err) {
        if (!isClip) throw err;

        // The session no longer accepts clips. Retrying is pointless.
        if (err instanceof ClipFormatRejectedError) {
          disableClips("the server no longer accepts clips for this session");
        } else if (++clipFailures >= MAX_CLIP_UPLOAD_FAILURES) {
          disableClips(
            `${clipFailures} consecutive clip uploads failed`,
          );
        }

        if (!payload.previewBlob) throw err;
        console.warn("[lookout] clip upload failed — retrying as a JPEG:", err);
        return await captureUploadConfirmRef.current({
          blob: payload.previewBlob,
          width: payload.width,
          height: payload.height,
          capturedAtMs: payload.capturedAtMs,
        });
      }
    };

    /** Await the in-flight upload, if any, and fold its result into the
     *  schedule. Returns the server's fresh nextExpectedAt, or null. */
    const settleInFlight = async (): Promise<string | null> => {
      if (!inFlight) return null;
      const pending = inFlight;
      try {
        return (await pending).nextExpectedAt;
      } catch (err) {
        // Pipeline failure (network / server / 409). The chain stays alive
        // on the local fallback cadence; useUploader has already surfaced
        // the error and any 409 conflict.
        console.warn("[lookout] capture upload failed:", err);
        return null;
      } finally {
        // Only clear if nothing newer replaced it.
        if (inFlight === pending) inFlight = null;
      }
    };

    const scheduleNext = (nextExpectedAt: string | null) => {
      if (cancelled) return;
      // nextExpectedAt is SERVER wall-clock — subtract our estimate of the
      // server's now, never the raw local clock. Raw Date.now() baked the
      // machine's clock skew into every delay: >30s of skew pushed every
      // capture out of the ±30s credit window (trackedSeconds stuck at 0),
      // and >60s pinned the delay at the clamp below, halving the capture
      // rate and with it the compiled video's length.
      const now = serverNowRef.current();
      const target = nextExpectedAt
        ? Date.parse(nextExpectedAt)
        : now + config.capture.intervalMs;
      // Defensive upper bound: never sleep longer than 2x interval.
      // Matches desktop's same clamp — protects against malformed
      // server timestamps.
      const uncapped = Math.max(0, target - now);
      if (uncapped > config.capture.intervalMs * 2) {
        // With the offset applied this should never bind for clock skew —
        // if it fires, something else is feeding us bad targets, and
        // silence here is how the skew bug ran unnoticed for three months.
        console.warn(
          `[lookout] next-capture delay ${uncapped}ms exceeds the ` +
            `2x-interval clamp — capping to ${config.capture.intervalMs * 2}ms`,
        );
      }
      const delay = Math.min(config.capture.intervalMs * 2, uncapped);
      if (intervalRef.current !== null) clearTimeout(intervalRef.current);
      intervalRef.current = setTimeout(tick, delay);
    };

    const tick = async () => {
      if (cancelled) return;

      // Settle the previous upload BEFORE cutting, so uploads stay ordered.
      // Every step of it is deadline-bounded (UPLOAD_STEP_TIMEOUT_MS), so a
      // dead socket can't park the loop here indefinitely.
      let nextExpectedAt = await settleInFlight();
      if (cancelled) return;

      try {
        // cut() finalizes the last interval's clip and immediately starts
        // recording the next one. A null cut (empty clip, encoder hiccup)
        // falls back to a single JPEG so the tick — and the session's
        // credit streak — never skips a beat.
        let payload: UploadPayload | null =
          (await clipRecorderRef.current?.cut()) ?? null;
        if (payload?.truncated) {
          console.warn(
            "[lookout] clip hit its frame cap — uploads are running behind " +
              "the capture cadence, so this clip covers a longer window.",
          );
        }
        if (!payload) {
          payload = await takeScreenshotRef.current();
        }
        if (payload) {
          callbacksRef.current.onCapture?.(payload);
          // Fire and hold, don't await: recording of the next clip is
          // already underway and must not wait on this.
          inFlight = uploadWithFallback(payload);
          // Refine the schedule the moment the confirm lands, if that
          // happens before the next tick — the same role desktop's third
          // select arm plays. Rejections are handled by settleInFlight;
          // swallow here so this never becomes an unhandled rejection.
          const pending = inFlight;
          pending
            .then((result) => {
              if (cancelled || inFlight !== pending) return;
              inFlight = null;
              scheduleNext(result.nextExpectedAt);
            })
            .catch(() => {});
        }
      } catch (err) {
        // Capture-side failure (canvas, encoder, no video). The upload
        // path has its own handling.
        console.warn("[lookout] capture cycle failed:", err);
      }
      if (cancelled) return;

      // Provisional: one interval out, refined above when the confirm
      // lands. When the upload outlives the interval the next tick settles
      // it first, which is what keeps uploads serialized.
      scheduleNext(nextExpectedAt);
    };

    if (clipRecorder) {
      // Give the opening clip a few frames before the first cut. Fixed, not
      // a multiple of the frame interval: this delay is how long the user
      // waits for the session to activate, and the seed clip is dropped
      // from the compiled video regardless.
      intervalRef.current = setTimeout(tick, CLIP_FIRST_CUT_DELAY_MS);
    } else {
      tick();
    }

    return () => {
      capturingRef.current = false;
      cancelled = true;
      if (intervalRef.current !== null) {
        clearTimeout(intervalRef.current);
        intervalRef.current = null;
      }
      clipRecorderRef.current?.stop();
      clipRecorderRef.current = null;
    };
  }, [
    capture.isSharing,
    isActive,
    config.capture.intervalMs,
    config.capture.maxWidth,
    config.capture.maxHeight,
    config.capture.jpegQuality,
    captureMode,
    session.clipsEnabled,
    session.frameIntervalMs,
    screenCapture.getVideo,
  ]);

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
        clipRecorderRef.current?.stop();
        clipRecorderRef.current = null;
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
    if (pauseInFlightRef.current) return;
    pauseInFlightRef.current = true;
    intentionalPauseRef.current = true;
    if (intervalRef.current) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
    // Discard the in-progress clip right away — don't keep grabbing
    // frames while the pause request is in flight.
    clipRecorderRef.current?.stop();
    clipRecorderRef.current = null;
    capturingRef.current = false;
    try {
      await session.pause();
      callbacksRef.current.onPause?.({ totalActiveSeconds: session.totalActiveSeconds });
    } finally {
      pauseInFlightRef.current = false;
    }
  }, [session.pause, session.totalActiveSeconds]);

  const resume = useCallback(async () => {
    if (resumeInFlightRef.current) return;
    resumeInFlightRef.current = true;
    intentionalPauseRef.current = false;
    try {
      await session.resume();
      callbacksRef.current.onResume?.();
    } finally {
      resumeInFlightRef.current = false;
    }
  }, [session.resume]);

  const stop = useCallback(async (options?: { name?: string; edit?: boolean }) => {
    if (stopInFlightRef.current) return;
    stopInFlightRef.current = true;
    if (intervalRef.current) {
      clearInterval(intervalRef.current);
      intervalRef.current = null;
    }
    clipRecorderRef.current?.stop();
    clipRecorderRef.current = null;
    capturingRef.current = false;
    capture.stopSharing();
    try {
      await session.stop(options?.name, { edit: options?.edit });
      callbacksRef.current.onStop?.({
        trackedSeconds: session.trackedSeconds,
        totalActiveSeconds: session.totalActiveSeconds,
      });
    } finally {
      stopInFlightRef.current = false;
    }
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
