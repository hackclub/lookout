/**
 * Integration tests for clip uploads (per-minute video capture units)
 * against a real Postgres.
 *
 * Covers: the session-level clips gate (grant vs silent downgrade), the
 * clip-first upload path (a webm/mp4 as the session's FIRST upload must
 * activate the session and seed credit-mode exactly like a JPEG), per-format
 * confirm validation (content type + size cap), frameCount persistence, the
 * capability fields on the session GET, the internal-API opt-in, and — most
 * importantly — that clients which know nothing about clips see byte-identical
 * legacy behavior.
 *
 * Requires the test postgres on port 5434 (see test/setup.ts).
 */
import { afterAll, afterEach, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { sql, eq } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";
import { setClock, resetClock } from "../src/lib/clock.js";
import { CLIP_FRAME_INTERVAL_MS } from "@lookout/shared";

let app: FastifyInstance;
const baseTime = new Date("2025-06-01T12:00:00.000Z");
let virtualNow = baseTime.getTime();

function advanceVirtualMs(ms: number) {
  virtualNow += ms;
}
function nowIso(): string {
  return new Date(virtualNow).toISOString();
}

beforeEach(async () => {
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) {
    app = await buildApp();
  }
  virtualNow = baseTime.getTime();
  setClock(() => new Date(virtualNow));
});

afterEach(() => {
  delete (globalThis as any).__r2HeadObjectOverride;
});

afterAll(async () => {
  resetClock();
  if (app) await app.close();
  await (db.$client as any).end?.();
});

async function createSession(clipsEnabled: boolean): Promise<{ id: string; token: string }> {
  const [s] = await db
    .insert(schema.sessions)
    .values({ name: "clips-test-session", clipsEnabled })
    .returning({ id: schema.sessions.id, token: schema.sessions.token });
  return s;
}

async function getUploadUrl(
  token: string,
  opts: { capturedAt?: string; format?: string } = {},
): Promise<{ status: number; body: any }> {
  const params = new URLSearchParams();
  if (opts.capturedAt) params.set("capturedAt", opts.capturedAt);
  if (opts.format) params.set("format", opts.format);
  const qs = params.toString();
  const r = await app.inject({
    method: "GET",
    url: `/api/sessions/${token}/upload-url${qs ? `?${qs}` : ""}`,
  });
  return { status: r.statusCode, body: r.json() };
}

async function confirmUpload(
  token: string,
  screenshotId: string,
  extra: Record<string, unknown> = {},
): Promise<{ status: number; body: any }> {
  const r = await app.inject({
    method: "POST",
    url: `/api/sessions/${token}/screenshots`,
    payload: { screenshotId, width: 1920, height: 1080, fileSize: 12345, ...extra },
  });
  return { status: r.statusCode, body: r.json() };
}

describe("clip format grant / downgrade", () => {
  it("grants webm on a clips-enabled session, with a .webm key", async () => {
    const s = await createSession(true);
    const { status, body } = await getUploadUrl(s.token, {
      capturedAt: nowIso(),
      format: "webm",
    });
    expect(status).toBe(200);
    expect(body.format).toBe("webm");
    expect(body.clipsEnabled).toBe(true);
    expect(body.frameIntervalMs).toBe(CLIP_FRAME_INTERVAL_MS);
    expect(body.r2Key).toMatch(/\.webm$/);
  });

  it("grants mp4 on a clips-enabled session (Safari / desktop path)", async () => {
    const s = await createSession(true);
    const { status, body } = await getUploadUrl(s.token, {
      capturedAt: nowIso(),
      format: "mp4",
    });
    expect(status).toBe(200);
    expect(body.format).toBe("mp4");
    expect(body.r2Key).toMatch(/\.mp4$/);
  });

  it("silently downgrades clip requests to jpeg when clips are disabled", async () => {
    const s = await createSession(false);
    const { status, body } = await getUploadUrl(s.token, {
      capturedAt: nowIso(),
      format: "webm",
    });
    // Not an error: the downgrade is the contract. A conforming client
    // reads the granted format and uploads a JPEG instead.
    expect(status).toBe(200);
    expect(body.format).toBe("jpeg");
    expect(body.clipsEnabled).toBe(false);
    expect(body.r2Key).toMatch(/\.jpg$/);

    const row = await db.query.screenshots.findFirst({
      where: eq(schema.screenshots.id, body.screenshotId),
    });
    expect(row?.format).toBe("jpeg");
  });

  it("keeps legacy requests (no format param) byte-identical to before", async () => {
    const s = await createSession(false);
    const { status, body } = await getUploadUrl(s.token, { capturedAt: nowIso() });
    expect(status).toBe(200);
    expect(body.format).toBe("jpeg");
    expect(body.r2Key).toMatch(/\.jpg$/);
    // All pre-clips fields still present and shaped as before.
    expect(body.uploadUrl).toBeTruthy();
    expect(body.screenshotId).toBeTruthy();
    expect(typeof body.minuteBucket).toBe("number");
    expect(body.nextExpectedAt).toBeTruthy();
    expect(body.trackingMode).toBe("credit");
  });
});

