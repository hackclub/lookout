/**
 * Integration tests for the stop-time edit flow against a real Postgres.
 *
 * The invariant under test: a session reaches `complete` exactly once, with
 * the user's cuts already applied. Editing happens during the stop-time
 * hold and is impossible afterwards, because `complete` is what programs
 * act on (heartbeat forwarding, submissions, the redirect hook).
 *
 * Requires the test docker postgres on port 5434 (see test/setup.ts).
 */
import { afterAll, beforeEach, describe, expect, it } from "vitest";
import type { FastifyInstance } from "fastify";
import { sql, eq } from "drizzle-orm";
import { EDIT_LEASE_SECONDS, EDIT_HOLD_MAX_MINUTES } from "@lookout/shared";
import { buildApp } from "../src/app.js";
import { db, schema } from "../src/db/index.js";

let app: FastifyInstance;

const UNITS = 10;

/**
 * Fixture clock, anchored so the seeded session stopped one minute ago.
 *
 * Deliberately RELATIVE. The edit hold has an absolute ceiling measured from
 * `stoppedAt` (EDIT_HOLD_MAX_MINUTES after the stop), so a session whose stop
 * is further in the past than that can never renew its lease. This was pinned
 * to a hardcoded "2026-07-01" — a future date when it was written, which
 * passed CI happily until the calendar caught up and then failed every lease
 * test with `held: false` for reasons that had nothing to do with leases.
 * Anchoring to now keeps the fixture describing a just-stopped session, which
 * is the state these tests are actually about.
 */
const T0 = new Date(Date.now() - (UNITS + 1) * 60_000);
const minute = (i: number) => new Date(T0.getTime() + i * 60_000);
const iso = (i: number) => minute(i).toISOString();

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

/**
 * Seed a session in its edit hold: stopped, compiled (original + unit map
 * written) but NOT published — `video_r2_key` is still null, exactly the
 * state the worker leaves a held session in.
 */
async function seedHeldSession(
  overrides: Partial<typeof schema.sessions.$inferInsert> = {},
) {
  const [s] = await db
    .insert(schema.sessions)
    .values({
      name: "edit-test",
      status: "stopped",
      trackingMode: "credit",
      trackedSeconds: 540,
      startedAt: minute(0),
      stoppedAt: minute(UNITS),
      editHoldUntil: new Date(Date.now() + EDIT_LEASE_SECONDS * 1000),
      videoR2Key: null,
      originalVideoR2Key: "timelapses/x/original.mp4",
      thumbnailR2Key: "timelapses/x/thumbnail.jpg",
      videoCopyAligned: true,
      videoUnits: Array.from({ length: UNITS }, (_, i) => ({
        capturedAt: iso(i),
        screenshotId: `00000000-0000-0000-0000-0000000000${String(i).padStart(2, "0")}`,
      })),
      ...overrides,
    })
    .returning({ id: schema.sessions.id, token: schema.sessions.token });

  await db.insert(schema.screenshots).values(
    Array.from({ length: UNITS }, (_, i) => ({
      sessionId: s.id,
      r2Key: `screenshots/${s.id}/${i}.jpg`,
      requestedAt: minute(i),
      capturedAt: minute(i),
      minuteBucket: i,
      confirmed: true,
      sampled: true,
      creditedSeconds: i === 0 ? 0 : 60,
    })),
  );

  return s;
}

/** An active session with one confirmed capture, ready to be stopped. */
async function seedActiveSession() {
  const [s] = await db
    .insert(schema.sessions)
    .values({
      name: "active-test",
      status: "active",
      trackingMode: "credit",
      trackedSeconds: 60,
      startedAt: minute(0),
    })
    .returning({ id: schema.sessions.id, token: schema.sessions.token });
  await db.insert(schema.screenshots).values({
    sessionId: s.id,
    r2Key: `screenshots/${s.id}/0.jpg`,
    requestedAt: minute(0),
    capturedAt: minute(0),
    minuteBucket: 0,
    confirmed: true,
    creditedSeconds: 0,
  });
  return s;
}

const load = (id: string) =>
  db.query.sessions.findFirst({ where: eq(schema.sessions.id, id) });

