import React, { useEffect, useState } from "react";
import { useLookout } from "../hooks/useLookout.js";
import { useLookoutContext } from "../LookoutProvider.js";
import { StatusBar } from "./StatusBar.js";
import { TimelapseEditor } from "./TimelapseEditor.js";
import { StopChoiceModal } from "./StopChoiceModal.js";
import { ScreenPreview } from "./ScreenPreview.js";
import { CameraPreview } from "./CameraPreview.js";
import { CameraSelector } from "./CameraSelector.js";
import { RecordingControls } from "./RecordingControls.js";
import { ProcessingState } from "./ProcessingState.js";
import { Button } from "../ui/Button.js";
import { Spinner } from "../ui/Spinner.js";
import { ErrorDisplay } from "../ui/ErrorDisplay.js";
import { PageContainer } from "../ui/PageContainer.js";
import { Overlay } from "../ui/Overlay.js";
import { colors, fontSize, fontWeight, spacing } from "../ui/theme.js";

/**
 * Drop-in recorder widget. Handles the full lifecycle:
 * screen/camera capture, upload, pause/resume/stop, compilation, video playback.
 *
 * Adapts its UI based on the configured `capture.mode`:
 * - `"screen"` (default): screen sharing flow with `getDisplayMedia`
 * - `"camera"`: webcam flow with live preview, device picker, then recording
 *
 * Must be used within a `<LookoutProvider>`.
 */
export interface LookoutRecorderProps {
  /** Offer "Edit & save" when stopping (default true). Programs embedding
   *  the recorder can pass false to keep stopping a one-click action. */
  editing?: boolean;
}

