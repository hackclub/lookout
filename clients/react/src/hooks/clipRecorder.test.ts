import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  CLIP_FRAME_INTERVAL_MS,
  CLIP_WEBCODECS_QP,
  CLIP_WEBCODECS_QP_STEP,
  FRAMES_PER_CLIP,
  MAX_CLIP_FRAME_OVERRUN,
  MAX_CLIP_BYTES,
} from "@lookout/shared";
import { ClipRecorder, webmHasFrames } from "./clipRecorder.js";

/**
 * A clip is cut by its upload tick, not by a timer — so when uploads run
 * behind, the clip keeps recording. These tests pin the bound on that, which
 * is what stops a bad connection from turning into lost footage:
 *
 * uncapped, a multi-minute stall produced a clip with several times the
 * nominal frame count, which blew MAX_CLIP_BYTES and was refused server-side
 * — costing the entire window — while still only ever rendering as ONE
 * second of output video.
 */

/** Bytes the fake encoder emits per drawn frame — mid-range for 1080p at the
 *  measured web bitrate (see CLIP_WEB_VIDEO_BITS_PER_SECOND's table). */
const BYTES_PER_FRAME = 300_000;

let framesRequested = 0;

class FakeMediaRecorder {
  static isTypeSupported = (mime: string): boolean => mime === "video/mp4;codecs=avc1.640028";
  state = "inactive";
  ondataavailable: ((e: { data: Blob }) => void) | null = null;
  onstop: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(
    _stream: MediaStream,
    public opts: { mimeType: string; videoBitsPerSecond: number },
  ) {}
  start() {
    this.state = "recording";
  }
  stop() {
    this.state = "inactive";
    // One blob sized to the frames the canvas actually pushed.
    this.ondataavailable?.({
      data: new Blob([new Uint8Array(framesRequested * BYTES_PER_FRAME)]),
    });
    this.onstop?.();
  }
}

/** A <video>-alike with decoded dimensions, plus a canvas whose
 *  captureStream/getContext/toBlob are stubbed enough for the recorder. */
function fakeVideo(): HTMLVideoElement {
  return { videoWidth: 1920, videoHeight: 1080 } as HTMLVideoElement;
}

beforeEach(() => {
  framesRequested = 0;
  ClipRecorder.webCodecsDisabled = false;
  vi.stubGlobal("MediaRecorder", FakeMediaRecorder);
  vi.spyOn(document, "createElement").mockImplementation(((tag: string) => {
    if (tag !== "canvas") throw new Error(`unexpected createElement(${tag})`);
    const track = {
      requestFrame: () => {
        framesRequested++;
      },
      stop: () => {},
    };
    const stream = { getVideoTracks: () => [track], getTracks: () => [track] };
    return {
      width: 0,
      height: 0,
      captureStream: () => stream,
      getContext: () => ({ drawImage: () => {}, imageSmoothingQuality: "" }),
      toBlob: (cb: (b: Blob | null) => void) => cb(new Blob(["preview"])),
    };
  }) as typeof document.createElement);
  // isSupported() probes for captureStream on the prototype; happy-dom has
  // no such method, and the recorder never calls the prototype's copy.
  (
    HTMLCanvasElement.prototype as unknown as { captureStream?: () => void }
  ).captureStream ??= () => {};
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("clip frame cap", () => {
  it("stops recording frames once a stalled window hits the cap", async () => {
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS);
    recorder.start();

    const cap = FRAMES_PER_CLIP * MAX_CLIP_FRAME_OVERRUN;
    // Ten intervals' worth of frame ticks — a multi-minute upload stall.
    for (let i = 0; i < cap * 10; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }

    const clip = await recorder.cut();
    expect(clip).not.toBeNull();
    expect(clip!.frameCount).toBeLessThanOrEqual(cap);
    expect(clip!.truncated).toBe(true);
    // The point of the cap: the clip is still small enough to be accepted.
    expect(clip!.blob.size).toBeLessThan(MAX_CLIP_BYTES);
    recorder.stop();
  });

  it("leaves a normal clip untruncated and uncapped", async () => {
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS);
    recorder.start();
    // start() draws one frame; add the rest of a nominal window.
    for (let i = 1; i < FRAMES_PER_CLIP; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }

    const clip = await recorder.cut();
    expect(clip!.truncated).toBe(false);
    // The nominal window's frames plus the one cut() draws to close the clip
    // — the closing frame is also what capturedAt is stamped against.
    expect(clip!.frameCount).toBe(FRAMES_PER_CLIP + 1);
    recorder.stop();
  });

  it("keeps the encoder settings when an oversize clip was merely stalled", async () => {
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS);
    recorder.start();
    const before = (recorder as unknown as { bitrate: number }).bitrate;

    // Force the clip over the byte cap by making each frame enormous, and
    // over the frame cap so it registers as stalled rather than mis-tuned.
    const cap = FRAMES_PER_CLIP * MAX_CLIP_FRAME_OVERRUN;
    for (let i = 0; i < cap * 2; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }
    (recorder as unknown as { maxFrames: number }).maxFrames = 1;

    await recorder.cut();
    // The backoff is permanent, so charging a network stall to the encoder
    // would leave the rest of the session soft.
    expect((recorder as unknown as { bitrate: number }).bitrate).toBe(before);
    recorder.stop();
  });
});

