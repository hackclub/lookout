import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { buildBrowserClientInfo, type SessionStatus } from "@lookout/shared";
import type { LookoutRecorder as WasmRecorder } from "@lookout/wasm";
import { useLookoutContext } from "../LookoutProvider.js";
import { useScreenCapture } from "./useScreenCapture.js";
import { useCameraCapture } from "./useCameraCapture.js";
import { useSilentAudioKeepAlive } from "./useSilentAudioKeepAlive.js";
import { createBrowserHost } from "../wasm/browserHost.js";
import { createRecorder } from "../wasm/loadRecorder.js";
import { retryTimingPatch } from "../wasm/retryTiming.js";
import {
  recorderErrorMessage,
  useRecorderClock,
  type RecorderState,
  type StopOutcome,
} from "../wasm/recorderState.js";
import type { LookoutState, LookoutActions, RecorderStatus, UploadState } from "../types.js";

// Injected at build time by tsup (see tsup.config.ts `define`).
declare const __LOOKOUT_VERSION__: string;
const SDK_VERSION = typeof __LOOKOUT_VERSION__ === "string" ? __LOOKOUT_VERSION__ : "0.0.0";

/**
 * Primary hook for Lookout integration.
 *
 * The recording itself — when to capture, how to upload, what to do when
 * the link drops, the order of a pause, the clock — is the shared Rust
 * recorder (`lookout-recorder`, running here as WebAssembly), the same
 * code the desktop app runs. This hook is the browser's view of it: it
 * hands the recorder a host made of `getDisplayMedia`/`getUserMedia`
 * frames, the WebCodecs clip encoder and `fetch`, draws the state the
 * recorder publishes, and turns the page's buttons into recorder actions.
 */
