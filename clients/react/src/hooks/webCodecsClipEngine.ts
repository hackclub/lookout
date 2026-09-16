import { Muxer, ArrayBufferTarget } from "mp4-muxer";
import {
  nativeClipBitsPerSecond,
  type CaptureFormat,
} from "@lookout/shared";

/** What a clip engine hands back to the recorder at cut time. */
export interface ClipEngineOutput {
  blob: Blob;
  format: Exclude<CaptureFormat, "jpeg">;
}

/** One clip's encode sink. The recorder owns the canvas, the cadence and
 *  the frame cap; an engine only turns drawn frames into a container. */
export interface ClipEngine {
  readonly kind: "webcodecs" | "mediarecorder";
  /** Sink the freshly drawn canvas. `ptsMs` is media time from clip start
   *  (frameIndex × cadence — mirrors the desktop encoders). */
  addFrame(canvas: HTMLCanvasElement, ptsMs: number, keyFrame: boolean): void;
  /** Finalize and return the container, or null when the engine failed or
   *  produced nothing. Idempotent-ish: safe to call once per engine. */
  finish(): Promise<ClipEngineOutput | null>;
  /** Discard everything without producing output. */
  cancel(): void;
}

// TS's lib.dom lags the WebCodecs registry: per-frame H.264 quantizer and
// the "quantizer" bitrate mode are spec'd but not in every lib version.
type AvcEncodeOptions = VideoEncoderEncodeOptions & {
  avc?: { quantizer?: number };
};
type AvcEncoderConfig = VideoEncoderConfig & {
  avc?: { format?: "avc" | "annexb" };
};

/** H.264 codec strings to probe, best profile first. The level byte is
 *  picked from the frame size — level 4.0 tops out at 1080p, so anything
 *  larger asks for 5.1. */
function avcCodecCandidates(width: number, height: number): string[] {
  const level = width * height > 1920 * 1088 ? "33" : "28"; // 5.1 : 4.0
  return [`avc1.6400${level}`, `avc1.4d00${level}`, `avc1.4200${level}`];
}

interface ResolvedConfig {
  config: AvcEncoderConfig;
  /** True when the encoder accepted per-frame-quantizer rate control —
   *  the mode that actually holds constant quality on sparse frames. */
  quantizerMode: boolean;
}

/** isConfigSupported probes are async and identical for every clip of a
 *  session (same dims, same cadence), so resolve once and reuse. */
const configCache = new Map<string, Promise<ResolvedConfig | null>>();

async function resolveConfig(
  width: number,
  height: number,
  frameIntervalMs: number,
): Promise<ResolvedConfig | null> {
  const base = {
    width,
    height,
    // Integer fps hint, exactly as the desktop passes to VideoToolbox —
    // any cadence at or above one second collapses to 1.
    framerate: Math.max(1, Math.round(1000 / Math.max(1, frameIntervalMs))),
    avc: { format: "avc" as const }, // avcC bitstream — what MP4 carries
  };
  for (const codec of avcCodecCandidates(width, height)) {
    // Constant-quality first: per-frame QP is the only rate control that
    // provably ignores our weird (near-zero) frame rate. QP pins the
    // quality directly, so the "quality" latency mode costs nothing and
    // lets the encoder try harder per bit.
    const candidates: ResolvedConfig[] = [
      {
        config: {
          ...base,
          codec,
          hardwareAcceleration: "prefer-hardware",
          latencyMode: "quality" as LatencyMode,
          bitrateMode: "quantizer" as VideoEncoderBitrateMode,
        },
        quantizerMode: true,
      },
      // VBR with real per-frame timestamps — the desktop formula verbatim
      // (bits per second of MEDIA time; see nativeClipBitsPerSecond).
      // "realtime" mirrors the desktop's VideoToolbox input mode AND
      // guarantees no B-frame reordering, which the MP4 muxing here does
      // not handle (chunks are laid down in arrival order).
      {
        config: {
          ...base,
          codec,
          hardwareAcceleration: "prefer-hardware",
          latencyMode: "realtime" as LatencyMode,
          bitrate: nativeClipBitsPerSecond(frameIntervalMs),
          bitrateMode: "variable" as VideoEncoderBitrateMode,
        },
        quantizerMode: false,
      },
      {
        config: {
          ...base,
          codec,
          hardwareAcceleration: "no-preference",
          latencyMode: "quality" as LatencyMode,
          bitrateMode: "quantizer" as VideoEncoderBitrateMode,
        },
        quantizerMode: true,
      },
      {
        config: {
          ...base,
          codec,
          hardwareAcceleration: "no-preference",
          latencyMode: "realtime" as LatencyMode,
          bitrate: nativeClipBitsPerSecond(frameIntervalMs),
          bitrateMode: "variable" as VideoEncoderBitrateMode,
        },
        quantizerMode: false,
      },
    ];
    for (const candidate of candidates) {
      try {
        const support = await VideoEncoder.isConfigSupported(candidate.config);
        if (support.supported) return candidate;
      } catch {
        // Malformed-config TypeError on engines that don't know a field —
        // treat as unsupported and keep probing.
      }
    }
  }
  return null;
}

