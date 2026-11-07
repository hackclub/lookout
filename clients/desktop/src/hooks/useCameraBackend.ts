/**
 * The webview half of camera recording.
 *
 * Camera sessions run through the core's capture loop like every other
 * session — the Rust-side cadence, sleep recovery, pause flush, clips and
 * upload pipeline — with only the *frame* coming from here, because the
 * camera itself is a `getUserMedia` stream. The core asks over three
 * events and this answers:
 *
 *   - `camera-start-request {requestId, deviceId}`: open the stream and
 *     wait until the hidden `<video>` has decoded dimensions, then
 *     `camera_request_done`. The core waits for this before its first
 *     tick, so the seed capture has a frame.
 *   - `camera-frame-request {requestId, deviceId}`: grab a JPEG off the
 *     video, then `camera_frame_delivered`.
 *   - `camera-stop`: release the stream (pause, stop, close).
 *
 * `ready` is true once the listeners are attached. A recording with a
 * camera source must not be opened before that — the start request would
 * go unanswered and time out. The `<video>` the ref points at must be in
 * the DOM for the whole life of the recording, loading skeleton included:
 * the start request arrives while the session is still being fetched.
 */
import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../logger.js";
import { useCameraCapture, waitForVideoReady } from "./useCameraCapture.js";

interface CameraRequest {
  requestId: number;
  deviceId: string;
}

export function useCameraBackend(active: boolean) {
  const camera = useCameraCapture();
  const videoRef = useRef<HTMLVideoElement>(null);
  const [ready, setReady] = useState(!active);

  const cameraRef = useRef(camera);
  cameraRef.current = camera;

  useEffect(() => {
    if (!active) {
      setReady(true);
      return;
    }
    let cancelled = false;
    const unlistens: UnlistenFn[] = [];

    const onStart = async (event: { payload: CameraRequest }) => {
      const { requestId, deviceId } = event.payload;
      let error: string | null = null;
      try {
        // Let the element mount before attaching a stream to it.
        await new Promise((r) => requestAnimationFrame(r));
        const stream = await cameraRef.current.startStream(deviceId);
        if (!stream) throw new Error("the camera stream could not be started");
        const video = videoRef.current;
        if (!video) throw new Error("no video element to play the camera into");
        video.srcObject = stream;
        try {
          await video.play();
        } catch {
          // Autoplay rules can reject play() on a muted stream in odd
          // states; readiness below is what actually matters.
        }
        await waitForVideoReady(video);
        console.log(`[camera] video ready: ${video.videoWidth}x${video.videoHeight}`);
      } catch (err) {
        error = err instanceof Error ? err.message : String(err);
        console.error(`[camera] start failed: ${error}`);
      }
      invoke("camera_request_done", { requestId, error }).catch(console.error);
    };

    const onFrame = async (event: { payload: CameraRequest }) => {
      const { requestId } = event.payload;
      const frame = await cameraRef.current.captureFrame(videoRef.current);
      const args = frame
        ? { requestId, base64: frame.base64, width: frame.width, height: frame.height, error: null }
        : {
            requestId,
            base64: null,
            width: null,
            height: null,
            error: "no camera frame — the stream isn't ready or has ended",
          };
      invoke("camera_frame_delivered", args).catch(console.error);
    };

    const onStop = () => {
      cameraRef.current.stopStream();
    };

    (async () => {
      unlistens.push(await listen<CameraRequest>("camera-start-request", (e) => void onStart(e)));
      unlistens.push(await listen<CameraRequest>("camera-frame-request", (e) => void onFrame(e)));
      unlistens.push(await listen("camera-stop", onStop));
      if (!cancelled) setReady(true);
    })().catch((e) => console.error("[camera] listener setup failed:", e));

    return () => {
      cancelled = true;
      unlistens.forEach((u) => u());
      cameraRef.current.stopStream();
    };
  }, [active]);

  return { videoRef, stream: camera.stream, ready };
}
