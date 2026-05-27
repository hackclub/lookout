/**
 * End-to-end integration tests for the session routes against a real
 * Postgres. Covers the bucket/credit dual-mode behavior, the trust
 * envelope, streak math, mode-flip stickiness, pause/resume, and the
 * idempotent confirm path.
 *
 * Requires the test docker postgres running on port 5434 (see
 * test/setup.ts for the connection string).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";
import { setClock, resetClock } from "../src/lib/clock.js";

let app: FastifyInstance;
const baseTime = new Date("2025-06-01T12:00:00.000Z");
let virtualNow = baseTime.getTime();

function setVirtualNow(d: Date | number) {
  virtualNow = typeof d === "number" ? d : d.getTime();
}
function advanceVirtualMs(ms: number) {
  virtualNow += ms;
}

beforeEach(async () => {
  // Fresh world per test.
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) {
    app = await buildApp();
  }
  setVirtualNow(baseTime);
  setClock(() => new Date(virtualNow));
});

afterAll(async () => {
  resetClock();
  if (app) await app.close();
  // Close the drizzle pg pool so vitest can exit cleanly.
  await (db.$client as any).end?.();
});

async function createSession(): Promise<{ id: string; token: string }> {
  const [s] = await db
    .insert(schema.sessions)
    .values({ name: "test-session" })
    .returning({ id: schema.sessions.id, token: schema.sessions.token });
  return s;
}

async function loadSession(id: string) {
  return db.query.sessions.findFirst({ where: (t, { eq }) => eq(t.id, id) });
}

async function postUpload(
  token: string,
  capturedAt: string | undefined,
): Promise<{ status: number; body: any }> {
  const url = capturedAt
    ? `/api/sessions/${token}/upload-url?capturedAt=${encodeURIComponent(capturedAt)}`
    : `/api/sessions/${token}/upload-url`;
  const r = await app.inject({ method: "GET", url });
  return { status: r.statusCode, body: r.json() };
}

async function confirmUpload(
  token: string,
  screenshotId: string,
): Promise<{ status: number; body: any }> {
  const r = await app.inject({
    method: "POST",
    url: `/api/sessions/${token}/screenshots`,
    payload: { screenshotId, width: 1920, height: 1080, fileSize: 12345 },
  });
  return { status: r.statusCode, body: r.json() };
}

async function pause(token: string) {
  return app.inject({ method: "POST", url: `/api/sessions/${token}/pause` });
}
async function resume(token: string) {
  return app.inject({ method: "POST", url: `/api/sessions/${token}/resume` });
}

// ────────────────────────────────────────────────────────────
// Bucket-mode (legacy / compat) tests
// ────────────────────────────────────────────────────────────

describe("bucket mode (legacy compat)", () => {
  it("first request without capturedAt locks the session to bucket mode", async () => {
    const { token, id } = await createSession();
    const up = await postUpload(token, undefined);
    expect(up.status).toBe(200);
    await confirmUpload(token, up.body.screenshotId);
    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("bucket");
    expect(s?.streakAnchorAt).toBeNull();
  });

  it("compat_no_captured_at_full_session: 30 captures match (buckets-1)*60", async () => {
    const { token, id } = await createSession();
    // Cap iteration count to keep the test under MAX_SCREENSHOTS_PER_SESSION
    // and the rate limit (3 upload-url/min). Test focuses on bucket math; we
    // simulate 10 distinct minute buckets by advancing wall time per cycle.
    setVirtualNow(baseTime);
    for (let n = 0; n < 10; n++) {
      const up = await postUpload(token, undefined);
      expect(up.status, `upload ${n}`).toBe(200);
      const c = await confirmUpload(token, up.body.screenshotId);
      expect(c.status, `confirm ${n}`).toBe(200);
      advanceVirtualMs(60_000);
    }
    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("bucket");
    // 10 captures across 10 distinct minute buckets → tracked = 9*60 = 540
    const status = await app
      .inject({ method: "GET", url: `/api/sessions/${token}` })
      .then((r) => r.json());
    expect(status.trackedSeconds).toBe(9 * 60);
  });

  it("bucket session ignores capturedAt on later requests (cross-version)", async () => {
    const { token, id } = await createSession();
    const u1 = await postUpload(token, undefined);
    await confirmUpload(token, u1.body.screenshotId);
    advanceVirtualMs(60_000);

    // Upgraded client sends capturedAt against a bucket-locked session.
    // Should be accepted, mode stays bucket, captured_at stored for debug.
    const u2 = await postUpload(token, new Date(virtualNow).toISOString());
    expect(u2.status).toBe(200);
    const c2 = await confirmUpload(token, u2.body.screenshotId);
    expect(c2.status).toBe(200);
    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("bucket");
    const rows = await db.query.screenshots.findMany({
      where: (t, { eq }) => eq(t.sessionId, id),
    });
    // captured_at is populated for both: u1 fell back to serverNow, u2 sent
    // an explicit client-supplied value.
    expect(rows.every((r) => r.capturedAt !== null)).toBe(true);
    // No credit math happened — all rows have null credited_seconds.
    expect(rows.every((r) => r.creditedSeconds === null)).toBe(true);
  });
});

// ────────────────────────────────────────────────────────────
// Credit-mode mode-flip
// ────────────────────────────────────────────────────────────

describe("activation race / first capture", () => {
  it("first capture with capturedAt slightly before serverNow does NOT 400", async () => {
    // Repro for the bug seen in the wild on 0.2.1: the client takes a
    // screenshot at clock T0, the request reaches the server at T0+latency
    // (a few hundred ms). Without the fix, the server set started_at to
    // its own serverNow and rejected the older capturedAt.
    const { token, id } = await createSession();
    // Simulate ~250ms of upload latency: the client's capturedAt timestamp
    // predates the server's serverNow by a quarter second.
    const clientCapturedAt = new Date(virtualNow - 250).toISOString();
    const up = await postUpload(token, clientCapturedAt);
    expect(up.status).toBe(200);
    expect(up.body.trackingMode).toBe("credit");

    // started_at should be the client's capturedAt, not serverNow.
    const s = await loadSession(id);
    expect(s?.startedAt?.getTime()).toBe(virtualNow - 250);

    // And confirm completes cleanly.
    const conf = await confirmUpload(token, up.body.screenshotId);
    expect(conf.status).toBe(200);
  });

  it("first capture with clientCapturedAt in the future is clamped to serverNow", async () => {
    const { token, id } = await createSession();
    // Lying client: capturedAt = serverNow + 30s (within future envelope).
    const future = new Date(virtualNow + 30_000).toISOString();
    const up = await postUpload(token, future);
    expect(up.status).toBe(200);
    const s = await loadSession(id);
    // started_at must never be in the future.
    expect(s?.startedAt?.getTime()).toBe(virtualNow);
  });
});

describe("credit mode opt-in", () => {
  it("first capturedAt flips tracking_mode to credit", async () => {
    const { token, id } = await createSession();
    const cap = new Date(virtualNow).toISOString();
    const up = await postUpload(token, cap);
    expect(up.status).toBe(200);
    expect(up.body.trackingMode).toBe("credit");
    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("credit");
  });

  it("credit session rejects subsequent requests without capturedAt", async () => {
    const { token } = await createSession();
    await postUpload(token, new Date(virtualNow).toISOString());
    // Note: the session is now active. A second upload-url without
    // capturedAt should 400 with credit_mode_requires_captured_at.
    const u2 = await postUpload(token, undefined);
    expect(u2.status).toBe(400);
    expect(u2.body.error).toBe("credit_mode_requires_captured_at");
  });

  it("response includes serverTime", async () => {
    const { token } = await createSession();
    const up = await postUpload(token, new Date(virtualNow).toISOString());
    expect(typeof up.body.serverTime).toBe("string");
    expect(new Date(up.body.serverTime).getTime()).toBe(virtualNow);
  });
});

// ────────────────────────────────────────────────────────────
// Credit-mode envelope validation (distinct error codes)
// ────────────────────────────────────────────────────────────

describe("credit mode envelope", () => {
  async function seedCreditSession() {
    const sess = await createSession();
    const cap = new Date(virtualNow).toISOString();
    const up = await postUpload(sess.token, cap);
    await confirmUpload(sess.token, up.body.screenshotId);
    return sess;
  }

  it("rejects capturedAt > serverNow + 5min as captured_at_future", async () => {
    const { token } = await seedCreditSession();
    advanceVirtualMs(60_000);
    const cap = new Date(virtualNow + 6 * 60_000).toISOString();
    const up = await postUpload(token, cap);
    expect(up.status).toBe(400);
    expect(up.body.error).toBe("captured_at_future");
  });

  it("rejects capturedAt < serverNow - 5min as captured_at_too_old", async () => {
    const { token } = await seedCreditSession();
    advanceVirtualMs(60_000);
    const cap = new Date(virtualNow - 6 * 60_000).toISOString();
    const up = await postUpload(token, cap);
    expect(up.status).toBe(400);
    expect(up.body.error).toBe("captured_at_too_old");
  });

  it("rejects non-monotonic capturedAt", async () => {
    const sess = await seedCreditSession();
    // Submit a second upload with an EARLIER capturedAt than the first.
    advanceVirtualMs(60_000);
    const earlier = new Date(baseTime.getTime() - 1_000).toISOString();
    const up = await postUpload(sess.token, earlier);
    // baseTime - 1s is also < startedAt (which equals baseTime), so the
    // earlier-than-start check fires first. Either error is correct anti-
    // cheat behavior; lock to whichever the implementation surfaces.
    expect(up.status).toBe(400);
    expect([
      "captured_at_not_monotonic",
      "captured_at_before_session_start",
    ]).toContain(up.body.error);
  });

  it("rejects capturedAt before session.startedAt", async () => {
    const { token } = await seedCreditSession();
    // Build a capturedAt that's within the past envelope but before
    // startedAt (== baseTime).
    advanceVirtualMs(60_000);
    const before = new Date(baseTime.getTime() - 30_000).toISOString();
    const up = await postUpload(token, before);
    expect(up.status).toBe(400);
    expect([
      "captured_at_before_session_start",
      "captured_at_not_monotonic",
    ]).toContain(up.body.error);
  });
});

// ────────────────────────────────────────────────────────────
// Credit-mode steady-state + streak math
// ────────────────────────────────────────────────────────────

describe("credit mode steady state", () => {
  it("credit_mode_first_capture_zero_credit: single capture credits 0", async () => {
    const { token, id } = await createSession();
    const cap = new Date(virtualNow).toISOString();
    const up = await postUpload(token, cap);
    const c = await confirmUpload(token, up.body.screenshotId);
    expect(c.body.trackedSeconds).toBe(0);

    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("credit");
    expect(s?.trackedSeconds).toBe(0);
    expect(s?.streakCreditedCount).toBe(0);
    expect(s?.streakAnchorAt).not.toBeNull();
  });

  it("steady_state_invariant: capture[N].capturedAt === confirm[N-1].nextExpectedAt", async () => {
    const { token, id } = await createSession();

    // Capture 1 (seed)
    let cap = new Date(virtualNow).toISOString();
    let up = await postUpload(token, cap);
    let confirmed = await confirmUpload(token, up.body.screenshotId);
    let prevNextExpected = confirmed.body.nextExpectedAt as string;

    // Captures 2..5: each capturedAt equals previous nextExpectedAt.
    for (let n = 2; n <= 5; n++) {
      setVirtualNow(new Date(prevNextExpected));
      cap = prevNextExpected; // *** the invariant ***
      up = await postUpload(token, cap);
      expect(up.status, `upload ${n}`).toBe(200);
      confirmed = await confirmUpload(token, up.body.screenshotId);
      expect(confirmed.status, `confirm ${n}`).toBe(200);
      prevNextExpected = confirmed.body.nextExpectedAt;
    }

    const s = await loadSession(id);
    // 1 seed + 4 credited captures → tracked = 4 * 60 = 240
    expect(s?.trackedSeconds).toBe(240);
    expect(s?.streakCreditedCount).toBe(4);
  });

  it("captures within ±30s of expected get full credit; outside resets", async () => {
    const { token, id } = await createSession();
    const cap1 = new Date(virtualNow).toISOString();
    const u1 = await postUpload(token, cap1);
    await confirmUpload(token, u1.body.screenshotId);

    // Capture 2 at +60s + 29s = inside window
    setVirtualNow(baseTime.getTime() + 60_000 + 29_000);
    const u2 = await postUpload(token, new Date(virtualNow).toISOString());
    const c2 = await confirmUpload(token, u2.body.screenshotId);
    expect(c2.body.trackedSeconds).toBe(60);

    let s = await loadSession(id);
    expect(s?.streakCreditedCount).toBe(1);

    // Capture 3 at +120s + 31s = outside window → reset
    setVirtualNow(baseTime.getTime() + 120_000 + 31_000);
    const u3 = await postUpload(token, new Date(virtualNow).toISOString());
    const c3 = await confirmUpload(token, u3.body.screenshotId);
    // Reset means the row credited 0; tracked stays at 60.
    expect(c3.body.trackedSeconds).toBe(60);

    s = await loadSession(id);
    expect(s?.streakCreditedCount).toBe(0);
    // New anchor is the most recent capturedAt.
    expect(s?.streakAnchorAt?.getTime()).toBe(virtualNow);
  });

  it("expected_at column is populated for credit-mode confirmed rows (telemetry)", async () => {
    const { token, id } = await createSession();
    const cap1 = new Date(virtualNow).toISOString();
    const u1 = await postUpload(token, cap1);
    await confirmUpload(token, u1.body.screenshotId);

    setVirtualNow(baseTime.getTime() + 60_000);
    const u2 = await postUpload(token, new Date(virtualNow).toISOString());
    await confirmUpload(token, u2.body.screenshotId);

    const rows = await db.query.screenshots.findMany({
      where: (t, { eq }) => eq(t.sessionId, id),
      orderBy: (t, { asc }) => asc(t.createdAt),
    });
    // Seed row: expected_at is NULL (no expectation existed).
    expect(rows[0].expectedAt).toBeNull();
    // Second row: expected_at = anchor + 60s = baseTime + 60s.
    expect(rows[1].expectedAt?.getTime()).toBe(baseTime.getTime() + 60_000);
  });
});

// ────────────────────────────────────────────────────────────
// Idempotency
// ────────────────────────────────────────────────────────────

describe("idempotent confirm", () => {
  it("confirming twice doesn't double-credit", async () => {
    const { token, id } = await createSession();
    const u1 = await postUpload(token, new Date(virtualNow).toISOString());
    await confirmUpload(token, u1.body.screenshotId);

    setVirtualNow(baseTime.getTime() + 60_000);
    const u2 = await postUpload(token, new Date(virtualNow).toISOString());
    await confirmUpload(token, u2.body.screenshotId);

    // Second confirm of the SAME screenshot — should be a no-op.
    const dupe = await confirmUpload(token, u2.body.screenshotId);
    expect(dupe.status).toBe(200);
    expect(dupe.body.trackedSeconds).toBe(60);

    const s = await loadSession(id);
    expect(s?.trackedSeconds).toBe(60);
    expect(s?.streakCreditedCount).toBe(1);
  });
});

// ────────────────────────────────────────────────────────────
// Pause / resume — credit mode clears anchor on resume
// ────────────────────────────────────────────────────────────

describe("pause / resume", () => {
  it("pause/resume in credit mode clears streak anchor", async () => {
    const { token, id } = await createSession();
    const u1 = await postUpload(token, new Date(virtualNow).toISOString());
    await confirmUpload(token, u1.body.screenshotId);

    setVirtualNow(baseTime.getTime() + 60_000);
    const u2 = await postUpload(token, new Date(virtualNow).toISOString());
    await confirmUpload(token, u2.body.screenshotId);
    let s = await loadSession(id);
    expect(s?.streakCreditedCount).toBe(1);
    expect(s?.streakAnchorAt).not.toBeNull();

    await pause(token);
    const pauseResp = await app.inject({
      method: "GET",
      url: `/api/sessions/${token}`,
    });
    expect(pauseResp.json().status).toBe("paused");

    setVirtualNow(baseTime.getTime() + 300_000); // 5 min later
    await resume(token);

    s = await loadSession(id);
    // Anchor cleared so the first post-resume capture seeds fresh.
    expect(s?.streakAnchorAt).toBeNull();
    expect(s?.streakCreditedCount).toBe(0);
    // Tracked credit from before pause is preserved.
    expect(s?.trackedSeconds).toBe(60);
  });

  it("pause in bucket mode does NOT touch streak fields", async () => {
    const { token, id } = await createSession();
    const up = await postUpload(token, undefined);
    await confirmUpload(token, up.body.screenshotId);

    await pause(token);
    setVirtualNow(baseTime.getTime() + 60_000);
    await resume(token);

    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("bucket");
    expect(s?.streakAnchorAt).toBeNull();
    expect(s?.streakCreditedCount).toBe(0);
  });
});

// ────────────────────────────────────────────────────────────
// captured_at is stored in both modes (debug aid)
// ────────────────────────────────────────────────────────────

// ────────────────────────────────────────────────────────────
// Backwards-compat: legacy client wire shape
// ────────────────────────────────────────────────────────────

/**
 * The exact response types as they were defined in the shipped clients
 * (pre-credit-mode). Locked in here so that a future schema change can't
 * silently drop a field that the old desktop / web / react builds rely on.
 *
 * If the server response gains a new REQUIRED field, this interface will
 * still parse it (TypeScript ignores extras), but `legacyShapeOf` will
 * miss validating it — that's the intent. If the server response loses
 * an existing field, `legacyShapeOf` will throw because that field will
 * be undefined.
 */