export function LookoutRecorder({ editing = true }: LookoutRecorderProps = {}) {
  const { state, actions } = useLookout();
  const { client, config } = useLookoutContext();
  const [resolvedToken, setResolvedToken] = useState<string | null>(null);
  // Set when the user chose "Edit & save": the session is held unpublished
  // and this view owns the editor until they publish.
  const [editorOpen, setEditorOpen] = useState(false);

  // The editor needs a concrete token string; resolve it once, up front,
  // so opening the editor is instant when the user asks for it.
  useEffect(() => {
    if (!editing) return;
    let cancelled = false;
    client
      .resolveToken()
      .then((t) => {
        if (!cancelled) setResolvedToken(t);
      })
      .catch(() => {
        // Best-effort: without a token we simply don't offer editing.
      });
    return () => {
      cancelled = true;
    };
  }, [editing, client]);

  const canEdit = editing && resolvedToken !== null;
  const [stopPrompt, setStopPrompt] = useState(false);
  const [stopping, setStopping] = useState(false);

  const confirmStop = async (withEdit: boolean) => {
    setStopping(true);
    try {
      // Open the editor optimistically for the edit path: the session is
      // held, so the editor can show its own "preparing" state while the
      // compile runs instead of leaving the user on a dead screen.
      if (withEdit) setEditorOpen(true);
      await actions.stop({ edit: withEdit });
      setStopPrompt(false);
    } finally {
      setStopping(false);
    }
  };

  if (state.status === "loading") {
    return (
      <PageContainer centered>
        <Spinner size="lg" />
      </PageContainer>
    );
  }

  if (state.status === "no-token") {
    return (
      <PageContainer centered>
        <h2 style={{ fontSize: fontSize.display, fontWeight: fontWeight.bold, color: colors.text.primary, marginBottom: spacing.sm }}>
          No session token
        </h2>
        <p style={{ fontSize: fontSize.xl, color: colors.text.secondary, textAlign: "center", maxWidth: 400 }}>
          This page requires a session token. You should have been redirected
          here from another service.
        </p>
      </PageContainer>
    );
  }

  if (state.status === "error") {
    return (
      <PageContainer centered>
        <ErrorDisplay error={state.error ?? "Unknown error"} variant="page" />
      </PageContainer>
    );
  }

  // Terminal states: show processing state inline
  if (
    state.status === "stopped" ||
    state.status === "compiling" ||
    state.status === "complete" ||
    state.status === "failed"
  ) {
    return (
      <>
        <PageContainer style={{ padding: spacing.xxl }}>
          {/* videoUrl is what ends the wait. useLookout fetches it the
              moment the session flips to complete; without it here,
              ProcessingState has no way to tell "still compiling" from
              "compiled, and here it is", so it sits on its spinner forever
              with the finished video already in hand. */}
          <ProcessingState
            status={state.status}
            trackedSeconds={state.trackedSeconds}
            videoUrl={state.videoUrl}
          />
        </PageContainer>

        {/* "Edit & save" opens over the host page rather than inside the
            recorder's own box. Embedders place the recorder in columns and
            cards of any width, and a timeline squeezed into one is a
            precision tool you can't be precise with; the overlay gets the
            viewport regardless.

            Deliberately not dismissible: there is no "leave without
            deciding" exit, because the session is unpublished until
            someone decides. Save is the way out — and if the tab goes
            away entirely, the edit lease lapses and it publishes as
            recorded. */}
        {editorOpen && resolvedToken && state.status !== "failed" && (
          <Overlay label="Review your timelapse" height="min(820px, 92vh)">
            <div
              style={{
                flex: "0 0 auto",
                padding: `${spacing.lg}px ${spacing.xl}px 0`,
              }}
            >
              <div
                style={{
                  fontSize: fontSize.xxl,
                  fontWeight: fontWeight.bold,
                  color: colors.text.primary,
                  letterSpacing: "-0.01em",
                }}
              >
                Review your timelapse
              </div>
              <div
                style={{
                  fontSize: fontSize.md,
                  color: colors.text.secondary,
                  marginTop: 2,
                }}
              >
                Cut anything you'd rather not share. Nothing is published
                until you save.
              </div>
            </div>
            <div
              style={{
                flex: "1 1 auto",
                minHeight: 0,
                padding: `${spacing.md}px ${spacing.xl}px ${spacing.xl}px`,
              }}
            >
              <TimelapseEditor
                token={resolvedToken}
                apiBaseUrl={config.apiBaseUrl}
                onApplied={() => setEditorOpen(false)}
              />
            </div>
          </Overlay>
        )}
      </>
    );
  }

  const isCamera = state.captureMode === "camera";

  // ─── Camera mode: preview → record flow ────────────────
  if (isCamera) {
    return (
      <PageContainer maxWidth={800} style={{ padding: spacing.xxl }}>
        <StatusBar
          displaySeconds={state.displaySeconds}
          screenshotCount={state.screenshotCount}
          uploads={state.uploads}
        />

        {/* Camera selector — show whenever devices are available and we're not mid-recording */}
        {state.availableCameras.length > 1 && (
          <CameraSelector
            devices={state.availableCameras}
            selectedDeviceId={state.selectedCameraId}
            onSelect={actions.selectCamera}
            disabled={state.isSharing}
          />
        )}

        {/* Preview/capture display */}
        {state.isPreviewing || state.previewStream ? (
          <CameraPreview
            stream={state.previewStream}
            fallbackImageUrl={state.lastScreenshotUrl}
          />
        ) : state.lastScreenshotUrl ? (
          <ScreenPreview imageUrl={state.lastScreenshotUrl} />
        ) : null}

        {/* Camera-specific controls */}
        {!state.isPreviewing && !state.isSharing ? (
          /* Phase 1: No stream yet — prompt to start camera */
          <CameraIdleControls
            status={state.status}
            onStartPreview={actions.startPreview}
            onStartRecording={actions.startSharing}
            onStop={() => setStopPrompt(true)}
          />
        ) : state.isPreviewing && !state.isSharing ? (
          /* Phase 2: Previewing — show "Start Recording" */
          <CameraPreviewControls
            onStartRecording={actions.startSharing}
            onStopPreview={actions.stopPreview}
          />
        ) : (
          /* Phase 3: Recording — standard recording controls */
          <RecordingControls
            status={state.status}
            isSharing={state.isSharing}
            onStartSharing={actions.startSharing}
            onPause={actions.pause}
            onResume={actions.resume}
            onStop={() => setStopPrompt(true)}
            captureMode="camera"
          />
        )}
        {stopPrompt && (
          <StopChoiceModal
            loading={stopping}
            onResume={() => setStopPrompt(false)}
            onStopAndSave={() => void confirmStop(false)}
            onEditAndSave={canEdit ? () => void confirmStop(true) : undefined}
          />
        )}
      </PageContainer>
    );
  }

  // ─── Screen mode (default) ─────────────────────────────
  return (
    <PageContainer maxWidth={800} style={{ padding: spacing.xxl }}>
      <StatusBar
        displaySeconds={state.displaySeconds}
        screenshotCount={state.screenshotCount}
        uploads={state.uploads}
      />
      <ScreenPreview imageUrl={state.lastScreenshotUrl} />
      <RecordingControls
        status={state.status}
        isSharing={state.isSharing}
        onStartSharing={actions.startSharing}
        onPause={actions.pause}
        onResume={actions.resume}
        onStop={() => setStopPrompt(true)}
        captureMode="screen"
      />
      {stopPrompt && (
        <StopChoiceModal
          loading={stopping}
          onResume={() => setStopPrompt(false)}
          onStopAndSave={() => void confirmStop(false)}
          onEditAndSave={canEdit ? () => void confirmStop(true) : undefined}
        />
      )}
    </PageContainer>
  );
}

// ─── Camera sub-controls ─────────────────────────────────

/** Controls shown when camera is idle (no stream). */
function CameraIdleControls({
  status,
  onStartPreview,
  onStartRecording,
  onStop,
}: {
  status: string;
  onStartPreview: () => void;
  onStartRecording: () => void;
  onStop: () => void;
}) {
  const isPaused = status === "paused";

  return (
    <div style={{
      display: "flex",
      alignItems: "center",
      gap: spacing.md,
      justifyContent: "center",
      flexWrap: "wrap",
    }}>
      {isPaused ? (
        <>
          <Button variant="primary" size="lg" onClick={onStartRecording}>
            Start Camera &amp; Resume
          </Button>
          <Button variant="danger" size="md" onClick={onStop}>
            Stop Session
          </Button>
        </>
      ) : (
        <Button variant="success" size="lg" onClick={onStartPreview}>
          Start Camera
        </Button>
      )}
    </div>
  );
}

/** Controls shown during camera preview (stream live, not recording). */
function CameraPreviewControls({
  onStartRecording,
  onStopPreview,
}: {
  onStartRecording: () => void;
  onStopPreview: () => void;
}) {
  return (
    <div style={{
      display: "flex",
      alignItems: "center",
      gap: spacing.md,
      justifyContent: "center",
      flexWrap: "wrap",
    }}>
      <Button variant="success" size="lg" onClick={onStartRecording}>
        Start Recording
      </Button>
      <Button variant="secondary" size="md" onClick={onStopPreview}>
        Cancel
      </Button>
    </div>
  );
}
