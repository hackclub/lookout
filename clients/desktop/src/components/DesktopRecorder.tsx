import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "../logger.js";
import { listen, emit } from "@tauri-apps/api/event";
import {
  useSession,
  useSessionTimer,
  formatTime,
  Button,
  ErrorDisplay,
  PageContainer,
  Spinner,
  Skeleton,
  colors,
  spacing,
  fontSize,
  fontWeight,
  radii,
} from "@lookout/react";
import { getReport } from "../logger.js";
import { NamingModal } from "./NamingModal.js";
import { useNativeCapture } from "../hooks/useNativeCapture.js";
import type { CaptureSource } from "../hooks/useNativeCapture.js";
import { useScreenPreview } from "../hooks/useScreenPreview.js";
import { useCameraCapture, waitForVideoReady } from "../hooks/useCameraCapture.js";
import { useSessionNotifications } from "../hooks/useSessionNotifications.js";

interface DesktopRecorderProps {
  token: string;
  source: CaptureSource[];
  onChangeSource: () => void;
  onBack: () => void;
  onViewSession: (token: string) => void;
}

const API_BASE = "https://lookout.hackclub.com";

function RecorderPreviewItem({ 
  src, 
  isMain, 
  captureUrl, 
  isMulti 
}: { 
  src: CaptureSource; 
  isMain: boolean; 
  captureUrl: string | null; 
  isMulti: boolean;
}) {
  const { previewUrl: livePreviewUrl } = useScreenPreview(
    isMain && captureUrl ? null : src,
    1
  );
  const previewUrl = (isMain ? captureUrl : null) || livePreviewUrl;

  if (!previewUrl) {
    return (
      <div style={{
        flex: 1, minHeight: 0, minWidth: 0,
        borderRadius: radii.lg, overflow: "hidden", background: colors.bg.sunken,
        border: `1px solid ${colors.border.default}`, aspectRatio: isMulti ? undefined : "16/9",
        display: "flex", alignItems: "center", justifyContent: "center",
      }}>
        <Spinner size="sm" />
      </div>
    );
  }

  return (
    <div style={{
      position: "relative", borderRadius: radii.lg,
      overflow: "hidden", background: colors.bg.sunken, border: `1px solid ${colors.border.default}`,
      flex: 1, minHeight: 0, minWidth: 0,
    }}>
      <img
        src={previewUrl}
        alt="Screen preview"
        style={{ width: "100%", height: "100%", objectFit: isMulti ? "cover" : "contain", display: "block" }}
      />
      {isMain && (
        <span style={{
          position: "absolute", bottom: 6, right: 6, fontSize: fontSize.xs,
          color: colors.badge.overlayText, background: colors.badge.overlayBg,
          padding: "2px 6px", borderRadius: radii.sm,
        }}>
          {captureUrl ? "Latest capture" : "Live preview"}
        </span>
      )}
    </div>
  );
}

function formatTimeTray(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  
  if (h > 0) return `${h}h ${m}m`;
  if (m === 0) return `< 1m`;
  return `${m}m`;
}