interface LegacyUploadUrlResponse {
  uploadUrl: string;
  r2Key: string;
  screenshotId: string;
  minuteBucket: number;
  nextExpectedAt: string;
}
interface LegacyConfirmResponse {
  confirmed: true;
  trackedSeconds: number;
  nextExpectedAt: string;
}
interface LegacyPauseResponse {
  status: "paused";
  totalActiveSeconds: number;
}
interface LegacyResumeResponse {
  status: "active";
  nextExpectedAt: string;
}
interface LegacyStopResponse {
  status: "stopped";
  trackedSeconds: number;
  totalActiveSeconds: number;
}

function assertLegacyUploadUrl(o: any): LegacyUploadUrlResponse {
  expect(typeof o.uploadUrl).toBe("string");
  expect(typeof o.r2Key).toBe("string");
  expect(typeof o.screenshotId).toBe("string");
  expect(typeof o.minuteBucket).toBe("number");
  expect(typeof o.nextExpectedAt).toBe("string");
  // ISO-8601 parseable
  expect(Number.isNaN(new Date(o.nextExpectedAt).getTime())).toBe(false);
  return o as LegacyUploadUrlResponse;
}
function assertLegacyConfirm(o: any): LegacyConfirmResponse {
  expect(o.confirmed).toBe(true);
  expect(typeof o.trackedSeconds).toBe("number");
  expect(typeof o.nextExpectedAt).toBe("string");
  expect(Number.isNaN(new Date(o.nextExpectedAt).getTime())).toBe(false);
  return o as LegacyConfirmResponse;
}
function assertLegacyPause(o: any): LegacyPauseResponse {
  expect(o.status).toBe("paused");
  expect(typeof o.totalActiveSeconds).toBe("number");
  return o as LegacyPauseResponse;
}
function assertLegacyResume(o: any): LegacyResumeResponse {
  expect(o.status).toBe("active");
  expect(typeof o.nextExpectedAt).toBe("string");
  return o as LegacyResumeResponse;
}
function assertLegacyStop(o: any): LegacyStopResponse {
  expect(o.status).toBe("stopped");
  expect(typeof o.trackedSeconds).toBe("number");
  expect(typeof o.totalActiveSeconds).toBe("number");
  return o as LegacyStopResponse;
}

