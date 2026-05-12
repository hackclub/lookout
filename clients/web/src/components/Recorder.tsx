import React, { useCallback, useEffect, useRef, useState } from "react";
import { SCREENSHOT_INTERVAL_MS } from "@lookout/shared";
import { useScreenCapture } from "../hooks/useScreenCapture.js";
import { useUploader } from "../hooks/useUploader.js";
import { StatusBar } from "./StatusBar.js";
import type { SessionStatus } from "@lookout/shared";

interface RecorderProps {
  sessionStatus: SessionStatus;
  trackedSeconds: number;
  screenshotCount: number;
  onPause: () => Promise<void>;
  onResume: () => Promise<void>;
  onStop: () => Promise<void>;
  onTrackedSecondsUpdate: (seconds: number) => void;
}

export function Recorder({
  sessionStatus,
  trackedSeconds,
  screenshotCount,
  onPause,
  onResume,
  onStop,
  onTrackedSecondsUpdate,
}: RecorderProps) {
  const { isSharing, startSharing, takeScreenshotAsync, stopSharing } =
    useScreenCapture();
  const uploader = useUploader();
  const {
    enqueueUpload,
    uploadState,
    trackedSeconds: uploadTrackedSeconds,
    lastImageUrl,
  } = uploader;
  // Holds the latest setTimeout id (self-scheduling chain following
  // server-provided nextExpectedAt) — never a setInterval.
  const intervalRef = useRef<ReturnType<typeof setTimeout>>();
  const hasStartedRef = useRef(false);
  const uploaderRef = useRef(uploader);
  uploaderRef.current = uploader;
  const [error, setError] = useState<string | null>(null);
  const isPaused = sessionStatus === "paused";
  const isActive = sessionStatus === "active" || sessionStatus === "pending";

  // Sync tracked seconds from uploader to session
  useEffect(() => {
    if (uploadTrackedSeconds > 0) {
      onTrackedSecondsUpdate(uploadTrackedSeconds);
    }
  }, [uploadTrackedSeconds, onTrackedSecondsUpdate]);

  const captureAndUpload = useCallback(async () => {
    const result = await takeScreenshotAsync();
    if (result) {
      enqueueUpload(result);
    }
  }, [takeScreenshotAsync, enqueueUpload]);

  // Capture immediately when screen sharing starts, then schedule each
  // subsequent capture from the server's `nextExpectedAt`. Falls back to
  // SCREENSHOT_INTERVAL_MS if the server didn't return one (e.g. first
  // capture or a network blip). Catch-up-on-miss: if the next target is in
  // the past, fire immediately rather than waiting another full interval.
  useEffect(() => {
    if (!isSharing || !isActive) {
      hasStartedRef.current = false;
      if (intervalRef.current) {
        clearTimeout(intervalRef.current);
        intervalRef.current = undefined;
      }
      return;
    }

    if (hasStartedRef.current) return;
    hasStartedRef.current = true;

    let cancelled = false;
    const tick = async () => {
      if (cancelled) return;
      await captureAndUpload();
      if (cancelled) return;
      const nextIso = uploaderRef.current.getNextExpectedAt();
      let delayMs: number;
      if (nextIso) {
        const parsed = Date.parse(nextIso);
        delayMs = Number.isFinite(parsed) ? parsed - Date.now() : SCREENSHOT_INTERVAL_MS;
      } else {
        delayMs = SCREENSHOT_INTERVAL_MS;
      }
      if (delayMs < 0) delayMs = 0;
      intervalRef.current = setTimeout(tick, delayMs);
    };
    tick();

    return () => {
      cancelled = true;
      if (intervalRef.current) {
        clearTimeout(intervalRef.current);
        intervalRef.current = undefined;
      }
    };
  }, [isSharing, isActive, captureAndUpload]);

  const handleStartSharing = async () => {
    setError(null);
    try {
      await startSharing();
      // If resuming from paused state, tell the server so the session
      // becomes active and the capture interval can start
      if (isPaused) {
        await onResume();
      }
    } catch (err) {
      const e = err instanceof Error ? err : new Error(String(err));
      if (e.name === "NotAllowedError") {
        setError("Screen sharing permission was denied. Please try again and select a screen to share.");
      } else if (e.name === "AbortError") {
        setError("Screen sharing was cancelled.");
      } else {
        setError(e.message || "Failed to start screen sharing.");
      }
    }
  };

  const handlePause = async () => {
    if (intervalRef.current) {
      clearTimeout(intervalRef.current);
      intervalRef.current = undefined;
    }
    hasStartedRef.current = false;
    await onPause();
  };

  const handleResume = async () => {
    await onResume();
    // useEffect will restart capture when isActive becomes true
  };

  const handleStop = async () => {
    if (intervalRef.current) {
      clearTimeout(intervalRef.current);
      intervalRef.current = undefined;
    }
    hasStartedRef.current = false;
    stopSharing();
    await onStop();
  };

  // Handle browser "Stop sharing" button ending the stream
  // Only auto-pause if the session was actually active (not pending)
  useEffect(() => {
    if (!isSharing && sessionStatus === "active" && hasStartedRef.current) {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = undefined;
      }
      hasStartedRef.current = false;
      onPause().catch(() => {});
    }
  }, [isSharing, sessionStatus, onPause]);

  return (
    <div style={styles.container}>
      {error && (
        <div style={styles.errorBanner}>
          <span>{error}</span>
          <button onClick={() => setError(null)} style={styles.dismissBtn}>
            Dismiss
          </button>
        </div>
      )}

      <StatusBar
        trackedSeconds={trackedSeconds}
        isActive={sessionStatus === "active" && isSharing}
        screenshotCount={screenshotCount}
        uploadPending={uploadState.pending}
        uploadCompleted={uploadState.completed}
        uploadFailed={uploadState.failed}
      />

      {/* Last screenshot preview */}
      {lastImageUrl && (
        <div style={styles.previewContainer}>
          <img
            src={lastImageUrl}
            alt="Last captured screenshot"
            style={styles.previewImage}
          />
          <span style={styles.previewLabel}>Latest screenshot</span>
        </div>
      )}

      <div style={styles.controls}>
        {!isSharing && isActive && (
          <button style={styles.startBtn} onClick={handleStartSharing}>
            Share Screen & Start Recording
          </button>
        )}

        {!isSharing && isPaused && (
          <>
            <button style={styles.resumeBtn} onClick={handleStartSharing}>
              Share Screen & Resume
            </button>
            <button style={styles.stopBtn} onClick={handleStop}>
              Stop Session
            </button>
          </>
        )}

        {isSharing && (sessionStatus === "active" || sessionStatus === "pending") && (
          <>
            <div style={styles.recordingDot} />
            <span style={styles.recordingText}>Recording</span>
            <button style={styles.pauseBtn} onClick={handlePause}>
              Pause
            </button>
            <button style={styles.stopBtn} onClick={handleStop}>
              Stop
            </button>
          </>
        )}
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  container: {
    maxWidth: 800,
    margin: "40px auto",
    padding: 24,
  },
  previewContainer: {
    position: "relative",
    marginBottom: 16,
    borderRadius: 8,
    overflow: "hidden",
    background: "#111",
    border: "1px solid #333",
  },
  previewImage: {
    width: "100%",
    display: "block",
  },
  previewLabel: {
    position: "absolute",
    bottom: 8,
    right: 8,
    fontSize: 12,
    color: "#aaa",
    background: "rgba(0,0,0,0.7)",
    padding: "2px 8px",
    borderRadius: 4,
  },
  controls: {
    display: "flex",
    alignItems: "center",
    gap: 12,
    justifyContent: "center",
    flexWrap: "wrap",
  },
  startBtn: {
    padding: "14px 28px",
    fontSize: 16,
    fontWeight: 600,
    background: "#22c55e",
    color: "#fff",
    border: "none",
    borderRadius: 8,
    cursor: "pointer",
  },
  pauseBtn: {
    padding: "10px 20px",
    fontSize: 14,
    fontWeight: 600,
    background: "#f59e0b",
    color: "#000",
    border: "none",
    borderRadius: 8,
    cursor: "pointer",
  },
  resumeBtn: {
    padding: "14px 28px",
    fontSize: 16,
    fontWeight: 600,
    background: "#3b82f6",
    color: "#fff",
    border: "none",
    borderRadius: 8,
    cursor: "pointer",
  },
  stopBtn: {
    padding: "10px 20px",
    fontSize: 14,
    fontWeight: 600,
    background: "#ef4444",
    color: "#fff",
    border: "none",
    borderRadius: 8,
    cursor: "pointer",
  },
  recordingDot: {
    width: 12,
    height: 12,
    borderRadius: "50%",
    background: "#ef4444",
    animation: "pulse 1.5s ease-in-out infinite",
  },
  recordingText: {
    fontSize: 14,
    fontWeight: 600,
    color: "#ef4444",
    marginRight: 8,
  },
  errorBanner: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
    padding: "12px 16px",
    marginBottom: 16,
    background: "rgba(239, 68, 68, 0.15)",
    border: "1px solid #ef4444",
    borderRadius: 8,
    color: "#fca5a5",
    fontSize: 14,
  },
  dismissBtn: {
    background: "none",
    border: "none",
    color: "#ef4444",
    cursor: "pointer",
    fontWeight: 600,
    fontSize: 13,
    flexShrink: 0,
  },
};
