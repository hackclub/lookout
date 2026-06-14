/**
 * Integration tests for programs, their API keys, and the admin dashboard.
 * Covers: program-key auth + session tagging (name + programId), rejection of
 * unknown keys, admin basic-auth gating, program CRUD, new-session URL
 * management, and delete-guard. Drives buildApp() via app.inject; requires the
 * docker test postgres on port 5434 (see test/setup.ts).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";

let app: FastifyInstance;

const ADMIN_AUTH =
  "Basic " +
  Buffer.from(
    `${process.env.ADMIN_USERNAME}:${process.env.ADMIN_PASSWORD}`,
  ).toString("base64");

beforeEach(async () => {
  await db.execute(
    sql`TRUNCATE screenshots, sessions, api_keys, programs, announcements RESTART IDENTITY CASCADE`,
  );
  if (!app) {
    app = await buildApp();
  }
});

afterAll(async () => {
  if (app) await app.close();
});

function createSession(apiKey?: string) {
  return app.inject({
    method: "POST",
    url: "/api/internal/sessions",
    headers: apiKey ? { "x-api-key": apiKey } : {},
    payload: {},
  });
}

async function createProgram(
  name: string,
  newSessionUrl?: string,
): Promise<{ id: string; key: string }> {
  const r = await app.inject({
    method: "POST",
    url: "/api/admin/programs",
    headers: { authorization: ADMIN_AUTH },
    payload: newSessionUrl ? { name, newSessionUrl } : { name },
  });
  expect(r.statusCode).toBe(201);
  return { id: r.json().id as string, key: r.json().key as string };
}

describe("internal auth", () => {
  it("rejects a missing or invalid key with 401", async () => {
    expect((await createSession()).statusCode).toBe(401);
    expect((await createSession("nope")).statusCode).toBe(401);
  });

  it("program key authorizes and tags the session with its program", async () => {
    const { key } = await createProgram("arcade");
    const r = await createSession(key);
    expect(r.statusCode).toBe(201);
    const sessionId = r.json().sessionId;

    const detail = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${sessionId}`,
      headers: { "x-api-key": key },
    });
    // Dual-write: legacy text name and canonical FK are both set.
    expect(detail.json().session.program).toBe("arcade");
    expect(detail.json().session.programId).toBeTruthy();
  });

  it("deleting a program revokes its key", async () => {
    const { id, key } = await createProgram("temp");
    expect((await createSession(key)).statusCode).toBe(201);

    // The program now has a session, so deletion is blocked.
    const blocked = await app.inject({
      method: "DELETE",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
    });
    expect(blocked.statusCode).toBe(409);

    // Remove the session, then deletion succeeds and the key stops authorizing.
    await db.execute(sql`DELETE FROM sessions`);
    const del = await app.inject({
      method: "DELETE",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
    });
    expect(del.statusCode).toBe(200);
    expect((await createSession(key)).statusCode).toBe(401);
  });
});

describe("admin dashboard", () => {
  it("rejects missing/wrong basic auth with 401", async () => {
    const noAuth = await app.inject({ method: "GET", url: "/api/admin/programs" });
    expect(noAuth.statusCode).toBe(401);
    expect(noAuth.headers["www-authenticate"]).toContain("Basic");

    const wrong = await app.inject({
      method: "GET",
      url: "/api/admin/programs",
      headers: {
        authorization: "Basic " + Buffer.from("admin:wrong").toString("base64"),
      },
    });
    expect(wrong.statusCode).toBe(401);
  });

  it("creates a program (+key), lists it, and rejects duplicate names", async () => {
    const created = await app.inject({
      method: "POST",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
      payload: { name: "blog", newSessionUrl: "https://blog.example.com/new?desktop=true" },
    });
    expect(created.statusCode).toBe(201);
    expect(created.json().key).toMatch(/^lk_[0-9a-f]{48}$/);
    expect(created.json().newSessionUrl).toBe(
      "https://blog.example.com/new?desktop=true",
    );

    const list = await app.inject({
      method: "GET",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(list.json().programs).toHaveLength(1);
    const prog = list.json().programs[0];
    expect(prog.name).toBe("blog");
    expect(prog.keys).toHaveLength(1);
    expect(prog.newSessionUrl).toBe("https://blog.example.com/new?desktop=true");

    const dup = await app.inject({
      method: "POST",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
      payload: { name: "blog" },
    });
    expect(dup.statusCode).toBe(409);
  });

  it("rejects a non-http new-session URL", async () => {
    const bad = await app.inject({
      method: "POST",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
      payload: { name: "weird", newSessionUrl: "javascript:alert(1)" },
    });
    expect(bad.statusCode).toBe(400);
  });

  it("sets and clears a program's new-session URL", async () => {
    const { id } = await createProgram("arcade");

    const set = await app.inject({
      method: "PATCH",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
      payload: { newSessionUrl: "https://arcade.example.com/new?desktop=true" },
    });
    expect(set.statusCode).toBe(200);
    expect(set.json().newSessionUrl).toBe(
      "https://arcade.example.com/new?desktop=true",
    );

    const clear = await app.inject({
      method: "PATCH",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
      payload: { newSessionUrl: "" },
    });
    expect(clear.statusCode).toBe(200);
    expect(clear.json().newSessionUrl).toBeNull();
  });

  it("sets a display name on create and via patch (set + clear)", async () => {
    const created = await app.inject({
      method: "POST",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
      payload: { name: "arcade", displayName: "Arcade" },
    });
    expect(created.statusCode).toBe(201);
    expect(created.json().displayName).toBe("Arcade");
    const id = created.json().id;

    const list = await app.inject({
      method: "GET",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(list.json().programs[0].displayName).toBe("Arcade");

    const set = await app.inject({
      method: "PATCH",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
      payload: { displayName: "The Arcade" },
    });
    expect(set.statusCode).toBe(200);
    expect(set.json().displayName).toBe("The Arcade");

    const clear = await app.inject({
      method: "PATCH",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
      payload: { displayName: "" },
    });
    expect(clear.statusCode).toBe(200);
    expect(clear.json().displayName).toBeNull();
  });

  it("reports per-program session aggregates and global totals", async () => {
    await createProgram("arcade");
    await createProgram("blog");

    // arcade: 2 complete (3600s + null→fall back to active 1800s), plus two
    // 'failed' rows that the admin stats split apart — one with no confirmed
    // screenshots (reported as "empty") and one with a confirmed screenshot
    // (a real "failed"). blog: 1 active. One NULL-program session (global key)
    // counts only in totals.
    const inserted = await db
      .insert(schema.sessions)
      .values([
        { program: "arcade", status: "complete", trackedSeconds: 3600 },
        { program: "arcade", status: "complete", totalActiveSeconds: 1800 },
        { program: "arcade", status: "failed", trackedSeconds: 600 },
        { program: "arcade", status: "failed", trackedSeconds: 300 },
        { program: "blog", status: "active", trackedSeconds: 120 },
        { program: null, status: "complete", trackedSeconds: 7200 },
      ])
      .returning({
        id: schema.sessions.id,
        program: schema.sessions.program,
        status: schema.sessions.status,
        trackedSeconds: schema.sessions.trackedSeconds,
      });

    // Give the 300s arcade 'failed' session a confirmed screenshot so it counts
    // as a real failure; the 600s one stays screenshot-less → "empty".
    const realFailed = inserted.find(
      (s) => s.program === "arcade" && s.status === "failed" && s.trackedSeconds === 300,
    );
    await db.insert(schema.screenshots).values({
      sessionId: realFailed!.id,
      r2Key: "test/shot.jpg",
      requestedAt: new Date(),
      minuteBucket: 0,
      confirmed: true,
    });

    const res = await app.inject({
      method: "GET",
      url: "/api/admin/programs",
      headers: { authorization: ADMIN_AUTH },
    });
    const body = res.json();

    const arcade = body.programs.find((p: any) => p.name === "arcade");
    expect(arcade.sessionCount).toBe(4);
    expect(arcade.trackedSeconds).toBe(3600 + 1800 + 600 + 300);
    expect(arcade.statusCounts).toMatchObject({
      complete: 2,
      empty: 1,
      failed: 1,
      active: 0,
    });

    const blog = body.programs.find((p: any) => p.name === "blog");
    expect(blog.sessionCount).toBe(1);
    expect(blog.statusCounts.active).toBe(1);

    // Totals span every session, including the NULL-program one.
    expect(body.totals.sessionCount).toBe(6);
    expect(body.totals.trackedSeconds).toBe(3600 + 1800 + 600 + 300 + 120 + 7200);
    expect(body.totals.statusCounts).toMatchObject({
      complete: 3,
      empty: 1,
      failed: 1,
      active: 1,
    });
  });

  it("serves the dashboard page behind basic auth", async () => {
    const page = await app.inject({
      method: "GET",
      url: "/admin",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(page.statusCode).toBe(200);
    expect(page.headers["content-type"]).toContain("text/html");
    expect(page.body).toContain("Programs");
  });
});

describe("public programs registry", () => {
  it("lists only programs with a new-session URL", async () => {
    await createProgram("withurl", "https://withurl.example.com/new?desktop=true");
    await createProgram("nourl");

    const res = await app.inject({ method: "GET", url: "/api/programs" });
    expect(res.statusCode).toBe(200);
    const programs = res.json().programs;
    expect(programs).toHaveLength(1);
    expect(programs[0]).toMatchObject({
      name: "withurl",
      // No display name set → falls back to the raw name.
      displayName: "withurl",
      newSessionUrl: "https://withurl.example.com/new?desktop=true",
    });
  });

  it("exposes the display name (when set) in the public registry", async () => {
    const { id } = await createProgram(
      "arcade",
      "https://arcade.example.com/new?desktop=true",
    );
    await app.inject({
      method: "PATCH",
      url: `/api/admin/programs/${id}`,
      headers: { authorization: ADMIN_AUTH },
      payload: { displayName: "Arcade" },
    });

    const res = await app.inject({ method: "GET", url: "/api/programs" });
    expect(res.json().programs[0]).toMatchObject({
      name: "arcade",
      displayName: "Arcade",
    });
  });

  it("returns an empty list when no program has a URL (client must not break)", async () => {
    await createProgram("nourl");
    const res = await app.inject({ method: "GET", url: "/api/programs" });
    expect(res.statusCode).toBe(200);
    expect(res.json().programs).toEqual([]);
  });
});

describe("announcements", () => {
  async function setAnnouncement(body: Record<string, unknown>) {
    return app.inject({
      method: "POST",
      url: "/api/admin/announcement",
      headers: { authorization: ADMIN_AUTH },
      payload: body,
    });
  }

  it("returns null when nothing is set (public + admin)", async () => {
    const pub = await app.inject({ method: "GET", url: "/api/announcement" });
    expect(pub.statusCode).toBe(200);
    expect(pub.json().announcement).toBeNull();

    const adm = await app.inject({
      method: "GET",
      url: "/api/admin/announcement",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(adm.json().announcement).toBeNull();
  });

  it("requires admin auth for the public endpoint to stay open but admin to be gated", async () => {
    const unauth = await app.inject({ method: "GET", url: "/api/admin/announcement" });
    expect(unauth.statusCode).toBe(401);
    // Public read needs no auth.
    const pub = await app.inject({ method: "GET", url: "/api/announcement" });
    expect(pub.statusCode).toBe(200);
  });

  it("sets an announcement and serves it publicly", async () => {
    const set = await setAnnouncement({
      level: "warning",
      message: "Scheduled maintenance tonight.",
      url: "https://status.example.com",
    });
    expect(set.statusCode).toBe(201);
    expect(set.json()).toMatchObject({
      level: "warning",
      message: "Scheduled maintenance tonight.",
      url: "https://status.example.com",
    });

    const pub = await app.inject({ method: "GET", url: "/api/announcement" });
    expect(pub.json().announcement).toMatchObject({
      level: "warning",
      message: "Scheduled maintenance tonight.",
      url: "https://status.example.com",
    });
  });

  it("keeps only the latest active announcement", async () => {
    await setAnnouncement({ level: "info", message: "First" });
    await setAnnouncement({ level: "danger", message: "Second" });

    const pub = await app.inject({ method: "GET", url: "/api/announcement" });
    expect(pub.json().announcement).toMatchObject({ level: "danger", message: "Second" });
  });

  it("clears the announcement (idempotently)", async () => {
    await setAnnouncement({ level: "info", message: "Hello" });

    const cleared = await app.inject({
      method: "DELETE",
      url: "/api/admin/announcement",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(cleared.statusCode).toBe(200);

    const pub = await app.inject({ method: "GET", url: "/api/announcement" });
    expect(pub.json().announcement).toBeNull();

    // Clearing again is a no-op, not an error.
    const again = await app.inject({
      method: "DELETE",
      url: "/api/admin/announcement",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(again.statusCode).toBe(200);
  });

  it("rejects an unknown level and a non-http url", async () => {
    const badLevel = await setAnnouncement({ level: "critical", message: "x" });
    expect(badLevel.statusCode).toBe(400);

    const badUrl = await setAnnouncement({
      level: "info",
      message: "x",
      url: "javascript:alert(1)",
    });
    expect(badUrl.statusCode).toBe(400);
  });

  it("allows an announcement without a url (null)", async () => {
    const set = await setAnnouncement({ level: "success", message: "All good!" });
    expect(set.statusCode).toBe(201);
    expect(set.json().url).toBeNull();

    const pub = await app.inject({ method: "GET", url: "/api/announcement" });
    expect(pub.json().announcement).toMatchObject({
      level: "success",
      message: "All good!",
      url: null,
    });
  });
});