async function putCuts(token: string, cuts: unknown, masks?: unknown) {
  const r = await app.inject({
    method: "PUT",
    url: `/api/sessions/${token}/cuts`,
    payload: masks !== undefined ? { cuts, masks } : { cuts },
  });
  return { status: r.statusCode, body: r.json() };
}

const getJson = async (url: string) => (await app.inject({ method: "GET", url })).json();

describe("POST /stop with { edit }", () => {
  it("opens a short first lease and still enqueues the compile", async () => {
    const s = await seedActiveSession();
    const r = await app.inject({
      method: "POST",
      url: `/api/sessions/${s.token}/stop`,
      payload: { edit: true },
    });
    expect(r.statusCode).toBe(200);
    expect(r.json().editHoldUntil).toBeTruthy();

    const row = await load(s.id);
    expect(row!.status).toBe("stopped");
    // One lease term, not a long fixed window: a client that asks for an
    // edit and never opens an editor publishes ~2 minutes later, not ~30.
    const heldForMs = row!.editHoldUntil!.getTime() - Date.now();
    expect(heldForMs).toBeGreaterThan((EDIT_LEASE_SECONDS - 20) * 1000);
    expect(heldForMs).toBeLessThanOrEqual(EDIT_LEASE_SECONDS * 1000 + 1000);
  });

  it("leaves old clients untouched — no body means no hold", async () => {
    const s = await seedActiveSession();
    const r = await app.inject({
      method: "POST",
      url: `/api/sessions/${s.token}/stop`,
    });
    expect(r.statusCode).toBe(200);
    expect(r.json().editHoldUntil).toBeUndefined();
    expect((await load(s.id))!.editHoldUntil).toBeNull();
  });

  it("ignores an unrecognised body instead of rejecting it", async () => {
    // /stop accepted and ignored any body before `edit` existed. A custom
    // client sending its own field must not start getting 400s.
    const s = await seedActiveSession();
    const r = await app.inject({
      method: "POST",
      url: `/api/sessions/${s.token}/stop`,
      payload: { reason: "user pressed stop", edit: false },
    });
    expect(r.statusCode).toBe(200);
    expect((await load(s.id))!.editHoldUntil).toBeNull();
  });

  it("does not hold a session with nothing recorded", async () => {
    const [s] = await db
      .insert(schema.sessions)
      .values({ name: "empty", status: "active", startedAt: minute(0) })
      .returning({ id: schema.sessions.id, token: schema.sessions.token });
    const r = await app.inject({
      method: "POST",
      url: `/api/sessions/${s.token}/stop`,
      payload: { edit: true },
    });
    expect(r.statusCode).toBe(200);
    expect(r.json().editHoldUntil).toBeUndefined();
    // No screenshots → failed, as before.
    expect((await load(s.id))!.status).toBe("failed");
  });
});

describe("POST /editing (lease renewal)", () => {
  it("extends the hold so an open editor is never cut off", async () => {
    const s = await seedHeldSession({
      // Down to the last few seconds of its term.
      editHoldUntil: new Date(Date.now() + 3_000),
    });
    const before = (await load(s.id))!.editHoldUntil!.getTime();

    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/editing` });
    expect(r.statusCode).toBe(200);
    expect(r.json().held).toBe(true);

    const after = (await load(s.id))!.editHoldUntil!.getTime();
    expect(after).toBeGreaterThan(before);
    // A full fresh lease, not a fixed deadline ticking down.
    expect(after - Date.now()).toBeGreaterThan((EDIT_LEASE_SECONDS - 10) * 1000);
  });

  it("renews through a lapse the expiry job hasn't processed yet", async () => {
    // A stalled network shouldn't end someone's edit in the seconds
    // between the term lapsing and the cron running.
    const s = await seedHeldSession({ editHoldUntil: new Date(Date.now() - 5_000) });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/editing` });
    expect(r.json().held).toBe(true);
    expect((await load(s.id))!.editHoldUntil!.getTime()).toBeGreaterThan(Date.now());
  });

  it("reports not-held once the session published, and doesn't resurrect it", async () => {
    const s = await seedHeldSession({
      status: "complete",
      editHoldUntil: null,
      videoR2Key: "timelapses/x/original.mp4",
    });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/editing` });
    expect(r.statusCode).toBe(200);
    expect(r.json().held).toBe(false);
    const row = await load(s.id);
    expect(row!.status).toBe("complete");
    expect(row!.editHoldUntil).toBeNull();
  });

  it("stops renewing past the absolute ceiling", async () => {
    // An editor left open overnight must not hold a program's session
    // forever, so the ceiling is measured from the stop.
    const s = await seedHeldSession({
      stoppedAt: new Date(Date.now() - (EDIT_HOLD_MAX_MINUTES + 5) * 60_000),
    });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/editing` });
    expect(r.json().held).toBe(false);
  });
});

