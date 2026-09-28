// Provider
export { LookoutProvider } from "./LookoutProvider.js";
export type { LookoutProviderProps } from "./LookoutProvider.js";

// Drop-in widget
export { LookoutRecorder } from "./components/LookoutRecorder.js";
export type { LookoutRecorderProps } from "./components/LookoutRecorder.js";

// Cut editor
export { TimelapseEditor } from "./components/TimelapseEditor.js";
export type { TimelapseEditorProps } from "./components/TimelapseEditor.js";
export { StopChoiceModal } from "./components/StopChoiceModal.js";
export type { StopChoiceModalProps } from "./components/StopChoiceModal.js";
export { useEditLease } from "./hooks/useEditLease.js";
export {
  regionsToCuts,
  cutsToRegions,
  normalizeRegions,
  gapIndices,
} from "./hooks/editorMath.js";
export type { UnitRegion, UnitMaskRegion, UnitBlurRegion } from "./hooks/editorMath.js";

// Sub-components
export { StatusBar } from "./components/StatusBar.js";
export type { StatusBarProps } from "./components/StatusBar.js";
export { RecordingControls } from "./components/RecordingControls.js";
export type { RecordingControlsProps } from "./components/RecordingControls.js";
export { ScreenPreview } from "./components/ScreenPreview.js";
export type { ScreenPreviewProps } from "./components/ScreenPreview.js";
export { CameraSelector } from "./components/CameraSelector.js";
export type { CameraSelectorProps } from "./components/CameraSelector.js";
export { CameraPreview } from "./components/CameraPreview.js";
export type { CameraPreviewProps } from "./components/CameraPreview.js";
export { ResultView } from "./components/ResultView.js";
export type { ResultViewProps } from "./components/ResultView.js";
export { ProcessingState } from "./components/ProcessingState.js";
export type { ProcessingStateProps } from "./components/ProcessingState.js";
export { VideoPlayer } from "./components/VideoPlayer.js";

// Gallery components
export { Gallery } from "./components/Gallery.js";
export type { GalleryProps, AddAnchor } from "./components/Gallery.js";
export { SessionCard } from "./components/SessionCard.js";
export type { SessionCardProps } from "./components/SessionCard.js";
export { SessionDetail } from "./components/SessionDetail.js";
export type { SessionDetailProps } from "./components/SessionDetail.js";

// Headless hooks
export { useLookout } from "./hooks/useLookout.js";
export { useScreenCapture } from "./hooks/useScreenCapture.js";
export { useCameraCapture } from "./hooks/useCameraCapture.js";
export { useUploader } from "./hooks/useUploader.js";
export type { UploadPayload } from "./hooks/useUploader.js";
export { ClipRecorder } from "./hooks/clipRecorder.js";
export type { ClipCaptureResult, ClipRecorderOptions } from "./hooks/clipRecorder.js";
export { drawScaledHQ } from "./hooks/hqScale.js";
export { useSession } from "./hooks/useSession.js";
export {
  useSessionTimer,
  useSessionTimerState,
  deriveDisplaySeconds,
  MAX_INTERPOLATION_S,
  formatTime,
  formatTrackedTime,
} from "./hooks/useSessionTimer.js";
export type { SessionTimerState } from "./hooks/useSessionTimer.js";
export { computeBestTrackedSeconds } from "./hooks/computeBestTracked.js";
export type { BestTrackedInputs } from "./hooks/computeBestTracked.js";
export { isTooShortToCompile } from "./hooks/tooShortToCompile.js";

// Gallery hooks
export { useTokenStore } from "./hooks/useTokenStore.js";
export type { TokenEntry, UseTokenStore } from "./hooks/useTokenStore.js";
export { useGallery } from "./hooks/useGallery.js";
export type { UseGalleryOptions, UseGallery as UseGalleryReturn } from "./hooks/useGallery.js";
export { useHashRouter } from "./hooks/useHashRouter.js";
export type { Route } from "./hooks/useHashRouter.js";

// API client (no React dependency)
export { createLookoutClient, HttpError } from "./api/client.js";
export type { LookoutClient, CreateClientOptions } from "./api/client.js";

// Types
export type {
  LookoutConfig,
  LookoutState,
  LookoutActions,
  LookoutCallbacks,
  CaptureSettings,
  CaptureMode,
  CameraSettings,
  RetrySettings,
  UploadState,
  CaptureResult,
  RecorderStatus,
  TokenProvider,
  ResolvedConfig,
} from "./types.js";

// Re-export shared types consumers need
export type { SessionStatus, SessionSummary, CutInterval, MaskRegion, BlurRegion } from "@lookout/shared";
export { SESSION_STATUSES } from "@lookout/shared";

// UI primitives
export * from "./ui/index.js";
export { setAccentColor } from "./ui/theme.js";
