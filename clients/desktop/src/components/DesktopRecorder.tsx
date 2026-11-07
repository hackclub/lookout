import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import {
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
import { CaptureErrorDialog } from "./CaptureErrorDialog.js";
import { openEditorWindow } from "./EditorWindow.js";
import { SourceLostPrompt } from "./SourceLostPrompt.js";
import { useScreenPreview } from "../hooks/useScreenPreview.js";
import { useWindowFocus } from "../hooks/useWindowFocus.js";
import { useCameraBackend } from "../hooks/useCameraBackend.js";
import { useRecorderNotices } from "../hooks/useRecorderNotices.js";
import { setCaptureActive } from "../captureGuard.js";
import {
  isPrompting,
  recorder,
  recorderErrorMessage,
  useRecorderClock,
  useRecorderState,
  type CaptureSource,
} from "../recorder.js";
import { getApiBase } from "../serverConfig.js";

interface DesktopRecorderProps {
  token: string;
  source: CaptureSource[];
  onChangeSource: () => void;
  onBack: () => void;
  onViewSession: (token: string) => void;
}

// Read once per webview load; Settings → Server reloads the view on change.
const API_BASE = getApiBase();

function RecorderPreviewItem({
  src,
  captureUrl,
  isMulti,
  live,
}: {
  src: CaptureSource;
  /** Latest uploaded capture, shown when the live loop is parked. */
  captureUrl: string | null;
  isMulti: boolean;
  /** Poll the native live preview. When false, useScreenPreview fetches at
   *  most one frame and parks — the tile freezes (or shows captureUrl)
   *  instead of burning a native capture per second off-focus. */
  live: boolean;
}) {
  const { previewUrl: livePreviewUrl } = useScreenPreview(src, 1, live);
  const showingLive = live && !!livePreviewUrl;
  const previewUrl = showingLive ? livePreviewUrl : captureUrl ?? livePreviewUrl;

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
      {!isMulti && (
        <span style={{
          position: "absolute", bottom: 6, right: 6, fontSize: fontSize.xs,
          color: colors.badge.overlayText, background: colors.badge.overlayBg,
          padding: "2px 6px", borderRadius: radii.sm,
        }}>
          {showingLive ? "Preview" : captureUrl ? "Latest capture" : "Preview"}
        </span>
      )}
    </div>
  );
}

/**
 * The recording screen. A view over the core's `RecorderState`: the
 * recording itself — the capture loop, the session's status, the clock, the
 * order of operations behind every button — lives in Rust and is shared
 * with the tray popup, the menu bar and any other shell. This component
 * opens the recording on mount, closes it on unmount, and relays clicks.
 */