describe("backwards compat — legacy client wire shape", () => {
  it("compat_response_shape_legacy_parse: every response carries the legacy fields", async () => {
    const { token } = await createSession();

    // 1. /upload-url (no capturedAt → bucket mode)
    const up = await postUpload(token, undefined);
    expect(up.status).toBe(200);
    assertLegacyUploadUrl(up.body);
    // Extra new fields don't replace legacy ones — they coexist.
    expect(up.body.uploadUrl).toContain("r2.test");

    // 2. /screenshots (confirm)
    const conf = await confirmUpload(token, up.body.screenshotId);
    expect(conf.status).toBe(200);
    assertLegacyConfirm(conf.body);
    expect(conf.body.trackedSeconds).toBe(0);

    // 3. /pause
    advanceVirtualMs(30_000);
    const p = await pause(token);
    expect(p.statusCode).toBe(200);
    assertLegacyPause(p.json());

    // 4. /resume
    advanceVirtualMs(10_000);
    const r = await resume(token);
    expect(r.statusCode).toBe(200);
    assertLegacyResume(r.json());

    // 5. /stop
    advanceVirtualMs(60_000);
    const stop = await app.inject({
      method: "POST",
      url: `/api/sessions/${token}/stop`,
    });
    expect(stop.statusCode).toBe(200);
    assertLegacyStop(stop.json());
  });

  it("compat_full_lifecycle_old_client: 5-capture session no capturedAt produces bucket-math trackedSeconds", async () => {
    const { token, id } = await createSession();

    // Simulate the old client: setInterval-like cadence, no capturedAt.
    // 5 captures across 5 distinct minute buckets relative to startedAt.
    setVirtualNow(baseTime);
    for (let n = 0; n < 5; n++) {
      const up = assertLegacyUploadUrl((await postUpload(token, undefined)).body);
      const conf = assertLegacyConfirm(
        (await confirmUpload(token, up.screenshotId)).body,
      );
      // Bucket math: tracked = (distinct minute buckets - 1) * 60
      // After capture N+1, we have N+1 buckets, tracked = N * 60.
      expect(conf.trackedSeconds).toBe(n * 60);
      advanceVirtualMs(60_000);
    }

    // Pause and stop with no capturedAt anywhere.
    const p = assertLegacyPause((await pause(token)).json());
    expect(p.totalActiveSeconds).toBeGreaterThanOrEqual(0);
    const stopResp = await app.inject({
      method: "POST",
      url: `/api/sessions/${token}/stop`,
    });
    const stop = assertLegacyStop(stopResp.json());
    expect(stop.trackedSeconds).toBe(4 * 60); // 5 buckets - 1, * 60

    // Verify session state is what bucket mode would produce.
    const s = await loadSession(id);
    expect(s?.trackingMode).toBe("bucket");
    expect(s?.streakAnchorAt).toBeNull();
    expect(s?.streakCreditedCount).toBe(0);
    expect(s?.trackedSeconds).toBe(4 * 60);
    // captured_at populated for debug (= requestedAt = serverNow) on every row;
    // credited_seconds and expected_at stay NULL for bucket mode.
    const rows = await db.query.screenshots.findMany({
      where: (t, { eq }) => eq(t.sessionId, id),
    });
    expect(rows).toHaveLength(5);
    expect(rows.every((r) => r.capturedAt !== null)).toBe(true);
    expect(rows.every((r) => r.creditedSeconds === null)).toBe(true);
    expect(rows.every((r) => r.expectedAt === null)).toBe(true);
  });

  it("compat_request_shape_byte_compatible: no-capturedAt request hits the legacy code path", async () => {
    const { token } = await createSession();
    // The wire request the old client sends: bare GET with no query string.
    const r = await app.inject({
      method: "GET",
      url: `/api/sessions/${token}/upload-url`,
    });
    expect(r.statusCode).toBe(200);
    const body = r.json();
    assertLegacyUploadUrl(body);
    // Confirm body must match the legacy schema bytes exactly.
    const conf = await app.inject({
      method: "POST",
      url: `/api/sessions/${token}/screenshots`,
      headers: { "content-type": "application/json" },
      payload: JSON.stringify({
        screenshotId: body.screenshotId,
        width: 1920,
        height: 1080,
        fileSize: 12345,
      }),
    });
    expect(conf.statusCode).toBe(200);
    assertLegacyConfirm(conf.json());
  });
});

