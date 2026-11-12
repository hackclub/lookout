import {
  MAX_WIDTH,
  MAX_HEIGHT,
  JPEG_QUALITY,
  CLIP_WEB_VIDEO_BITS_PER_SECOND,
  CLIP_WEB_MIN_BITS_PER_SECOND,
  CLIP_WEBCODECS_QP,
  CLIP_WEBCODECS_QP_STEP,
  CLIP_WEBCODECS_QP_MAX,
  MAX_CLIP_FRAME_OVERRUN,
  MAX_CLIP_BYTES,
  SCREENSHOT_INTERVAL_MS,
  type CaptureFormat,
} from "@lookout/shared";
import { drawScaledHQ } from "./hqScale.js";
import {
  WebCodecsClipEngine,
  type ClipEngine,
  type ClipEngineOutput,
} from "./webCodecsClipEngine.js";

/** One finalized per-minute clip, ready for the upload pipeline. */
export interface ClipCaptureResult {
  blob: Blob;
  format: Exclude<CaptureFormat, "jpeg">;
  width: number;
  height: number;
  /** Frames drawn into the clip. Informational — the server/worker derive
   *  the real count by demuxing. */
  frameCount: number;
  /** True when the clip hit its frame cap, i.e. the window it covers ran
   *  long because the previous upload was still draining. The clip is
   *  still perfectly usable; the caller may want to log the stall. */
  truncated: boolean;
  /** Client-clock ms timestamp stamped at cut time — the clip's capture
   *  moment for credit-mode purposes (one clip = one capture unit). */
  capturedAtMs: number;
  /** JPEG snapshot of the clip's last frame, for the UI preview only. */
  previewBlob: Blob | null;
}

interface MimeCandidate {
  mime: string;
  format: Exclude<CaptureFormat, "jpeg">;
}

/** MediaRecorder preference order: H.264/MP4 first, WebM only as a fallback
 *  for engines that cannot record MP4 (Firefox).
 *
 *  This is the opposite of what raw compression efficiency suggests, and
 *  it is deliberate. Our content is sparse 1080p screen frames, which is
 *  exactly where the browsers' realtime libvpx configuration falls apart.
 *  Measured at matched output size (Chromium 148, 1080p, PSNR vs source):
 *
 *      ~115 KB/frame    H.264 30.9 dB    VP9 22.3 dB
 *      ~190 KB/frame    H.264 34.3 dB    VP9 24.7 dB
 *      ~335 KB/frame    H.264 38.6 dB    VP9 28.6 dB
 *
 *  H.264 is 8-10 dB better for the same bytes, and its quality is even
 *  across the clip, where VP9 spends nearly everything on the keyframe
 *  and leaves the other 14 frames soft. (VP8 is worse still: its rate
 *  control is inert below ~10 Mbps — identical bytes at 0.8M, 2M and 5M.)
 *  This mirrors the desktop app's own benchmarks, which rejected libvpx
 *  for the same workload.
 *
 *  The profile-specific strings come first so we get High profile where
 *  it is offered; bare "video/mp4" is the Safari path. */
const MIME_CANDIDATES: MimeCandidate[] = [
  { mime: "video/mp4;codecs=avc1.640028", format: "mp4" }, // H.264 High 4.0
  { mime: "video/mp4;codecs=avc1.4d0028", format: "mp4" }, // H.264 Main 4.0
  { mime: "video/mp4;codecs=avc1.42e01e", format: "mp4" }, // H.264 Baseline 3.0
  { mime: "video/mp4", format: "mp4" },
  { mime: "video/webm;codecs=vp9", format: "webm" },
  { mime: "video/webm;codecs=vp8", format: "webm" },
  { mime: "video/webm", format: "webm" },
];

function pickMimeCandidate(): MimeCandidate | null {
  if (typeof MediaRecorder === "undefined") return null;
  for (const c of MIME_CANDIDATES) {
    try {
      if (MediaRecorder.isTypeSupported(c.mime)) return c;
    } catch {
      // isTypeSupported can throw on exotic UAs — treat as unsupported
    }
  }
  return null;
}

/** EBML ID of a WebM Cluster — the element that carries encoded frames. */
const WEBM_CLUSTER_ID = [0x1f, 0x43, 0xb6, 0x75];

/**
 * Whether a WebM recording holds any frames at all.
 *
 * Firefox's MediaRecorder can stop() cleanly yet hand back nothing but the
 * container header (~217 bytes: EBML + Segment + Info + Tracks, no Cluster)
 * when the canvas track delivered no frames during the clip — see Mozilla
 * bug 1344524. Such a clip is undecodable; uploaded, it costs the minute
 * anyway and fails the session's compile. Exported for tests.
 */
