/**
 * The browser as the shared recorder's host.
 *
 * The recorder (Rust, `lookout-recorder`, running as WebAssembly) owns the
 * recording: the schedule, the uploader and its backlog, the pause flush,
 * every retry. What it asks of the page is exactly what only the page can
 * do, and that is all this file is:
 *
 * - `request`: `fetch` with a deadline, the way the API client always did.
 * - `beginCapture`: frames off the live `<video>` (screen or camera) drawn
 *   through the stepped high-quality downscale, as full JPEGs for upload,
 *   preview JPEGs for the UI, and into the `ClipRecorder` for clips.
 * - `sourceOpen` / `sourceClose`: a camera's warm-up and release.
 * - `emit`: the recorder's events, to the hook.
 */
import { UPLOAD_STEP_TIMEOUT_MS } from "@lookout/shared";
import type {
  LookoutCaptureSession,
  LookoutCaptureSpec,
  LookoutClip,
  LookoutFrame,
  LookoutHost,
  LookoutSource,
  LookoutTransportRequest,
  LookoutTransportResponse,
} from "@lookout/wasm";
import { ClipRecorder } from "../hooks/clipRecorder.js";
import { drawScaledHQ } from "../hooks/hqScale.js";
import type { CaptureResult } from "../types.js";

/** Preview JPEGs: the size and quality the live preview always used. */
const PREVIEW_MAX_W = 854;
const PREVIEW_MAX_H = 480;
const PREVIEW_JPEG_QUALITY = 0.65;

/** `canvas.toBlob` has no failure path, only silence: bound it. */
const TO_BLOB_TIMEOUT_MS = 10_000;

export interface BrowserCaptureSettings {
  maxWidth: number;
  maxHeight: number;
  /** 0–1. */
  jpegQuality: number;
}

/** What the host needs from the page, read live (refs, not snapshots). */
export interface BrowserHostOptions {
  /** The live capture `<video>` (screen share or camera), or null. */
  getVideo: () => HTMLVideoElement | null;
  /** Open the camera (acquire a stream) if none is live. Screen sources
   *  are opened by the user's own click, so this is only called for
   *  cameras. */
  openCamera: (deviceId: string) => Promise<void>;
  /** Release the camera. */
  closeCamera: () => void;
  settings: BrowserCaptureSettings;
  clientInfo: string;
  /** Whether the live preview is being looked at. */
  wantsPreviewFrames?: () => boolean;
  /** The recorder's events, by wire name. */
  onEvent: (name: string, payload: unknown) => void;
  /** A full-size tick capture was taken (before upload). */
  onCapture?: (capture: CaptureResult) => void;
  log?: (line: string) => void;
  /** Override the transport (tests). Defaults to `fetch`. */
  fetchImpl?: typeof fetch;
  /** Override the capture session (tests, or a host with its own frame
   *  source). Defaults to the canvas-and-ClipRecorder session. */
  captureSession?: (source: LookoutSource, spec: LookoutCaptureSpec) => LookoutCaptureSession;
}

/** An `AbortSignal` that fires after `ms`, where the engine has one. */
function deadline(ms: number): AbortSignal | undefined {
  return typeof AbortSignal !== "undefined" && typeof AbortSignal.timeout === "function"
    ? AbortSignal.timeout(ms)
    : undefined;
}

function isTimeout(err: unknown): boolean {
  return err instanceof Error && (err.name === "TimeoutError" || err.name === "AbortError");
}

/** `fetch` as the recorder's transport. Rejections carry `{ timeout, message }`. */
export async function browserRequest(
  req: LookoutTransportRequest,
  fetchImpl: typeof fetch = fetch,
): Promise<LookoutTransportResponse> {
  const headers: Record<string, string> = {};
  let body: BodyInit | undefined;
  if (req.json !== undefined) {
    headers["Content-Type"] = "application/json";
    body = req.json;
  } else if (req.body !== undefined) {
    if (req.contentType) headers["Content-Type"] = req.contentType;
    // A fresh ArrayBuffer-backed copy: the view may be into wasm memory,
    // which the request must not hold on to.
    body = new Uint8Array(req.body);
  }
  let res: Response;
  try {
    res = await fetchImpl(req.url, {
      method: req.method,
      headers,
      body,
      signal: deadline(req.timeoutMs || UPLOAD_STEP_TIMEOUT_MS),
    });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    throw { timeout: isTimeout(err), message };
  }
  let bytes: Uint8Array;
  try {
    bytes = new Uint8Array(await res.arrayBuffer());
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    throw { timeout: isTimeout(err), message: `response body: ${message}` };
  }
  return { status: res.status, reason: res.statusText, body: bytes };
}

async function canvasToJpeg(canvas: HTMLCanvasElement, quality: number): Promise<LookoutFrame | null> {
  const blob = await new Promise<Blob | null>((resolve) => {
    let done = false;
    const finish = (b: Blob | null) => {
      if (!done) {
        done = true;
        resolve(b);
      }
    };
    try {
      canvas.toBlob((b) => finish(b), "image/jpeg", quality);
    } catch {
      finish(null);
    }
    setTimeout(() => finish(null), TO_BLOB_TIMEOUT_MS);
  });
  if (!blob) return null;
  return {
    bytes: new Uint8Array(await blob.arrayBuffer()),
    width: canvas.width,
    height: canvas.height,
  };
}