/**
 * WebCodecs clip engine: VideoEncoder + mp4-muxer.
 *
 * This is the browser path that reaches desktop quality, and for the same
 * reasons the desktop does — the encoder sees each frame's real
 * presentation timestamp (MediaRecorder discards wall-clock spacing
 * entirely; see CLIP_WEB_VIDEO_BITS_PER_SECOND) and, on Chromium, runs in
 * per-frame-quantizer mode so quality is constant instead of rationed.
 * On Safari, VideoEncoder IS VideoToolbox — literally the desktop app's
 * encoder — fed the same derived bitrate and integer-fps hint.
 *
 * Everything here fails soft: configuration resolves asynchronously while
 * early frames buffer, and any error flips `failed` so the recorder falls
 * back to the MediaRecorder engine (and this tick to a JPEG) instead of
 * costing the user a minute.
 */
export class WebCodecsClipEngine implements ClipEngine {
  readonly kind = "webcodecs" as const;

  /** Set on any unrecoverable error. The recorder reads this to decide
   *  whether to stop trying WebCodecs for the rest of the session. */
  failed = false;

  private encoder: VideoEncoder | null = null;
  private chunks: Array<{
    chunk: EncodedVideoChunk;
    meta: EncodedVideoChunkMetadata | undefined;
  }> = [];
  /** Frames drawn before the async config probe resolved. The probe takes
   *  microtasks, frames arrive seconds apart — this should never hold more
   *  than the opening frame. Bounded anyway: a stuck probe must not pin
   *  dozens of raw 1080p frames in memory. */
  private pendingFrames: Array<{ frame: VideoFrame; keyFrame: boolean }> = [];
  private ready: Promise<void>;
  private quantizerMode = false;
  /** No new frames accepted (finish() or cancel() was called). */
  private done = false;
  /** Deliberately discarded — init must not keep building the encoder. */
  private cancelled = false;

  static isSupported(): boolean {
    return (
      typeof VideoEncoder !== "undefined" &&
      typeof VideoFrame !== "undefined" &&
      typeof VideoEncoder.isConfigSupported === "function"
    );
  }

  constructor(
    private width: number,
    private height: number,
    private frameIntervalMs: number,
    /** H.264 QP for quantizer mode. Owned by the recorder so its oversize
     *  backoff persists across clips. Ignored in VBR mode. */
    private quantizer: number,
  ) {
    this.ready = this.init();
  }

  private async init(): Promise<void> {
    try {
      const key = `${this.width}x${this.height}@${this.frameIntervalMs}`;
      let resolved = configCache.get(key);
      if (!resolved) {
        resolved = resolveConfig(this.width, this.height, this.frameIntervalMs);
        configCache.set(key, resolved);
      }
      const picked = await resolved;
      // A finish() racing this probe still wants the encoder built (it
      // awaits `ready` and then flushes); only a cancel() abandons it.
      if (this.cancelled) return;
      if (!picked) {
        // Don't cache the failure: a null verdict costs one re-probe per
        // clip at most (the recorder latches away from WebCodecs after the
        // first failed cut), and it must not fossilize a transient error.
        configCache.delete(key);
        this.fail("no supported H.264 encoder config");
        return;
      }
      this.quantizerMode = picked.quantizerMode;
      this.encoder = new VideoEncoder({
        output: (chunk, meta) => {
          this.chunks.push({ chunk, meta });
        },
        error: (err) => {
          this.fail(`encoder error: ${err?.message ?? err}`);
        },
      });
      this.encoder.configure(picked.config);
      const pending = this.pendingFrames;
      this.pendingFrames = [];
      for (const { frame, keyFrame } of pending) {
        this.encodeFrame(frame, keyFrame);
      }
    } catch (err) {
      this.fail(`encoder init failed: ${err}`);
    }
  }

