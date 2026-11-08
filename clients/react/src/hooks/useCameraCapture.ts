import { useRef, useState, useCallback, useEffect } from "react";
import type { CaptureResult, CaptureSettings } from "../types.js";
import { useLookoutContext } from "../LookoutProvider.js";
import { waitForVideoReady, captureFrameAsJpeg } from "./captureUtils.js";

/**
 * Handles getUserMedia (webcam), device enumeration, canvas snapshots,
 * and stream lifecycle.
 *
 * Supports a two-phase flow for camera mode:
 *   1. **Preview** — `startPreview()` acquires the camera stream so the UI
 *      can show a live `<video>` and a device picker *before* recording.
 *   2. **Recording** — `startSharing()` reuses the preview stream (or
 *      acquires one if preview wasn't started) and sets `isSharing = true`,
 *      which tells `useLookout` to begin the capture-upload loop.
 *
 * Mirrors the base return shape of `useScreenCapture` (`isSharing`,
 * `startSharing`, `takeScreenshot`, `stopSharing`) so `useLookout` can
 * delegate to either hook interchangeably, plus camera-specific extras.
 */
export function useCameraCapture(overrides?: CaptureSettings) {
  let settings: {
    maxWidth: number;
    maxHeight: number;
    jpegQuality: number;
    deviceId?: string;
    userMediaConstraints?: MediaTrackConstraints;
  };

  try {
    const { config } = useLookoutContext();
    settings = {
      maxWidth: overrides?.maxWidth ?? config.capture.maxWidth,
      maxHeight: overrides?.maxHeight ?? config.capture.maxHeight,
      jpegQuality: overrides?.jpegQuality ?? config.capture.jpegQuality,
      deviceId: overrides?.camera?.deviceId ?? config.capture.camera.deviceId,
      userMediaConstraints:
        overrides?.camera?.userMediaConstraints ??
        config.capture.camera.userMediaConstraints,
    };
  } catch {
    // Standalone mode — no provider
    settings = {
      maxWidth: overrides?.maxWidth ?? 1920,
      maxHeight: overrides?.maxHeight ?? 1080,
      jpegQuality: overrides?.jpegQuality ?? 0.85,
      deviceId: overrides?.camera?.deviceId,
      userMediaConstraints: overrides?.camera?.userMediaConstraints,
    };
  }

  const streamRef = useRef<MediaStream | null>(null);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [isSharing, setIsSharing] = useState(false);
  const [isPreviewing, setIsPreviewing] = useState(false);
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);
  const [devices, setDevices] = useState<MediaDeviceInfo[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(
    settings.deviceId ?? null,
  );
  const settingsRef = useRef(settings);
  settingsRef.current = settings;

  // ─── Device enumeration ────────────────────────────────
  const enumerateDevices = useCallback(async () => {
    try {
      const all = await navigator.mediaDevices.enumerateDevices();
      const cameras = all.filter((d) => d.kind === "videoinput");
      setDevices(cameras);
      return cameras;
    } catch {
      return [];
    }
  }, []);

  // Enumerate on mount and listen for device changes
  useEffect(() => {
    enumerateDevices();
    const handler = () => enumerateDevices();
    navigator.mediaDevices.addEventListener("devicechange", handler);
    return () =>
      navigator.mediaDevices.removeEventListener("devicechange", handler);
  }, [enumerateDevices]);

  // ─── Internal: acquire stream ──────────────────────────
  const acquireStream = useCallback(
    async (deviceIdOverride?: string) => {
      const s = settingsRef.current;
      const videoConstraints: MediaTrackConstraints = {
        width: { ideal: s.maxWidth, max: s.maxWidth },
        height: { ideal: s.maxHeight, max: s.maxHeight },
        ...s.userMediaConstraints,
      };

      const devId = deviceIdOverride ?? selectedDeviceId ?? s.deviceId;
      if (devId) {
        videoConstraints.deviceId = { exact: devId };
      }

      const stream = await navigator.mediaDevices.getUserMedia({
        video: videoConstraints,
        audio: false,
      });

      // Stop any previous stream before replacing refs
      streamRef.current?.getTracks().forEach((t) => t.stop());

      streamRef.current = stream;
      setPreviewStream(stream);

      const video = document.createElement("video");
      video.srcObject = stream;
      video.muted = true;
      video.playsInline = true;
      await video.play();
      await waitForVideoReady(video);

      videoRef.current = video;

      if (!canvasRef.current) {
        canvasRef.current = document.createElement("canvas");
      }

      const handleEnded = () => {
        streamRef.current = null;
        setPreviewStream(null);
        setIsPreviewing(false);
        setIsSharing(false);
      };
      stream.getVideoTracks()[0].addEventListener("ended", handleEnded);

      // Re-enumerate after first getUserMedia — Safari may now expose labels
      enumerateDevices();

      return stream;
    },
    [selectedDeviceId, enumerateDevices],
  );

  // ─── Preview (stream without capture loop) ─────────────
  const startPreview = useCallback(async () => {
    await acquireStream();
    setIsPreviewing(true);
    setIsSharing(false);
  }, [acquireStream]);

  const stopPreview = useCallback(() => {
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
    setPreviewStream(null);
    videoRef.current = null;
    setIsPreviewing(false);
    setIsSharing(false);
  }, []);

  // ─── Recording (triggers capture loop via isSharing) ───
  const startSharing = useCallback(async () => {
    // If already previewing, reuse the existing stream
    if (!streamRef.current) {
      await acquireStream();
    }
    setIsPreviewing(false);
    setIsSharing(true);
  }, [acquireStream]);

  const takeScreenshot = useCallback((): Promise<CaptureResult | null> => {
    const video = videoRef.current;
    const canvas = canvasRef.current;
    const s = settingsRef.current;
    if (!video || !canvas || !streamRef.current) {
      return Promise.resolve(null);
    }
    return captureFrameAsJpeg(video, canvas, s);
  }, []);

  const stopSharing = useCallback(() => {
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
    setPreviewStream(null);
    videoRef.current = null;
    setIsPreviewing(false);
    setIsSharing(false);
  }, []);

  // ─── Device selection ──────────────────────────────────
  const selectDevice = useCallback(
    async (deviceId: string) => {
      setSelectedDeviceId(deviceId);
      // If stream is live (preview or recording), restart with new device
      if (streamRef.current) {
        const wasSharing = isSharing;
        try {
          await acquireStream(deviceId);
          // Restore the same mode (preview vs sharing)
          if (wasSharing) {
            setIsPreviewing(false);
            setIsSharing(true);
          } else {
            setIsPreviewing(true);
            setIsSharing(false);
          }
        } catch {
          // Device switch failed — stop everything
          streamRef.current = null;
          setPreviewStream(null);
          setIsPreviewing(false);
          setIsSharing(false);
        }
      }
    },
    [isSharing, acquireStream],
  );

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      streamRef.current?.getTracks().forEach((t) => t.stop());
    };
  }, []);

  // Stable accessor for the live capture <video> — for the capture host,
  // which needs the current element inside callbacks without re-render
  // staleness.
  const getVideo = useCallback(() => videoRef.current, []);

  /** Whether a stream is live right now (preview or recording). */
  const hasStream = useCallback(() => streamRef.current !== null, []);

  return {
    isSharing,
    startSharing,
    takeScreenshot,
    stopSharing,
    getVideo,
    hasStream,
    // Camera-specific:
    devices,
    selectedDeviceId,
    selectDevice,
    // Preview:
    isPreviewing,
    previewStream,
    startPreview,
    stopPreview,
  };
}