describe("clip-first sessions (no JPEG ever)", () => {
  it("activates, seeds credit mode, and credits the second clip", async () => {
    const s = await createSession(true);

    // First upload of the session is a clip — activation + credit-mode
    // flip must work exactly as they do for a JPEG first upload.
    const first = await getUploadUrl(s.token, {
      capturedAt: nowIso(),
      format: "webm",
    });
    expect(first.status).toBe(200);
    expect(first.body.format).toBe("webm");
    const firstConfirm = await confirmUpload(s.token, first.body.screenshotId, {
      frameCount: 20,
    });
    expect(firstConfirm.status).toBe(200);
    // Seed capture: credits 0, like every credit-mode session.
    expect(firstConfirm.body.trackedSeconds).toBe(0);

    const session = await db.query.sessions.findFirst({
      where: eq(schema.sessions.id, s.id),
    });
    expect(session?.status).toBe("active");
    expect(session?.trackingMode).toBe("credit");

    // Second clip lands on the 60s mark → credits a full minute.
    advanceVirtualMs(60_000);
    const second = await getUploadUrl(s.token, {
      capturedAt: nowIso(),
      format: "webm",
    });
    expect(second.status).toBe(200);
    const secondConfirm = await confirmUpload(s.token, second.body.screenshotId, {
      frameCount: 20,
    });
    expect(secondConfirm.status).toBe(200);
    expect(secondConfirm.body.trackedSeconds).toBe(60);
  });

  it("persists frameCount and format on the confirmed row", async () => {
    const s = await createSession(true);
    const up = await getUploadUrl(s.token, { capturedAt: nowIso(), format: "webm" });
    const confirm = await confirmUpload(s.token, up.body.screenshotId, {
      frameCount: 17,
    });
    expect(confirm.status).toBe(200);

    const row = await db.query.screenshots.findFirst({
      where: eq(schema.screenshots.id, up.body.screenshotId),
    });
    expect(row?.confirmed).toBe(true);
    expect(row?.format).toBe("webm");
    expect(row?.frameCount).toBe(17);
  });

  it("mixed sessions are legal: a jpeg fallback minute confirms fine", async () => {
    const s = await createSession(true);
    const clip = await getUploadUrl(s.token, { capturedAt: nowIso(), format: "webm" });
    await confirmUpload(s.token, clip.body.screenshotId, { frameCount: 20 });

    // Client hit an encoder hiccup and fell back to a JPEG for this tick.
    advanceVirtualMs(60_000);
    const jpeg = await getUploadUrl(s.token, { capturedAt: nowIso() });
    expect(jpeg.body.format).toBe("jpeg");
    const confirm = await confirmUpload(s.token, jpeg.body.screenshotId);
    expect(confirm.status).toBe(200);
    expect(confirm.body.trackedSeconds).toBe(60);
  });
});