export async function webmHasFrames(blob: Blob): Promise<boolean> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const [a, b, c, d] = WEBM_CLUSTER_ID;
  for (let i = 0; i + 3 < bytes.length; i++) {
    if (bytes[i] === a && bytes[i + 1] === b && bytes[i + 2] === c && bytes[i + 3] === d)
      return true;
  }
  return false;
}

function canvasCaptureSupported(): boolean {
  return (
    typeof HTMLCanvasElement !== "undefined" &&
    typeof HTMLCanvasElement.prototype.captureStream === "function"
  );
}

/**
 * The legacy encode sink: canvas.captureStream() into a bitrate-capped
 * MediaRecorder. Still the path for engines without WebCodecs H.264
 * support (notably Firefox), and the fallback whenever the WebCodecs
 * engine fails. Its known quality ceiling — realtime rate control that
 * ignores wall-clock frame spacing — is documented at
 * CLIP_WEB_VIDEO_BITS_PER_SECOND.
 */
class MediaRecorderClipEngine implements ClipEngine {
  readonly kind = "mediarecorder" as const;
  private stream: MediaStream;
  private recorder: MediaRecorder;
  private parts: Blob[] = [];
  private format: Exclude<CaptureFormat, "jpeg">;
  private contentType: string;

  constructor(canvas: HTMLCanvasElement, mime: MimeCandidate, bitrate: number) {
    // captureStream(0) = frames only on explicit requestFrame(), keeping
    // encode work at exactly our cadence. Some engines put requestFrame on
    // the track (spec), others on the stream (older Firefox), some lack it
    // entirely — fall back to auto-capture on canvas change.
    let stream: MediaStream;
    try {
      stream = canvas.captureStream(0);
      if (!this.streamHasRequestFrame(stream)) {
        stream.getTracks().forEach((t) => t.stop());
        stream = canvas.captureStream();
      }
    } catch {
      stream = canvas.captureStream();
    }
    this.stream = stream;
    this.format = mime.format;
    this.contentType = mime.mime.split(";")[0];
    this.recorder = new MediaRecorder(stream, {
      mimeType: mime.mime,
      videoBitsPerSecond: bitrate,
    });
    this.recorder.ondataavailable = (e) => {
      if (e.data && e.data.size > 0) this.parts.push(e.data);
    };
    this.recorder.start();
  }

  private streamHasRequestFrame(stream: MediaStream): boolean {
    const track = stream.getVideoTracks()[0] as MediaStreamTrack & {
      requestFrame?: () => void;
    };
    const s = stream as MediaStream & { requestFrame?: () => void };
    return (
      typeof track?.requestFrame === "function" ||
      typeof s.requestFrame === "function"
    );
  }

  addFrame(): void {
    const track = this.stream.getVideoTracks()[0] as
      | (MediaStreamTrack & { requestFrame?: () => void })
      | undefined;
    if (typeof track?.requestFrame === "function") {
      track.requestFrame();
    } else {
      (
        this.stream as MediaStream & { requestFrame?: () => void }
      ).requestFrame?.();
    }
  }

  async finish(): Promise<ClipEngineOutput | null> {
    const recorder = this.recorder;
    const stopped = new Promise<void>((resolve) => {
      recorder.onstop = () => resolve();
      recorder.onerror = () => resolve();
    });
    try {
      recorder.stop();
    } catch {
      // stop() throws if already inactive — treat as stopped
    }
    // Never let a wedged encoder stall the capture loop.
    await Promise.race([
      stopped,
      new Promise<void>((resolve) => setTimeout(resolve, 10_000)),
    ]);
    this.stream.getTracks().forEach((t) => t.stop());
    if (this.parts.length === 0) return null;
    const blob = new Blob(this.parts, { type: this.contentType });
    // A header-only WebM is "no output": cut() turns it into this tick's
    // JPEG fallback instead of uploading a clip nothing can decode.
    if (this.format === "webm" && !(await webmHasFrames(blob))) {
      console.warn(
        `[lookout] MediaRecorder clip held no frames (${blob.size} bytes) — ` +
          "falling back to a JPEG for this capture.",
      );
      return null;
    }
    return { blob, format: this.format };
  }

  cancel(): void {
    try {
      if (this.recorder.state !== "inactive") this.recorder.stop();
    } catch {
      // already inactive
    }
    this.stream.getTracks().forEach((t) => t.stop());
    this.parts = [];
  }
}