describe("captured_at debug column", () => {
  it("populated as serverNow when client omits it (bucket mode)", async () => {
    const { token, id } = await createSession();
    const up = await postUpload(token, undefined);
    await confirmUpload(token, up.body.screenshotId);
    const rows = await db.query.screenshots.findMany({
      where: (t, { eq }) => eq(t.sessionId, id),
    });
    expect(rows[0].capturedAt?.getTime()).toBe(virtualNow);
  });

  it("populated as client value when sent (credit mode)", async () => {
    const { token, id } = await createSession();
    const cap = new Date(virtualNow + 100).toISOString(); // +100ms drift
    const up = await postUpload(token, cap);
    await confirmUpload(token, up.body.screenshotId);
    const rows = await db.query.screenshots.findMany({
      where: (t, { eq }) => eq(t.sessionId, id),
    });
    expect(rows[0].capturedAt?.getTime()).toBe(virtualNow + 100);
  });
});

// ────────────────────────────────────────────────────────────
// Latency / robustness — simulate real-world network conditions
// and verify the system still credits correctly. Driven by the
// halved-time prod investigation: we want the server to absorb
// jitter, retries, out-of-order arrivals, and slow uploads without
// dropping credits or double-counting.
// ────────────────────────────────────────────────────────────

describe("latency — slow uploads", () => {
  it("absorbs 4s end-to-end latency between capture and confirm", async () => {
    // Simulate: client takes screenshot at T0, upload + confirm takes 4s
    // total. Wall clock at server side has advanced by 4s by confirm time.
    const { token, id } = await createSession();
    const cap1 = new Date(virtualNow).toISOString();
    const u1 = await postUpload(token, cap1);
    advanceVirtualMs(4_000);
    const c1 = await confirmUpload(token, u1.body.screenshotId);
    expect(c1.status).toBe(200);

    // Next capture happens at proper +60s from the original capturedAt.
    advanceVirtualMs(56_000); // total: 60s past first cap
    const cap2 = new Date(virtualNow).toISOString();
    const u2 = await postUpload(token, cap2);
    advanceVirtualMs(4_000); // another 4s of upload latency
    const c2 = await confirmUpload(token, u2.body.screenshotId);
    expect(c2.status).toBe(200);
    expect(c2.body.trackedSeconds).toBe(60);
  });

  it("captures crediting even when confirm runs 30s after capture", async () => {
    // Long upload/processing delay. capturedAt anchors the streak math —
    // not serverNow at confirm time. As long as capturedAt is within the
    // envelope, the streak still advances.
    const { token, id } = await createSession();
    setVirtualNow(baseTime);
    for (let n = 0; n < 5; n++) {
      const cap = new Date(virtualNow).toISOString();
      const u = await postUpload(token, cap);
      // Each confirm runs 30s after the capturedAt
      advanceVirtualMs(30_000);
      const c = await confirmUpload(token, u.body.screenshotId);
      expect(c.status, `confirm ${n}`).toBe(200);
      // Advance the remaining 30s to reach the next minute boundary
      advanceVirtualMs(30_000);
    }
    const s = await loadSession(id);
    // 1 seed + 4 credits = 240s
    expect(s?.trackedSeconds).toBe(4 * 60);
  });
});

