/**
 * Read-only API keys: a second key on a program that reads the program's
 * sessions (list, details, batch, activity) and is refused every write.
 * Minted and revoked from the admin API; a program's full key can't be
 * revoked there.
 *
 * Requires the test postgres (see test/setup.ts).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { randomBytes } from "node:crypto";
import { sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";
import { READ_ONLY_ROUTES } from "../src/routes/internal.js";

let app: FastifyInstance;

const ADMIN_AUTH =
  "Basic " +
  Buffer.from(`${process.env.ADMIN_USERNAME}:${process.env.ADMIN_PASSWORD}`).toString("base64");

beforeEach(async () => {
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) app = await buildApp();
});

afterAll(async () => {
  if (app) await app.close();
  await (db.$client as any).end?.();
});

/** A program with its full key, and a read-only key minted through the admin API. */
async function program() {
  const name = `ro-${randomBytes(4).toString("hex")}`;
  const [p] = await db.insert(schema.programs).values({ name }).returning({ id: schema.programs.id });
  const [full] = await db
    .insert(schema.apiKeys)
    .values({ name, programId: p.id })
    .returning({ key: schema.apiKeys.key });
  const minted = await app.inject({
    method: "POST",
    url: `/api/admin/programs/${p.id}/keys`,
    headers: { authorization: ADMIN_AUTH },
    payload: { label: "assay" },
  });
  expect(minted.statusCode).toBe(201);
  expect(minted.json()).toMatchObject({ name: `${name}:assay`, readOnly: true });
  return { id: p.id, name, full: full.key, readOnly: minted.json().key as string, readOnlyId: minted.json().id as string };
}

async function createSession(key: string) {
  const r = await app.inject({
    method: "POST",
    url: "/api/internal/sessions",
    headers: { "x-api-key": key },
    payload: {},
  });
  return r;
}

describe("read-only keys", () => {
  it("read the program's sessions", async () => {
    const p = await program();
    const created = await createSession(p.full);
    expect(created.statusCode).toBe(201);
    const { sessionId } = created.json();

    const reads = [
      { method: "GET" as const, url: "/api/internal/sessions" },
      { method: "GET" as const, url: `/api/internal/sessions/${sessionId}` },
      { method: "GET" as const, url: `/api/internal/sessions/${sessionId}/activity` },
      { method: "POST" as const, url: "/api/internal/sessions/batch", payload: { sessionIds: [sessionId] } },
    ];
    for (const r of reads) {
      const res = await app.inject({ ...r, headers: { "x-api-key": p.readOnly } });
      expect(res.statusCode, `${r.method} ${r.url}`).toBe(200);
    }
  });

  it("are refused every write, and the session is untouched", async () => {
    const p = await program();
    const { sessionId } = (await createSession(p.full)).json();

    const writes = [
      { method: "POST" as const, url: "/api/internal/sessions", payload: {} },
      { method: "POST" as const, url: `/api/internal/sessions/${sessionId}/stop`, payload: {} },
      { method: "POST" as const, url: `/api/internal/sessions/${sessionId}/panel-resolved`, payload: {} },
      { method: "POST" as const, url: `/api/internal/sessions/${sessionId}/view-url`, payload: { viewUrl: null } },
      { method: "DELETE" as const, url: `/api/internal/sessions/${sessionId}` },
      { method: "POST" as const, url: `/api/internal/sessions/${sessionId}/restore`, payload: {} },
      { method: "POST" as const, url: `/api/internal/sessions/${sessionId}/recompile`, payload: {} },
    ];
    for (const w of writes) {
      const res = await app.inject({ ...w, headers: { "x-api-key": p.readOnly } });
      expect(res.statusCode, `${w.method} ${w.url}`).toBe(403);
    }
    const row = await db.query.sessions.findFirst({ where: (t, { eq }) => eq(t.id, sessionId) });
    expect(row?.status).toBe("pending");
    expect(row?.deleteAfter).toBeNull();
  });

  it("are refused token lookups (not on the read-only list)", async () => {
    const p = await program();
    const { token } = (await createSession(p.full)).json();
    const res = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/by-token/${token}`,
      headers: { "x-api-key": p.readOnly },
    });
    expect(res.statusCode).toBe(403);
  });

  it("only reach their own program's sessions", async () => {
    const a = await program();
    const b = await program();
    const { sessionId } = (await createSession(a.full)).json();
    for (const url of [`/api/internal/sessions/${sessionId}`, `/api/internal/sessions/${sessionId}/activity`]) {
      const res = await app.inject({ method: "GET", url, headers: { "x-api-key": b.readOnly } });
      expect(res.statusCode).toBe(404);
    }
  });

  it("name only routes that exist", () => {
    // A typo here would silently close a read to read-only keys, or leave a
    // stale entry behind after a route is renamed.
    for (const r of READ_ONLY_ROUTES) {
      const [method, url] = r.split(" ");
      expect(app.hasRoute({ method: method as "GET" | "POST", url }), r).toBe(true);
    }
  });

  it("leave a full key's access alone", async () => {
    const p = await program();
    const created = await createSession(p.full);
    expect(created.statusCode).toBe(201);
    const stop = await app.inject({
      method: "POST",
      url: `/api/internal/sessions/${created.json().sessionId}/stop`,
      headers: { "x-api-key": p.full },
      payload: {},
    });
    expect(stop.statusCode).not.toBe(403);
  });
});

describe("admin: read-only keys", () => {
  it("are listed as read-only under their program", async () => {
    const p = await program();
    const res = await app.inject({ method: "GET", url: "/api/admin/programs", headers: { authorization: ADMIN_AUTH } });
    expect(res.statusCode).toBe(200);
    const listed = res.json().programs.find((x: { id: string }) => x.id === p.id);
    expect(listed.keys.map((k: { readOnly: boolean }) => k.readOnly).sort()).toEqual([false, true]);
  });

  it("refuse a duplicate label and a bad one", async () => {
    const p = await program();
    for (const [label, status] of [["assay", 409], ["Has Spaces", 400], ["", 400]] as const) {
      const res = await app.inject({
        method: "POST",
        url: `/api/admin/programs/${p.id}/keys`,
        headers: { authorization: ADMIN_AUTH },
        payload: { label },
      });
      expect(res.statusCode, label).toBe(status);
    }
  });

  it("revoke a read-only key, never a full one", async () => {
    const p = await program();
    const full = await db.query.apiKeys.findFirst({ where: (t, { eq }) => eq(t.key, p.full) });
    const refuse = await app.inject({
      method: "DELETE",
      url: `/api/admin/keys/${full!.id}`,
      headers: { authorization: ADMIN_AUTH },
    });
    expect(refuse.statusCode).toBe(409);

    const revoke = await app.inject({
      method: "DELETE",
      url: `/api/admin/keys/${p.readOnlyId}`,
      headers: { authorization: ADMIN_AUTH },
    });
    expect(revoke.statusCode).toBe(200);
    const after = await app.inject({ method: "GET", url: "/api/internal/sessions", headers: { "x-api-key": p.readOnly } });
    expect(after.statusCode).toBe(401);
  });

  it("need admin auth to mint", async () => {
    const p = await program();
    const res = await app.inject({ method: "POST", url: `/api/admin/programs/${p.id}/keys`, payload: { label: "x" } });
    expect(res.statusCode).toBe(401);
  });
});