  private fail(why: string): void {
    if (!this.failed) {
      this.failed = true;
      console.warn(`[lookout] WebCodecs clip engine: ${why}`);
    }
    this.discardPending();
    if (this.encoder) {
      try {
        this.encoder.close();
      } catch {
        // already closed
      }
      this.encoder = null;
    }
  }

  private discardPending(): void {
    for (const { frame } of this.pendingFrames) {
      try {
        frame.close();
      } catch {
        // already closed
      }
    }
    this.pendingFrames = [];
  }

  private encodeFrame(frame: VideoFrame, keyFrame: boolean): void {
    const encoder = this.encoder;
    if (!encoder) {
      frame.close();
      return;
    }
    try {
      const opts: AvcEncodeOptions = { keyFrame };
      if (this.quantizerMode) {
        opts.avc = { quantizer: this.quantizer };
      }
      encoder.encode(frame, opts);
    } catch (err) {
      this.fail(`encode failed: ${err}`);
    } finally {
      try {
        frame.close();
      } catch {
        // encode() may have taken ownership on some engines
      }
    }
  }

  addFrame(canvas: HTMLCanvasElement, ptsMs: number, keyFrame: boolean): void {
    if (this.failed || this.done) return;
    let frame: VideoFrame;
    try {
      frame = new VideoFrame(canvas, {
        timestamp: ptsMs * 1000, // microseconds
        duration: this.frameIntervalMs * 1000,
      });
    } catch (err) {
      this.fail(`VideoFrame from canvas failed: ${err}`);
      return;
    }
    if (!this.encoder) {
      // Config probe still in flight (first frame, typically).
      if (this.pendingFrames.length >= 4) {
        frame.close();
        this.fail("config probe never resolved");
        return;
      }
      this.pendingFrames.push({ frame, keyFrame });
      return;
    }
    this.encodeFrame(frame, keyFrame);
  }

  async finish(): Promise<ClipEngineOutput | null> {
    this.done = true;
    await this.ready;
    this.discardPending();
    const encoder = this.encoder;
    if (this.failed || !encoder) return null;
    try {
      await encoder.flush();
    } catch (err) {
      this.fail(`flush failed: ${err}`);
      return null;
    }
    try {
      encoder.close();
    } catch {
      // already closed
    }
    this.encoder = null;
    if (this.chunks.length === 0) return null;

    // Chunks must be in presentation order — an encoder that reorders
    // (B-frames) would need composition offsets this muxing doesn't write.
    // No probed config should produce them; refuse rather than ship a
    // container with scrambled timestamps.
    for (let i = 1; i < this.chunks.length; i++) {
      if (this.chunks[i].chunk.timestamp <= this.chunks[i - 1].chunk.timestamp) {
        this.fail("encoder emitted reordered frames");
        return null;
      }
    }

    try {
      const target = new ArrayBufferTarget();
      const muxer = new Muxer({
        target,
        video: { codec: "avc", width: this.width, height: this.height },
        // Whole clip is in memory anyway; a front-loaded moov lets the
        // worker (and any preview player) read it in one pass.
        fastStart: "in-memory",
      });
      for (const { chunk, meta } of this.chunks) {
        muxer.addVideoChunk(chunk, meta);
      }
      muxer.finalize();
      return {
        blob: new Blob([target.buffer], { type: "video/mp4" }),
        format: "mp4",
      };
    } catch (err) {
      this.fail(`mp4 mux failed: ${err}`);
      return null;
    } finally {
      this.chunks = [];
    }
  }

  cancel(): void {
    // Deliberate teardown (pause/stop/unmount) — not a failure: it must
    // never warn, and never count against WebCodecs for the session.
    this.done = true;
    this.cancelled = true;
    this.discardPending();
    if (this.encoder) {
      try {
        this.encoder.close();
      } catch {
        // already closed
      }
      this.encoder = null;
    }
    this.chunks = [];
  }
}
