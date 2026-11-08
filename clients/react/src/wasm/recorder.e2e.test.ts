/**
 * The shared recorder, running as the real WebAssembly build, driven
 * through the browser host against a fake Lookout server — the same
 * ordering the desktop tests pin, proven on the browser path:
 *
 *   - opening an active session seeds a capture at once;
 *   - pause flushes the partial minute (`final: true`) BEFORE the pause
 *     POST, and the uploads stay in order;
 *   - resume POSTs first, then seeds again — stamped at the resume;
 *   - stop renames, stops with retries, and leaves the session stopped;
 *   - the host's camera is opened before the loop starts and released on
 *     pause.
 *
 * Requires `packages/wasm/dist` (run `npm run build -w packages/wasm`).
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import type {
  LookoutCaptureSession,
  LookoutClip,
  LookoutFrame,
  LookoutRecorder,
} from "@lookout/wasm";
import { createBrowserHost } from "./browserHost.js";
import { createRecorder, setRecorderTiming, setWasmInput } from "./loadRecorder.js";
import type { RecorderState } from "./recorderState.js";

// vitest runs from the package directory; happy-dom rewrites
// import.meta.url, so resolve from the process instead.
const WASM = resolve(process.cwd(), "../../packages/wasm/dist/lookout_web_bg.wasm");

/** Two real seconds stand in for a minute. */
const INTERVAL_MS = 2_000;
const BASE = "https://api.test";
const TOKEN = "t".repeat(64);

interface Req {
  method: string;
  path: string;
  body: string;
}

/** A fake Lookout server behind `fetch`: the session lifecycle and the
 *  presign → PUT → confirm pipeline, with a request log. Credits a flat
 *  interval per confirm after the seed, a fixed partial for a `final`. */
class FakeServer {
  status = "active";
  tracked = 0;
  shots = 0;
  requests: Req[] = [];
  failNext: Array<{ method: string; path: string; status: number }> = [];

  fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
    const method = init?.method ?? "GET";
    let body = "";
    if (typeof init?.body === "string") body = init.body;
    else if (init?.body instanceof Uint8Array) body = `<${init.body.byteLength} bytes>`;
    this.requests.push({ method, path: url.pathname, body });
    const planned = this.failNext.findIndex((f) => f.method === method && f.path === url.pathname);
    if (planned >= 0) {
      const [f] = this.failNext.splice(planned, 1);
      return new Response(JSON.stringify({ error: "injected" }), { status: f.status });
    }
    const json = (v: unknown, status = 200) =>
      new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json" } });
    const now = new Date().toISOString();
    const next = new Date(Date.now() + INTERVAL_MS).toISOString();
    const p = url.pathname;
    if (p === `/api/sessions/${TOKEN}` && method === "GET") {
      return json({
        status: this.status,
        name: "Test",
        trackedSeconds: this.tracked,
        screenshotCount: this.shots,
        startedAt: null,
        createdAt: now,
        totalActiveSeconds: 0,
        clipsEnabled: false,
      });
    }
    if (p.endsWith("/status")) return json({ status: this.status, trackedSeconds: this.tracked });
    if (p.endsWith("/pause")) {
      if (this.status === "active") this.status = "paused";
      return json({ status: "paused", totalActiveSeconds: 0 });
    }
    if (p.endsWith("/resume")) {
      if (this.status !== "paused") return json({ error: `Session is ${this.status}` }, 409);
      this.status = "active";
      return json({ status: "active", nextExpectedAt: next, serverTime: now });
    }
    if (p.endsWith("/stop")) {
      this.status = "stopped";
      return json({ status: "stopped", trackedSeconds: this.tracked, totalActiveSeconds: 0 });
    }
    if (p.endsWith("/name")) return json({ name: "x" });
    if (p.endsWith("/upload-url")) {
      if (this.status !== "active") return json({ error: "paused" }, 409);
      this.shots += 1;
      return json({
        uploadUrl: `${BASE}/r2/${this.shots}`,
        r2Key: `k/${this.shots}`,
        screenshotId: `shot-${this.shots}`,
        minuteBucket: 0,
        nextExpectedAt: next,
        serverTime: now,
        format: "jpeg",
      });
    }
    if (p.startsWith("/r2/")) return new Response("", { status: 200 });
    if (p.endsWith("/screenshots")) {
      if (this.status !== "active") return json({ error: "paused" }, 409);
      const isFinal = body.includes('"final":true');
      if (isFinal) this.tracked += 1;
      else if (this.shots > 1) this.tracked += INTERVAL_MS / 1000;
      return json({ confirmed: true, trackedSeconds: this.tracked, nextExpectedAt: next, serverTime: now });
    }
    return json({ error: "not found" }, 404);
  };

  paths(): string[] {
    return this.requests.map((r) => `${r.method} ${r.path}`);
  }

  finals(): number {
    return this.requests.filter((r) => r.body.includes('"final":true')).length;
  }
}