describe("GET /units", () => {
  it("is editable during the hold and exposes the unit map", async () => {
    const s = await seedHeldSession();
    const body = await getJson(`/api/sessions/${s.token}/units`);
    expect(body.editable).toBe(true);
    expect(body.units).toHaveLength(UNITS);
    expect(body.units[3].capturedAt).toBe(iso(3));
    expect(body.originalVideoUrl).toContain("https://");
    expect(body.editHoldUntil).toBeTruthy();
  });

  it("reports 'preparing' while the compile is in flight", async () => {
    const s = await seedHeldSession({ videoUnits: null, originalVideoR2Key: null });
    const body = await getJson(`/api/sessions/${s.token}/units`);
    expect(body.editable).toBe(false);
    expect(body.editableReason).toBe("preparing");
    // The hold is still surfaced so clients wait rather than give up.
    expect(body.editHoldUntil).toBeTruthy();
  });

  it("reports 'preparing' — not a hard failure — once the worker claims the job", async () => {
    // Regression: the worker flips a held session to `compiling` within a
    // second of the stop, which is the state the editor almost always
    // opens into. Reporting it as un-editable made "Edit & save" fail
    // immediately for every user.
    const s = await seedHeldSession({
      status: "compiling",
      videoUnits: null,
      originalVideoR2Key: null,
    });
    const body = await getJson(`/api/sessions/${s.token}/units`);
    expect(body.editable).toBe(false);
    expect(body.editableReason).toBe("preparing");
    expect(body.editHoldUntil).toBeTruthy();
    // The client needs this to size its progress estimate. One less than
    // the capture count: the compiler excludes the seed capture from the
    // video (see dropSeedUnit), so promising UNITS would overstate the
    // finished timelapse by a minute.
    expect(body.expectedUnits).toBe(UNITS - 1);
  });

  it("reports a failed compile as failed, not as something to wait for", async () => {
    const s = await seedHeldSession({ status: "failed" });
    const body = await getJson(`/api/sessions/${s.token}/units`);
    expect(body.editable).toBe(false);
    expect(body.editableReason).toBe("failed");
  });

  it("refuses editing once the session is published", async () => {
    const s = await seedHeldSession({
      status: "complete",
      editHoldUntil: null,
      videoR2Key: "timelapses/x/original.mp4",
    });
    const body = await getJson(`/api/sessions/${s.token}/units`);
    expect(body.editable).toBe(false);
    expect(body.editableReason).toBe("published");
    expect(body.originalVideoUrl).toBeNull();
  });

  it("refuses editing after the hold lapses", async () => {
    const s = await seedHeldSession({
      editHoldUntil: new Date(Date.now() - 1000),
    });
    const body = await getJson(`/api/sessions/${s.token}/units`);
    expect(body.editable).toBe(false);
    expect(body.editableReason).toBe("not_ready");
  });
});

