/**
 * CORS preflight must allow every method the SDK sends cross-origin.
 *
 * The SDK runs embedded on program origins and calls PATCH /name and PUT
 * /cuts against Lookout. @fastify/cors 11 narrowed its default to
 * GET,HEAD,POST; if the methods list ever falls back to that default, the
 * browser blocks rename and cut edits before the request is sent, and no
 * other test notices, because app.inject never runs a preflight.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import Fastify from "fastify";
import { buildApp, isAllowedOrigin, registerCommon } from "../src/app.js";

let app: FastifyInstance;

beforeAll(async () => {
  app = await buildApp();
});

afterAll(async () => {
  await app.close();
});

describe("CORS preflight", () => {
  it.each(["PATCH", "PUT", "POST", "DELETE"])("allows %s", async (method) => {
    const res = await app.inject({
      method: "OPTIONS",
      url: "/api/sessions/x/name",
      headers: {
        origin: "https://program.hackclub.com",
        "access-control-request-method": method,
      },
    });
    expect(res.statusCode).toBe(204);
    const allowed = String(res.headers["access-control-allow-methods"])
      .split(",")
      .map((m) => m.trim());
    expect(allowed).toContain(method);
  });
});

describe("origins", () => {
  it("answers a disallowed origin normally, just without CORS headers", async () => {
    // Used to be an Error → 500 → Sentry issue (LOOKOUT-BACKEND-F).
    const res = await app.inject({
      method: "GET",
      url: "/api/health",
      headers: { origin: "https://www.oxygenated.uk" },
    });
    expect(res.statusCode).toBe(200);
    expect(res.headers["access-control-allow-origin"]).toBeUndefined();
  });

  it("allows *.hackclub.com", async () => {
    const res = await app.inject({
      method: "GET",
      url: "/api/health",
      headers: { origin: "https://program.hackclub.com" },
    });
    expect(res.statusCode).toBe(200);
    expect(res.headers["access-control-allow-origin"]).toBe(
      "https://program.hackclub.com",
    );
  });
});

describe("isAllowedOrigin", () => {
  const prod = { isDev: false, baseUrlHostname: "lookout.example.org" };
  it.each([
    [undefined, true],
    ["tauri://localhost", true],
    ["http://tauri.localhost", true],
    ["https://hackclub.com", true],
    ["https://x.hackclub.com", true],
    ["https://lookout.example.org", true],
    ["https://evilhackclub.com", false],
    ["https://hackclub.com.evil.example", false],
    ["http://localhost:5173", false],
    ["not a url", false],
  ])("%s → %s", (origin, expected) => {
    expect(isAllowedOrigin(origin, prod)).toBe(expected);
  });
});

describe("database outage", () => {
  it("maps a transient DB error to 503 + Retry-After", async () => {
    const outage = Fastify();
    await registerCommon(outage);
    outage.get("/boom", async () => {
      throw new Error("Failed query: select 1", {
        cause: Object.assign(new Error("the database system is shutting down"), {
          code: "57P03",
        }),
      });
    });
    outage.get("/bug", async () => {
      throw new Error("not a db problem");
    });

    const down = await outage.inject({ method: "GET", url: "/boom" });
    expect(down.statusCode).toBe(503);
    expect(down.headers["retry-after"]).toBe("5");

    const bug = await outage.inject({ method: "GET", url: "/bug" });
    expect(bug.statusCode).toBe(500);
    await outage.close();
  });
});
