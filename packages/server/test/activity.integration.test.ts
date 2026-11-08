/**
 * Interaction metadata (`activity`) on the confirm: stored on the capture,
 * readable on the internal API only, and never able to fail a confirm.
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

beforeEach(async () => {
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) app = await buildApp();
  virtualNow = baseTime.getTime();
  setClock(() => new Date(virtualNow));
});

afterAll(async () => {
  resetClock();
  if (app) await app.close();
  await (db.$client as any).end?.();
});

async function createSession(): Promise<{ id: string; token: string }> {
  const [s] = await db
    .insert(schema.sessions)
    .values({ name: "activity-test" })
    .returning({ id: schema.sessions.id, token: schema.sessions.token });
  return s;
}

async function makeApiKey(): Promise<string> {
  const [row] = await db
    .insert(schema.apiKeys)
    .values({ name: `activity-test-${Date.now()}-${Math.random()}` })
    .returning({ key: schema.apiKeys.key });
  return row.key;
}

const ACTIVITY = {
  v: 1,
  samples: [
    {
      at: baseTime.getTime(),
      method: "ax",
      cursor: { display: 1, x: 0.25, y: 0.75, inCapture: true },
      frontmost: { app: "com.microsoft.VSCode", captured: true },
      windows: [{ id: 77, active: true }],
    },
  ],
};

async function capture(token: string, activity?: unknown) {
  const capturedAt = new Date(virtualNow).toISOString();
  const up = await app.inject({
    method: "GET",
    url: `/api/sessions/${token}/upload-url?capturedAt=${encodeURIComponent(capturedAt)}`,
  });
  expect(up.statusCode).toBe(200);
  const screenshotId = up.json().screenshotId as string;
  const confirm = await app.inject({
    method: "POST",
    url: `/api/sessions/${token}/screenshots`,
    payload: {
      screenshotId,
      width: 1920,
      height: 1080,
      fileSize: 12345,
      ...(activity === undefined ? {} : { activity }),
    },
  });
  return { screenshotId, confirm };
}

describe("capture activity", () => {
  it("is stored with the capture and read back on the internal API", async () => {
    const s = await createSession();
    const { screenshotId, confirm } = await capture(s.token, ACTIVITY);
    expect(confirm.statusCode).toBe(200);

    const key = await makeApiKey();
    const r = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${s.id}/activity`,
      headers: { "x-api-key": key },
    });
    expect(r.statusCode).toBe(200);
    expect(r.json()).toEqual({
      sessionId: s.id,
      captures: [
        {
          screenshotId,
          capturedAt: baseTime.toISOString(),
          format: "jpeg",
          final: false,
          activity: ACTIVITY,
        },
      ],
    });
  });

  it("never fails a confirm: malformed activity is dropped, the minute still lands", async () => {
    const s = await createSession();
    const { screenshotId, confirm } = await capture(s.token, { samples: "nope" });
    expect(confirm.statusCode).toBe(200);
    const row = await db.query.screenshots.findFirst({ where: (t, { eq }) => eq(t.id, screenshotId) });
    expect(row?.confirmed).toBe(true);
    expect(row?.activity).toBeNull();
  });

  it("is null for clients that don't send it", async () => {
    const s = await createSession();
    const { screenshotId, confirm } = await capture(s.token);
    expect(confirm.statusCode).toBe(200);
    const row = await db.query.screenshots.findFirst({ where: (t, { eq }) => eq(t.id, screenshotId) });
    expect(row?.activity).toBeNull();
  });

  it("needs an api key, and appears on no token endpoint", async () => {
    const s = await createSession();
    await capture(s.token, ACTIVITY);

    const noKey = await app.inject({ method: "GET", url: `/api/internal/sessions/${s.id}/activity` });
    expect(noKey.statusCode).toBe(401);

    for (const url of [
      `/api/sessions/${s.token}`,
      `/api/sessions/${s.token}/status`,
      `/api/sessions/${s.token}/units`,
      `/api/sessions/${s.token}/timings`,
    ]) {
      const r = await app.inject({ method: "GET", url });
      expect(r.body).not.toContain("com.microsoft.VSCode");
      expect(r.body).not.toContain("inCapture");
    }
  });
});