const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xd9]);

/** A capture session that always has a frame and never a clip. */
function fakeCapture(log: string[]): LookoutCaptureSession {
  return {
    async grab(jpegMode): Promise<{ jpeg?: LookoutFrame }> {
      log.push(`grab:${jpegMode}`);
      return jpegMode === "none" ? {} : { jpeg: { bytes: JPEG, width: 4, height: 3 } };
    },
    async clipCut(): Promise<LookoutClip | null> {
      return null;
    },
    clipDiscard() {},
    async flushFrame(): Promise<LookoutFrame | null> {
      log.push("flush");
      return { bytes: JPEG, width: 4, height: 3 };
    },
    watchForLostSource: () => false,
  };
}

interface Harness {
  server: FakeServer;
  recorder: LookoutRecorder;
  states: RecorderState[];
  events: string[];
  log: string[];
  camera: string[];
}

async function harness(status = "active", source: "screen" | "camera" = "camera"): Promise<Harness> {
  const server = new FakeServer();
  server.status = status;
  const states: RecorderState[] = [];
  const events: string[] = [];
  const log: string[] = [];
  const camera: string[] = [];
  const host = createBrowserHost({
    getVideo: () => null,
    openCamera: async (id) => {
      camera.push(`open:${id}`);
    },
    closeCamera: () => {
      camera.push("close");
    },
    settings: { maxWidth: 1920, maxHeight: 1080, jpegQuality: 0.85 },
    clientInfo: "Lookout Test/0.0.0",
    onEvent: (name, payload) => {
      events.push(name);
      if (name === "recorder-state" && payload) states.push(payload as RecorderState);
    },
    fetchImpl: server.fetch,
    captureSession: () => fakeCapture(log),
    log: () => {},
  });
  const recorder = await createRecorder(host);
  const src = source === "camera" ? { type: "camera", deviceId: "cam-1" } : { type: "screen", sources: [] };
  const state = (await recorder.open(TOKEN, BASE, src)) as RecorderState;
  states.push(state);
  return { server, recorder, states, events, log, camera };
}

async function until(pred: () => boolean, ms = 5_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!pred()) {
    if (Date.now() > deadline) throw new Error("timed out waiting");
    await new Promise((r) => setTimeout(r, 20));
  }
}

const hasWasm = existsSync(WASM);
// WASM resolves from the working directory, so running vitest from anywhere
// but this package skips the suite. In CI a silent skip means the browser
// recorder went untested, so fail instead.
if (!hasWasm && process.env.CI) {
  throw new Error(`${WASM} is missing: run \`npm run build -w packages/wasm\` before these tests`);
}
const d = hasWasm ? describe : describe.skip;