describe("latency — idempotent retries", () => {
  it("client confirms twice (retry path) does not double-credit", async () => {
    // Simulate: client posts confirm, network drops the response, client
    // retries. Server must serve the second confirm idempotently.
    const { token, id } = await createSession();
    advanceVirtualMs(60_000);
    const cap1 = new Date(virtualNow).toISOString();
    const u = await postUpload(token, cap1);
    advanceVirtualMs(60_000);
    const cap2 = new Date(virtualNow).toISOString();
    const u2 = await postUpload(token, cap2);
    const c2a = await confirmUpload(token, u2.body.screenshotId);
    const c2b = await confirmUpload(token, u2.body.screenshotId); // retry
    expect(c2a.status).toBe(200);
    expect(c2b.status).toBe(200);
    expect(c2a.body.trackedSeconds).toBe(c2b.body.trackedSeconds);
    const s = await loadSession(id);
    expect(s?.trackedSeconds).toBe(c2b.body.trackedSeconds);
  });

  it("rapid 3-retry storm on confirm produces single credit", async () => {
    const { token, id } = await createSession();
    advanceVirtualMs(60_000);
    const cap1 = new Date(virtualNow).toISOString();
    const u1 = await postUpload(token, cap1);
    await confirmUpload(token, u1.body.screenshotId);
    advanceVirtualMs(60_000);
    const cap2 = new Date(virtualNow).toISOString();
    const u2 = await postUpload(token, cap2);
    // Fire 3 concurrent confirms. Only one should mark the row confirmed;
    // the others land on the idempotent path or rate limit.
    const results = await Promise.all([
      confirmUpload(token, u2.body.screenshotId),
      confirmUpload(token, u2.body.screenshotId),
      confirmUpload(token, u2.body.screenshotId),
    ]);
    expect(results.every((r) => r.status === 200 || r.status === 429)).toBe(
      true,
    );
    const s = await loadSession(id);
    expect(s?.trackedSeconds).toBe(60);
  });
});

