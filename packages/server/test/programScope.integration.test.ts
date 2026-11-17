/**
 * Integration tests: every per-session internal route is scoped to the
 * program that created the session.
 *
 * Session ids are public (every shared media URL carries one), and the GET
 * response carries the session token, so an unscoped read let any program
 * with a key take over another program's recording. Another program's
 * session must look exactly like one that doesn't exist.
 *
 * Requires the test postgres (see test/setup.ts).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { randomBytes } from "node:crypto";
import { eq, sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";

let app: FastifyInstance;

beforeEach(async () => {
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) app = await buildApp();
});

afterAll(async () => {
  if (app) await app.close();
  await (db.$client as any).end?.();
});

async function programKey(): Promise<string> {
  const name = `scope-${randomBytes(4).toString("hex")}`;
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

describe("GET /api/internal/sessions/:id", () => {
  it("serves the owner, token included", async () => {
    const key = await programKey();
    const { sessionId, token } = await createSession(key);
    const r = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${sessionId}`,
      headers: { "x-api-key": key },
    });
    expect(r.statusCode).toBe(200);
    expect(r.json().session.token).toBe(token);
  });

  it("404s another program, and never leaks the token", async () => {
    const owner = await programKey();
    const other = await programKey();
    const { sessionId, token } = await createSession(owner);
    const r = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${sessionId}`,
      headers: { "x-api-key": other },
    });
    expect(r.statusCode).toBe(404);
    expect(r.body).not.toContain(token);
  });
});

describe("POST /api/internal/sessions/batch", () => {
  async function batch(key: string, sessionIds: string[]) {
    return app.inject({
      method: "POST",
      url: "/api/internal/sessions/batch",
      headers: { "x-api-key": key },
      payload: { sessionIds },
    });
  }

  it("answers for each session as the single GET does", async () => {
    const key = await programKey();
    const a = await createSession(key);
    const b = await createSession(key);

    const r = await batch(key, [a.sessionId, b.sessionId]);
    expect(r.statusCode).toBe(200);

    const body = r.json();
    expect(body.missing).toEqual([]);
    expect(body.sessions.map((x: any) => x.session.id)).toEqual([a.sessionId, b.sessionId]);

    for (const { sessionId } of [a, b]) {
      const single = await app.inject({
        method: "GET",
        url: `/api/internal/sessions/${sessionId}`,
        headers: { "x-api-key": key },
      });
      expect(body.sessions.find((x: any) => x.session.id === sessionId)).toEqual(single.json());
    }
  });

  it("counts each session's own screenshots", async () => {
    const key = await programKey();
    const a = await createSession(key);
    const b = await createSession(key);

    const shot = (sessionId: string, minute: number, clientInfo: string | null) => ({
      sessionId,
      r2Key: `screenshots/${sessionId}/${minute}.jpg`,
      minuteBucket: minute,
      confirmed: true,
      requestedAt: new Date(Date.UTC(2025, 5, 1, 12, minute)),
      clientInfo,
    });
    await db.insert(schema.screenshots).values([
      shot(a.sessionId, 0, "Lookout Desktop/1.0 (first)"),
      shot(a.sessionId, 1, "Lookout Desktop/1.0 (later)"),
      shot(a.sessionId, 2, null),
      shot(b.sessionId, 0, null),
    ]);

    const body = (await batch(key, [a.sessionId, b.sessionId])).json();
    const byId = new Map(body.sessions.map((x: any) => [x.session.id, x]));

    expect(byId.get(a.sessionId)).toMatchObject({ screenshotCount: 3, clientInfo: "Lookout Desktop/1.0 (first)" });
    expect(byId.get(b.sessionId)).toMatchObject({ screenshotCount: 1, clientInfo: null });
  });

  it("lists another program's sessions and unknown ids as missing, without their tokens", async () => {
    const owner = await programKey();
    const other = await programKey();
    const mine = await createSession(other);
    const theirs = await createSession(owner);
    const unknown = "00000000-0000-4000-8000-000000000000";

    const r = await batch(other, [mine.sessionId, theirs.sessionId, unknown]);
    expect(r.statusCode).toBe(200);

    const body = r.json();
    expect(body.sessions.map((x: any) => x.session.id)).toEqual([mine.sessionId]);
    expect(body.missing).toEqual([theirs.sessionId, unknown]);
    expect(r.body).not.toContain(theirs.token);
  });

  it("refuses an empty or oversized batch", async () => {
    const key = await programKey();
    expect((await batch(key, [])).statusCode).toBe(400);

    const tooMany = Array.from({ length: 101 }, (_, i) =>
      `00000000-0000-4000-8000-${i.toString().padStart(12, "0")}`,
    );
    expect((await batch(key, tooMany)).statusCode).toBe(400);
  });
});

describe("POST /api/internal/sessions/:id/stop", () => {
  it("404s another program and leaves the session running", async () => {
    const owner = await programKey();
    const other = await programKey();
    const { sessionId } = await createSession(owner);
    const r = await app.inject({
      method: "POST",
      url: `/api/internal/sessions/${sessionId}/stop`,
      headers: { "x-api-key": other },
    });
    expect(r.statusCode).toBe(404);
    const [row] = await db
      .select({ status: schema.sessions.status })
      .from(schema.sessions)
      .where(eq(schema.sessions.id, sessionId));
    expect(row.status).toBe("pending");
  });

  it("still lets the owner stop it", async () => {
    const key = await programKey();
    const { sessionId } = await createSession(key);
    const r = await app.inject({
      method: "POST",
      url: `/api/internal/sessions/${sessionId}/stop`,
      headers: { "x-api-key": key },
    });
    expect(r.statusCode).toBe(200);
  });
});

describe("POST /api/internal/sessions/:id/recompile", () => {
  it("404s another program and leaves a failed session failed", async () => {
    const owner = await programKey();
    const other = await programKey();
    const { sessionId } = await createSession(owner);
    await db
      .update(schema.sessions)
      .set({ status: "failed" })
      .where(eq(schema.sessions.id, sessionId));
    const r = await app.inject({
      method: "POST",
      url: `/api/internal/sessions/${sessionId}/recompile`,
      headers: { "x-api-key": other },
    });
    expect(r.statusCode).toBe(404);
    const [row] = await db
      .select({ status: schema.sessions.status })
      .from(schema.sessions)
      .where(eq(schema.sessions.id, sessionId));
    expect(row.status).toBe("failed");
  });
});