/** One capture run: the canvases and the clip recorder for it. */
class BrowserCaptureSession implements LookoutCaptureSession {
  private full: HTMLCanvasElement = document.createElement("canvas");
  private preview: HTMLCanvasElement | null = null;
  private clip: ClipRecorder | null = null;
  private clipUnavailable = false;
  private haveFrame = false;

  constructor(
    private readonly opts: BrowserHostOptions,
    private readonly spec: LookoutCaptureSpec,
  ) {}

  private video(): HTMLVideoElement {
    const video = this.opts.getVideo();
    if (!video || video.videoWidth === 0 || video.videoHeight === 0) {
      throw new Error("Video not ready — no frames received");
    }
    return video;
  }

  /** Draw the current video frame into the full-size canvas. */
  private drawFull(video: HTMLVideoElement): void {
    const scale = Math.min(
      this.spec.maxWidth / video.videoWidth,
      this.spec.maxHeight / video.videoHeight,
      1,
    );
    this.full.width = Math.round(video.videoWidth * scale);
    this.full.height = Math.round(video.videoHeight * scale);
    drawScaledHQ(video, video.videoWidth, video.videoHeight, this.full);
    this.haveFrame = true;
  }

  private drawPreview(): HTMLCanvasElement {
    const preview = (this.preview ??= document.createElement("canvas"));
    const scale = Math.min(PREVIEW_MAX_W / this.full.width, PREVIEW_MAX_H / this.full.height, 1);
    preview.width = Math.max(2, Math.round(this.full.width * scale));
    preview.height = Math.max(2, Math.round(this.full.height * scale));
    drawScaledHQ(this.full, this.full.width, this.full.height, preview);
    return preview;
  }

  /** Record the current frame into the clip, starting one if needed. */
  private recordClip(video: HTMLVideoElement): string | undefined {
    if (this.clipUnavailable) return "clip recording not supported in this browser";
    try {
      if (!this.clip) {
        if (!ClipRecorder.isSupported()) {
          this.clipUnavailable = true;
          return "clip recording not supported in this browser";
        }
        this.clip = new ClipRecorder(video, this.spec.frameIntervalMs, {
          maxWidth: this.spec.maxWidth,
          maxHeight: this.spec.maxHeight,
          jpegQuality: this.opts.settings.jpegQuality,
          externalCadence: true,
        });
      }
      this.clip.recordFrame();
      return undefined;
    } catch (err) {
      // A broken clip is dropped; the loop counts the failure toward
      // its latch and the interval still uploads as a JPEG.
      this.clip?.stop();
      this.clip = null;
      return err instanceof Error ? err.message : String(err);
    }
  }

  async grab(jpegMode: "none" | "preview" | "full", record: boolean) {
    const video = this.video();
    this.drawFull(video);
    const capturedAtMs = Date.now();
    const clipError = record ? this.recordClip(video) : undefined;
    let jpeg: LookoutFrame | undefined;
    if (jpegMode === "full") {
      const frame = await canvasToJpeg(this.full, this.opts.settings.jpegQuality);
      if (!frame) throw new Error("JPEG encode failed");
      jpeg = frame;
      this.opts.onCapture?.({
        blob: new Blob([frame.bytes as unknown as BlobPart], { type: "image/jpeg" }),
        width: frame.width,
        height: frame.height,
        capturedAtMs,
      });
    } else if (jpegMode === "preview") {
      jpeg = (await canvasToJpeg(this.drawPreview(), PREVIEW_JPEG_QUALITY)) ?? undefined;
    }
    return { jpeg, clipError };
  }

  async clipCut(): Promise<LookoutClip | null> {
    const clip = this.clip;
    if (!clip) return null;
    const cut = await clip.cut();
    if (!cut) return null;
    return {
      bytes: new Uint8Array(await cut.blob.arrayBuffer()),
      contentType: cut.blob.type || (cut.format === "mp4" ? "video/mp4" : "video/webm"),
      format: cut.format,
      frameCount: cut.frameCount,
      width: cut.width,
      height: cut.height,
    };
  }

  clipDiscard(): void {
    this.clip?.stop();
    this.clip = null;
  }

  async flushFrame(): Promise<LookoutFrame | null> {
    if (!this.haveFrame) return null;
    return canvasToJpeg(this.full, this.opts.settings.jpegQuality);
  }

  watchForLostSource(): boolean {
    // The stream's own `ended` event is the signal here, not a run of
    // failed grabs.
    return false;
  }
}

/** Build the host object the wasm recorder is constructed with. */
export function createBrowserHost(opts: BrowserHostOptions): LookoutHost {
  const fetchImpl = opts.fetchImpl ?? fetch;
  return {
    clientInfo: opts.clientInfo,
    request: (req) => browserRequest(req, fetchImpl),
    emit: (name, payload) => opts.onEvent(name, payload),
    wantsPreviewFrames: () => opts.wantsPreviewFrames?.() ?? true,
    setTrayTitle: () => {},
    sourceOpen: async (source: LookoutSource) => {
      if (source.type === "camera") await opts.openCamera(source.deviceId);
    },
    sourceClose: (source: LookoutSource) => {
      if (source.type === "camera") opts.closeCamera();
    },
    beginCapture: (source, spec) =>
      opts.captureSession ? opts.captureSession(source, spec) : new BrowserCaptureSession(opts, spec),
    log: opts.log ?? ((line: string) => console.debug(`[lookout] ${line}`)),
  };
}