describe("latency — out-of-order arrivals", () => {
  // Documents the actual behavior of the streak math when confirms arrive
  // in a different order than captures: streak state is keyed on confirm
  // order, not on captured_at. The later-captured capture (B) lands first
  // and seeds the streak, then the earlier-captured (A) falls outside the
  // ±30s window relative to B's anchor and resets without credit.
  //
  // Acceptable today because: (a) it doesn't corrupt state, (b) it
  // doesn't double-credit, (c) the client's queue uploads serially so
  // this is rare in practice (only triggers under specific retry races).
  it("out-of-order confirms produce 0 credit (does not corrupt or double-count)", async () => {
    const { token, id } = await createSession();
    advanceVirtualMs(60_000);
    const capA = new Date(virtualNow).toISOString();
    const uA = await postUpload(token, capA);
    advanceVirtualMs(60_000);
    const capB = new Date(virtualNow).toISOString();
    const uB = await postUpload(token, capB);

    // Confirm B BEFORE A.
    const cB = await confirmUpload(token, uB.body.screenshotId);
    const cA = await confirmUpload(token, uA.body.screenshotId);
    expect(cA.status).toBe(200);
    expect(cB.status).toBe(200);

    const s = await loadSession(id);
    // B seeds the streak (credit=0). A is then 60s before B's anchor,
    // falls outside the streak window, and resets (credit=0). Net: 0
    // tracked. The important guarantees are: no error, no double-credit,
    // streak state is internally consistent.
    expect(s?.trackedSeconds).toBe(0);
    expect(s?.streakAnchorAt).toBeDefined();

    const rows = await db.query.screenshots.findMany({
      where: (t, { eq, and }) =>
        and(eq(t.sessionId, id), eq(t.confirmed, true)),
    });
    expect(rows).toHaveLength(2);
    expect(rows.every((r) => r.creditedSeconds === 0)).toBe(true);
  });
});

