/**
 * /api/health is what Orchard's readiness probe hits during a rolling deploy.
 * If it stops answering 200 on a healthy pod, new pods never become ready and
 * every deploy stalls, so pin its contract here.
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

describe("GET /api/health", () => {
  it("answers 200 when the database is reachable", async () => {
    const res = await app.inject({ method: "GET", url: "/api/health" });
    expect(res.statusCode).toBe(200);
    expect(res.json()).toEqual({ status: "ok" });
  });
});