export function DesktopRecorder({ token, source, onChangeSource: _onChangeSource, onBack, onViewSession }: DesktopRecorderProps) {
  const isMacOS = navigator.userAgent.includes("Mac");
  const isCamera = source.length === 1 && source[0].type === "camera";
  const session = useSession();
  const camera = useCameraCapture();

  // Wrap camera.captureFrame to pass the rendered video element
  const cameraFrameCapture = useCallback(async () => {
    return camera.captureFrame(cameraVideoRef.current);
  }, [camera]);

  // When the capture loop discovers the server stopped the session (e.g. cron
  // auto-stop), sync the session hook so the UI navigates correctly.
  const handleSessionTerminated = useCallback((status: string) => {
    console.warn(`[session] capture loop detected terminal status: ${status}`);
    session.syncStatus();
  }, [session]);

  // Pass camera captureFrame to the native capture hook for camera sources
  const capture = useNativeCapture(
    token,
    API_BASE,
    source,
    isCamera ? cameraFrameCapture : undefined,
    handleSessionTerminated,
  );

  // Native OS notifications: alert the user when a session pauses, errors,
  // or fails — useful when the app is in the background.
  useSessionNotifications({
    isCapturing: capture.isCapturing,
    captureError: capture.error,
    status: session.status,
  });


  // Stale capture detection — warn if no successful capture for 3+ minutes
  // while the app is visible (ignore sleep/background since that's expected).
  const STALE_CAPTURE_MS = 3 * 60_000;
  const [captureStale, setCaptureStale] = useState(false);
  useEffect(() => {
    if (!capture.isCapturing || !capture.lastCaptureAt) {
      setCaptureStale(false);
      return;
    }
    const check = () => {
      if (document.visibilityState !== "visible") return;
      const age = Date.now() - capture.lastCaptureAt!;
      setCaptureStale(age > STALE_CAPTURE_MS);
    };
    check();
    const id = setInterval(check, 10_000);
    const onVisChange = () => check();
    document.addEventListener("visibilitychange", onVisChange);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVisChange);
    };
  }, [capture.isCapturing, capture.lastCaptureAt]);

  const displaySeconds = useSessionTimer(
    capture.trackedSeconds || session.trackedSeconds,
    capture.isCapturing,
  );

  const [pauseLoading, setPauseLoading] = useState(false);
  const [resumeLoading, setResumeLoading] = useState(false);
  const [stopLoading, setStopLoading] = useState(false);
  const [isPrompting, setIsPrompting] = useState(false);
  const stopActionHandled = useRef(false);
  const autoStarted = useRef(false);

  // Camera video ref for live preview during recording
  const cameraVideoRef = useRef<HTMLVideoElement>(null);

  // Attach camera stream to video element for live preview
  useEffect(() => {
    if (cameraVideoRef.current && camera.stream) {
      cameraVideoRef.current.srcObject = camera.stream;
    }
  }, [camera.stream]);

  // Camera device ID (only meaningful when isCamera is true)
  const cameraDeviceId = isCamera ? String(source[0].id) : "";

  // Start camera stream, attach to video element, and wait for decoded dimensions.
  // Must only be called when the <video> element is in the DOM (session not "loading").
  const startCameraAndWait = useCallback(async () => {
    await new Promise((r) => requestAnimationFrame(r));
    const stream = await camera.startStream(cameraDeviceId);
    if (cameraVideoRef.current && stream) {
      cameraVideoRef.current.srcObject = stream;
      try { await cameraVideoRef.current.play(); } catch {}
      await waitForVideoReady(cameraVideoRef.current);
      console.log(`[session] camera video ready: ${cameraVideoRef.current?.videoWidth}x${cameraVideoRef.current?.videoHeight}`);
    }
  }, [camera, cameraDeviceId]);

  // Auto-start recording (and camera stream) when component mounts and session is ready.
  // For camera sources, we must wait until cameraVideoRef is in the DOM (i.e. session
  // is no longer "loading") before starting the stream, otherwise the <video> element
  // won't exist for captureFrame to draw from.
  useEffect(() => {
    if (autoStarted.current) return;
    if (session.status === "loading" || session.status === "error") return;
    const isSessionActive = session.status === "active" || session.status === "pending";
    if (!isSessionActive || capture.isCapturing) return;

    autoStarted.current = true;
    (async () => {
      if (isCamera) {
        await startCameraAndWait();
        // Camera sources use JS-side capture loop, so we need to
        // explicitly start the Rust tray ticker for the menu bar time.
        invoke("start_tray_ticker", { trackedSeconds: 0 }).catch(console.error);
      }
      capture.startCapturing();
    })();
  }, [session.status, capture.isCapturing, capture, isCamera, cameraDeviceId, camera]);

  // Navigate to session detail when terminal state is reached
  useEffect(() => {
    if (["stopped", "compiling", "complete", "failed"].includes(session.status) && !isPrompting && !stopLoading) {
      onViewSession(token);
    }
  }, [session.status, token, onViewSession, isPrompting, stopLoading]);

  // Cleanup camera stream on unmount — use refs to avoid stale closures
  const isCameraRef = useRef(isCamera);
  isCameraRef.current = isCamera;
  const cameraRef = useRef(camera);
  cameraRef.current = camera;
  useEffect(() => {
    return () => {
      if (isCameraRef.current) {
        cameraRef.current.stopStream();
      }
    };
  }, []);

  // Finalize stop: optionally name, then stop the session.
  const handleConfirmStop = useCallback(async (name: string | null) => {
    if (stopActionHandled.current) return;
    stopActionHandled.current = true;
    setStopLoading(true);
    console.log(`[session] stopping, name: ${name?.trim() || "(none)"}`);
    if (name && name.trim()) {
      try {
        await fetch(`${API_BASE}/api/sessions/${token}/name`, {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ name: name.trim() }),
        });
      } catch (e) {
        console.warn("[session] rename failed (non-fatal):", e);
      }
    }

    try {
      await session.stop();
      console.log("[session] stopped, navigating to session detail");
      if (isCamera) camera.stopStream();
      setIsPrompting(false);
      setStopLoading(false);
    } catch (e) {
      console.error("[session] failed to stop session", e);
      setStopLoading(false);
      stopActionHandled.current = false;
    }
  }, [token, session, isCamera, camera]);

  const handlePause = useCallback(async () => {
    console.log("[session] pausing...");
    setPauseLoading(true);
    capture.stopCapturing();
    invoke("pause_tray_ticker").catch(console.error);
    if (isCamera) camera.stopStream();
    await session.pause();
    console.log("[session] paused");
    setPauseLoading(false);
  }, [capture, session, isCamera, camera]);

  const handleResume = useCallback(async () => {
    console.log("[session] resuming...");
    setResumeLoading(true);
    await session.resume();
    if (isCamera) {
      console.log(`[session] restarting camera stream for device ${cameraDeviceId}`);
      await startCameraAndWait();
    }
    invoke("resume_tray_ticker", { trackedSeconds: capture.trackedSeconds }).catch(console.error);
    await capture.startCapturing();
    console.log("[session] resumed");
    setResumeLoading(false);
  }, [capture, session, isCamera, cameraDeviceId, camera, startCameraAndWait]);

  // Stop button: pause session + stop capture + show naming modal
  const handleStopClick = useCallback(async () => {
    console.log("[session] stop clicked, pausing and opening naming modal");
    capture.stopCapturing();
    invoke("pause_tray_ticker").catch(console.error);
    if (isCamera) camera.stopStream();
    await session.pause();
    setIsPrompting(true);
  }, [capture, session, isCamera, camera]);

  // Resume from naming modal: close modal, resume recording
  const handleResumeFromModal = useCallback(async () => {
    console.log("[session] resume from modal, closing and resuming");
    setIsPrompting(false);
    setResumeLoading(true);
    await session.resume();
    if (isCamera) {
      await startCameraAndWait();
    }
    invoke("resume_tray_ticker", { trackedSeconds: capture.trackedSeconds }).catch(console.error);
    await capture.startCapturing();
    setResumeLoading(false);
  }, [capture, session, isCamera, cameraDeviceId, camera, startCameraAndWait]);

  const isActive = session.status === "active" || session.status === "pending";
  const isPaused = session.status === "paused";

  // Pin controls to pre-action state during transitions to prevent flashes.
  let controlMode: "recording" | "paused";
  if (pauseLoading || ((stopLoading || isPrompting) && isActive)) {
    controlMode = "recording";
  } else if (resumeLoading || ((stopLoading || isPrompting) && isPaused)) {
    controlMode = "paused";
  } else if (capture.isCapturing) {
    controlMode = "recording";
  } else if (isPaused) {
    controlMode = "paused";
  } else {
    controlMode = "recording";
  }

  const screenshotCount = session.screenshotCount + capture.screenshotCount;

  // Keep a ref of the latest state
  const trayStateRef = useRef({ displaySeconds, screenshotCount, controlMode });
  trayStateRef.current = { displaySeconds, screenshotCount, controlMode };

  // Listen for tray requesting initial state (fallback)
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen("tray-ready", () => {
      emit("tray-state", trayStateRef.current).catch(console.error);
    }).then(fn => unlisten = fn);
    return () => unlisten?.();
  }, []);

  // Sync menu-bar state to Rust + the tray window.
  //
  // IMPORTANT: do NOT call `update_tray_time` on every displaySeconds change.
  // The Rust tray ticker is the authoritative source for the menu-bar title
  // and runs at 1s cadence; calling update_tray_time from JS every second
  // creates two writers fighting over the title, which produces the visible
  // flicker between "<1m"/"1m" and "1m"/"2m" at the minute boundaries
  // (JS interpolates to one second, Rust to another).
  //
  // We still split the writes:
  //   - `update_tray_time`: pause/resume only (the icon/title needs to
  //     change immediately when paused, which the Rust ticker won't do
  //     within its 1s tick window).
  //   - `set_tray_state` + `tray-state` event: tray-window UI updates
  //     fine-grained per second.

  // controlMode + initial-show effect (low frequency)
  useEffect(() => {
    invoke("show_tray", { timeText: formatTimeTray(displaySeconds) }).catch(console.error);
    invoke("update_tray_time", {
      timeText: formatTimeTray(displaySeconds),
      isPaused: controlMode === "paused",
    }).catch(console.error);
    // (displaySeconds intentionally read-only here — not in deps. We just
    // need a sensible initial title; the Rust ticker takes over from there.)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [controlMode]);

  // Per-second state sync (tray window, not menu-bar title)
  useEffect(() => {
    const state = {
      displaySeconds,
      screenshotCount,
      controlMode,
      updatedAt: Date.now(),
    };
    invoke("set_tray_state", { state }).catch(console.error);
    emit("tray-state", state).catch(console.error);

    // Sync tracked seconds to Rust tray timer so it stays accurate
    // after server corrections or session timer updates.
    if (capture.trackedSeconds > 0) {
      invoke("sync_tray_tracked_seconds", { trackedSeconds: capture.trackedSeconds }).catch(console.error);
    }
  }, [displaySeconds, screenshotCount, controlMode]);

  // Hide tray on unmount or session end
  useEffect(() => {
    return () => {
      invoke("hide_tray").catch(console.error);
      invoke("stop_tray_ticker").catch(console.error);
    };
  }, []);

  // Listen to tray actions
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<string>("tray-action", (event) => {
      if (event.payload === "pause") {
        handlePause();
      } else if (event.payload === "resume") {
        handleResume();
      } else if (event.payload === "stop") {
        handleStopClick();
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, [handlePause, handleResume, handleStopClick]);

  // Loading/skeleton state
  if (session.status === "loading") {
    return (
      <div style={{
        display: "flex", flexDirection: "column", height: "100%",
        maxWidth: 480, margin: "0 auto", padding: spacing.lg, boxSizing: "border-box",
      }}>
        <div style={{ flexShrink: 0, marginBottom: spacing.lg }}>
          <Skeleton height={56} borderRadius={radii.md} />
        </div>
        <div style={{ flex: 1 }}>
          <Skeleton aspectRatio="16/9" borderRadius={radii.lg} />
        </div>
        <div style={{ flexShrink: 0, display: "flex", gap: spacing.md, marginTop: spacing.lg }}>
          <Skeleton height={48} borderRadius={radii.lg} style={{ flex: 1 }} />
          <Skeleton height={48} borderRadius={radii.lg} style={{ flex: 1 }} />
        </div>
      </div>
    );
  }

  if (session.status === "error") {
    return (
      <PageContainer centered>
        <ErrorDisplay
          variant="page"
          title="Session Error"
          error={session.error || "Unknown error"}
          action={{ label: isMacOS ? "\u2190 Gallery" : "Back to Gallery", onClick: onBack }}
        />
      </PageContainer>
    );
  }

  // Terminal states — show spinner while navigation happens
  if (["stopped", "compiling", "complete", "failed"].includes(session.status)) {
    return (
      <PageContainer centered>
        <Spinner size="md" />
      </PageContainer>
    );
  }

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100%",
      maxWidth: 480, margin: "0 auto", padding: spacing.lg, boxSizing: "border-box",
    }}>
      {/* Status card — recording indicator + timer + screenshot count */}
      <div style={{
        flexShrink: 0,
        display: "flex", alignItems: "center", justifyContent: "space-between",
        padding: `${spacing.md}px ${spacing.xl}px`,
        background: colors.bg.surface, borderRadius: radii.md,
        border: `1px solid ${colors.border.default}`,
        marginBottom: spacing.lg,
      }}>
        <div style={{ display: "flex", alignItems: "center", gap: spacing.sm }}>
          {controlMode === "recording" && (
            <div style={{
              width: 10, height: 10, borderRadius: "50%", background: colors.status.danger,
              animation: "pulse 1.5s ease-in-out infinite", flexShrink: 0,
            }} />
          )}
          {controlMode === "paused" && (
            <span style={{ color: colors.text.tertiary, flexShrink: 0, lineHeight: 1, display: "inline-flex", alignItems: "center" }}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                <rect x="7" y="5" width="4" height="14" rx="1" />
                <rect x="13" y="5" width="4" height="14" rx="1" />
              </svg>
            </span>
          )}
          <span style={{
            fontSize: fontSize.timer,
            fontWeight: fontWeight.bold,
            fontVariantNumeric: "tabular-nums",
            color: colors.text.primary,
          }}>
            {formatTime(displaySeconds)}
          </span>
        </div>
        <span style={{ fontSize: fontSize.md, color: colors.text.secondary }}>
          {screenshotCount} {screenshotCount === 1 ? "screenshot" : "screenshots"}
        </span>
      </div>

      {/* Preview — fills available space */}
      <div style={{
        flex: 1,
        minHeight: 0,
        display: "flex",
        flexDirection: isCamera ? "column" : "row",
        gap: !isCamera && source.length > 1 && !capture.lastScreenshotUrl ? spacing.xs : 0,
        marginBottom: spacing.lg
      }}>
        {/* Hidden video element for camera sources — needed for captureFrame to draw from canvas */}
        {isCamera && (
          <video
            ref={cameraVideoRef}
            autoPlay
            muted
            playsInline
            style={{ position: "absolute", width: 0, height: 0, opacity: 0, pointerEvents: "none" }}
          />
        )}
        {capture.lastScreenshotUrl ? (
          <div style={{
            position: "relative", borderRadius: radii.lg,
            overflow: "hidden", background: colors.bg.sunken, border: `1px solid ${colors.border.default}`,
            flex: 1, minHeight: 0, minWidth: 0,
          }}>
            <img
              src={capture.lastScreenshotUrl}
              alt="Screen preview"
              style={{ width: "100%", height: "100%", objectFit: "contain", display: "block" }}
            />
            <span style={{
              position: "absolute", bottom: 6, right: 6, fontSize: fontSize.xs,
              color: colors.badge.overlayText, background: colors.badge.overlayBg,
              padding: "2px 6px", borderRadius: radii.sm,
            }}>
              Latest capture
            </span>
          </div>
        ) : (
          source.map((src) => (
            <RecorderPreviewItem
              key={`${src.type}:${src.id}`}
              src={src}
              isMain={false}
              captureUrl={null}
              isMulti={source.length > 1}
            />
          ))
        )}
      </div>

      {captureStale && !capture.error && (
        <div style={{
          padding: `${spacing.sm}px ${spacing.md}px`,
          background: colors.status.warning + "1a",
          border: `1px solid ${colors.status.warning}`,
          borderRadius: radii.md,
          marginBottom: spacing.md,
          fontSize: fontSize.sm,
          color: colors.status.warning,
        }}>
          Screenshots haven't been captured in a while. Your recording may not be saving.
        </div>
      )}

      {capture.error && (
        <ErrorDisplay variant="banner" error={capture.error} onCopy={() => navigator.clipboard.writeText(getReport())} />
      )}

      {/* Buttons — half-and-half at bottom */}
      <div style={{ flexShrink: 0, display: "flex", gap: spacing.md }}>
        {controlMode === "recording" && (
          <>
            <Button
              variant="warning"
              size="lg"
              loading={pauseLoading}
              onClick={handlePause}
              disabled={stopLoading || isPrompting}
              fullWidth
              style={{ flex: 1 }}
            >
              Pause
            </Button>
            <Button
              variant="danger"
              size="lg"
              loading={stopLoading}
              onClick={handleStopClick}
              disabled={pauseLoading || stopLoading || isPrompting}
              fullWidth
              style={{ flex: 1 }}
            >
              Stop
            </Button>
          </>
        )}

        {controlMode === "paused" && (
          <>
            <Button
              variant="success"
              size="lg"
              loading={resumeLoading}
              onClick={handleResume}
              disabled={stopLoading || isPrompting}
              fullWidth
              style={{ flex: 1 }}
            >
              Resume
            </Button>
            <Button
              variant="danger"
              size="lg"
              loading={stopLoading}
              onClick={handleStopClick}
              disabled={resumeLoading || stopLoading || isPrompting}
              fullWidth
              style={{ flex: 1 }}
            >
              Stop
            </Button>
          </>
        )}
      </div>

      {isPrompting && (
        <NamingModal
          loading={stopLoading}
          onConfirm={handleConfirmStop}
          onResume={handleResumeFromModal}
        />
      )}
    </div>
  );
}