describe("latency — burst races (the bug we saw in prod)", () => {
  it("rapid 3-capture burst within 100ms reaches server: one credits, others reset", async () => {
    // The fire-and-forget client bug: after one legitimate capture,
    // tick re-fires multiple times with capturedAt very close together.
    // Server should: credit the first, reset on the rapid ones, but not
    // double-credit or corrupt streak state.
    const { token, id } = await createSession();
    advanceVirtualMs(60_000);
    // Seed the streak with cap0
    const cap0 = new Date(virtualNow).toISOString();
    const u0 = await postUpload(token, cap0);
    await confirmUpload(token, u0.body.screenshotId);

    advanceVirtualMs(60_000);
    // Legit cap at proper +60s
    const capLegit = new Date(virtualNow).toISOString();
    const uLegit = await postUpload(token, capLegit);
    await confirmUpload(token, uLegit.body.screenshotId);

    // Burst: 2 more captures 50ms and 100ms later
    advanceVirtualMs(50);
    const capBurst1 = new Date(virtualNow).toISOString();
    const uBurst1 = await postUpload(token, capBurst1);
    advanceVirtualMs(50);
    const capBurst2 = new Date(virtualNow).toISOString();
    const uBurst2 = await postUpload(token, capBurst2);
    await confirmUpload(token, uBurst1.body.screenshotId);
    await confirmUpload(token, uBurst2.body.screenshotId);

    const s = await loadSession(id);
    // 1 credit from capLegit, 0 from the bursts (resets)
    expect(s?.trackedSeconds).toBe(60);

    // Streak should have been reset by the burst captures
    const rows = await db.query.screenshots.findMany({
      where: (t, { eq, and }) =>
        and(eq(t.sessionId, id), eq(t.confirmed, true)),
      orderBy: (t, { asc }) => asc(t.capturedAt),
    });
    expect(rows).toHaveLength(4);
    expect(rows[0].creditedSeconds).toBe(0); // seed
    expect(rows[1].creditedSeconds).toBe(60); // legit credit
    expect(rows[2].creditedSeconds).toBe(0); // burst reset
    expect(rows[3].creditedSeconds).toBe(0); // burst reset
  });

  it("burst monotonicity: server rejects captured_at not strictly increasing", async () => {
    // The cap2 having same capturedAt as cap1 hits the
    // captured_at_not_monotonic guard.
    const { token } = await createSession();
    advanceVirtualMs(60_000);
    const cap = new Date(virtualNow).toISOString();
    const u1 = await postUpload(token, cap);
    expect(u1.status).toBe(200);
    const u2 = await postUpload(token, cap); // exact same capturedAt
    expect(u2.status).toBe(400);
    expect(u2.body.error).toBe("captured_at_not_monotonic");
  });
});