/** Per-recorder display knobs. Cadence and rate control are deliberately
 *  NOT options: the frame interval is server-authoritative (constructor
 *  arg, from the session response) and the quality dials are the shared
 *  constants. */
export interface ClipRecorderOptions {
  maxWidth?: number;
  maxHeight?: number;
  jpegQuality?: number;
  /** The caller drives the frame cadence by calling `recordFrame()` (the
   *  shared capture loop does, at the server's interval); the recorder
   *  runs no timer of its own and `cut()` does not draw a final frame —
   *  the caller already recorded it. */
  externalCadence?: boolean;
  /** @deprecated Faster cadence for the FIRST clip only, from when the
   *  opening clip was cut after CLIP_FIRST_CUT_DELAY_MS. The seed tick now
   *  fires immediately (see useLookout), so there is no opening window to
   *  densify — useLookout no longer passes this. Kept for API compat;
   *  after the first cut the recorder reverts to `frameIntervalMs`. */
  openingFrameIntervalMs?: number;
}

/**
 * Records the shared screen into per-minute video clips.
 *
 * Owns an offscreen canvas fed from the caller's `<video>` (the
 * getDisplayMedia sink) every `frameIntervalMs`, sunk into an encode
 * engine. `cut()` finalizes the current clip and immediately starts the
 * next one, so the serial per-minute upload pipeline stays exactly as it
 * is for JPEGs — a clip is one capture unit.
 *
 * Engine preference: WebCodecs (VideoEncoder + MP4 mux — desktop-class
 * quality, see webCodecsClipEngine.ts) when the browser supports H.264
 * encoding there, else canvas.captureStream + MediaRecorder. Any WebCodecs
 * failure permanently downgrades the session to MediaRecorder, and any
 * unusable cut falls back to a single JPEG for that tick — clips are an
 * enhancement, a capture a minute is the contract.
 *
 * Clips are VFR: on a static screen the encoder legitimately emits few
 * frames. That's fine — the worker demuxes and normalizes each clip to
 * one second of output video regardless of frame count.
 */
export class ClipRecorder {
  /** Session-wide latch: once the WebCodecs engine fails, stop offering it.
   *  Static so every recorder (and every restart) sees the verdict. */
  static webCodecsDisabled = false;

  private video: HTMLVideoElement;
  private frameIntervalMs: number;
  private openingFrameIntervalMs: number | null;
  private canvas: HTMLCanvasElement | null = null;
  private engine: ClipEngine | null = null;
  private frameCount = 0;
  private frameTimer: ReturnType<typeof setInterval> | null = null;
  private mime: MimeCandidate | null;
  /** Live MediaRecorder-engine bitrate. Starts at the measured-optimal web
   *  rate and halves whenever a finished clip overruns MAX_CLIP_BYTES — see
   *  `cut()`. Browsers whose rate control does honour real frame spacing
   *  would otherwise blow the cap on every single clip. */
  private bitrate = CLIP_WEB_VIDEO_BITS_PER_SECOND;
  /** Live WebCodecs-engine quantizer (H.264 QP). Coarsens when a finished
   *  clip overruns MAX_CLIP_BYTES — the constant-quality analogue of the
   *  bitrate backoff above. */
  private quantizer = CLIP_WEBCODECS_QP;
  /** Hard frame cap for one clip — see MAX_CLIP_FRAME_OVERRUN. Derived from
   *  the SERVER's cadence, not the default constant, so a server that
   *  dictates a different frameIntervalMs still gets a correct cap. */
  private maxFrames: number;
  private externalCadence: boolean;
  // Opening cadence lives in its own field (cleared after the first cut),
  // so it's excluded from the always-resolved options.
  private opts: Required<Omit<ClipRecorderOptions, "openingFrameIntervalMs" | "externalCadence">>;

  static isSupported(): boolean {
    return (
      WebCodecsClipEngine.isSupported() ||
      (canvasCaptureSupported() && pickMimeCandidate() !== null)
    );
  }

