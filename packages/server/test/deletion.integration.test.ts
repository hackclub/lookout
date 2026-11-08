/**
 * Integration tests for program-requested session deletion.
 *
 * The contract: DELETE /api/internal/sessions/:id hides the session at once
 * and schedules the purge SESSION_DELETION_GRACE_MINUTES out; restore undoes
 * it inside the window and refuses after; the purge job destroys only
 * sessions whose window has run out and that aren't mid-compile.
 *
 * Requires the test docker postgres on port 5434 (see test/setup.ts).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { randomBytes } from "node:crypto";
import { eq, sql } from "drizzle-orm";
import { SESSION_DELETION_GRACE_MINUTES } from "@lookout/shared";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";
import { purgeDeletedSessions } from "../src/lib/deletion.js";

let app: FastifyInstance;

beforeEach(async () => {
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) {
    app = await buildApp();
  }
});

afterAll(async () => {
  if (app) await app.close();
  await (db.$client as any).end?.();
});

/** A fresh program and its key. Programs aren't truncated between tests. */
async function programKey(): Promise<string> {
  const name = `deletion-${randomBytes(4).toString("hex")}`;
  const [program] = await db
    .insert(schema.programs)
    .values({ name })
    .returning({ id: schema.programs.id });
  const [row] = await db
    .insert(schema.apiKeys)
    .values({ name, programId: program.id })
    .returning({ key: schema.apiKeys.key });
  return row.key;
}

async function createSession(key: string) {
  const r = await app.inject({
    method: "POST",
    url: "/api/internal/sessions",
    headers: { "x-api-key": key },
    payload: {},
  });
  expect(r.statusCode).toBe(201);
  return r.json() as { token: string; sessionId: string };
}

function deleteSession(key: string, sessionId: string) {
  return app.inject({
    method: "DELETE",
    url: `/api/internal/sessions/${sessionId}`,
    headers: { "x-api-key": key },
  });
}

function restoreSession(key: string, sessionId: string) {
  return app.inject({
    method: "POST",
    url: `/api/internal/sessions/${sessionId}/restore`,
    headers: { "x-api-key": key },
  });
}

async function sessionRow(sessionId: string) {
  const [row] = await db
    .select()
    .from(schema.sessions)
    .where(eq(schema.sessions.id, sessionId));
  return row;
}

describe("DELETE /api/internal/sessions/:id", () => {
  it("schedules the purge a grace period out and hides the session", async () => {
    const key = await programKey();
    const { token, sessionId } = await createSession(key);

    const before = Date.now();
    const r = await deleteSession(key, sessionId);
    expect(r.statusCode).toBe(202);

    const deleteAfter = new Date(r.json().deleteAfter).getTime();
    expect(deleteAfter).toBeGreaterThanOrEqual(
      before + SESSION_DELETION_GRACE_MINUTES * 60_000 - 1000,
    );

    const get = await app.inject({ method: "GET", url: `/api/sessions/${token}` });
    expect(get.statusCode).toBe(404);

    const batch = await app.inject({
      method: "POST",
      url: "/api/sessions/batch",
      payload: { tokens: [token] },
    });
    expect(batch.json().sessions).toEqual([]);

    // The owning program can still see it, and when it goes.
    const internal = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${sessionId}`,
      headers: { "x-api-key": key },
    });
    expect(internal.statusCode).toBe(200);
    expect(internal.json().session.deleteAfter).toBe(r.json().deleteAfter);
  });

  it("stops serving published media immediately", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    await db
      .update(schema.sessions)
      .set({
        status: "complete",
        videoR2Key: "timelapses/x/original.mp4",
        thumbnailR2Key: "timelapses/x/thumbnail.jpg",
      })
      .where(eq(schema.sessions.id, sessionId));

    await deleteSession(key, sessionId);

    const video = await app.inject({ method: "GET", url: `/api/media/${sessionId}/video.mp4` });
    expect(video.statusCode).toBe(404);
    const thumb = await app.inject({ method: "GET", url: `/api/media/${sessionId}/thumbnail.jpg` });
    expect(thumb.statusCode).toBe(404);
  });

  it("is idempotent and never extends the deadline", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);

    const first = await deleteSession(key, sessionId);
    const second = await deleteSession(key, sessionId);
    expect(second.statusCode).toBe(202);
    expect(second.json().deleteAfter).toBe(first.json().deleteAfter);
  });

  it("refuses another program's session as if it didn't exist", async () => {
    const owner = await programKey();
    const other = await programKey();
    const { sessionId } = await createSession(owner);

    const r = await deleteSession(other, sessionId);
    expect(r.statusCode).toBe(404);
    expect((await sessionRow(sessionId)).deleteAfter).toBeNull();
  });

  it("blocks force-stop while scheduled", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    await deleteSession(key, sessionId);

    const stop = await app.inject({
      method: "POST",
      url: `/api/internal/sessions/${sessionId}/stop`,
      headers: { "x-api-key": key },
    });
    expect(stop.statusCode).toBe(409);
  });
});

describe("POST /api/internal/sessions/:id/restore", () => {
  it("brings the session back inside the grace period", async () => {
    const key = await programKey();
    const { token, sessionId } = await createSession(key);
    await deleteSession(key, sessionId);

    const r = await restoreSession(key, sessionId);
    expect(r.statusCode).toBe(200);
    expect((await sessionRow(sessionId)).deleteAfter).toBeNull();

    const get = await app.inject({ method: "GET", url: `/api/sessions/${token}` });
    expect(get.statusCode).toBe(200);
  });

  it("409s for a session that isn't scheduled", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    const r = await restoreSession(key, sessionId);
    expect(r.statusCode).toBe(409);
  });

  it("410s once the grace period has run out", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    await db
      .update(schema.sessions)
      .set({ deleteAfter: new Date(Date.now() - 1000) })
      .where(eq(schema.sessions.id, sessionId));

    const r = await restoreSession(key, sessionId);
    expect(r.statusCode).toBe(410);
  });
});

describe("purgeDeletedSessions", () => {
  it("destroys sessions past their deadline, screenshots included", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    await db.insert(schema.screenshots).values({
      sessionId,
      r2Key: `screenshots/${sessionId}/a.jpg`,
      requestedAt: new Date(),
      minuteBucket: 0,
      confirmed: true,
    });
    await db
      .update(schema.sessions)
      .set({ deleteAfter: new Date(Date.now() - 1000) })
      .where(eq(schema.sessions.id, sessionId));

    await purgeDeletedSessions();

    expect(await sessionRow(sessionId)).toBeUndefined();
    const shots = await db
      .select()
      .from(schema.screenshots)
      .where(eq(schema.screenshots.sessionId, sessionId));
    expect(shots).toEqual([]);
  });

  it("leaves sessions still inside their grace period", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    await deleteSession(key, sessionId);

    await purgeDeletedSessions();

    expect(await sessionRow(sessionId)).toBeDefined();
  });

  it("waits for a compile already in flight", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    await db
      .update(schema.sessions)
      .set({ status: "compiling", deleteAfter: new Date(Date.now() - 1000) })
      .where(eq(schema.sessions.id, sessionId));

    await purgeDeletedSessions();

    expect(await sessionRow(sessionId)).toBeDefined();
  });
});