describe("PUT /cuts", () => {
  it("normalizes the list and previews the post-cut tracked time", async () => {
    const s = await seedHeldSession();
    // Two adjacent intervals covering minutes 3..5 → merged; units 3 and 4
    // are cut, each worth 60 credited seconds.
    const { status, body } = await putCuts(s.token, [
      { start: iso(3), end: iso(4) },
      { start: iso(4), end: iso(5) },
    ]);
    expect(status).toBe(200);
    expect(body.cuts).toEqual([{ start: iso(3), end: iso(5) }]);
    expect(body.unitsTotal).toBe(UNITS);
    expect(body.unitsCut).toBe(2);
    expect(body.uncutTrackedSeconds).toBe(540);
    expect(body.trackedSeconds).toBe(420);

    const row = await load(s.id);
    expect(row!.cuts).toEqual([{ start: iso(3), end: iso(5) }]);
    expect(row!.cutSeconds).toBe(120);
  });

  it("flows into /timings, /:token and /batch", async () => {
    const s = await seedHeldSession();
    await putCuts(s.token, [{ start: iso(3), end: iso(5) }]);

    const session = await getJson(`/api/sessions/${s.token}`);
    expect(session.trackedSeconds).toBe(420);
    expect(session.uncutTrackedSeconds).toBe(540);
    expect(session.cutSeconds).toBe(120);

    const timings = await getJson(`/api/sessions/${s.token}/timings`);
    expect(timings.count).toBe(8);
    expect(timings.timestamps).not.toContain(iso(3));
    expect(timings.timestamps).not.toContain(iso(4));
    expect(timings.timestamps).toContain(iso(5));
    expect(timings.cutCount).toBe(2);
    expect(timings.cutTimestamps).toBeUndefined();

    const withCut = await getJson(`/api/sessions/${s.token}/timings?includeCut=true`);
    expect(withCut.cutTimestamps).toEqual([iso(3), iso(4)]);

    const batch = (
      await app.inject({
        method: "POST",
        url: "/api/sessions/batch",
        payload: { tokens: [s.token] },
      })
    ).json();
    expect(batch.sessions[0].trackedSeconds).toBe(420);
  });

  it("clears edits with an empty list", async () => {
    const s = await seedHeldSession();
    await putCuts(s.token, [{ start: iso(3), end: iso(5) }]);
    const { status, body } = await putCuts(s.token, []);
    expect(status).toBe(200);
    expect(body.trackedSeconds).toBe(540);
    const row = await load(s.id);
    expect(row!.cuts).toEqual([]);
    expect(row!.cutSeconds).toBe(0);
  });

  it("rejects a list that removes the entire timelapse", async () => {
    const s = await seedHeldSession();
    const { status, body } = await putCuts(s.token, [{ start: iso(0), end: iso(60) }]);
    expect(status).toBe(400);
    expect(body.error).toMatch(/entire timelapse/);
  });

  it("rejects malformed intervals", async () => {
    const s = await seedHeldSession();
    expect((await putCuts(s.token, [{ start: iso(5), end: iso(3) }])).status).toBe(400);
    expect((await putCuts(s.token, [{ start: "garbage", end: iso(3) }])).status).toBe(400);
  });

  it("persists privacy masks alongside cuts", async () => {
    const s = await seedHeldSession();
    const testMask = {
      id: "mask-1",
      start: iso(2),
      end: iso(5),
      x: 0.1,
      y: 0.2,
      width: 0.3,
      height: 0.4,
    };
    const { status, body } = await putCuts(s.token, [{ start: iso(3), end: iso(4) }], [testMask]);
    expect(status).toBe(200);
    expect(body.masks).toHaveLength(1);
    expect(body.masks[0]).toMatchObject({
      id: "mask-1",
      start: iso(2),
      end: iso(5),
      x: 0.1,
      y: 0.2,
      width: 0.3,
      height: 0.4,
    });

    const units = await getJson(`/api/sessions/${s.token}/units`);
    expect(units.masks).toHaveLength(1);
    expect(units.masks[0].id).toBe("mask-1");

    const row = await load(s.id);
    expect(row!.masks).toHaveLength(1);
  });

  it("cannot touch a published session", async () => {
    const s = await seedHeldSession({
      status: "complete",
      editHoldUntil: null,
      videoR2Key: "timelapses/x/original.mp4",
    });
    const { status, body } = await putCuts(s.token, [{ start: iso(3), end: iso(5) }]);
    expect(status).toBe(409);
    expect(body.error).toMatch(/published/);
    expect((await load(s.id))!.cuts).toBeNull();
  });

  it("cannot touch a session mid-publish", async () => {
    const s = await seedHeldSession({ status: "compiling" });
    expect((await putCuts(s.token, [])).status).toBe(409);
  });
});