  constructor(
    video: HTMLVideoElement,
    frameIntervalMs: number,
    opts?: ClipRecorderOptions,
  ) {
    const mime = pickMimeCandidate();
    if (!mime && !WebCodecsClipEngine.isSupported()) {
      throw new Error("Clip recording not supported in this browser");
    }
    this.video = video;
    this.frameIntervalMs = frameIntervalMs;
    this.openingFrameIntervalMs = opts?.openingFrameIntervalMs ?? null;
    this.externalCadence = opts?.externalCadence ?? false;
    this.maxFrames =
      Math.ceil(SCREENSHOT_INTERVAL_MS / Math.max(1, frameIntervalMs)) *
      MAX_CLIP_FRAME_OVERRUN;
    this.mime = mime;
    this.opts = {
      maxWidth: opts?.maxWidth ?? MAX_WIDTH,
      maxHeight: opts?.maxHeight ?? MAX_HEIGHT,
      jpegQuality: opts?.jpegQuality ?? JPEG_QUALITY,
    };
  }

  /** Start recording a fresh clip. No-op if already recording. */
  start(): void {
    if (this.engine) return;
    if (this.video.videoWidth === 0 || this.video.videoHeight === 0) {
      throw new Error("Video not ready — cannot start clip recorder");
    }

    const scale = Math.min(
      this.opts.maxWidth / this.video.videoWidth,
      this.opts.maxHeight / this.video.videoHeight,
      1,
    );
    const canvas = document.createElement("canvas");
    // Encoder-friendly even dimensions; the size is fixed for the clip's
    // life (both engines require a constant stream resolution).
    canvas.width = Math.max(2, Math.round((this.video.videoWidth * scale) / 2) * 2);
    canvas.height = Math.max(2, Math.round((this.video.videoHeight * scale) / 2) * 2);
    this.canvas = canvas;
    this.frameCount = 0;

    if (!ClipRecorder.webCodecsDisabled && WebCodecsClipEngine.isSupported()) {
      this.engine = new WebCodecsClipEngine(
        canvas.width,
        canvas.height,
        this.frameIntervalMs,
        this.quantizer,
      );
    } else if (this.mime && canvasCaptureSupported()) {
      this.engine = new MediaRecorderClipEngine(canvas, this.mime, this.bitrate);
    } else {
      this.canvas = null;
      throw new Error("No clip encoder available");
    }

    this.drawFrame();
    if (this.externalCadence) return;
    const cadence = this.openingFrameIntervalMs ?? this.frameIntervalMs;
    this.frameTimer = setInterval(() => this.drawFrame(), cadence);
  }

  /** Record the current video frame into the clip (external cadence).
   *  Starts a clip if none is in progress. Throws when the video is not
   *  ready or no encoder is available. */
  recordFrame(): void {
    if (!this.engine) this.start();
    this.drawFrame();
  }

  /** Whether a clip is in progress. */
  get recording(): boolean {
    return this.engine !== null;
  }

  private drawFrame(): void {
    const canvas = this.canvas;
    if (!canvas || this.video.videoWidth === 0 || this.video.videoHeight === 0)
      return;
    // Frame cap. A clip is cut by its upload tick, so a slow uplink stretches
    // the window this clip covers — and every extra frame is more bytes
    // against MAX_CLIP_BYTES, for a clip that renders as one second either
    // way. Past the cap, stop feeding the encoder and stop the timer: the
    // clip stays uploadable, and we stop burning CPU compositing frames
    // nothing will ever see.
    if (this.frameCount >= this.maxFrames) {
      if (this.frameTimer) {
        clearInterval(this.frameTimer);
        this.frameTimer = null;
      }
      return;
    }
    // Stepped high-quality downscale — the video is native resolution now
    // (a Retina display hands us 4-5K), and a single bilinear drawImage of
    // that aliases UI text badly. The desktop client area-averages for the
    // same reason.
    drawScaledHQ(this.video, this.video.videoWidth, this.video.videoHeight, canvas);
    // Media time mirrors the desktop encoders: frame index × nominal
    // cadence, independent of wall clock.
    this.engine?.addFrame(
      canvas,
      this.frameCount * this.frameIntervalMs,
      this.frameCount === 0,
    );
    this.frameCount++;
  }