export function useLookout(): { state: LookoutState; actions: LookoutActions } {
  const { config, client } = useLookoutContext();
  const callbacksRef = useRef(config.callbacks);
  callbacksRef.current = config.callbacks;

  const captureMode = config.capture.mode;
  const screenCapture = useScreenCapture();
  const cameraCapture = useCameraCapture();
  // Both hooks are always called (rules of hooks); only the active one's
  // methods are invoked.
  const capture = captureMode === "camera" ? cameraCapture : screenCapture;

  // ── The recorder ─────────────────────────────────────────────────
  const recorderRef = useRef<WasmRecorder | null>(null);
  const [recorderState, setRecorderState] = useState<RecorderState | null>(null);
  const [tokenStatus, setTokenStatus] = useState<"resolving" | "none" | "ok">("resolving");
  const tokenRef = useRef<string | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);
  const [uploads, setUploads] = useState<UploadState>({ pending: 0, completed: 0, failed: 0 });
  const [lastScreenshotUrl, setLastScreenshotUrl] = useState<string | null>(null);
  // The session's status after the recorder is done with it: compiling,
  // then complete or failed, from the status poll below.
  const [terminalStatus, setTerminalStatus] = useState<SessionStatus | null>(null);
  const [terminalTracked, setTerminalTracked] = useState<number | null>(null);
  const [videoUrl, setVideoUrl] = useState<string | null>(null);

  const getVideoRef = useRef<() => HTMLVideoElement | null>(() => null);
  getVideoRef.current = captureMode === "camera" ? cameraCapture.getVideo : screenCapture.getVideo;
  const openCameraRef = useRef<(deviceId: string) => Promise<void>>(async () => {});
  openCameraRef.current = async () => {
    if (!cameraCapture.hasStream()) await cameraCapture.startSharing();
  };
  const closeCameraRef = useRef<() => void>(() => {});
  closeCameraRef.current = () => cameraCapture.stopSharing();

  const clientInfo = useMemo(
    () => buildBrowserClientInfo({ type: "sdk", version: SDK_VERSION, embeddedApp: config.appName }),
    [config.appName],
  );

  // Create the recorder once; it lives as long as the hook.
  useEffect(() => {
    let cancelled = false;
    let created: WasmRecorder | null = null;
    const host = createBrowserHost({
      getVideo: () => getVideoRef.current(),
      openCamera: (deviceId) => openCameraRef.current(deviceId),
      closeCamera: () => closeCameraRef.current(),
      settings: {
        maxWidth: config.capture.maxWidth,
        maxHeight: config.capture.maxHeight,
        jpegQuality: config.capture.jpegQuality,
      },
      clientInfo,
      wantsPreviewFrames: () =>
        typeof document === "undefined" || document.visibilityState !== "hidden",
      onCapture: (c) => callbacksRef.current.onCapture?.(c),
      onEvent: (name, payload) => {
        if (cancelled) return;
        switch (name) {
          case "recorder-state":
            setRecorderState((payload as RecorderState | null) ?? null);
            break;
          case "capture-preview-frame": {
            const p = payload as { previewBase64: string };
            if (p.previewBase64) setLastScreenshotUrl(`data:image/jpeg;base64,${p.previewBase64}`);
            break;
          }
          case "capture-tick-result": {
            const r = payload as { screenshotId: string; trackedSeconds: number };
            setUploads((u) => ({ ...u, completed: u.completed + 1 }));
            callbacksRef.current.onUploadSuccess?.({
              screenshotId: r.screenshotId,
              trackedSeconds: r.trackedSeconds,
            });
            break;
          }
          case "capture-tick-error": {
            const e = payload as { message: string };
            setUploads((u) => ({ ...u, failed: u.failed + 1 }));
            callbacksRef.current.onUploadFailure?.(new Error(e.message));
            break;
          }
          case "recorder-notice": {
            const n = payload as { title: string; body: string };
            console.warn(`[lookout] ${n.title}: ${n.body}`);
            break;
          }
          default:
            break;
        }
      },
    });
    (async () => {
      try {
        const recorder = await createRecorder(host);
        if (cancelled) {
          recorder.free();
          return;
        }
        created = recorder;
        recorderRef.current = recorder;
        // `retry.maxRetries` / `retry.retryDelays` predate the engine; keep
        // honouring them. Merged over the current timing, not replacing it.
        const retryPatch = retryTimingPatch(config.retry);
        if (retryPatch) recorder.setTiming({ ...recorder.timing(), ...retryPatch });
        let token: string;
        try {
          token = await client.resolveToken();
        } catch {
          token = "";
        }
        if (cancelled) return;
        if (!token) {
          setTokenStatus("none");
          return;
        }
        tokenRef.current = token;
        setTokenStatus("ok");
        const source =
          captureMode === "camera"
            ? { type: "camera", deviceId: cameraCapture.selectedDeviceId ?? config.capture.camera.deviceId ?? "" }
            : { type: "screen", sources: [] };
        const state = (await recorder.open(token, config.apiBaseUrl, source)) as RecorderState;
        if (cancelled) return;
        setRecorderState(state);
        // The recorder starts capturing on an active session, but a page
        // has no stream until the user shares one: park the session
        // paused (flushing nothing) so the server doesn't accrue dead
        // time, exactly as the page always did on reload.
        if (state.phase === "recording") {
          try {
            await recorder.pause();
          } catch {
            // Refused or failed: the state event says what happened.
          }
        }
      } catch (err) {
        if (!cancelled) {
          const message = recorderErrorMessage(err);
          callbacksRef.current.onError?.(new Error(message), "open");
          setLocalError(message);
        }
      }
    })();
    return () => {
      cancelled = true;
      recorderRef.current = null;
      if (created) {
        // Flush and release; the page is leaving. Best effort.
        void created
          .close()
          .catch(() => {})
          .finally(() => created?.free());
      }
    };
    // The recorder is bound to the provider's config for its lifetime.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, clientInfo, captureMode]);

  const phase = recorderState?.phase ?? null;
  const sessionStatus = (recorderState?.sessionStatus ?? null) as SessionStatus | null;

  // ── Pending uploads from the recorder's own count ────────────────
  useEffect(() => {
    const pending = recorderState?.pendingUploads ?? 0;
    setUploads((u) => (u.pending === pending ? u : { ...u, pending }));
  }, [recorderState?.pendingUploads]);

  // ── Status: the recorder's, then the compile poll's ──────────────
  const status: RecorderStatus = useMemo(() => {
    if (localError) return "error";
    if (tokenStatus === "none") return "no-token";
    if (tokenStatus === "resolving" || !recorderState) return "loading";
    if (phase === "loadError") return "error";
    if (phase === "loading") return "loading";
    if (terminalStatus) return terminalStatus;
    return sessionStatus ?? "loading";
  }, [localError, tokenStatus, recorderState, phase, sessionStatus, terminalStatus]);

  const error =
    localError ?? (phase === "loadError" ? recorderState?.sessionError ?? "Failed to load session" : null);

  // Status transition callbacks.
  const prevStatusRef = useRef<RecorderStatus>(status);
  useEffect(() => {
    const prev = prevStatusRef.current;
    if (prev !== status) {
      callbacksRef.current.onStatusChange?.(prev, status);
      if (status === "failed") callbacksRef.current.onCompilationFailed?.();
      prevStatusRef.current = status;
    }
  }, [status]);

  // Poll the compile once the session is stopped, until it is complete
  // or failed; then fetch the video.
  const pollable = status === "stopped" || status === "compiling";
  useEffect(() => {
    if (!pollable) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const data = await client.getStatus();
        if (cancelled) return;
        setTerminalStatus(data.status);
        setTerminalTracked(data.trackedSeconds);
      } catch (e) {
        console.warn("[session] poll error:", e);
      }
    };
    const id = setInterval(tick, config.statusPollIntervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [pollable, client, config.statusPollIntervalMs]);

  useEffect(() => {
    if (status !== "complete") return;
    let cancelled = false;
    client
      .getVideo()
      .then((data: { videoUrl: string }) => {
        if (cancelled) return;
        setVideoUrl(data.videoUrl);
        callbacksRef.current.onComplete?.({ videoUrl: data.videoUrl });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [status, client]);

  // ── Sharing ↔ recording ──────────────────────────────────────────
  const isActive = sessionStatus === "active" || sessionStatus === "pending";

  // Keep the page "audible" while recording so the browser doesn't
  // throttle timers when the user backgrounds the tab. The recorder's
  // loop runs on the page's event loop, so this matters here.
  useSilentAudioKeepAlive(capture.isSharing && isActive);

  // The stream ended while recording (the user hit the browser's "Stop
  // sharing", the camera was unplugged): pause, which flushes the last
  // frame and parks the session so it doesn't accrue dead time.
  const wasSharingRef = useRef(capture.isSharing);
  useEffect(() => {
    const was = wasSharingRef.current;
    wasSharingRef.current = capture.isSharing;
    if (was && !capture.isSharing && phase === "recording") {
      recorderRef.current?.pause().catch(() => {});
      callbacksRef.current.onShareStop?.();
    }
  }, [capture.isSharing, phase]);

  // Sharing started while the session is paused ("Share screen & resume"
  // after a reload, or the camera coming up): resume.
  const intentionalPauseRef = useRef(false);
  useEffect(() => {
    if (capture.isSharing && phase === "paused" && !intentionalPauseRef.current) {
      recorderRef.current
        ?.resume()
        .then(() => callbacksRef.current.onResume?.())
        .catch(() => {});
    }
  }, [capture.isSharing, phase]);

  // Auto-start
  useEffect(() => {
    if (config.autoStart && !capture.isSharing && isActive) {
      capture.startSharing().catch(() => {});
    }
  }, [config.autoStart, isActive, capture.isSharing, capture.startSharing]);

  // ── Actions ──────────────────────────────────────────────────────
  const startSharing = useCallback(async () => {
    try {
      intentionalPauseRef.current = false;
      await capture.startSharing();
      callbacksRef.current.onShareStart?.();
      // The resume is the effect above, on isSharing turning true.
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
      setLocalError(message);
    }
  }, [capture, captureMode]);

  const stopSharing = useCallback(() => {
    capture.stopSharing();
    // The pause is the effect above, on isSharing turning false.
  }, [capture]);

  const pause = useCallback(async () => {
    const recorder = recorderRef.current;
    if (!recorder) return;
    intentionalPauseRef.current = true;
    try {
      const state = (await recorder.pause()) as RecorderState;
      callbacksRef.current.onPause?.({ totalActiveSeconds: state.trackedSeconds });
    } catch (err) {
      // A refused action (already pausing, nothing open) is not an error
      // for the page; a failed one is reported by the state's actionError.
      console.warn("[lookout] pause:", recorderErrorMessage(err));
    }
  }, []);

  const resume = useCallback(async () => {
    const recorder = recorderRef.current;
    if (!recorder) return;
    intentionalPauseRef.current = false;
    try {
      await recorder.resume();
      callbacksRef.current.onResume?.();
    } catch (err) {
      console.warn("[lookout] resume:", recorderErrorMessage(err));
    }
  }, []);

  const stop = useCallback(
    async (options?: { name?: string; edit?: boolean }) => {
      const recorder = recorderRef.current;
      if (!recorder) return;
      intentionalPauseRef.current = true;
      // Stop = pause (flushing the partial minute first) into the prompt,
      // then finish: rename, stop with retries, done. Both are the
      // recorder's; this just chains them for a page that has already
      // asked its own question.
      const current = recorder.state() as RecorderState | undefined;
      if (current && current.phase !== "stopPrompting") {
        await recorder.beginStop();
      }
      const outcome = (await recorder.finishStop(options?.name ?? null, options?.edit ?? false)) as StopOutcome;
      capture.stopSharing();
      const after = recorder.state() as RecorderState | undefined;
      setTerminalStatus(outcome.status as SessionStatus);
      callbacksRef.current.onStop?.({
        trackedSeconds: after?.trackedSeconds ?? 0,
        totalActiveSeconds: after?.trackedSeconds ?? 0,
      });
    },
    [capture],
  );

  // ── State ────────────────────────────────────────────────────────
  const displaySeconds = useRecorderClock(recorderState?.clock ?? null);
  const trackedSeconds = Math.max(recorderState?.trackedSeconds ?? 0, terminalTracked ?? 0);
  const isRecording = capture.isSharing && isActive;

  const state: LookoutState = {
    status,
    isSharing: capture.isSharing,
    isRecording,
    trackedSeconds,
    displaySeconds: isActive ? displaySeconds : trackedSeconds,
    screenshotCount: recorderState?.captureCount ?? 0,
    uploads,
    lastScreenshotUrl,
    videoUrl,
    error,
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