// ── WebCodecs engine selection and fallback ─────────────────────────

/** Minimal WebCodecs stubs. The chunk class doubles as the global because
 *  mp4-muxer type-checks samples with instanceof. */
class FakeEncodedVideoChunk {
  constructor(
    public type: "key" | "delta",
    public timestamp: number,
    public duration: number,
    private data: Uint8Array,
  ) {}
  get byteLength() {
    return this.data.byteLength;
  }
  copyTo(dest: Uint8Array) {
    dest.set(this.data);
  }
}

class FakeVideoFrame {
  constructor(
    public source: unknown,
    public init: { timestamp: number; duration: number },
  ) {}
  close() {}
}

let webCodecsSupported = true;
let webCodecsBytesPerChunk = 1000;
let webCodecsEncodeCalls: Array<{ quantizer?: number }> = [];

class FakeVideoEncoder {
  static isConfigSupported = async (config: {
    bitrateMode?: string;
    codec: string;
  }) => ({
    supported: webCodecsSupported && config.bitrateMode === "quantizer",
    config,
  });

  private emitted = 0;
  constructor(
    private callbacks: {
      output: (chunk: unknown, meta: unknown) => void;
      error: (err: Error) => void;
    },
  ) {}
  configure(_config: unknown) {}
  encode(
    frame: FakeVideoFrame,
    opts?: { keyFrame?: boolean; avc?: { quantizer?: number } },
  ) {
    webCodecsEncodeCalls.push({ quantizer: opts?.avc?.quantizer });
    const meta =
      this.emitted++ === 0
        ? { decoderConfig: { description: new Uint8Array([1, 100, 0, 40]) } }
        : {};
    this.callbacks.output(
      new FakeEncodedVideoChunk(
        opts?.keyFrame ? "key" : "delta",
        frame.init.timestamp,
        frame.init.duration,
        new Uint8Array(webCodecsBytesPerChunk),
      ),
      meta,
    );
  }
  async flush() {}
  close() {}
}

function stubWebCodecs() {
  webCodecsEncodeCalls = [];
  vi.stubGlobal("VideoEncoder", FakeVideoEncoder);
  vi.stubGlobal("VideoFrame", FakeVideoFrame);
  vi.stubGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
}