  /**
   * Finalize the current clip and immediately start the next one.
   * Returns null when the clip is unusable (no frames / empty blob) —
   * callers should fall back to a single-JPEG capture for that tick.
   */
  async cut(): Promise<ClipCaptureResult | null> {
    const engine = this.engine;
    const canvas = this.canvas;
    if (!engine || !canvas) return null;

    // Final frame + timestamp: capturedAt is the moment the clip is cut,
    // which is what keeps the per-minute credit cadence monotonic. Under
    // an external cadence the caller recorded the final frame itself.
    if (!this.externalCadence) this.drawFrame();
    const capturedAtMs = Date.now();
    const frameCount = this.frameCount;
    const truncated = frameCount >= this.maxFrames;
    if (this.frameTimer) {
      clearInterval(this.frameTimer);
      this.frameTimer = null;
    }

    // Never let a wedged encoder stall the capture loop.
    const TIMED_OUT = Symbol("finish timeout");
    let out = await Promise.race([
      engine.finish(),
      new Promise<typeof TIMED_OUT>((resolve) =>
        setTimeout(() => resolve(TIMED_OUT), 10_000),
      ),
    ]);
    if (out === TIMED_OUT) {
      engine.cancel();
      out = null;
    }

    const width = canvas.width;
    const height = canvas.height;
    const previewBlob = await new Promise<Blob | null>((resolve) => {
      canvas.toBlob((b) => resolve(b), "image/jpeg", this.opts.jpegQuality);
      setTimeout(() => resolve(null), 5_000);
    });

    // A WebCodecs engine that ate frames and produced nothing is broken
    // for this browser — downgrade the session to MediaRecorder rather
    // than losing a capture a minute to the same failure.
    if (!out && frameCount > 0 && engine.kind === "webcodecs") {
      ClipRecorder.webCodecsDisabled = true;
      console.warn(
        "[lookout] WebCodecs clip engine produced no output — falling back " +
          "to MediaRecorder for the rest of this session.",
      );
    }

    const blob = out && frameCount > 0 && out.blob.size > 0 ? out.blob : null;

    // Oversize clips are rejected server-side (HeadObject vs
    // MAX_CLIP_BYTES), which would cost the whole minute. Back the active
    // engine's quality dial off rather than upload a clip we know will be
    // refused; the floor/ceiling is the coarsest setting worth uploading.
    let oversize = false;
    if (blob && blob.size > MAX_CLIP_BYTES) {
      oversize = true;
      // ...but only when the clip was a NORMAL one. A truncated clip is
      // oversize because the network stalled and it covers several minutes,
      // not because the encoder is mis-tuned. The backoff is permanent
      // (it never ratchets back), so blaming the encoder for a network
      // event would leave the rest of the session soft — the exact
      // failure mode where a user on bad wifi ends up with a worse
      // timelapse than one on no wifi at all.
      if (truncated) {
        console.warn(
          `[lookout] clip was ${blob.size} bytes (cap ${MAX_CLIP_BYTES}) after ` +
            `running long on a slow upload — keeping encoder settings`,
        );
      } else if (engine.kind === "webcodecs") {
        const coarser = Math.min(
          CLIP_WEBCODECS_QP_MAX,
          this.quantizer + CLIP_WEBCODECS_QP_STEP,
        );
        if (coarser !== this.quantizer) {
          console.warn(
            `[lookout] clip was ${blob.size} bytes (cap ${MAX_CLIP_BYTES}) — ` +
              `coarsening encoder QP ${this.quantizer} -> ${coarser}`,
          );
          this.quantizer = coarser;
        }
      } else {
        const reduced = Math.max(
          CLIP_WEB_MIN_BITS_PER_SECOND,
          Math.round(this.bitrate / 2),
        );
        if (reduced !== this.bitrate) {
          console.warn(
            `[lookout] clip was ${blob.size} bytes (cap ${MAX_CLIP_BYTES}) — ` +
              `dropping encoder bitrate ${this.bitrate} -> ${reduced}`,
          );
          this.bitrate = reduced;
        }
      }
    }

    // Tear down and restart for the next minute — after the size check, so
    // any backoff above applies to the clip we're about to start. The
    // opening cadence only ever applies to the first clip; every later
    // interval is full-length.
    this.openingFrameIntervalMs = null;
    this.teardown();
    try {
      this.start();
    } catch {
      // Video may be momentarily not-ready; the caller's next tick falls
      // back to JPEG and recording resumes when start() next succeeds.
    }

    // A null/empty/oversize clip falls back to a single JPEG for this
    // tick, so the capture cadence and credit streak never skip.
    if (!blob || !out || oversize) return null;

    return {
      blob,
      format: out.format,
      width,
      height,
      frameCount,
      truncated,
      capturedAtMs,
      previewBlob,
    };
  }

  /** Stop and discard the in-progress clip (pause/stop/unmount). */
  stop(): void {
    this.engine?.cancel();
    this.teardown();
  }

  private teardown(): void {
    if (this.frameTimer) {
      clearInterval(this.frameTimer);
      this.frameTimer = null;
    }
    this.engine = null;
    this.canvas = null;
    this.frameCount = 0;
  }
}
