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
import { buildApp } from "../src/app.js";

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