describe("engine selection", () => {
  beforeEach(() => {
    webCodecsSupported = true;
    webCodecsBytesPerChunk = 1000;
  });

  it("prefers the WebCodecs engine and cuts an MP4", async () => {
    stubWebCodecs();
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS);
    recorder.start();
    // Real frames arrive seconds apart; let the async config probe settle
    // before the synchronous draw loop below.
    await new Promise((r) => setTimeout(r, 0));
    for (let i = 1; i < FRAMES_PER_CLIP; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }

    const clip = await recorder.cut();
    expect(clip).not.toBeNull();
    expect(clip!.format).toBe("mp4");
    expect(clip!.blob.type).toBe("video/mp4");
    // MediaRecorder was never involved.
    expect(framesRequested).toBe(0);
    // The recorder's QP flowed into every frame.
    expect(
      webCodecsEncodeCalls.every((c) => c.quantizer === CLIP_WEBCODECS_QP),
    ).toBe(true);
    expect(ClipRecorder.webCodecsDisabled).toBe(false);
    recorder.stop();
  });

  it("downgrades the session to MediaRecorder after a WebCodecs failure", async () => {
    webCodecsSupported = false; // engine exists but no config is accepted
    stubWebCodecs();
    // Distinct cadence: the engine caches supported configs per
    // dimensions+cadence, and this test needs a fresh (failing) probe.
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS + 1);
    recorder.start();
    await new Promise((r) => setTimeout(r, 0));
    (recorder as unknown as { drawFrame(): void }).drawFrame();

    // The failing engine costs this tick (JPEG fallback) and latches the
    // session away from WebCodecs.
    const first = await recorder.cut();
    expect(first).toBeNull();
    expect(ClipRecorder.webCodecsDisabled).toBe(true);

    // cut() restarted the recorder on the MediaRecorder engine.
    for (let i = 0; i < FRAMES_PER_CLIP; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }
    const second = await recorder.cut();
    expect(second).not.toBeNull();
    expect(second!.format).toBe("mp4");
    expect(framesRequested).toBeGreaterThan(0);
    recorder.stop();
  });

  it("coarsens the quantizer when a normal WebCodecs clip overruns the cap", async () => {
    stubWebCodecs();
    webCodecsBytesPerChunk = Math.ceil((MAX_CLIP_BYTES + 1) / (FRAMES_PER_CLIP + 1));
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS + 2);
    recorder.start();
    await new Promise((r) => setTimeout(r, 0));
    for (let i = 1; i < FRAMES_PER_CLIP; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }

    const clip = await recorder.cut();
    // Oversize clips are withheld (the tick falls back to a JPEG)...
    expect(clip).toBeNull();
    // ...and the next clip encodes coarser, but WebCodecs stays enabled.
    expect((recorder as unknown as { quantizer: number }).quantizer).toBe(
      CLIP_WEBCODECS_QP + CLIP_WEBCODECS_QP_STEP,
    );
    expect(ClipRecorder.webCodecsDisabled).toBe(false);
    recorder.stop();
  });
});

// ── Header-only WebM (Firefox) ──────────────────────────────────────

/** EBML header + Segment + Info + Tracks, no Cluster — what Firefox's
 *  MediaRecorder hands back when its canvas track got no frames. */
const WEBM_HEADER_ONLY = new Uint8Array([
  0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01, 0x42, 0xf7, 0x81, 0x01,
  0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
  0x16, 0x54, 0xae, 0x6b, 0x80,
]);
const WEBM_CLUSTER = new Uint8Array([0x1f, 0x43, 0xb6, 0x75, 0x01, 0xff, 0xa3, 0x81, 0x00]);

let webmPayload: Uint8Array = WEBM_HEADER_ONLY;

class FakeWebmMediaRecorder extends FakeMediaRecorder {
  static isTypeSupported = (mime: string): boolean => mime === "video/webm;codecs=vp8";
  stop() {
    this.state = "inactive";
    this.ondataavailable?.({ data: new Blob([webmPayload as BlobPart]) });
    this.onstop?.();
  }
}

describe("header-only WebM clips", () => {
  beforeEach(() => {
    vi.stubGlobal("MediaRecorder", FakeWebmMediaRecorder);
  });

  it("detects whether a WebM holds a Cluster", async () => {
    expect(await webmHasFrames(new Blob([WEBM_HEADER_ONLY as BlobPart]))).toBe(false);
    expect(
      await webmHasFrames(new Blob([WEBM_HEADER_ONLY as BlobPart, WEBM_CLUSTER as BlobPart])),
    ).toBe(true);
  });

  it("falls back to a JPEG instead of returning a frameless clip", async () => {
    webmPayload = WEBM_HEADER_ONLY;
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS);
    recorder.start();
    for (let i = 1; i < FRAMES_PER_CLIP; i++) {
      (recorder as unknown as { drawFrame(): void }).drawFrame();
    }
    expect(await recorder.cut()).toBeNull();
    // The engine is unaffected — the next clip records normally.
    expect(recorder.recording).toBe(true);
    recorder.stop();
  });

  it("keeps a WebM clip that carries frames", async () => {
    webmPayload = new Uint8Array([...WEBM_HEADER_ONLY, ...WEBM_CLUSTER]);
    const recorder = new ClipRecorder(fakeVideo(), CLIP_FRAME_INTERVAL_MS);
    recorder.start();
    const clip = await recorder.cut();
    expect(clip).not.toBeNull();
    expect(clip!.format).toBe("webm");
    recorder.stop();
  });
});