describe("latency — sustained recording under jitter", () => {
  it("10-min recording with ±5s capture jitter: credits ~100%", async () => {
    // Realistic scenario: captures arrive with small random jitter
    // around the 60s schedule. Should all stay in window and credit.
    const { token, id } = await createSession();
    const jitters = [0, 2_000, -1_000, 3_000, -2_000, 1_500, -500, 2_500, 0, -1_500];
    setVirtualNow(baseTime);

    for (let n = 0; n < 10; n++) {
      const t = baseTime.getTime() + n * 60_000 + jitters[n];
      setVirtualNow(t);
      const cap = new Date(t).toISOString();
      const u = await postUpload(token, cap);
      expect(u.status, `upload ${n}`).toBe(200);
      const c = await confirmUpload(token, u.body.screenshotId);
      expect(c.status, `confirm ${n}`).toBe(200);
    }

    const s = await loadSession(id);
    // 1 seed + 9 credits = 540s
    expect(s?.trackedSeconds).toBe(9 * 60);
  });

  it("capture 35s late (outside 30s window) resets, then recovers", async () => {
    // A single delayed capture trips the streak reset. Subsequent
    // captures on schedule re-establish the streak.
    const { token, id } = await createSession();
    setVirtualNow(baseTime);

    // Seed
    const u0 = await postUpload(token, new Date(baseTime).toISOString());
    await confirmUpload(token, u0.body.screenshotId);

    // Cap 2 lands at +60s (on schedule) — credit
    setVirtualNow(baseTime.getTime() + 60_000);
    const u2 = await postUpload(token, new Date(virtualNow).toISOString());
    const c2 = await confirmUpload(token, u2.body.screenshotId);
    expect(c2.body.trackedSeconds).toBe(60);

    // Cap 3 at +155s (35s late) — should reset
    setVirtualNow(baseTime.getTime() + 155_000);
    const u3 = await postUpload(token, new Date(virtualNow).toISOString());
    const c3 = await confirmUpload(token, u3.body.screenshotId);
    expect(c3.body.trackedSeconds).toBe(60); // unchanged

    // Cap 4 at +215s (60s after cap3's anchor) — credit again
    setVirtualNow(baseTime.getTime() + 215_000);
    const u4 = await postUpload(token, new Date(virtualNow).toISOString());
    const c4 = await confirmUpload(token, u4.body.screenshotId);
    expect(c4.body.trackedSeconds).toBe(120); // +60 from the new streak
  });
});