describe("per-format confirm validation", () => {
  it("rejects a clip row whose stored object is not the granted content type", async () => {
    const s = await createSession(true);
    const up = await getUploadUrl(s.token, { capturedAt: nowIso(), format: "webm" });
    // Simulate an object that bypassed the presigned URL's signed type.
    (globalThis as any).__r2HeadObjectOverride = {
      ContentType: "image/jpeg",
      ContentLength: 1024,
    };
    const confirm = await confirmUpload(s.token, up.body.screenshotId, {
      frameCount: 20,
    });
    expect(confirm.status).toBe(400);
    expect(confirm.body.error).toContain("video/webm");
  });

  it("rejects a clip larger than the clip size cap", async () => {
    const s = await createSession(true);
    const up = await getUploadUrl(s.token, { capturedAt: nowIso(), format: "webm" });
    (globalThis as any).__r2HeadObjectOverride = {
      ContentType: "video/webm",
      ContentLength: 9 * 1024 * 1024,
    };
    const confirm = await confirmUpload(s.token, up.body.screenshotId, {
      frameCount: 20,
    });
    expect(confirm.status).toBe(400);
    expect(confirm.body.error).toContain("too large");
  });

  it("rejects a header-only clip so the client falls back to its JPEG", async () => {
    const s = await createSession(true);
    const up = await getUploadUrl(s.token, { capturedAt: nowIso(), format: "webm" });
    // Firefox's MediaRecorder with a starved canvas track: the bare WebM
    // header, no frames.
    (globalThis as any).__r2HeadObjectOverride = {
      ContentType: "video/webm",
      ContentLength: 217,
    };
    const confirm = await confirmUpload(s.token, up.body.screenshotId, {
      frameCount: 7,
    });
    expect(confirm.status).toBe(400);
    expect(confirm.body.error).toContain("no video");

    const row = await db.query.screenshots.findFirst({
      where: eq(schema.screenshots.id, up.body.screenshotId),
    });
    expect(row?.confirmed).toBe(false);
  });
});

describe("capability discovery", () => {
  it("session GET carries clipsEnabled + frameIntervalMs", async () => {
    const on = await createSession(true);
    const off = await createSession(false);

    const rOn = await app.inject({ method: "GET", url: `/api/sessions/${on.token}` });
    expect(rOn.statusCode).toBe(200);
    expect(rOn.json().clipsEnabled).toBe(true);
    expect(rOn.json().frameIntervalMs).toBe(CLIP_FRAME_INTERVAL_MS);

    const rOff = await app.inject({ method: "GET", url: `/api/sessions/${off.token}` });
    expect(rOff.statusCode).toBe(200);
    expect(rOff.json().clipsEnabled).toBe(false);
  });
});

describe("internal API opt-out", () => {
  async function makeApiKey(): Promise<string> {
    const [row] = await db
      .insert(schema.apiKeys)
      .values({ name: `clips-test-${Date.now()}-${Math.random()}` })
      .returning({ key: schema.apiKeys.key });
    return row.key;
  }

  it("enables clips by default, and only clips:false opts out", async () => {
    const key = await makeApiKey();

    // Explicit true, omitted, and explicit false — the three ways a program
    // can express intent. Only the last one turns clips off.
    const withClips = await app.inject({
      method: "POST",
      url: "/api/internal/sessions",
      headers: { "x-api-key": key },
      payload: { name: "clips-on", clips: true },
    });
    expect(withClips.statusCode).toBe(201);

    const without = await app.inject({
      method: "POST",
      url: "/api/internal/sessions",
      headers: { "x-api-key": key },
      payload: { name: "clips-default" },
    });
    expect(without.statusCode).toBe(201);

    const optedOut = await app.inject({
      method: "POST",
      url: "/api/internal/sessions",
      headers: { "x-api-key": key },
      payload: { name: "clips-off", clips: false },
    });
    expect(optedOut.statusCode).toBe(201);

    const onRow = await db.query.sessions.findFirst({
      where: eq(schema.sessions.id, withClips.json().sessionId),
    });
    const defaultRow = await db.query.sessions.findFirst({
      where: eq(schema.sessions.id, without.json().sessionId),
    });
    const offRow = await db.query.sessions.findFirst({
      where: eq(schema.sessions.id, optedOut.json().sessionId),
    });
    expect(onRow?.clipsEnabled).toBe(true);
    // The whole point of the flip: saying nothing gets you clips.
    expect(defaultRow?.clipsEnabled).toBe(true);
    expect(offRow?.clipsEnabled).toBe(false);

    // Internal GET surfaces the flag for program backends/ops.
    const detail = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${withClips.json().sessionId}`,
      headers: { "x-api-key": key },
    });
    expect(detail.statusCode).toBe(200);
    expect(detail.json().session.clipsEnabled).toBe(true);
  });
});
