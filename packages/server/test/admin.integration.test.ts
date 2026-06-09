/**
 * Integration tests for program-based API keys and the admin dashboard.
 * Covers: program-key auth + session tagging, rejection of unknown keys,
 * admin basic-auth gating, and key CRUD. Drives buildApp() via app.inject;
 * requires the docker test postgres on port 5434 (see test/setup.ts).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db } from "../src/db/index.js";

let app: FastifyInstance;

const ADMIN_AUTH =
  "Basic " +
  Buffer.from(
    `${process.env.ADMIN_USERNAME}:${process.env.ADMIN_PASSWORD}`,
  ).toString("base64");

beforeEach(async () => {
  await db.execute(
    sql`TRUNCATE screenshots, sessions, api_keys RESTART IDENTITY CASCADE`,
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

async function createKey(name: string): Promise<string> {
  const r = await app.inject({
    method: "POST",
    url: "/api/admin/keys",
    headers: { authorization: ADMIN_AUTH },
    payload: { name },
  });
  expect(r.statusCode).toBe(201);
  return r.json().key as string;
}

describe("internal auth", () => {
  it("rejects a missing or invalid key with 401", async () => {
    expect((await createSession()).statusCode).toBe(401);
    expect((await createSession("nope")).statusCode).toBe(401);
  });

  it("program key authorizes and tags the session with its name", async () => {
    const key = await createKey("arcade");
    const r = await createSession(key);
    expect(r.statusCode).toBe(201);
    const sessionId = r.json().sessionId;

    const detail = await app.inject({
      method: "GET",
      url: `/api/internal/sessions/${sessionId}`,
      headers: { "x-api-key": key },
    });
    expect(detail.json().session.program).toBe("arcade");
  });

  it("a deleted program key stops authorizing", async () => {
    const key = await createKey("temp");
    expect((await createSession(key)).statusCode).toBe(201);

    const list = await app.inject({
      method: "GET",
      url: "/api/admin/keys",
      headers: { authorization: ADMIN_AUTH },
    });
    const id = list.json().keys.find((k: any) => k.name === "temp").id;

    const del = await app.inject({
      method: "DELETE",
      url: `/api/admin/keys/${id}`,
      headers: { authorization: ADMIN_AUTH },
    });
    expect(del.statusCode).toBe(200);
    expect((await createSession(key)).statusCode).toBe(401);
  });
});

describe("admin dashboard", () => {
  it("rejects missing/wrong basic auth with 401", async () => {
    const noAuth = await app.inject({ method: "GET", url: "/api/admin/keys" });
    expect(noAuth.statusCode).toBe(401);
    expect(noAuth.headers["www-authenticate"]).toContain("Basic");

    const wrong = await app.inject({
      method: "GET",
      url: "/api/admin/keys",
      headers: {
        authorization: "Basic " + Buffer.from("admin:wrong").toString("base64"),
      },
    });
    expect(wrong.statusCode).toBe(401);
  });

  it("creates, lists, and rejects duplicate names", async () => {
    const created = await app.inject({
      method: "POST",
      url: "/api/admin/keys",
      headers: { authorization: ADMIN_AUTH },
      payload: { name: "blog" },
    });
    expect(created.statusCode).toBe(201);
    expect(created.json().key).toMatch(/^lk_[0-9a-f]{48}$/);

    const list = await app.inject({
      method: "GET",
      url: "/api/admin/keys",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(list.json().keys).toHaveLength(1);
    expect(list.json().keys[0].name).toBe("blog");

    const dup = await app.inject({
      method: "POST",
      url: "/api/admin/keys",
      headers: { authorization: ADMIN_AUTH },
      payload: { name: "blog" },
    });
    expect(dup.statusCode).toBe(409);
  });

  it("serves the dashboard page behind basic auth", async () => {
    const page = await app.inject({
      method: "GET",
      url: "/admin",
      headers: { authorization: ADMIN_AUTH },
    });
    expect(page.statusCode).toBe(200);
    expect(page.headers["content-type"]).toContain("text/html");
    expect(page.body).toContain("Program API Keys");
  });
});