d("the shared recorder in the browser host", () => {
  beforeAll(() => {
    setWasmInput(readFileSync(WASM));
    setRecorderTiming({
      captureIntervalMs: INTERVAL_MS,
      retryDelaysMs: [60, 120, 240],
      stepTimeoutMs: 1_000,
      apiTimeoutMs: 1_000,
      backlogMaxAgeMs: 8_000,
      flushBudgetMs: 2_000,
    });
  });

  let h: Harness | null = null;
  afterEach(async () => {
    if (h) {
      await h.recorder.close();
      h.recorder.free();
      h = null;
    }
  });

  it("takes the SDK's retry settings through setTiming, defaulting attempts to three", async () => {
    h = await harness("active");
    // The override in beforeAll predates maxAttempts and leaves it out.
    expect(h.recorder.timing().maxAttempts).toBe(3);
    h.recorder.setTiming({ ...h.recorder.timing(), maxAttempts: 6, retryDelaysMs: [10, 20, 30] });
    expect(h.recorder.timing()).toMatchObject({ maxAttempts: 6, retryDelaysMs: [10, 20, 30], captureIntervalMs: INTERVAL_MS });
  });

  it("opens an active session, opens the camera first, and seeds a capture at once", async () => {
    h = await harness("active");
    const { server, states, camera, log } = h;
    expect(states.at(-1)?.phase).toBe("recording");
    expect(camera[0]).toBe("open:cam-1");
    await until(() => server.paths().includes(`POST /api/sessions/${TOKEN}/screenshots`));
    const paths = server.paths();
    expect(paths[0]).toBe(`GET /api/sessions/${TOKEN}`);
    expect(paths.indexOf(`GET /api/sessions/${TOKEN}/upload-url`)).toBeGreaterThan(0);
    expect(log[0]).toBe("grab:full");
    // The seed is stamped: capturedAt travels on the presign.
    const presign = server.requests.find((r) => r.path.endsWith("/upload-url"));
    expect(presign).toBeDefined();
    await until(() => (h?.states.at(-1)?.captureCount ?? 0) >= 1);
  });

  it("pause flushes the partial minute before the pause POST and releases the camera", async () => {
    h = await harness("active");
    const { server, recorder, camera } = h;
    await until(() => server.paths().includes(`POST /api/sessions/${TOKEN}/screenshots`));
    const state = (await recorder.pause()) as RecorderState;
    expect(state.phase).toBe("paused");
    expect(server.status).toBe("paused");
    const paths = server.paths();
    const lastConfirm = paths.lastIndexOf(`POST /api/sessions/${TOKEN}/screenshots`);
    const pause = paths.indexOf(`POST /api/sessions/${TOKEN}/pause`);
    expect(server.finals()).toBe(1);
    expect(lastConfirm).toBeLessThan(pause);
    expect(camera.at(-1)).toBe("close");
    // The clock stopped; the credited time is the server's.
    expect(state.clock.running).toBe(false);
    expect(state.trackedSeconds).toBe(server.tracked);
  });

  it("resume POSTs first, then reopens the camera and seeds again", async () => {
    h = await harness("paused");
    const { server, recorder, camera } = h;
    expect(h.states.at(-1)?.phase).toBe("paused");
    expect(server.paths().some((p) => p.endsWith("/upload-url"))).toBe(false);
    const state = (await recorder.resume()) as RecorderState;
    expect(state.phase).toBe("recording");
    await until(() => server.paths().includes(`POST /api/sessions/${TOKEN}/screenshots`));
    const paths = server.paths();
    const resume = paths.indexOf(`POST /api/sessions/${TOKEN}/resume`);
    const presign = paths.indexOf(`GET /api/sessions/${TOKEN}/upload-url`);
    expect(resume).toBeGreaterThanOrEqual(0);
    expect(presign).toBeGreaterThan(resume);
    expect(camera[0]).toBe("open:cam-1");
  });

  it("a failed pause puts the loop back", async () => {
    h = await harness("active");
    const { server, recorder } = h;
    await until(() => server.paths().includes(`POST /api/sessions/${TOKEN}/screenshots`));
    server.failNext.push({ method: "POST", path: `/api/sessions/${TOKEN}/pause`, status: 500 });
    await expect(recorder.pause()).rejects.toMatchObject({ kind: "api", status: 500 });
    const state = recorder.state() as RecorderState;
    expect(state.phase).toBe("recording");
    expect(state.actionError).toContain("500");
    expect(server.status).toBe("active");
  });

  it("stop pauses into the prompt, then renames and stops", async () => {
    h = await harness("active");
    const { server, recorder } = h;
    await until(() => server.paths().includes(`POST /api/sessions/${TOKEN}/screenshots`));
    const prompt = (await recorder.beginStop()) as RecorderState;
    expect(prompt.phase).toBe("stopPrompting");
    expect(server.status).toBe("paused");
    const outcome = (await recorder.finishStop("My lapse", false)) as { status: string; edit: boolean };
    expect(outcome.status).toBe("stopped");
    const paths = server.paths();
    const rename = paths.indexOf(`PATCH /api/sessions/${TOKEN}/name`);
    const stop = paths.indexOf(`POST /api/sessions/${TOKEN}/stop`);
    expect(rename).toBeGreaterThan(paths.indexOf(`POST /api/sessions/${TOKEN}/pause`));
    expect(stop).toBeGreaterThan(rename);
    expect((recorder.state() as RecorderState).phase).toBe("finished");
    expect(server.status).toBe("stopped");
  });

  it("keeps capturing on the grid across a dropped link and delivers the backlog", async () => {
    h = await harness("active");
    const { server, recorder } = h;
    await until(() => server.paths().includes(`POST /api/sessions/${TOKEN}/screenshots`));
    // Every request fails for a while; captures keep being taken.
    const realFetch = server.fetch;
    let offline = true;
    const flaky = async (input: RequestInfo | URL, init?: RequestInit) => {
      if (offline) throw new TypeError("Failed to fetch");
      return realFetch(input, init);
    };
    // Swap the transport under the host by pointing the server's fetch at
    // the flaky one: the host calls `server.fetch` by reference.
    (server as unknown as { fetch: typeof flaky }).fetch = flaky;
    await new Promise((r) => setTimeout(r, INTERVAL_MS * 2 + 200));
    offline = false;
    await until(() => (recorder.state() as RecorderState).captureCount >= 3, 10_000);
    const state = recorder.state() as RecorderState;
    expect(state.phase).toBe("recording");
    expect(state.pendingUploads).toBe(0);
    expect(server.tracked).toBeGreaterThanOrEqual(2 * (INTERVAL_MS / 1000));
  });
});