export function DesktopRecorder({ token, source, onChangeSource, onBack, onViewSession }: DesktopRecorderProps) {
  const isMacOS = navigator.userAgent.includes("Mac");
  const isCamera = source.length === 1 && source[0].type === "camera";
  const windowFocused = useWindowFocus();

  // The camera's frames come from this webview; the loop that asks for them
  // runs in the core. Listeners must be up before the recording opens.
  const camera = useCameraBackend(isCamera);
  const { state: anyState, ready } = useRecorderState();
  // A state from a recording being replaced (the previous source, a previous
  // token) is not ours to draw.
  const state = anyState && anyState.token === token ? anyState : null;

  const sourceKey = source.map((s) => `${s.type}:${s.id}`).join(",");
  const sourceRef = useRef(source);
  sourceRef.current = source;

  // Open on mount, close on unmount. Closing flushes the partial minute and
  // leaves the session itself alone (a source change drops straight back
  // into recording on the next pick).
  useEffect(() => {
    if (!ready || !camera.ready) return;
    recorder.open(token, API_BASE, sourceRef.current).catch((e) => {
      console.error("[recorder] open failed:", recorderErrorMessage(e));
    });
    return () => {
      recorder.close().catch(console.error);
    };
  }, [token, sourceKey, ready, camera.ready]);

  // Native OS notifications for pauses, capture failures and lost sources:
  // the core decides what's worth one.
  useRecorderNotices(state?.capturing ?? false);

  // Publish capture state for the updater, which must not install (and so
  // exit the process) while a session is recording. See captureGuard.
  useEffect(() => {
    setCaptureActive(state?.capturing ?? false);
    return () => setCaptureActive(false);
  }, [state?.capturing]);

  // The live preview. The core sends the latest frame — in-between frames
  // at the clip cadence while the window is focused, and every uploaded
  // capture — on one channel; the same element is a live preview when
  // watched and the latest capture when not.
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const blobUrlRef = useRef<string | null>(null);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen<{ previewBase64: string; previewWidth: number; previewHeight: number }>(
      "capture-preview-frame",
      (event) => {
        const { previewBase64 } = event.payload;
        if (!previewBase64) return;
        const bytes = Uint8Array.from(atob(previewBase64), (c) => c.charCodeAt(0));
        const url = URL.createObjectURL(new Blob([bytes], { type: "image/jpeg" }));
        if (blobUrlRef.current) URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = url;
        setPreviewUrl(url);
      },
    ).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
      if (blobUrlRef.current) {
        URL.revokeObjectURL(blobUrlRef.current);
        blobUrlRef.current = null;
      }
    };
  }, []);

  // Navigate to the session view once the recording is over.
  useEffect(() => {
    if (state?.phase === "finished") onViewSession(token);
  }, [state?.phase, token, onViewSession]);

  const displaySeconds = useRecorderClock(state?.clock ?? null);

  /** Capture error the user asked to see explained. The banner stays put —
   *  a recording in progress shouldn't be buried under a modal unless the
   *  user opens one. */
  const [captureErrorDetail, setCaptureErrorDetail] = useState<string | null>(null);

  /** Back to the source picker, closing whatever is open first.
   *
   *  The session is deliberately left alone — still open, still holding its
   *  tracked time — so the next pick drops straight back into recording
   *  instead of starting a second session. `onChangeSource` hands the screen
   *  back to the compositor on the way out. */
  const reselectSource = useCallback(() => {
    console.log("[session] source lost — sending the user back to the picker");
    setCaptureErrorDetail(null);
    onChangeSource();
  }, [onChangeSource]);

  const logFailure = (what: string) => (e: unknown) =>
    console.error(`[recorder] ${what} failed: ${recorderErrorMessage(e)}`);

  const handlePause = useCallback(() => {
    recorder.pause().catch(logFailure("pause"));
  }, []);

  const handleResume = useCallback(() => {
    recorder.resume().catch(logFailure("resume"));
  }, []);

  const handleStopClick = useCallback(() => {
    recorder.beginStop().catch(logFailure("stop"));
  }, []);

  // "Keep recording" from the prompt. Reached from the source-lost prompt
  // too: there is no stream to resume onto, so it means "give me a source",
  // not "start the loop again on a dead node".
  const handleResumeFromModal = useCallback(() => {
    if (state?.sourceLost) {
      reselectSource();
      return;
    }
    recorder.cancelStop().catch(logFailure("keep recording"));
  }, [state?.sourceLost, reselectSource]);

  // Finalize stop: optionally name, then stop the session. With `edit`, the
  // timelapse is held unpublished after compiling so the user can cut it
  // first — programs only ever observe it once, finished.
  const finalizeStop = useCallback(async (name: string | null, edit: boolean) => {
    try {
      const outcome = await recorder.finishStop(name, edit);
      console.log(`[session] stopped (${outcome.status})`);
      if (outcome.edit) void openEditorWindow(token);
    } catch (e) {
      logFailure("finish stop")(e);
    }
  }, [token]);

  const handleConfirmStop = useCallback(
    (name: string | null) => finalizeStop(name, false),
    [finalizeStop],
  );

  const handleEditAndSave = useCallback(
    (name: string | null) => finalizeStop(name, true),
    [finalizeStop],
  );

  // The hidden video element for camera sources. In the DOM for the whole
  // life of the recording — the core's start request arrives while the
  // session is still being fetched.
  const hiddenVideo = isCamera ? (
    <video
      ref={camera.videoRef}
      autoPlay
      muted
      playsInline
      style={{ position: "absolute", width: 0, height: 0, opacity: 0, pointerEvents: "none" }}
    />
  ) : null;

  // Loading/skeleton state
  if (!state || state.phase === "loading") {
    return (
      <div style={{
        display: "flex", flexDirection: "column", height: "100%",
        maxWidth: 480, margin: "0 auto", padding: spacing.lg, boxSizing: "border-box",
      }}>
        {hiddenVideo}
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

  if (state.phase === "loadError") {
    return (
      <PageContainer centered>
        {hiddenVideo}
        <ErrorDisplay
          variant="page"
          title="Session Error"
          error={state.sessionError || "Unknown error"}
          action={{ label: isMacOS ? "← Gallery" : "Back to Gallery", onClick: onBack }}
        />
      </PageContainer>
    );
  }

  // Terminal state — show spinner while navigation happens
  if (state.phase === "finished") {
    return (
      <PageContainer centered>
        {hiddenVideo}
        <Spinner size="md" />
      </PageContainer>
    );
  }

  const { phase, controlMode, captureCount, sourceLost, captureError, actionError } = state;
  const pauseLoading = phase === "pausing";
  const resumeLoading = phase === "resuming";
  const stopLoading = phase === "stopPausing" || phase === "stopFinishing";
  const prompting = isPrompting(phase);
  // The core reads the SERVER-credited count for this, never the on-screen
  // clock: the clock interpolates between confirms, so it passes 1:00 a
  // round trip before that minute exists as a capture unit.
  const tooShort = state.tooShortToCompile;
  const captureStale = state.stale && !captureError;

  return (
    <div style={{
      display: "flex", flexDirection: "column", height: "100%",
      maxWidth: 480, margin: "0 auto", padding: spacing.lg, boxSizing: "border-box",
    }}>
      {hiddenVideo}
      {/* Status card — recording indicator + timer + capture count */}
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
          {/* One capture unit per recorded minute — a JPEG screenshot on
              legacy sessions, a ~6-frame clip on clips sessions. */}
          {captureCount} {captureCount === 1 ? "capture" : "captures"}
        </span>
      </div>

      {/* Preview — fills available space. The core feeds this image
          directly once the loop is running (see the preview listener
          above); until the first frame, the native preview protocol
          shows the sources. */}
      <div style={{
        flex: 1,
        minHeight: 0,
        display: "flex",
        flexDirection: isCamera ? "column" : "row",
        gap: !isCamera && source.length > 1 && !previewUrl ? spacing.xs : 0,
        marginBottom: spacing.lg
      }}>
        {previewUrl ? (
          <div style={{
            position: "relative", borderRadius: radii.lg,
            overflow: "hidden", background: colors.bg.sunken, border: `1px solid ${colors.border.default}`,
            flex: 1, minHeight: 0, minWidth: 0,
          }}>
            <img
              src={previewUrl}
              alt="Screen preview"
              style={{ width: "100%", height: "100%", objectFit: "contain", display: "block" }}
            />
            <span style={{
              position: "absolute", bottom: 6, right: 6, fontSize: fontSize.xs,
              color: colors.badge.overlayText, background: colors.badge.overlayBg,
              padding: "2px 6px", borderRadius: radii.sm,
            }}>
              {windowFocused && state.capturing ? "Preview" : "Latest capture"}
            </span>
          </div>
        ) : (
          source.map((src) => (
            <RecorderPreviewItem
              key={`${src.type}:${src.id}`}
              src={src}
              captureUrl={null}
              isMulti={source.length > 1}
              live={windowFocused && !isCamera}
            />
          ))
        )}
      </div>

      {captureStale && (
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

      {captureError && (
        <ErrorDisplay
          variant="banner"
          error={captureError}
          reassurance="Your earlier progress is saved. Restart the app if this keeps happening."
          action={{ label: "Details", onClick: () => setCaptureErrorDetail(captureError) }}
          onCopy={() => navigator.clipboard.writeText(getReport(captureError))}
        />
      )}

      {actionError && !captureError && (
        <ErrorDisplay
          variant="banner"
          error={actionError}
          reassurance="The recording is unchanged — try again."
          onCopy={() => navigator.clipboard.writeText(getReport(actionError))}
        />
      )}

      {captureErrorDetail && (
        <CaptureErrorDialog
          error={captureErrorDetail}
          fallbackTitle="The recording hit a capture error"
          onDismiss={() => setCaptureErrorDetail(null)}
        />
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
              disabled={stopLoading || prompting}
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
              disabled={pauseLoading || stopLoading || prompting}
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
              disabled={stopLoading || prompting || !!sourceLost}
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
              disabled={resumeLoading || stopLoading || prompting}
              fullWidth
              style={{ flex: 1 }}
            >
              Stop
            </Button>
          </>
        )}
      </div>

      {prompting && (
        <NamingModal
          tooShort={tooShort}
          loading={phase === "stopFinishing"}
          onConfirm={handleConfirmStop}
          onEditAndSave={handleEditAndSave}
          onResume={handleResumeFromModal}
        />
      )}

      {/* The screencast ended without the recording ending. Nothing below
          this can capture anything, so ask for a new source outright —
          stood down while one of the other two dialogs is up, since both
          are reached from its own buttons. */}
      {sourceLost && !prompting && !captureErrorDetail && (
        <SourceLostPrompt
          loss={sourceLost}
          onReselect={reselectSource}
          onStop={handleStopClick}
          onDetails={
            sourceLost.detail
              ? () => setCaptureErrorDetail(sourceLost.detail!)
              : undefined
          }
          stopping={stopLoading}
        />
      )}
    </div>
  );
}