describe("POST /compile (publish)", () => {
  it("publishes the original instantly when there are no cuts", async () => {
    const s = await seedHeldSession();
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(200);
    expect(r.json()).toMatchObject({ status: "complete", instant: true });

    const row = await load(s.id);
    expect(row!.status).toBe("complete");
    expect(row!.videoR2Key).toBe("timelapses/x/original.mp4");
    expect(row!.editHoldUntil).toBeNull();
    // No worker round-trip, so no recompile is consumed.
    expect(row!.recompileCount).toBe(0);
  });

  it("routes sessions with masks to worker compile even if there are no cuts", async () => {
    const s = await seedHeldSession();
    await putCuts(s.token, [], [{
      id: "mask-1",
      start: iso(2),
      end: iso(5),
      x: 0.1,
      y: 0.2,
      width: 0.3,
      height: 0.4,
    }]);

    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(200);
    expect(r.json()).toMatchObject({ status: "compiling", instant: false });

    const row = await load(s.id);
    expect(row!.status).toBe("compiling");
    expect(row!.recompileCount).toBe(1);
  });

  it("echoes the session's redirectUrl on both publish paths", async () => {
    // The recording client fires the redirect hook straight off this
    // response the instant publish lands, so it must carry the URL —
    // whether the publish is instant (no cuts) or a worker hand-off.
    const instant = await seedHeldSession({ redirectUrl: "https://example.com/done" });
    const r1 = await app.inject({
      method: "POST",
      url: `/api/sessions/${instant.token}/compile`,
    });
    expect(r1.json()).toMatchObject({
      status: "complete",
      instant: true,
      redirectUrl: "https://example.com/done",
    });

    const withCuts = await seedHeldSession({ redirectUrl: "https://example.com/done" });
    await putCuts(withCuts.token, [{ start: iso(3), end: iso(5) }]);
    const r2 = await app.inject({
      method: "POST",
      url: `/api/sessions/${withCuts.token}/compile`,
    });
    expect(r2.json()).toMatchObject({
      status: "compiling",
      instant: false,
      redirectUrl: "https://example.com/done",
    });

    // No redirect configured → null, not undefined or omitted.
    const none = await seedHeldSession();
    const r3 = await app.inject({
      method: "POST",
      url: `/api/sessions/${none.token}/compile`,
    });
    expect(r3.json().redirectUrl).toBeNull();
  });

  it("hands off to the worker when cuts must be baked in", async () => {
    const s = await seedHeldSession();
    await putCuts(s.token, [{ start: iso(3), end: iso(5) }]);
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(200);
    expect(r.json()).toMatchObject({ status: "compiling", instant: false });

    const row = await load(s.id);
    expect(row!.status).toBe("compiling");
    expect(row!.recompileCount).toBe(1);
    // Hold cleared so the expiry job can't publish the uncut original out
    // from under the pending cut-compile.
    expect(row!.editHoldUntil).toBeNull();
    // Still unpublished until the worker finishes.
    expect(row!.videoR2Key).toBeNull();
  });

  it("is idempotent against the expiry job winning the race", async () => {
    const s = await seedHeldSession({
      status: "complete",
      editHoldUntil: null,
      videoR2Key: "timelapses/x/original.mp4",
    });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(200);
    expect(r.json()).toMatchObject({ status: "complete", instant: true });
  });

  it("202s while a publish is already running", async () => {
    const s = await seedHeldSession({ status: "compiling" });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(202);
  });

  it("409s once the hold has lapsed", async () => {
    const s = await seedHeldSession({ editHoldUntil: new Date(Date.now() - 1000) });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(409);
  });

  it("lets the user publish mid-compile by dropping the hold", async () => {
    // "I don't want to edit after all" must work even while the preview is
    // still building: clearing the hold makes the in-flight build publish
    // when it finishes, instead of making the user wait for a preview they
    // just declined.
    const s = await seedHeldSession({
      status: "compiling",
      videoUnits: null,
      originalVideoR2Key: null,
    });
    const r = await app.inject({ method: "POST", url: `/api/sessions/${s.token}/compile` });
    expect(r.statusCode).toBe(200);
    expect(r.json().instant).toBe(false);
    expect((await load(s.id))!.editHoldUntil).toBeNull();
  });
});

describe("held sessions in status reads", () => {
  it("look unpublished, with the hold deadline attached", async () => {
    const s = await seedHeldSession();
    const status = await getJson(`/api/sessions/${s.token}/status`);
    expect(status.status).toBe("stopped");
    expect(status.videoUrl).toBeUndefined();
    expect(status.editable).toBe(true);
    expect(status.editHoldUntil).toBeTruthy();
  });
});
