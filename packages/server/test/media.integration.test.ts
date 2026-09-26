/**
 * Integration tests for the public media endpoints against a real Postgres.
 *
 * The contract: `/api/media/:id/video.mp4` and `thumbnail.jpg` are the
 * permanent URLs programs and clients hold on to. With R2_PUBLIC_DOMAIN set
 * they redirect to the object's stable public URL; without it they fall back
 * to presigning (video) or streaming the bytes (thumbnail). The editor's
 * uncut original is served by a different, token-gated route that this
 * change deliberately leaves presigned.
 *
 * Requires the test docker postgres on port 5434 (see test/setup.ts).
 */
import { afterAll, afterEach, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { sql } from "drizzle-orm";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";

let app: FastifyInstance;
const savedDomain = process.env.R2_PUBLIC_DOMAIN;

beforeEach(async () => {
  await db.execute(sql`TRUNCATE screenshots, sessions RESTART IDENTITY CASCADE`);
  if (!app) {
    app = await buildApp();
  }
});

afterEach(() => {
  if (savedDomain === undefined) delete process.env.R2_PUBLIC_DOMAIN;
  else process.env.R2_PUBLIC_DOMAIN = savedDomain;
});

afterAll(async () => {
  if (app) await app.close();
  await (db.$client as any).end?.();
});

const VIDEO_KEY = "timelapses/x/edited.mp4";
const THUMB_KEY = "timelapses/x/thumbnail.jpg";

/** A published session: `complete`, with a video and thumbnail in R2. */
async function seedSession(
  overrides: Partial<typeof schema.sessions.$inferInsert> = {},
) {
  const now = new Date();
  const [s] = await db
    .insert(schema.sessions)
    .values({
      name: "media-test",
      status: "complete",
      trackingMode: "credit",
      trackedSeconds: 60,
      startedAt: new Date(now.getTime() - 120_000),
      stoppedAt: now,
      videoR2Key: VIDEO_KEY,
      originalVideoR2Key: VIDEO_KEY,
      thumbnailR2Key: THUMB_KEY,
      videoCopyAligned: true,
      videoUnits: [],
      ...overrides,
    })
    .returning({ id: schema.sessions.id });
  return s.id;
}

describe("GET /api/media/:id/video.mp4", () => {
  it("presigns when no public domain is configured", async () => {
    delete process.env.R2_PUBLIC_DOMAIN;
    const id = await seedSession();
    const r = await app.inject({ method: "GET", url: `/api/media/${id}/video.mp4` });
    expect(r.statusCode).toBe(302);
    // The presigner is stubbed in test/setup.ts to return this fixed URL.
    expect(r.headers.location).toBe("https://r2.test/fake-upload-url");
    expect(r.headers["cache-control"]).toBe("public, max-age=1800");
  });

  it("redirects to the stable public URL when a public domain is configured", async () => {
    process.env.R2_PUBLIC_DOMAIN = "cdn.example.com";
    const id = await seedSession();
    const r = await app.inject({ method: "GET", url: `/api/media/${id}/video.mp4` });
    expect(r.statusCode).toBe(302);
    expect(r.headers.location).toBe(`https://cdn.example.com/${VIDEO_KEY}`);
    expect(r.headers["cache-control"]).toBe("public, max-age=1800");
  });

  it("still 404s for an unpublished session, public domain or not", async () => {
    process.env.R2_PUBLIC_DOMAIN = "cdn.example.com";
    const id = await seedSession({ status: "stopped", videoR2Key: null });
    const r = await app.inject({ method: "GET", url: `/api/media/${id}/video.mp4` });
    expect(r.statusCode).toBe(404);
  });
});

describe("GET /api/media/:id/thumbnail.jpg", () => {
  it("streams the bytes when no public domain is configured", async () => {
    delete process.env.R2_PUBLIC_DOMAIN;
    const id = await seedSession();
    const r = await app.inject({ method: "GET", url: `/api/media/${id}/thumbnail.jpg` });
    expect(r.statusCode).toBe(200);
    expect(r.headers["content-type"]).toBe("image/jpeg");
  });

  it("redirects to the stable public URL when a public domain is configured", async () => {
    process.env.R2_PUBLIC_DOMAIN = "cdn.example.com";
    const id = await seedSession();
    const r = await app.inject({ method: "GET", url: `/api/media/${id}/thumbnail.jpg` });
    expect(r.statusCode).toBe(302);
    expect(r.headers.location).toBe(`https://cdn.example.com/${THUMB_KEY}`);
    expect(r.headers["cache-control"]).toContain("max-age=86400");
  });
});
